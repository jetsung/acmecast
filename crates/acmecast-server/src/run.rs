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

use acmecast_pipeline::step::StepLogLevel;

use crate::history_cache::{HistoryCache, HistoryMeta, assemble_final_json};

/// 生成启动系统日志条目（step_index = -1）。
pub(crate) fn startup_log(
    pipeline: &Pipeline,
    history_id: i64,
    source: TriggerSource,
    created_at: chrono::DateTime<chrono::Utc>,
) -> StepLogRecord {
    let version = env!("CARGO_PKG_VERSION");
    let steps_summary: Vec<String> = pipeline
        .steps
        .iter()
        .map(|s| {
            format!(
                "{}[{}:{}]",
                s.order_index,
                s.type_id,
                if s.enabled { "enabled" } else { "disabled" }
            )
        })
        .collect();
    let message = format!(
        "流水线启动: pipeline_id={}, name={}, history_id={}, trigger={}, version={}, steps=[{}]",
        pipeline.id,
        pipeline.name,
        history_id,
        source.as_str(),
        version,
        steps_summary.join(", ")
    );
    let output = serde_json::json!({
        "pipeline_id": pipeline.id,
        "pipeline_name": pipeline.name,
        "history_id": history_id,
        "trigger_source": source.as_str(),
        "version": version,
        "steps": pipeline.steps.iter().map(|s| serde_json::json!({
            "order_index": s.order_index,
            "type_id": s.type_id,
            "enabled": s.enabled,
        })).collect::<Vec<_>>(),
    });
    StepLogRecord {
        step_index: -1,
        level: StepLogLevel::Info,
        message,
        created_at,
        output: Some(output),
    }
}

/// 创建一条 `running` 的运行历史并写入启动日志，返回其主键。
///
/// 终态由 [`execute_run`] **原地**写回同一条记录：另插一行会让这条记录
/// 永远停在 running，所有「查运行中」的判定都会把流水线当成还在跑。
pub(crate) async fn start_run(
    db: &DatabaseConnection,
    pipeline: &Pipeline,
    source: TriggerSource,
    cache: Option<&HistoryCache>,
    broadcaster: Option<&crate::history_stream::SseBroadcaster>,
) -> acmecast_pipeline::Result<i64> {
    let now = chrono::Utc::now();
    let history_id = HistoryRepository::new(db)
        .record(HistoryRecord {
            pipeline_id: pipeline.id,
            trigger_source: source,
            status: RunStatus::Running,
            started_at: now,
            finished_at: None,
            error_message: None,
            logs: Vec::new(),
        })
        .await?;

    let log = startup_log(pipeline, history_id, source, now);
    HistoryRepository::new(db).append_log(history_id, log.clone()).await?;

    if let Some(cache) = cache {
        let meta = HistoryMeta {
            id: history_id,
            pipeline_id: pipeline.id,
            trigger_source: source,
            status: RunStatus::Running,
            started_at: now,
            finished_at: None,
            error_message: None,
        };
        cache.set_meta(&meta).await;
        cache.push_log(history_id, &log).await;
    }

    if let Some(broadcaster) = broadcaster {
        broadcaster.broadcast_started(pipeline.id, history_id, source.as_str(), now);
        broadcaster.broadcast_log(pipeline.id, history_id, &log);
    }

    Ok(history_id)
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
/// 是否为 `round <n> miss` 日志（锚定 `round `+数字+` miss` 且含 `resolvers:`）。
fn is_round_miss(message: &str) -> bool {
    if !message.contains("resolvers:") {
        return false;
    }
    let mut from = 0usize;
    while let Some(rel) = message[from..].find("round ") {
        let pos = from + rel;
        let after = &message[pos + "round ".len()..];
        let digits = after.chars().take_while(|c| c.is_ascii_digit()).count();
        if digits > 0 && after[digits..].starts_with(" miss") {
            return true;
        }
        from = pos + "round ".len();
        if from >= message.len() {
            break;
        }
    }
    false
}

/// 入库前精简：仅保留最后一条 `round * miss`，其余 miss 日志移除。
///
/// 双轨语义：SSE 实时推送在 `RunExecutionSink` 中已全量写入
/// `HistoryCache::push_log` 与 `SseBroadcaster::broadcast_log`，此函数仅
/// 在 `execute_run` 的终态组装阶段对 `step_logs` 做精简，不影响已广播的实时流。
pub(crate) fn compact_propagation_miss_logs(logs: &mut Vec<StepLogRecord>) {
    let Some(keep) = logs.iter().rposition(|log| is_round_miss(&log.message)) else {
        return;
    };
    let mut idx = 0usize;
    logs.retain(|log| {
        let is_miss = is_round_miss(&log.message);
        let keep_this = !(is_miss && idx != keep);
        idx += 1;
        keep_this
    });
}


/// 入库前精简：仅保留最后一条 `proxy: ` 代理行，其余移除。
///
/// 与 [`compact_propagation_miss_logs`] 同一双轨策略：SSE 实时流全量保留
/// （DoH 回退行与凭据代理行多行独立可视），落库前只留最后一条代理行。
pub(crate) fn compact_proxy_logs(logs: &mut Vec<StepLogRecord>) {
    let Some(keep) = logs
        .iter()
        .rposition(|log| log.message.starts_with("proxy: "))
    else {
        return;
    };
    let mut idx = 0usize;
    logs.retain(|log| {
        let is_proxy = log.message.starts_with("proxy: ");
        let keep_this = !(is_proxy && idx != keep);
        idx += 1;
        keep_this
    });
}

/// 执行过程事件分流：向外部 `EventSink` 透传的同时，将增量日志写入 `HistoryCache`，并广播至 SSE。
/// 双轨语义：此处的 `Log` 分支保持全量 `round * miss`（每一轮 miss 均写入缓存与 SSE），
/// 精简仅发生在 `execute_run` 终态的 `compact_propagation_miss_logs` 阶段，不在此处过滤。
#[derive(Debug)]
struct RunExecutionSink<'a> {
    cache: Option<&'a HistoryCache>,
    broadcaster: Option<&'a crate::history_stream::SseBroadcaster>,
    upstream: Option<&'a dyn acmecast_pipeline::EventSink>,
}

#[async_trait::async_trait]
impl<'a> acmecast_pipeline::EventSink for RunExecutionSink<'a> {
    async fn publish(&self, event: acmecast_pipeline::PipelineEvent) {
        if let (acmecast_pipeline::PipelineEvent::Log { run_id, log, .. }, Some(cache)) = (&event, self.cache) {
            cache.push_log(*run_id, log).await;
        }
        if let Some(broadcaster) = self.broadcaster {
            broadcaster.publish(event.clone()).await;
        }
        if let Some(upstream) = self.upstream {
            upstream.publish(event).await;
        }
    }
}

/// 执行一条已定义的流水线，并把终态与各步骤日志写回 `history_id`。
///
/// 流水线**本身**失败（某步骤报错）属于正常业务结果，返回 `Ok(outcome)`；
/// `Err` 留给执行器或存储层出问题的情况——那时历史会停在 running，由调用方
/// 决定重试策略（cron 触发靠它保留触发点）。
///
/// `events` 是可选的事件订阅端（如 webhook 通知）；
/// `cache` 是可选的 Redis / Valkey 缓存层；
/// `broadcaster` 是可选的 SSE 广播器。
#[allow(clippy::too_many_arguments)]
pub(crate) async fn execute_run(
    db: &DatabaseConnection,
    steps: &StepRegistry,
    credentials: &CredentialStore<'_>,
    definition: &PipelineDefinition,
    history_id: i64,
    source: TriggerSource,
    events: Option<&dyn acmecast_pipeline::EventSink>,
    cache: Option<&HistoryCache>,
    broadcaster: Option<&crate::history_stream::SseBroadcaster>,
) -> acmecast_pipeline::Result<RunOutcome> {
    let state = DatabaseStateStore::new(db);
    let execution_sink = RunExecutionSink {
        cache,
        broadcaster,
        upstream: events,
    };
    let mut runner = PipelineRunner::new(steps, credentials, &state).with_trigger(source);
    runner = runner.with_events(&execution_sink);

    let outcome = runner.run(definition, history_id).await?;

    let final_status = if outcome.is_success() {
        RunStatus::Success
    } else {
        RunStatus::Failed
    };
    let error_msg = failure_message(&outcome);
    let finished_at = chrono::Utc::now();

    let started_at = if let Some(cache) = cache {
        cache
            .get_meta(history_id)
            .await
            .map(|m| m.started_at)
            .unwrap_or(finished_at)
    } else {
        finished_at
    };

    let mut step_logs: Vec<StepLogRecord> = outcome
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
    // 入库前精简：仅保留最后一条 `round * miss` 与最后一条 `proxy: `，
    // 不影响 SSE 已推送的实时流。
    compact_propagation_miss_logs(&mut step_logs);
    compact_proxy_logs(&mut step_logs);
    let final_meta = HistoryMeta {
        id: history_id,
        pipeline_id: definition.id,
        trigger_source: source,
        status: final_status,
        started_at,
        finished_at: Some(finished_at),
        error_message: error_msg.clone(),
    };
    let final_json = assemble_final_json(&final_meta, &step_logs);
    tracing::debug!(history_id, "流水线终态整条 JSON 组装完成: {final_json}");

    // 终态以整条 JSON 原子落库，落库失败重试一次
    let finish_attempt = HistoryRepository::new(db)
        .finish(
            history_id,
            final_status,
            error_msg.clone(),
            step_logs.clone(),
        )
        .await;

    let finish_res = match finish_attempt {
        Ok(()) => Ok(()),
        Err(err) => {
            tracing::warn!(history_id, %err, "原子落库失败，重试一次");
            HistoryRepository::new(db)
                .finish(history_id, final_status, error_msg, step_logs)
                .await
        }
    };

    match finish_res {
        Ok(()) => {
            if let Some(cache) = cache {
                cache.finalize(history_id, Some(&final_meta)).await;
            }
        }
        Err(err) => {
            tracing::error!(
                history_id,
                %err,
                "原子落库重试后仍失败，保留缓存供后续补偿查询"
            );
            return Err(err);
        }
    }

    Ok(outcome)
}

#[cfg(test)]
mod tests {
    use super::*;
    use acmecast_store::repository::PipelineStep;

    #[test]
    fn startup_log_structure() {
        let pipeline = Pipeline {
            id: 42,
            name: "test-pipeline".to_owned(),
            description: None,
            enabled: true,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            steps: vec![
                PipelineStep {
                    id: 1,
                    order_index: 0,
                    type_id: "cert.apply".to_owned(),
                    input: serde_json::json!({}),
                    enabled: true,
                },
                PipelineStep {
                    id: 2,
                    order_index: 1,
                    type_id: "cert.deploy".to_owned(),
                    input: serde_json::json!({}),
                    enabled: false,
                },
            ],
        };
        let log = startup_log(&pipeline, 100, TriggerSource::Manual, chrono::Utc::now());
        assert_eq!(log.step_index, -1);
        assert_eq!(log.level, StepLogLevel::Info);
        assert!(log.message.contains("pipeline_id=42"));
        assert!(log.message.contains("name=test-pipeline"));
        assert!(log.message.contains("history_id=100"));
        assert!(log.message.contains("trigger=manual"));
        assert!(log.message.contains("0[cert.apply:enabled]"));
        assert!(log.message.contains("1[cert.deploy:disabled]"));
        let out = log.output.unwrap();
        assert_eq!(out["pipeline_id"], 42);
        assert_eq!(out["history_id"], 100);
        assert_eq!(out["trigger_source"], "manual");
    }
    #[test]
    fn compact_no_miss_unchanged() {
        let mut logs = vec![make_log("propagation start timeout=600 interval=10 (record: _acme-challenge.example.com)"), make_log("round 1 hit (resolver: 阿里 DNS)"), make_log("propagation succeeded after 1 rounds")];
        let len = logs.len();
        compact_propagation_miss_logs(&mut logs);
        assert_eq!(logs.len(), len);
    }

    /// 多条 `proxy: ` 行只留最后一条；无/单条代理行为 no-op。
    #[test]
    fn compact_proxy_logs_keeps_last_only() {
        // 多条 → 仅保留最后一条。
        let mut logs = vec![
            make_log("proxy: via socks5://1.2.3.4:7890"),
            make_log("round 1 hit (resolver: Cloudflare DNS)"),
            make_log("proxy: via socks5://5.6.7.8:1080"),
            make_log("propagation succeeded after 1 rounds"),
        ];
        compact_proxy_logs(&mut logs);
        let proxies: Vec<_> = logs.iter().filter(|l| l.message.starts_with("proxy: ")).collect();
        assert_eq!(proxies.len(), 1, "{logs:?}");
        assert!(proxies[0].message.contains("5.6.7.8"), "{logs:?}");
        assert_eq!(logs.len(), 3, "其余日志不受影响: {logs:?}");

        // 无代理行 → no-op。
        let mut none = vec![make_log("round 1 hit (resolver: 阿里 DNS)")];
        let len = none.len();
        compact_proxy_logs(&mut none);
        assert_eq!(none.len(), len);

        // 单条 → no-op。
        let mut single = vec![
            make_log("propagation start timeout=600 interval=10"),
            make_log("proxy: via http://10.0.0.1:3128"),
            make_log("round 1 hit (resolver: 阿里 DNS)"),
        ];
        let len = single.len();
        compact_proxy_logs(&mut single);
        assert_eq!(single.len(), len, "单条代理行应原样保留: {single:?}");
    }

    #[test]
    fn compact_single_miss_unchanged() {
        let mut logs = vec![make_log("propagation start timeout=600 interval=10 (record: _acme-challenge.example.com) resolvers: 阿里 DNS"), make_log("round 1 miss (resolvers: 阿里 DNS)"), make_log("round 1 hit (resolver: 阿里 DNS)")];
        compact_propagation_miss_logs(&mut logs);
        assert_eq!(logs.iter().filter(|l| is_round_miss(&l.message)).count(), 1);
    }

    #[test]
    fn compact_two_miss_keeps_last() {
        let mut logs = vec![
            make_log("propagation start timeout=600 interval=10 (record: _acme-challenge.example.com) resolvers: 阿里 DNS"),
            make_log("round 1 miss (resolvers: 阿里 DNS)"),
            make_log("round 2 miss (resolvers: 阿里 DNS)"),
            make_log("round 2 hit (resolver: 阿里 DNS)"),
        ];
        compact_propagation_miss_logs(&mut logs);
        let misses: Vec<_> = logs.iter().filter(|l| is_round_miss(&l.message)).collect();
        assert_eq!(misses.len(), 1, "{logs:?}");
        assert!(misses[0].message.contains("round 2 miss"), "{misses:?}");
    }

    #[test]
    fn compact_23_miss_keeps_only_last() {
        let mut logs: Vec<StepLogRecord> = vec![make_log("propagation start timeout=600 interval=10 (record: _acme-challenge.ooos.top) authoritative: 权威 NS（ooos.top） resolvers: Cloudflare DNS, 阿里 DNS")];
        for i in 1..=23 {
            logs.push(make_log(format!("round {i} miss (resolvers: 权威 NS（ooos.top）)")));
        }
        logs.push(make_log("propagation succeeded after 23 rounds"));
        compact_propagation_miss_logs(&mut logs);
        let misses: Vec<_> = logs.iter().filter(|l| is_round_miss(&l.message)).collect();
        assert_eq!(misses.len(), 1, "仅保留最后一条 miss: {logs:?}");
        assert!(misses[0].message.contains("round 23 miss"), "{misses:?}");
        assert!(misses[0].message.contains("权威 NS（ooos.top）"), "{misses:?}");
        // 非 miss 不受影响
        assert!(logs.iter().any(|l| l.message.contains("propagation start")));
        assert!(logs.iter().any(|l| l.message.contains("propagation succeeded")));
    }

    #[test]
    fn compact_mixed_hit_not_removed() {
        let mut logs = vec![
            make_log("propagation start timeout=600 interval=10 (record: _acme-challenge.example.com) resolvers: 阿里 DNS"),
            make_log("round 1 miss (resolvers: 阿里 DNS)"),
            make_log("round 2 miss (resolvers: 阿里 DNS)"),
            make_log("round 2 hit (resolver: 阿里 DNS)"),
            make_log("round 3 miss (resolvers: 阿里 DNS)"),
            make_log("round 3 hit (resolver: 阿里 DNS)"),
        ];
        compact_propagation_miss_logs(&mut logs);
        let misses: Vec<_> = logs.iter().filter(|l| is_round_miss(&l.message)).collect();
        assert_eq!(misses.len(), 1);
        assert!(misses[0].message.contains("round 3 miss"));
        assert_eq!(logs.iter().filter(|l| l.message.contains(" hit ")).count(), 2);
    }

    #[test]
    fn compact_round_substring_not_miss() {
        let mut logs = vec![
            make_log("round 1 miss (resolvers: 权威 NS（ooos.top）)"),
            make_log("certificate issued, round trip ok"),
            make_log("round-trip latency 12ms"),
            make_log("round 2 miss (resolvers: 权威 NS（ooos.top）)"),
        ];
        compact_propagation_miss_logs(&mut logs);
        assert_eq!(logs.len(), 3, "含 round 子串的非 miss 不应被误删: {logs:?}");
        assert!(logs.iter().any(|l| l.message.contains("round trip")));
        assert!(logs.iter().any(|l| l.message.contains("round-trip")));
        let misses: Vec<_> = logs.iter().filter(|l| is_round_miss(&l.message)).collect();
        assert_eq!(misses.len(), 1);
        assert!(misses[0].message.contains("round 2 miss"));
    }

    fn make_log(msg: impl Into<String>) -> StepLogRecord {
        StepLogRecord { step_index: 0, level: StepLogLevel::Info, message: msg.into(), created_at: chrono::Utc::now(), output: None }
    }
}
