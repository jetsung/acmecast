//! 触发记录仓储。
//!
//! 触发记录既是审计日志（9.7：按时间倒序查询、标明来源），也是续期触发
//! 去重（9.4）的持久化依据——「同一时间窗口内不重复触发」靠查这里最近
//! 一次同源触发的时间来判定，进程重启后去重依然成立。

use chrono::{DateTime, Utc};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait, PaginatorTrait, QueryFilter,
    QueryOrder, Set,
};

use crate::entity::{pipeline::TriggerSource, trigger_log};
use crate::error::Result;
use crate::repository::MAX_PAGE_SIZE;

/// 一条待落库的触发记录。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TriggerLogInput {
    /// 被触发的流水线。
    pub pipeline_id: i64,
    /// 触发来源。
    pub source: TriggerSource,
    /// 触发说明；例如续期触发时记录命中的证书域名集合。
    pub detail: Option<String>,
    /// 触发时间。
    pub triggered_at: DateTime<Utc>,
}

/// 一条已读出的触发记录。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TriggerLogEntry {
    /// 主键。
    pub id: i64,
    /// 被触发的流水线。
    pub pipeline_id: i64,
    /// 触发来源。
    pub source: TriggerSource,
    /// 触发说明。
    pub detail: Option<String>,
    /// 触发时间。
    pub triggered_at: DateTime<Utc>,
}

/// 一页触发记录。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TriggerLogPage {
    /// 本页记录，按触发时间倒序。
    pub items: Vec<TriggerLogEntry>,
    /// 符合条件的总条数。
    pub total: u64,
    /// 当前页码。
    pub page: u64,
    /// 每页条数（已归一）。
    pub page_size: u64,
}

/// 触发记录仓储。
#[derive(Debug)]
pub struct TriggerLogRepository<'db> {
    db: &'db DatabaseConnection,
}

impl<'db> TriggerLogRepository<'db> {
    /// 绑定数据库连接。
    #[must_use]
    pub fn new(db: &'db DatabaseConnection) -> Self {
        Self { db }
    }

    /// 落库一条触发记录，返回主键。
    pub async fn record(&self, input: TriggerLogInput) -> Result<i64> {
        let row = trigger_log::ActiveModel {
            pipeline_id: Set(input.pipeline_id),
            source: Set(input.source.as_str().to_owned()),
            detail: Set(input.detail),
            triggered_at: Set(input.triggered_at),
            ..Default::default()
        }
        .insert(self.db)
        .await?;

        Ok(row.id)
    }

    /// 某条流水线最近一次指定来源的触发时间；从未触发过则为 `None`。
    ///
    /// 续期触发去重（9.4）用它判定「同一时间窗口内是否已触发过」——查库
    /// 而不是进程内计数，重启后窗口内去重依然生效。
    pub async fn last_triggered_at(
        &self,
        pipeline_id: i64,
        source: TriggerSource,
    ) -> Result<Option<DateTime<Utc>>> {
        let row = trigger_log::Entity::find()
            .filter(trigger_log::Column::PipelineId.eq(pipeline_id))
            .filter(trigger_log::Column::Source.eq(source.as_str()))
            .order_by_desc(trigger_log::Column::TriggeredAt)
            .order_by_desc(trigger_log::Column::Id)
            .one(self.db)
            .await?;

        Ok(row.map(|entry| entry.triggered_at))
    }

    /// 分页查询触发记录，按触发时间倒序（9.7）。
    ///
    /// `pipeline_id` 为 `None` 时查全部流水线。
    pub async fn list(
        &self,
        pipeline_id: Option<i64>,
        page: u64,
        page_size: u64,
    ) -> Result<TriggerLogPage> {
        let page = page.max(1);
        let page_size = page_size.clamp(1, MAX_PAGE_SIZE);

        let mut select = trigger_log::Entity::find();
        if let Some(pipeline_id) = pipeline_id {
            select = select.filter(trigger_log::Column::PipelineId.eq(pipeline_id));
        }

        let paginator = select
            .order_by_desc(trigger_log::Column::TriggeredAt)
            // 同一时刻的记录靠主键定序，翻页不重不漏。
            .order_by_desc(trigger_log::Column::Id)
            .paginate(self.db, page_size);

        let total = paginator.num_items().await?;
        let rows = paginator.fetch_page(page - 1).await?;

        Ok(TriggerLogPage {
            items: rows.into_iter().map(entry_from_row).collect(),
            total,
            page,
            page_size,
        })
    }
}

/// 把一行记录转成领域类型。
///
/// 来源无法识别时退回「手动」而不报错：触发记录是审计数据，一条来源
/// 脏数据不该让整页查询失败；查询方需要严格口径时看原始字符串。
fn entry_from_row(row: trigger_log::Model) -> TriggerLogEntry {
    TriggerLogEntry {
        id: row.id,
        pipeline_id: row.pipeline_id,
        source: TriggerSource::parse(&row.source).unwrap_or(TriggerSource::Manual),
        detail: row.detail,
        triggered_at: row.triggered_at,
    }
}
