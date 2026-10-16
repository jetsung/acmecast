//! `notify.summary`：流水线末尾的运行简讯步骤。
//!
//! 从产物与数据库（流水线定义、运行历史）读取本次运行的素材，组装
//! [`NotificationMessage`] 后按步骤选定的渠道直接投递——绕过
//! `WebhookEventSink` 的事件订阅过滤（步骤选定即投递）。
//! 投递是尽力而为的旁路：失败只记 WARN，步骤状态仍为成功。
//!
//! 步骤状态推导依赖「失败即中止」不变量：本步骤能执行说明此前所有启用
//! 的步骤都已成功，因此运行结果恒为成功；排在本步之后的启用步骤记为
//! 未执行（`notify.summary` 应放在流水线末尾，正常配置下不出现）。

use std::sync::Arc;

use acmecast_pipeline::{Artifacts, PipelineStep, Result, StepContext, StepOutput};
use acmecast_store::entity::history;
use acmecast_store::repository::{Pipeline, PipelineRepository};
use chrono::Utc;
use schemars::JsonSchema;
use sea_orm::{DatabaseConnection, EntityTrait};
use serde::Deserialize;

use crate::channel::{ChannelConfig, ChannelRegistry};
use crate::delivery::{DeliveryOptions, deliver};
use crate::message::{NotificationMessage, SummaryDetail, SummaryStep, SummaryStepStatus};
use crate::sink::EVENT_CERT_APPLY;

/// 步骤类型标识，同时作为简讯消息的事件标识与投递日志的事件名。
const TYPE_ID: &str = "notify.summary";

/// `notify.summary` 的输入。
#[derive(Debug, Clone, Default, Deserialize, JsonSchema)]
pub struct NotifySummaryInput {
    /// 接收简讯的渠道名列表（`config.toml` `[[notifications]]` 的 `name`）。
    ///
    /// 选定的渠道覆盖其自身的 `events` 过滤；选项由前端从渠道列表端点注入。
    #[serde(default)]
    #[schemars(description = "接收简讯的渠道；选项来自已声明的通知渠道列表")]
    pub channels: Vec<String>,
}

/// 运行简讯步骤。
#[derive(Debug)]
pub struct NotifySummaryStep {
    channels: Arc<Vec<ChannelConfig>>,
    registry: Arc<ChannelRegistry>,
    db: DatabaseConnection,
    client: reqwest::Client,
}

impl NotifySummaryStep {
    /// 用渠道配置、适配器注册表、数据库连接与 HTTP 客户端装配。
    ///
    /// `channels` 取 `config.toml` 声明的全部渠道（含未启用的）：未启用的
    /// 渠道被步骤选中时跳过并记 WARN，而不是当作未知名字。
    #[must_use]
    pub fn new(
        channels: Vec<ChannelConfig>,
        registry: Arc<ChannelRegistry>,
        db: DatabaseConnection,
        client: reqwest::Client,
    ) -> Self {
        Self {
            channels: Arc::new(channels),
            registry,
            db,
            client,
        }
    }
}

#[async_trait::async_trait]
impl PipelineStep for NotifySummaryStep {
    fn type_id(&self) -> &'static str {
        TYPE_ID
    }

    fn input_schema(&self) -> Option<schemars::schema::RootSchema> {
        Some(schemars::schema_for!(NotifySummaryInput))
    }

    async fn execute(&self, ctx: &mut StepContext<'_>) -> Result<StepOutput> {
        let input: NotifySummaryInput = ctx.input_as()?;
        if input.channels.is_empty() {
            ctx.log_warn("未选择通知渠道（channels 为空），简讯不投递");
            return Ok(StepOutput::empty());
        }

        // 选定的渠道必须已在配置中声明；未知名字定位到 channels 字段。
        let mut selected = Vec::with_capacity(input.channels.len());
        for name in &input.channels {
            let Some(channel) = self
                .channels
                .iter()
                .find(|channel| &channel.name == name)
            else {
                let declared: Vec<&str> = self
                    .channels
                    .iter()
                    .map(|channel| channel.name.as_str())
                    .collect();
                return Err(acmecast_pipeline::Error::InvalidInput {
                    field: "channels".to_owned(),
                    reason: format!(
                        "未知渠道 `{name}`（已声明：{}）",
                        if declared.is_empty() {
                            "（暂无）".to_owned()
                        } else {
                            declared.join("、")
                        }
                    ),
                });
            };
            if !channel.enabled {
                ctx.log_warn(format!(
                    "渠道 `{name}` 未启用（enabled = false），跳过投递"
                ));
                tracing::warn!(channel = %name, "notify.summary 选定渠道未启用，跳过投递");
                continue;
            }
            selected.push(channel.clone());
        }
        if selected.is_empty() {
            ctx.log_warn("选定的渠道均未启用，简讯未投递");
            return Ok(StepOutput::empty());
        }

        let message = assemble_message(
            &self.db,
            ctx.pipeline_id(),
            ctx.run_id(),
            ctx.step_order(),
            ctx.artifacts(),
        )
        .await;

        // 复用统一投递路径（超时 + 退避重试 + 可观测日志）；失败不改变步骤状态。
        let options = DeliveryOptions::default();
        for channel in &selected {
            match deliver(&self.client, &self.registry, channel, &message, &options).await {
                Ok(()) => ctx.log_info(format!("渠道 `{}` 简讯已投递", channel.name)),
                Err(error) => {
                    ctx.log_warn(format!("渠道 `{}` 简讯投递失败：{error}", channel.name));
                    tracing::warn!(
                        channel = %channel.name,
                        %error,
                        "notify.summary 简讯投递失败（步骤不受影响）"
                    );
                }
            }
        }
        Ok(StepOutput::empty())
    }
}

/// 组装简讯：查库取流水线定义与运行历史，产物取域名列表。
///
/// 素材尽力而为：流水线或历史被清理时降级为占位文案，绝不因组装失败
/// 放弃投递（与 `WebhookEventSink` 的降级同源）。
async fn assemble_message(
    db: &DatabaseConnection,
    pipeline_id: i64,
    run_id: i64,
    step_order: i32,
    artifacts: &Artifacts,
) -> NotificationMessage {
    let pipeline = PipelineRepository::new(db)
        .find(pipeline_id)
        .await
        .ok()
        .flatten();
    let pipeline_name = pipeline
        .as_ref()
        .map(|pipeline| pipeline.name.clone())
        .unwrap_or_else(|| format!("流水线 {pipeline_id}"));

    let row = history::Entity::find_by_id(run_id)
        .one(db)
        .await
        .ok()
        .flatten();
    let trigger = row
        .as_ref()
        .map(|row| row.trigger_source.clone())
        .unwrap_or_else(|| "unknown".to_owned());
    // 此刻运行尚未结束（`finished_at` 为空），总耗时 = 组装时刻 − 开始时刻。
    let duration_secs = row
        .as_ref()
        .map(|row| (Utc::now() - row.started_at).num_seconds().max(0) as u64)
        .unwrap_or(0);

    let steps = pipeline
        .as_ref()
        .map(|pipeline| summarize_steps(pipeline, step_order))
        .unwrap_or_default();

    NotificationMessage {
        event: TYPE_ID.to_owned(),
        title: "运行简讯".to_owned(),
        pipeline: pipeline_name,
        trigger,
        occurred_at: Utc::now(),
        domains: domains_of(artifacts, pipeline.as_ref()),
        target: None,
        failure: None,
        summary: Some(SummaryDetail {
            duration_secs,
            steps,
        }),
    }
}

/// 按「失败即中止」不变量推导各步骤状态。
///
/// 停用 → 跳过；启用且序号 ≤ 本步 → 成功；序号在本步之后 → 未执行。
fn summarize_steps(pipeline: &Pipeline, current_order: i32) -> Vec<SummaryStep> {
    pipeline
        .steps
        .iter()
        .map(|step| SummaryStep {
            type_id: step.type_id.clone(),
            status: if !step.enabled {
                SummaryStepStatus::Skipped
            } else if step.order_index <= current_order {
                SummaryStepStatus::Succeeded
            } else {
                SummaryStepStatus::NotRun
            },
        })
        .collect()
}

/// 本次申请的域名：优先读 `cert.apply` 产出的 `domains` 产物，
/// 缺失时回落到流水线定义里的输入（与 `WebhookEventSink` 同源）。
fn domains_of(artifacts: &Artifacts, pipeline: Option<&Pipeline>) -> Vec<String> {
    artifacts
        .source_of("domains")
        .and_then(|artifact| serde_json::from_value(artifact.value.clone()).ok())
        .or_else(|| {
            pipeline
                .and_then(|pipeline| {
                    pipeline
                        .steps
                        .iter()
                        .find(|step| step.type_id == EVENT_CERT_APPLY)
                })
                .and_then(|step| step.input.get("domains"))
                .and_then(|value| serde_json::from_value(value.clone()).ok())
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::sync::Mutex;

    use acmecast_access::{CredentialRegistry, CredentialStore};
    use acmecast_core::CredentialCipher;
    use acmecast_pipeline::StepLogLevel;
    use acmecast_store::entity::{history, pipeline, pipeline_step};
    use acmecast_store::migrate;
    use sea_orm::{ActiveModelTrait, Database, Set};
    use serde_json::Value;
    use wiremock::matchers::method;
    use wiremock::{Mock, MockServer, ResponseTemplate};

    /// 内存版流水线级存储：简讯步骤不触它，够 `StepContext` 构造即可。
    #[derive(Debug, Default)]
    struct MemoryState(Mutex<BTreeMap<(i64, String), Value>>);

    #[async_trait::async_trait]
    impl acmecast_pipeline::PipelineStateStore for MemoryState {
        async fn get(
            &self,
            pipeline_id: i64,
            key: &str,
        ) -> acmecast_pipeline::Result<Option<Value>> {
            Ok(self
                .0
                .lock()
                .expect("锁不应中毒")
                .get(&(pipeline_id, key.to_owned()))
                .cloned())
        }

        async fn set(
            &self,
            pipeline_id: i64,
            key: &str,
            value: Value,
        ) -> acmecast_pipeline::Result<()> {
            self.0
                .lock()
                .expect("锁不应中毒")
                .insert((pipeline_id, key.to_owned()), value);
            Ok(())
        }
    }

    /// 建一个跑完迁移的内存库。
    async fn mem_db() -> DatabaseConnection {
        let db = Database::connect("sqlite::memory:")
            .await
            .expect("应能连上内存库");
        migrate(&db).await.expect("迁移应成功");
        db
    }

    /// 插入一条「申请成功 + 停用部署 + 简讯」的流水线，返回主键。
    async fn insert_pipeline(db: &DatabaseConnection) -> i64 {
        let now = Utc::now();
        let inserted = pipeline::ActiveModel {
            name: Set("summary-pipeline".to_owned()),
            enabled: Set(true),
            description: Set(None),
            created_at: Set(now),
            updated_at: Set(now),
            ..Default::default()
        }
        .insert(db)
        .await
        .expect("插入流水线应成功");
        let pipeline_id = inserted.id;

        let steps = [
            (
                0,
                EVENT_CERT_APPLY,
                true,
                serde_json::json!({
                    "domains": ["a.example.com", "b.example.com"],
                    "challenge": "dns-01",
                }),
            ),
            (
                1,
                "cert.deploy",
                false,
                serde_json::json!({ "target": "local", "config": {} }),
            ),
            (2, TYPE_ID, true, serde_json::json!({ "channels": [] })),
        ];
        for (order, type_id, enabled, input) in steps {
            pipeline_step::ActiveModel {
                id: Default::default(),
                pipeline_id: Set(pipeline_id),
                order_index: Set(order),
                type_id: Set(type_id.to_owned()),
                input: Set(input),
                enabled: Set(enabled),
            }
            .insert(db)
            .await
            .expect("插入步骤应成功");
        }
        pipeline_id
    }

    /// 插入一条「90 秒前开始、仍在运行」的历史，返回主键。
    async fn insert_history(db: &DatabaseConnection, pipeline_id: i64) -> i64 {
        history::ActiveModel {
            id: Default::default(),
            pipeline_id: Set(pipeline_id),
            trigger_source: Set("manual".to_owned()),
            status: Set("running".to_owned()),
            started_at: Set(Utc::now() - chrono::Duration::seconds(90)),
            finished_at: Set(None),
            error_message: Set(None),
        }
        .insert(db)
        .await
        .expect("插入历史应成功")
        .id
    }

    /// `cert.apply` 产出的产物：域名 + 证书材料。
    fn apply_artifacts() -> Artifacts {
        let mut artifacts = Artifacts::new();
        let produced: BTreeMap<String, Value> = [
            ("cert_pem".to_owned(), serde_json::json!("PEM")),
            ("key_pem".to_owned(), serde_json::json!("KEY")),
            (
                "domains".to_owned(),
                serde_json::json!(["a.example.com", "b.example.com"]),
            ),
        ]
        .into_iter()
        .collect();
        artifacts.merge(0, EVENT_CERT_APPLY, produced);
        artifacts
    }

    fn channel(name: &str, url: &str, enabled: bool) -> ChannelConfig {
        ChannelConfig {
            name: name.to_owned(),
            provider: "generic".to_owned(),
            url: url.to_owned(),
            secret: None,
            events: vec![],
            enabled,
            sign: None,
            method: "POST".to_owned(),
            headers: BTreeMap::new(),
            body_template: None,
            smtp: None,
        }
    }

    fn credential_store(db: &DatabaseConnection) -> CredentialStore<'_> {
        let cipher = CredentialCipher::from_base64(&CredentialCipher::generate_key_base64())
            .expect("密钥应可用");
        CredentialStore::new(
            db,
            Arc::new(CredentialRegistry::new()),
            Arc::new(cipher),
        )
    }

    #[tokio::test]
    async fn empty_channels_skip_delivery_with_warning() {
        let db = mem_db().await;
        let pipeline_id = insert_pipeline(&db).await;
        let run_id = insert_history(&db, pipeline_id).await;
        let step = NotifySummaryStep::new(
            vec![channel("lark", "http://127.0.0.1:9/hook", true)],
            Arc::new(crate::default_registry()),
            db.clone(),
            crate::http_client(),
        );

        let input = serde_json::json!({ "channels": [] });
        let artifacts = apply_artifacts();
        let credentials = credential_store(&db);
        let state = MemoryState::default();
        let mut ctx = StepContext::new(
            pipeline_id,
            run_id,
            2,
            &input,
            &artifacts,
            &credentials,
            &state,
        );
        let output = step.execute(&mut ctx).await.expect("空渠道应正常完成");
        assert!(output.artifacts.is_empty(), "简讯步骤不产出产物");
        assert!(
            ctx.take_logs().iter().any(|log| {
                log.level == StepLogLevel::Warn && log.message.contains("未选择通知渠道")
            }),
            "应记录未选择渠道的 WARN"
        );
    }

    #[tokio::test]
    async fn unknown_channel_fails_with_field_located_error() {
        let db = mem_db().await;
        let pipeline_id = insert_pipeline(&db).await;
        let run_id = insert_history(&db, pipeline_id).await;
        let step = NotifySummaryStep::new(
            vec![channel("lark", "http://127.0.0.1:9/hook", true)],
            Arc::new(crate::default_registry()),
            db.clone(),
            crate::http_client(),
        );

        let input = serde_json::json!({ "channels": ["nonexistent"] });
        let artifacts = apply_artifacts();
        let credentials = credential_store(&db);
        let state = MemoryState::default();
        let mut ctx = StepContext::new(
            pipeline_id,
            run_id,
            2,
            &input,
            &artifacts,
            &credentials,
            &state,
        );
        let error = step.execute(&mut ctx).await.expect_err("未知渠道应失败");
        let text = error.to_string();
        assert!(text.contains("channels"), "错误应定位到 channels 字段: {text}");
        assert!(text.contains("nonexistent"), "错误应指明未知渠道名: {text}");
    }

    #[tokio::test]
    async fn disabled_channels_are_skipped_without_delivery() {
        let db = mem_db().await;
        let pipeline_id = insert_pipeline(&db).await;
        let run_id = insert_history(&db, pipeline_id).await;
        let step = NotifySummaryStep::new(
            vec![channel("ops", "http://127.0.0.1:9/hook", false)],
            Arc::new(crate::default_registry()),
            db.clone(),
            crate::http_client(),
        );

        let input = serde_json::json!({ "channels": ["ops"] });
        let artifacts = apply_artifacts();
        let credentials = credential_store(&db);
        let state = MemoryState::default();
        let mut ctx = StepContext::new(
            pipeline_id,
            run_id,
            2,
            &input,
            &artifacts,
            &credentials,
            &state,
        );
        step.execute(&mut ctx).await.expect("停用渠道应正常完成");
        assert!(
            ctx.take_logs()
                .iter()
                .any(|log| log.level == StepLogLevel::Warn && log.message.contains("未启用")),
            "应记录渠道未启用的 WARN"
        );
    }

    #[tokio::test]
    async fn selected_channels_receive_summary_payload() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;

        let db = mem_db().await;
        let pipeline_id = insert_pipeline(&db).await;
        let run_id = insert_history(&db, pipeline_id).await;
        let step = NotifySummaryStep::new(
            vec![channel("lark", &format!("{}/hook", server.uri()), true)],
            Arc::new(crate::default_registry()),
            db.clone(),
            crate::http_client(),
        );

        let input = serde_json::json!({ "channels": ["lark"] });
        let artifacts = apply_artifacts();
        let credentials = credential_store(&db);
        let state = MemoryState::default();
        let mut ctx = StepContext::new(
            pipeline_id,
            run_id,
            2,
            &input,
            &artifacts,
            &credentials,
            &state,
        );
        step.execute(&mut ctx).await.expect("正常投递应成功");

        let requests = server.received_requests().await.unwrap_or_default();
        assert_eq!(requests.len(), 1, "应投递一条简讯");
        let body: Value = serde_json::from_slice(&requests[0].body).expect("应为 JSON");
        assert_eq!(body["event"], "notify.summary");
        assert_eq!(body["title"], "运行简讯");
        assert_eq!(body["pipeline"], "summary-pipeline");
        assert_eq!(body["trigger"], "manual");
        assert_eq!(body["summary"]["result"], "success");
        assert!(
            body["summary"]["duration_secs"].as_u64().is_some_and(|s| s >= 89),
            "总耗时应取自运行历史: {}",
            body["summary"]["duration_secs"]
        );
        assert_eq!(body["summary"]["steps"][0]["type_id"], EVENT_CERT_APPLY);
        assert_eq!(body["summary"]["steps"][0]["status"], "succeeded");
        assert_eq!(body["summary"]["steps"][1]["type_id"], "cert.deploy");
        assert_eq!(body["summary"]["steps"][1]["status"], "skipped");
        assert_eq!(body["summary"]["steps"][2]["status"], "succeeded");
        assert_eq!(body["domains"][0], "a.example.com");
    }

    #[test]
    fn input_schema_declares_string_array_channels() {
        let step = NotifySummaryStep::new(
            vec![],
            Arc::new(crate::default_registry()),
            DatabaseConnection::Disconnected,
            crate::http_client(),
        );
        let schema = step.input_schema().expect("应声明输入结构");
        let rendered = serde_json::to_value(&schema).expect("schema 应可序列化");
        assert_eq!(rendered["properties"]["channels"]["type"], "array");
        assert_eq!(rendered["properties"]["channels"]["items"]["type"], "string");
    }

    #[test]
    fn summarize_steps_derives_statuses_from_enabled_and_order() {
        let pipeline = Pipeline {
            id: 1,
            name: "x".to_owned(),
            description: None,
            enabled: true,
            steps: vec![
                acmecast_store::repository::PipelineStep {
                    id: 1,
                    order_index: 0,
                    type_id: "cert.apply".to_owned(),
                    input: serde_json::json!({}),
                    enabled: true,
                },
                acmecast_store::repository::PipelineStep {
                    id: 2,
                    order_index: 1,
                    type_id: "cert.deploy".to_owned(),
                    input: serde_json::json!({}),
                    enabled: false,
                },
                acmecast_store::repository::PipelineStep {
                    id: 3,
                    order_index: 2,
                    type_id: TYPE_ID.to_owned(),
                    input: serde_json::json!({}),
                    enabled: true,
                },
                acmecast_store::repository::PipelineStep {
                    id: 4,
                    order_index: 3,
                    type_id: "cert.store".to_owned(),
                    input: serde_json::json!({}),
                    enabled: true,
                },
            ],
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        let steps = summarize_steps(&pipeline, 2);
        let statuses: Vec<SummaryStepStatus> =
            steps.iter().map(|step| step.status).collect();
        assert_eq!(
            statuses,
            vec![
                SummaryStepStatus::Succeeded,
                SummaryStepStatus::Skipped,
                SummaryStepStatus::Succeeded,
                SummaryStepStatus::NotRun,
            ]
        );
    }

    #[test]
    fn domains_fall_back_to_pipeline_input_when_artifact_missing() {
        let pipeline = Pipeline {
            id: 1,
            name: "x".to_owned(),
            description: None,
            enabled: true,
            steps: vec![acmecast_store::repository::PipelineStep {
                id: 1,
                order_index: 0,
                type_id: EVENT_CERT_APPLY.to_owned(),
                input: serde_json::json!({ "domains": ["fallback.example.com"] }),
                enabled: true,
            }],
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        let artifacts = Artifacts::new();
        assert_eq!(
            domains_of(&artifacts, Some(&pipeline)),
            vec!["fallback.example.com".to_owned()]
        );
        // 产物在时优先于流水线输入。
        assert_eq!(
            domains_of(&apply_artifacts(), Some(&pipeline)),
            vec!["a.example.com".to_owned(), "b.example.com".to_owned()]
        );
    }
}
