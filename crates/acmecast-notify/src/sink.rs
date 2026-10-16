//! 事件订阅端：运行结束时组装简讯并旁路投递。
//!
//! `WebhookEventSink` 只对运行结束事件感兴趣：`Succeeded` 投成功简讯，
//! `Failed` 投失败简讯。渠道来自流水线定义的 `notification_channels`
//! （通知类凭据 ID），运行时逐个解密取配置——不再有 `config.toml` 渠道，
//! 也不再逐步骤推送。组装与投递都在 spawn 出的任务里做：`publish` 本身
//! 必须立刻返回，这是 `EventSink` 的约定。

use acmecast_access::{CredentialRegistry, CredentialStore};
use acmecast_core::CredentialCipher;
use acmecast_pipeline::{EventSink, PipelineEvent};
use acmecast_store::entity::history;
use acmecast_store::repository::{Pipeline, PipelineRepository};
use chrono::Utc;
use sea_orm::{DatabaseConnection, EntityTrait};
use std::sync::Arc;

use crate::channel::{ChannelConfig, ChannelRegistry, default_registry};
use crate::delivery::{DeliveryOptions, deliver, http_client};
use crate::message::{
    FailureDetail, NotificationMessage, SummaryDetail, SummaryStep, SummaryStepStatus,
};
use crate::notify_credential::channel_config_from_credential;

/// `cert.apply` 步骤的类型标识；成功简讯的域名列表从它的输入取。
pub const EVENT_CERT_APPLY: &str = "cert.apply";

/// 流水线运行成功事件。
pub const EVENT_PIPELINE_SUCCEEDED: &str = "pipeline.succeeded";

/// 流水线运行失败事件。
pub const EVENT_PIPELINE_FAILED: &str = "pipeline.failed";

/// 事件标识 → 消息标题。
///
/// 新增事件时扩这里；投递路径上遇到表外事件只可能是防御性场景。
#[must_use]
pub fn event_title(event: &str) -> Option<&'static str> {
    match event {
        EVENT_PIPELINE_SUCCEEDED => Some("流水线运行成功"),
        EVENT_PIPELINE_FAILED => Some("流水线运行失败"),
        _ => None,
    }
}

/// webhook 事件订阅端。
#[derive(Debug, Clone)]
pub struct WebhookEventSink {
    db: DatabaseConnection,
    registry: Arc<ChannelRegistry>,
    credentials: Arc<CredentialRegistry>,
    cipher: Option<Arc<CredentialCipher>>,
    options: DeliveryOptions,
    client: reqwest::Client,
    global_proxy_url: Option<String>,
}

impl WebhookEventSink {
    /// 用数据库连接、凭据注册表与加密器装配。
    ///
    /// `cipher` 缺失时渠道无法解密：投递路径记 WARN 并跳过，装配本身
    /// 不失败——未配置密钥的服务本来就没有可投递的渠道。
    #[must_use]
    pub fn new(
        db: DatabaseConnection,
        credentials: Arc<CredentialRegistry>,
        cipher: Option<Arc<CredentialCipher>>,
        options: DeliveryOptions,
        global_proxy_url: Option<impl Into<String>>,
    ) -> Self {
        Self::with_registry(
            db,
            Arc::new(default_registry()),
            credentials,
            cipher,
            options,
            global_proxy_url,
        )
    }

    /// 显式注入适配器注册表（测试用）。
    #[must_use]
    pub fn with_registry(
        db: DatabaseConnection,
        registry: Arc<ChannelRegistry>,
        credentials: Arc<CredentialRegistry>,
        cipher: Option<Arc<CredentialCipher>>,
        options: DeliveryOptions,
        global_proxy_url: Option<impl Into<String>>,
    ) -> Self {
        Self {
            db,
            registry,
            credentials,
            cipher,
            options,
            client: http_client(),
            global_proxy_url: global_proxy_url.map(Into::into),
        }
    }

    /// 把查库、组装与投递挪到旁路：引擎只等「事件已受理」。
    ///
    /// 单渠道内部带重试；渠道之间顺序发送，避免多渠道同时失败时的
    /// 退避任务并发膨胀。投递是尽力而为：失败只记 WARN，不影响运行结果。
    fn spawn_delivery(
        &self,
        pipeline_id: i64,
        run_id: i64,
        event: String,
        failure: Option<FailureDetail>,
    ) {
        let db = self.db.clone();
        let registry = Arc::clone(&self.registry);
        let credentials = Arc::clone(&self.credentials);
        let cipher = self.cipher.clone();
        let options = self.options.clone();
        let client = self.client.clone();
        let global_proxy_url = self.global_proxy_url.clone();
        tokio::spawn(async move {
            // 渠道列表来自流水线定义；流水线已被删除时无从投递。
            let pipeline = match PipelineRepository::new(&db).find(pipeline_id).await {
                Ok(Some(pipeline)) => pipeline,
                Ok(None) => {
                    tracing::debug!(pipeline_id, "流水线已不存在，跳过通知投递");
                    return;
                }
                Err(error) => {
                    tracing::warn!(pipeline_id, %error, "读取流水线定义失败，跳过通知投递");
                    return;
                }
            };
            if pipeline.notification_channels.is_empty() {
                return;
            }
            let Some(cipher) = cipher else {
                tracing::warn!(
                    pipeline_id,
                    "未配置凭据加密密钥，通知渠道无法解密，运行结束简讯不投递"
                );
                return;
            };
            let channels = resolve_channels(
                &db,
                &credentials,
                &cipher,
                &pipeline.notification_channels,
                global_proxy_url.as_deref(),
            )
            .await;
            if channels.is_empty() {
                return;
            }
            let message = assemble_message(&db, &pipeline, run_id, &event, failure).await;
            for channel in &channels {
                let _ = deliver(&client, &registry, channel, &message, &options).await;
            }
        });
    }
}

#[async_trait::async_trait]
impl EventSink for WebhookEventSink {
    async fn publish(&self, event: PipelineEvent) {
        // 只关心运行结束；开始/步骤/日志事件不在推送词汇内。
        match event {
            PipelineEvent::Succeeded {
                pipeline_id,
                run_id,
                ..
            } => {
                self.spawn_delivery(
                    pipeline_id,
                    run_id,
                    EVENT_PIPELINE_SUCCEEDED.to_owned(),
                    None,
                );
            }
            PipelineEvent::Failed {
                pipeline_id,
                run_id,
                step_order,
                type_id,
                reason,
            } => {
                let failure = FailureDetail {
                    // 事件里的序号从 0 起；对外展示统一从 1 起（与运行历史一致）。
                    step: step_order + 1,
                    step_type: type_id,
                    reason,
                };
                self.spawn_delivery(
                    pipeline_id,
                    run_id,
                    EVENT_PIPELINE_FAILED.to_owned(),
                    Some(failure),
                );
            }
            _ => {}
        }
    }
}

/// 把 `notification_channels` 的凭据 ID 逐个解析为可投递的渠道配置。
///
/// 单个渠道失败（凭据被删、解密失败、字段非法）只记 WARN 并跳过，
/// 其余渠道继续投递——通知是尽力而为的旁路。
async fn resolve_channels(
    db: &DatabaseConnection,
    registry: &Arc<CredentialRegistry>,
    cipher: &Arc<CredentialCipher>,
    ids: &[i64],
    global_proxy_url: Option<&str>,
) -> Vec<ChannelConfig> {
    let store = CredentialStore::new(db, Arc::clone(registry), Arc::clone(cipher));
    let mut channels = Vec::with_capacity(ids.len());
    for id in ids {
        match store.resolve(*id).await {
            Ok(credential) => {
                match channel_config_from_credential(&credential, global_proxy_url) {
                    Ok(channel) => channels.push(channel),
                    Err(error) => {
                        tracing::warn!(credential_id = *id, %error, "通知渠道字段无法转换为渠道配置，跳过");
                    }
                }
            }
            Err(error) => {
                tracing::warn!(credential_id = *id, %error, "通知渠道凭据解析失败，跳过");
            }
        }
    }
    channels
}

/// 从库中取消息素材并组装。
///
/// 素材尽力而为：运行历史读取失败时降级为占位文案，绝不因组装失败
/// 放弃投递——「发生了什么」比「描述得多完整」更重要。
async fn assemble_message(
    db: &DatabaseConnection,
    pipeline: &Pipeline,
    run_id: i64,
    event: &str,
    failure: Option<FailureDetail>,
) -> NotificationMessage {
    let row = history::Entity::find_by_id(run_id)
        .one(db)
        .await
        .ok()
        .flatten();
    let trigger = row
        .as_ref()
        .map(|row| row.trigger_source.clone())
        .unwrap_or_else(|| "unknown".to_owned());
    // 终态落库与事件投递是并行的两条旁路：`finished_at` 已写就取它，
    // 否则用组装时刻近似（与历史展示的毫秒级差异可忽略）。
    let duration_secs = row
        .as_ref()
        .map(|row| {
            let end = row.finished_at.unwrap_or_else(Utc::now);
            (end - row.started_at).num_seconds().max(0) as u64
        })
        .unwrap_or(0);

    // 失败步序号对外从 1 起，与流水线的 `order_index`（0 起）换算。
    let failed_order = failure.as_ref().map(|detail| detail.step - 1);
    let summary = SummaryDetail {
        duration_secs,
        steps: summarize_steps(pipeline, failed_order),
    };

    NotificationMessage {
        event: event.to_owned(),
        title: event_title(event).unwrap_or("通知").to_owned(),
        pipeline: pipeline.name.clone(),
        trigger,
        occurred_at: Utc::now(),
        domains: cert_apply_domains(pipeline),
        failure,
        summary: Some(summary),
    }
}

/// 按「失败即中止」不变量推导各步骤状态。
///
/// 停用 → 跳过（无论运行是否中止）；启用且恰为失败步 → 失败；
/// 排在失败步之后 → 未执行；其余 → 成功（成功运行则全部启用步骤成功）。
fn summarize_steps(pipeline: &Pipeline, failed_order: Option<i32>) -> Vec<SummaryStep> {
    pipeline
        .steps
        .iter()
        .map(|step| SummaryStep {
            type_id: step.type_id.clone(),
            status: if !step.enabled {
                SummaryStepStatus::Skipped
            } else if Some(step.order_index) == failed_order {
                SummaryStepStatus::Failed
            } else if failed_order.is_some_and(|failed| step.order_index > failed) {
                SummaryStepStatus::NotRun
            } else {
                SummaryStepStatus::Succeeded
            },
        })
        .collect()
}

/// 本次申请的域名：取流水线定义里 `cert.apply` 步骤的输入。
fn cert_apply_domains(pipeline: &Pipeline) -> Vec<String> {
    pipeline
        .steps
        .iter()
        .find(|step| step.type_id == EVENT_CERT_APPLY)
        .and_then(|step| step.input.get("domains"))
        .and_then(|value| serde_json::from_value(value.clone()).ok())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use acmecast_store::repository::PipelineStep;

    fn pipeline_with_steps(enabled: &[bool]) -> Pipeline {
        Pipeline {
            id: 1,
            name: "示例".to_owned(),
            description: None,
            enabled: true,
            notification_channels: Vec::new(),
            steps: enabled
                .iter()
                .enumerate()
                .map(|(order, enabled)| PipelineStep {
                    id: order as i64 + 1,
                    order_index: order as i32,
                    type_id: format!("step.{order}"),
                    input: serde_json::json!({}),
                    enabled: *enabled,
                })
                .collect(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    #[test]
    fn event_title_covers_run_end_events_only() {
        assert_eq!(
            event_title(EVENT_PIPELINE_SUCCEEDED),
            Some("流水线运行成功")
        );
        assert_eq!(event_title(EVENT_PIPELINE_FAILED), Some("流水线运行失败"));
        assert_eq!(event_title(EVENT_CERT_APPLY), None);
        assert_eq!(event_title("cert.deploy"), None);
    }

    #[test]
    fn summarize_steps_marks_disabled_as_skipped() {
        let pipeline = pipeline_with_steps(&[true, false, true]);
        let steps = summarize_steps(&pipeline, None);
        assert_eq!(steps[0].status, SummaryStepStatus::Succeeded);
        assert_eq!(steps[1].status, SummaryStepStatus::Skipped);
        assert_eq!(steps[2].status, SummaryStepStatus::Succeeded);
    }

    #[test]
    fn summarize_steps_marks_failed_and_not_run_after_failure() {
        let pipeline = pipeline_with_steps(&[true, true, true, false]);
        // 第 2 步（order_index = 1）失败：之前成功、它失败、之后未执行、
        // 停用步骤仍标跳过。
        let steps = summarize_steps(&pipeline, Some(1));
        assert_eq!(steps[0].status, SummaryStepStatus::Succeeded);
        assert_eq!(steps[1].status, SummaryStepStatus::Failed);
        assert_eq!(steps[2].status, SummaryStepStatus::NotRun);
        assert_eq!(steps[3].status, SummaryStepStatus::Skipped);
    }
}
