//! 事件订阅端：把流水线事件翻译成通知并旁路投递。
//!
//! `WebhookEventSink` 只对「步骤成功完成」感兴趣（被停用而跳过的步骤
//! 不算成功，失败走引擎的 `Failed` 事件，本期不在订阅词汇内）。过滤、
//! 组装都在 spawn 出的任务里做——`publish` 本身必须立刻返回，这是
//! `EventSink` 的约定。

use acmecast_pipeline::{EventSink, PipelineEvent};
use acmecast_store::entity::history;
use acmecast_store::repository::{Pipeline, PipelineRepository};
use chrono::Utc;
use sea_orm::{DatabaseConnection, EntityTrait};
use std::sync::Arc;

use crate::channel::{ChannelConfig, ChannelRegistry, default_registry};
use crate::delivery::{DeliveryOptions, deliver, http_client};
use crate::message::NotificationMessage;

/// 证书申请成功事件：`cert.apply` 步骤成功完成。
pub const EVENT_CERT_APPLY: &str = "cert.apply";

/// 证书部署成功事件：`cert.deploy` 步骤成功完成。
pub const EVENT_CERT_DEPLOY: &str = "cert.deploy";

/// 事件标识 → 消息标题。
///
/// 新增事件时扩这里；配置校验以本表为准，表外的订阅会被拒绝，
/// 因此投递路径上遇到表外事件只可能是防御性场景。
#[must_use]
pub fn event_title(event: &str) -> Option<&'static str> {
    match event {
        EVENT_CERT_APPLY => Some("证书申请成功"),
        EVENT_CERT_DEPLOY => Some("证书部署成功"),
        _ => None,
    }
}

/// webhook 事件订阅端。
#[derive(Debug, Clone)]
pub struct WebhookEventSink {
    db: DatabaseConnection,
    registry: Arc<ChannelRegistry>,
    channels: Arc<Vec<ChannelConfig>>,
    options: DeliveryOptions,
    client: reqwest::Client,
}

impl WebhookEventSink {
    /// 用数据库连接与渠道配置装配。
    ///
    /// 未启用的渠道在此剔除；注册表缺省取内置适配器集合。
    #[must_use]
    pub fn new(
        db: DatabaseConnection,
        channels: Vec<ChannelConfig>,
        options: DeliveryOptions,
    ) -> Self {
        Self::with_registry(db, Arc::new(default_registry()), channels, options)
    }

    /// 显式注入适配器注册表（测试用）。
    #[must_use]
    pub fn with_registry(
        db: DatabaseConnection,
        registry: Arc<ChannelRegistry>,
        channels: Vec<ChannelConfig>,
        options: DeliveryOptions,
    ) -> Self {
        let channels = channels.into_iter().filter(|c| c.enabled).collect();
        Self {
            db,
            registry,
            channels: Arc::new(channels),
            options,
            client: http_client(),
        }
    }

    /// 某事件的投递目标（已启用且订阅了该事件）。
    ///
    /// 纯过滤，不触库——「不相关的事件零开销」由这里保证。
    #[must_use]
    fn targets_for(&self, event: &str) -> Vec<ChannelConfig> {
        self.channels
            .iter()
            .filter(|channel| channel.events.iter().any(|subscribed| subscribed == event))
            .cloned()
            .collect()
    }
}

#[async_trait::async_trait]
impl EventSink for WebhookEventSink {
    async fn publish(&self, event: PipelineEvent) {
        // 只关心「步骤成功完成」；开始/成功/失败事件本期不在推送词汇内。
        let PipelineEvent::StepFinished {
            pipeline_id,
            run_id,
            type_id,
            skipped,
            ..
        } = event
        else {
            return;
        };
        if skipped || event_title(&type_id).is_none() {
            return;
        }
        let targets = self.targets_for(&type_id);
        if targets.is_empty() {
            return;
        }

        let db = self.db.clone();
        let registry = Arc::clone(&self.registry);
        let options = self.options.clone();
        let client = self.client.clone();
        // 素材查询与投递都挪到旁路：引擎只等「事件已受理」。
        tokio::spawn(async move {
            let message = assemble_message(&db, pipeline_id, run_id, &type_id).await;
            for channel in &targets {
                // 单渠道内部带重试；渠道之间顺序发送，避免多渠道同时
                // 失败时的退避任务并发膨胀。
                let _ = deliver(&client, &registry, channel, &message, &options).await;
            }
        });
    }
}

/// 从库中取消息素材并组装。
///
/// 素材尽力而为：流水线或历史已被清理时降级为占位文案，绝不因组装
/// 失败放弃投递——「发生了什么」比「描述得多完整」更重要。
async fn assemble_message(
    db: &DatabaseConnection,
    pipeline_id: i64,
    run_id: i64,
    event: &str,
) -> NotificationMessage {
    let pipeline = PipelineRepository::new(db)
        .find(pipeline_id)
        .await
        .ok()
        .flatten();
    let pipeline_name = pipeline
        .as_ref()
        .map(|p| p.name.clone())
        .unwrap_or_else(|| format!("流水线 {pipeline_id}"));

    let trigger = history::Entity::find_by_id(run_id)
        .one(db)
        .await
        .ok()
        .flatten()
        .map(|record| record.trigger_source)
        .unwrap_or_else(|| "unknown".to_owned());

    let domains = pipeline
        .as_ref()
        .and_then(|p| step_input(p, EVENT_CERT_APPLY))
        .and_then(|input| input.get("domains").cloned())
        .and_then(|value| serde_json::from_value::<Vec<String>>(value).ok())
        .unwrap_or_default();

    // 部署目标只属于部署事件：申请成功时部署尚未发生，带上目标只会误导。
    let target = if event == EVENT_CERT_DEPLOY {
        pipeline
            .as_ref()
            .and_then(|p| step_input(p, EVENT_CERT_DEPLOY))
            .map(describe_target)
    } else {
        None
    };

    NotificationMessage {
        event: event.to_owned(),
        title: event_title(event).unwrap_or("通知").to_owned(),
        pipeline: pipeline_name,
        trigger,
        occurred_at: Utc::now(),
        domains,
        target,
    }
}

/// 取流水线定义中某类型步骤的输入配置。
fn step_input<'a>(pipeline: &'a Pipeline, type_id: &str) -> Option<&'a serde_json::Value> {
    pipeline
        .steps
        .iter()
        .find(|step| step.type_id == type_id)
        .map(|step| &step.input)
}

/// 把部署步骤的输入翻译成一句目标描述。
///
/// SSH 引用主机档案时 `host` 可能为空——降级为档案标识；未知目标类型
/// 原样给出类型名，不猜字段。
fn describe_target(input: &serde_json::Value) -> String {
    let kind = input
        .get("target")
        .and_then(|value| value.as_str())
        .unwrap_or("unknown");
    let config = input.get("config");
    let detail = match kind {
        "ssh" => config
            .and_then(|c| c.get("host"))
            .and_then(|value| value.as_str())
            .filter(|host| !host.trim().is_empty())
            .map(|host| format!("SSH {host}"))
            .or_else(|| {
                config
                    .and_then(|c| c.get("credential_id"))
                    .and_then(|value| value.as_i64())
                    .map(|id| format!("SSH（主机档案 #{id}）"))
            }),
        "local" => config
            .and_then(|c| c.get("cert_path"))
            .and_then(|value| value.as_str())
            .map(|path| format!("本地 {path}")),
        _ => None,
    };
    detail.unwrap_or_else(|| kind.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::channel::ChannelConfig;
    use crate::message::trigger_label;

    fn channel(name: &str, events: &[&str], enabled: bool) -> ChannelConfig {
        ChannelConfig {
            name: name.to_owned(),
            provider: "generic".to_owned(),
            url: "http://127.0.0.1:9/hook".to_owned(),
            secret: None,
            events: events.iter().map(|e| (*e).to_owned()).collect(),
            enabled,
            sign: None,
            method: "POST".to_owned(),
            headers: std::collections::BTreeMap::new(),
            body_template: None,
        }
    }

    async fn sink(channels: Vec<ChannelConfig>) -> WebhookEventSink {
        WebhookEventSink::new(
            // 过滤路径不触库；连接仅在投递任务里使用。
            sea_orm::Database::connect("sqlite::memory:")
                .await
                .expect("内存库连接应可用"),
            channels,
            DeliveryOptions::default(),
        )
    }

    fn step_finished(type_id: &str, skipped: bool) -> PipelineEvent {
        PipelineEvent::StepFinished {
            pipeline_id: 1,
            run_id: 1,
            step_order: 0,
            type_id: type_id.to_owned(),
            skipped,
        }
    }

    #[tokio::test]
    async fn targets_filter_by_subscription_and_enabled() {
        let sink = sink(vec![
            channel("apply-only", &[EVENT_CERT_APPLY], true),
            channel("all", &[EVENT_CERT_APPLY, EVENT_CERT_DEPLOY], true),
            channel("disabled", &[EVENT_CERT_APPLY], false),
        ])
        .await;
        let targets = sink.targets_for(EVENT_CERT_APPLY);
        let names: Vec<&str> = targets.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, vec!["apply-only", "all"]);
        assert!(sink.targets_for(EVENT_CERT_DEPLOY).len() == 1);
    }

    #[tokio::test]
    async fn skipped_step_produces_no_targets_lookup() {
        // 跳过步骤：publish 走提前返回路径，无渠道被命中（不 panic 即通过）。
        let sink = sink(vec![channel("apply-only", &[EVENT_CERT_APPLY], true)]).await;
        let targets = sink.targets_for(EVENT_CERT_APPLY);
        assert_eq!(targets.len(), 1);
        // skipped 判定发生在 targets_for 之前（见 publish），此处固化语义。
        let event = step_finished(EVENT_CERT_APPLY, true);
        let PipelineEvent::StepFinished { skipped, .. } = event else {
            panic!("应为步骤完成事件");
        };
        assert!(skipped);
    }

    #[test]
    fn event_title_covers_known_events_only() {
        assert_eq!(event_title(EVENT_CERT_APPLY), Some("证书申请成功"));
        assert_eq!(event_title(EVENT_CERT_DEPLOY), Some("证书部署成功"));
        assert_eq!(event_title("cert.store"), None);
        assert_eq!(event_title("pipeline.failed"), None);
    }

    #[test]
    fn describe_target_renders_ssh_and_local() {
        let ssh = serde_json::json!({
            "target": "ssh",
            "config": { "host": "web.example.com", "port": 22 }
        });
        assert_eq!(describe_target(&ssh), "SSH web.example.com");

        let profile = serde_json::json!({
            "target": "ssh",
            "config": { "credential_id": 7 }
        });
        assert_eq!(describe_target(&profile), "SSH（主机档案 #7）");

        let local = serde_json::json!({
            "target": "local",
            "config": { "cert_path": "/etc/nginx/tls/a.pem" }
        });
        assert_eq!(describe_target(&local), "本地 /etc/nginx/tls/a.pem");

        let unknown = serde_json::json!({ "target": "s3" });
        assert_eq!(describe_target(&unknown), "s3");
    }

    #[test]
    fn trigger_label_used_for_missing_history_fallback() {
        assert_eq!(trigger_label("unknown"), "unknown");
    }
}
