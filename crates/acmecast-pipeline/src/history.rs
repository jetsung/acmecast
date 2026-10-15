//! 运行历史与步骤日志的落库与查询。

use acmecast_store::entity::{history, history_log, pipeline::TriggerSource};
use acmecast_store::repository::{DEFAULT_PAGE_SIZE, MAX_PAGE_SIZE};
use chrono::{DateTime, Utc};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait, PaginatorTrait, QueryFilter,
    QueryOrder, Set, TransactionTrait,
};

use crate::error::{Error, Result};
use crate::runner::RunOutcome;
use crate::step::StepLogLevel;

/// 一条待落库或已读出的步骤日志。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct StepLogRecord {
    /// 属于哪一步，与流水线步骤的 `order_index` 对应。
    pub step_index: i32,
    /// 级别。
    pub level: StepLogLevel,
    /// 内容。
    pub message: String,
    /// 产生时间。
    pub created_at: DateTime<Utc>,
    /// 结构化输出；步骤产生可程序化读取的产物摘要时附带。
    pub output: Option<serde_json::Value>,
}

/// 一次运行的待落库内容。
#[derive(Debug, Clone)]
pub struct HistoryRecord {
    /// 所属流水线。
    pub pipeline_id: i64,
    /// 触发来源。
    pub trigger_source: TriggerSource,
    /// 运行结果。
    pub status: history::RunStatus,
    /// 开始时间。
    pub started_at: DateTime<Utc>,
    /// 结束时间；仍在运行中时为 `None`。
    pub finished_at: Option<DateTime<Utc>>,
    /// 失败原因摘要。
    pub error_message: Option<String>,
    /// 各步骤的日志。
    pub logs: Vec<StepLogRecord>,
}

impl HistoryRecord {
    /// 把一次运行的结果转成待落库的记录。
    ///
    /// 失败摘要取 [`RunOutcome::failure`] 的原因，并冠上失败步骤的类型与序号——
    /// 列表里只看到「内部错误」而无从知道是哪一步，等于没记。
    #[must_use]
    pub fn from_outcome(
        pipeline_id: i64,
        trigger_source: TriggerSource,
        outcome: &RunOutcome,
        started_at: DateTime<Utc>,
    ) -> Self {
        let status = if outcome.is_success() {
            history::RunStatus::Success
        } else {
            history::RunStatus::Failed
        };

        let error_message = outcome.failure.as_ref().map(|failure| {
            format!(
                "第 {} 步（{}）失败：{}",
                failure.step_order + 1,
                failure.type_id,
                failure.reason
            )
        });

        // 日志按步骤归属。每条都带上它属于哪一步，否则运行历史里
        // 只剩一堆无主的文本，看不出是谁说的。
        let logs = outcome
            .steps
            .iter()
            .flat_map(|step| {
                step.logs.iter().map(move |log| StepLogRecord {
                    step_index: step.order_index,
                    level: log.level,
                    message: log.message.clone(),
                    created_at: log.created_at,
                    output: log.output.clone(),
                })
            })
            .collect();

        Self {
            pipeline_id,
            trigger_source,
            status,
            started_at,
            finished_at: Some(Utc::now()),
            error_message,
            logs,
        }
    }
}

/// 历史查询条件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryQuery {
    /// 只看某条流水线；`None` 表示全部。
    pub pipeline_id: Option<i64>,
    /// 页码，从 1 起。
    pub page: u64,
    /// 每页条数。
    pub page_size: u64,
}

impl Default for HistoryQuery {
    fn default() -> Self {
        Self {
            pipeline_id: None,
            page: 1,
            page_size: DEFAULT_PAGE_SIZE,
        }
    }
}

/// 一条运行历史（概要，不含日志）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryEntry {
    /// 主键。
    pub id: i64,
    /// 所属流水线。
    pub pipeline_id: i64,
    /// 触发来源。
    pub trigger_source: TriggerSource,
    /// 运行结果。
    pub status: history::RunStatus,
    /// 开始时间。
    pub started_at: DateTime<Utc>,
    /// 结束时间。
    pub finished_at: Option<DateTime<Utc>>,
    /// 失败原因摘要。
    pub error_message: Option<String>,
}

/// 一页运行历史。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryPage {
    /// 本页记录，按开始时间倒序。
    pub items: Vec<HistoryEntry>,
    /// 符合条件的总条数。
    pub total: u64,
    /// 当前页码。
    pub page: u64,
    /// 每页条数（已归一）。
    pub page_size: u64,
}

/// 运行历史仓储。
#[derive(Debug)]
pub struct HistoryRepository<'db> {
    db: &'db DatabaseConnection,
}

impl<'db> HistoryRepository<'db> {
    /// 绑定数据库连接。
    #[must_use]
    pub fn new(db: &'db DatabaseConnection) -> Self {
        Self { db }
    }

    /// 落库一次运行（历史一条 + 其全部日志），返回历史主键。
    ///
    /// 整个写入在一个事务里：半条历史（比如有记录但日志没写进去）比没有更糟，
    /// 排查时会被它误导。
    pub async fn record(&self, record: HistoryRecord) -> Result<i64> {
        let transaction = self.db.begin().await?;

        let history_id = history::ActiveModel {
            pipeline_id: Set(record.pipeline_id),
            trigger_source: Set(record.trigger_source.as_str().to_owned()),
            status: Set(record.status.as_str().to_owned()),
            started_at: Set(record.started_at),
            finished_at: Set(record.finished_at),
            error_message: Set(record.error_message),
            ..Default::default()
        }
        .insert(&transaction)
        .await?
        .id;

        for log in record.logs {
            history_log::ActiveModel {
                history_id: Set(history_id),
                step_index: Set(log.step_index),
                level: Set(log.level.as_str().to_owned()),
                message: Set(log.message),
                output: Set(log.output),
                created_at: Set(log.created_at),
                ..Default::default()
            }
            .insert(&transaction)
            .await?;
        }

        transaction.commit().await?;
        Ok(history_id)
    }

    /// 往已有运行追加单条日志（如启动日志）。
    pub async fn append_log(&self, history_id: i64, log: StepLogRecord) -> Result<()> {
        history_log::ActiveModel {
            history_id: Set(history_id),
            step_index: Set(log.step_index),
            level: Set(log.level.as_str().to_owned()),
            message: Set(log.message),
            output: Set(log.output),
            created_at: Set(log.created_at),
            ..Default::default()
        }
        .insert(self.db)
        .await?;
        Ok(())
    }

    /// 把一条**运行中**的历史更新为终态，并补写日志。
    ///
    /// 运行记录的生命周期是「先插 running、跑完原地改终态」：如果终态另插
    /// 一条新记录，原 running 行会永远停在 running，所有「查运行中」的
    /// 判定（如调度触发的去重）都会把这条流水线当成还在运行。
    /// 整个更新在事务里：状态改了、日志没进去同样会误导排查。
    pub async fn finish(
        &self,
        history_id: i64,
        status: history::RunStatus,
        error_message: Option<String>,
        logs: Vec<StepLogRecord>,
    ) -> Result<()> {
        let transaction = self.db.begin().await?;

        let row = history::Entity::find_by_id(history_id)
            .one(&transaction)
            .await?
            .ok_or_else(|| {
                Error::Core(acmecast_core::Error::Internal(format!(
                    "运行历史 {history_id} 不存在，无法写入终态"
                )))
            })?;

        history::ActiveModel {
            id: Set(row.id),
            status: Set(status.as_str().to_owned()),
            finished_at: Set(Some(Utc::now())),
            error_message: Set(error_message),
            ..Default::default()
        }
        .update(&transaction)
        .await?;

        for log in logs {
            history_log::ActiveModel {
                history_id: Set(history_id),
                step_index: Set(log.step_index),
                level: Set(log.level.as_str().to_owned()),
                message: Set(log.message),
                output: Set(log.output),
                created_at: Set(log.created_at),
                ..Default::default()
            }
            .insert(&transaction)
            .await?;
        }

        transaction.commit().await?;
        Ok(())
    }

    /// 分页查询历史，按主键倒序（保证顺序稳定）。
    pub async fn list(&self, query: HistoryQuery) -> Result<HistoryPage> {
        let page = query.page.max(1);
        let page_size = query.page_size.clamp(1, MAX_PAGE_SIZE);

        let mut select = history::Entity::find();
        if let Some(pipeline_id) = query.pipeline_id {
            select = select.filter(history::Column::PipelineId.eq(pipeline_id));
        }

        let paginator = select
            // 按主键倒序，ID 最大的记录排在最前；翻页不重不漏。
            .order_by_desc(history::Column::Id)
            .paginate(self.db, page_size);

        let total = paginator.num_items().await?;
        let rows = paginator.fetch_page(page - 1).await?;

        let items = rows
            .into_iter()
            .map(|row| {
                Ok(HistoryEntry {
                    id: row.id,
                    pipeline_id: row.pipeline_id,
                    trigger_source: parse_trigger(&row.trigger_source)?,
                    status: parse_status(&row.status)?,
                    started_at: row.started_at,
                    finished_at: row.finished_at,
                    error_message: row.error_message,
                })
            })
            .collect::<Result<Vec<_>>>()?;

        Ok(HistoryPage {
            items,
            total,
            page,
            page_size,
        })
    }

    /// 某条流水线是否还有**运行中**的记录。
    ///
    /// 调度引擎触发前用它去重（9.4：运行中的流水线不被重复触发），手动触发
    /// 同样以它挡并发（409）。判定只看 `status = running`——正常流程下运行
    /// 结束会被原地更新为终态；跨进程遗留的 running（进程崩溃/被杀后不可能
    /// 再有执行者写回终态）由服务启动时的 [`Self::fail_stale_running`] 统一
    /// 标记为失败。因此「运行中」的判定只覆盖本进程生命周期，正是确实可能
    /// 有执行者在跑的区间。
    pub async fn has_running(&self, pipeline_id: i64) -> Result<bool> {
        let row = history::Entity::find()
            .filter(history::Column::PipelineId.eq(pipeline_id))
            .filter(history::Column::Status.eq(history::RunStatus::Running.as_str()))
            .one(self.db)
            .await?;

        Ok(row.is_some())
    }

    /// 把跨进程遗留的 `running` 历史统一标记为失败，返回标记条数。
    ///
    /// 运行历史「先插 running、跑完原地改终态」的生命周期依附于进程：进程
    /// 崩溃或被杀后，遗留的 running 记录不可能再有执行者回来写终态，却会让
    /// [`Self::has_running`] 永远判「运行中」——手动触发 409、调度触发跳过，
    /// 流水线从此无法再触发。本方法只在服务启动时调用（此刻不可能有任何
    /// 流水线在本进程内运行，清点全部 running 是安全的）；逐条失败仅记日志，
    /// 不阻断启动。
    pub async fn fail_stale_running(&self) -> Result<u64> {
        let stale = history::Entity::find()
            .filter(history::Column::Status.eq(history::RunStatus::Running.as_str()))
            .all(self.db)
            .await?;
        if stale.is_empty() {
            return Ok(0);
        }

        let mut marked = 0_u64;
        for row in stale {
            let result = history::ActiveModel {
                id: Set(row.id),
                status: Set(history::RunStatus::Failed.as_str().to_owned()),
                finished_at: Set(Some(Utc::now())),
                error_message: Set(Some(
                    "服务重启时运行中断：执行进程已退出，实际结果未知".to_owned(),
                )),
                ..Default::default()
            }
            .update(self.db)
            .await;
            match result {
                Ok(_) => marked += 1,
                Err(error) => tracing::warn!(
                    history_id = row.id,
                    %error,
                    "遗留的运行中历史标记为失败时出错"
                ),
            }
        }
        Ok(marked)
    }

    /// 读某次运行的全部日志，按产生时间与步骤序号排序。
    pub async fn logs_of(&self, history_id: i64) -> Result<Vec<StepLogRecord>> {
        let rows = history_log::Entity::find()
            .filter(history_log::Column::HistoryId.eq(history_id))
            .order_by_asc(history_log::Column::CreatedAt)
            .order_by_asc(history_log::Column::Id)
            .all(self.db)
            .await?;

        rows.into_iter()
            .map(|row| {
                Ok(StepLogRecord {
                    step_index: row.step_index,
                    level: StepLogLevel::parse(&row.level).ok_or_else(|| {
                        Error::Core(acmecast_core::Error::Internal(format!(
                            "日志 {} 的级别 `{}` 无法识别",
                            row.id, row.level
                        )))
                    })?,
                    message: row.message,
                    created_at: row.created_at,
                    output: row.output,
                })
            })
            .collect()
    }
}

/// 解析触发来源，无法识别时报错而不是默认成某个值——
/// 库里的脏数据该暴露出来，静默归类会让统计口径悄悄出错。
fn parse_trigger(raw: &str) -> Result<TriggerSource> {
    TriggerSource::parse(raw).ok_or_else(|| {
        Error::Core(acmecast_core::Error::Internal(format!(
            "历史记录里的触发来源 `{raw}` 无法识别"
        )))
    })
}

/// 解析运行状态。
fn parse_status(raw: &str) -> Result<history::RunStatus> {
    history::RunStatus::parse(raw).ok_or_else(|| {
        Error::Core(acmecast_core::Error::Internal(format!(
            "历史记录里的状态 `{raw}` 无法识别"
        )))
    })
}
