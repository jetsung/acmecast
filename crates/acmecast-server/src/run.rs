//! 流水线运行的公共装配：创建运行历史、执行、写回终态。
//!
//! 调度触发的运行与手动触发的运行走的是同一条链路——两处各写一遍
//! 「先插 running、跑完原地改终态」必然漂移，而漂移的后果是运行记录
//! 停在 running 或日志丢失，两者都很难从现象反推回原因。

use acmecast_access::CredentialStore;
use acmecast_pipeline::{
    DatabaseStateStore, HistoryRecord, HistoryRepository, PipelineDefinition, PipelineRunner,
    RunOutcome, StepDefinition, StepLogRecord, StepRegistry,
};
use acmecast_store::entity::history::RunStatus;
use acmecast_store::entity::pipeline::TriggerSource;
use acmecast_store::repository::Pipeline;
use sea_orm::DatabaseConnection;

/// 把读出的流水线定义转成执行器需要的形态。
///
/// 执行顺序由各步的 `order_index` 决定，与数组顺序无关。
#[must_use]
pub(crate) fn definition_of(pipeline: &Pipeline) -> PipelineDefinition {
    PipelineDefinition {
        id: pipeline.id,
        steps: pipeline
            .steps
            .iter()
            .map(|step| StepDefinition {
                order_index: step.order_index,
                type_id: step.type_id.clone(),
                input: step.input.clone(),
                enabled: step.enabled,
            })
            .collect(),
    }
}

/// 创建一条 `running` 的运行历史，返回其主键。
///
/// 终态由 [`execute_run`] **原地**写回同一条记录：另插一行会让这条记录
/// 永远停在 running，所有「查运行中」的判定都会把流水线当成还在跑。
pub(crate) async fn start_run(
    db: &DatabaseConnection,
    pipeline_id: i64,
    source: TriggerSource,
) -> acmecast_pipeline::Result<i64> {
    HistoryRepository::new(db)
        .record(HistoryRecord {
            pipeline_id,
            trigger_source: source,
            status: RunStatus::Running,
            started_at: chrono::Utc::now(),
            finished_at: None,
            error_message: None,
            logs: Vec::new(),
        })
        .await
}

/// 失败摘要：冠上失败步骤的序号与类型。
///
/// 列表里只看到「内部错误」而无从知道是哪一步，等于没记。
#[must_use]
fn failure_summary(outcome: &RunOutcome) -> Option<String> {
    outcome.failure.as_ref().map(|failure| {
        format!(
            "第 {} 步（{}）：{}",
            failure.step_order + 1,
            failure.type_id,
            failure.reason
        )
    })
}

/// 运行历史里要落库的失败摘要。
///
/// 成功时为 `None`；失败但拿不到具体原因（理论上不可达）时也要给出
/// 「未知原因」——留空会让运行历史看起来像一次没有解释的失败。
#[must_use]
pub(crate) fn failure_message(outcome: &RunOutcome) -> Option<String> {
    if outcome.is_success() {
        return None;
    }
    Some(failure_summary(outcome).unwrap_or_else(|| "未知原因".to_owned()))
}

/// 执行一条已定义的流水线，并把终态与各步骤日志写回 `history_id`。
///
/// 流水线**本身**失败（某步骤报错）属于正常业务结果，返回 `Ok(outcome)`；
/// `Err` 留给执行器或存储层出问题的情况——那时历史会停在 running，由调用方
/// 决定重试策略（cron 触发靠它保留触发点）。
///
/// `events` 是可选的 webhook 订阅端（来自 [`crate::RuntimeState::notifier`]）：
/// 手动与调度两条触发路径都必须透传，通知才不会因触发方式而缺席。
pub(crate) async fn execute_run(
    db: &DatabaseConnection,
    steps: &StepRegistry,
    credentials: &CredentialStore<'_>,
    definition: &PipelineDefinition,
    history_id: i64,
    source: TriggerSource,
    events: Option<&dyn acmecast_pipeline::EventSink>,
) -> acmecast_pipeline::Result<RunOutcome> {
    let state = DatabaseStateStore::new(db);
    let mut runner = PipelineRunner::new(steps, credentials, &state).with_trigger(source);
    if let Some(sink) = events {
        runner = runner.with_events(sink);
    }
    let outcome = runner.run(definition, history_id).await?;

    HistoryRepository::new(db)
        .finish(
            history_id,
            if outcome.is_success() {
                RunStatus::Success
            } else {
                RunStatus::Failed
            },
            failure_message(&outcome),
            outcome
                .steps
                .iter()
                .flat_map(|step| {
                    step.logs.iter().map(move |log| StepLogRecord {
                        step_index: step.order_index,
                        level: log.level,
                        message: log.message.clone(),
                        created_at: log.created_at,
                    })
                })
                .collect(),
        )
        .await?;

    Ok(outcome)
}
