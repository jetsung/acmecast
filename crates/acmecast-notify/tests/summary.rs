//! `NotifySummaryStep` 的端到端行为：以库中流水线定义与运行历史为素材，
//! 组装简讯并投递到步骤选定的渠道。
//!
//! 投递断言用 `generic` 渠道（统一 JSON 负载字段清晰，文本格式由
//! message 单测覆盖）。「选定渠道覆盖 `events` 过滤」由渠道只订阅
//! `cert.apply`/`cert.deploy` 却仍收到简讯来证明。

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use acmecast_access::{CredentialRegistry, CredentialStore};
use acmecast_core::CredentialCipher;
use acmecast_notify::{ChannelConfig, EVENT_CERT_APPLY, EVENT_CERT_DEPLOY, NotifySummaryStep};
use acmecast_pipeline::{Artifacts, PipelineStateStore, PipelineStep, StepContext};
use acmecast_store::entity::{history, pipeline, pipeline_step};
use acmecast_store::migrate;
use chrono::Utc;
use sea_orm::{ActiveModelTrait, Database, DatabaseConnection, Set};
use serde_json::Value;
use wiremock::matchers::method;
use wiremock::{Mock, MockServer, ResponseTemplate};

/// 内存版流水线级存储：简讯步骤不触它，够 `StepContext` 构造即可。
#[derive(Debug, Default)]
struct MemoryState(Mutex<BTreeMap<(i64, String), Value>>);

#[async_trait::async_trait]
impl PipelineStateStore for MemoryState {
    async fn get(&self, pipeline_id: i64, key: &str) -> acmecast_pipeline::Result<Option<Value>> {
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

async fn database() -> DatabaseConnection {
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
        name: Set("e2e-summary-pipeline".to_owned()),
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
            EVENT_CERT_DEPLOY,
            false,
            serde_json::json!({ "target": "local", "config": {} }),
        ),
        (
            2,
            "notify.summary",
            true,
            serde_json::json!({ "channels": ["it"] }),
        ),
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

/// 插入一条「30 秒前开始、仍在运行」的历史，返回主键。
async fn insert_history(db: &DatabaseConnection, pipeline_id: i64) -> i64 {
    history::ActiveModel {
        id: Default::default(),
        pipeline_id: Set(pipeline_id),
        trigger_source: Set("cron".to_owned()),
        status: Set("running".to_owned()),
        started_at: Set(Utc::now() - chrono::Duration::seconds(30)),
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

fn channel(name: &str, url: &str, events: &[&str]) -> ChannelConfig {
    ChannelConfig {
        name: name.to_owned(),
        provider: "generic".to_owned(),
        url: url.to_owned(),
        secret: None,
        events: events.iter().map(|event| (*event).to_owned()).collect(),
        enabled: true,
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
    CredentialStore::new(db, Arc::new(CredentialRegistry::new()), Arc::new(cipher))
}

#[tokio::test]
async fn summary_delivers_full_material_despite_events_filter() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;

    let db = database().await;
    let pipeline_id = insert_pipeline(&db).await;
    let run_id = insert_history(&db, pipeline_id).await;

    // 渠道的 events 不含 notify.summary（它根本不是可订阅事件）——
    // 步骤选定即投递，不受 events 过滤阻拦。
    let step = NotifySummaryStep::new(
        vec![channel(
            "it",
            &format!("{}/hook", server.uri()),
            &[EVENT_CERT_APPLY, EVENT_CERT_DEPLOY],
        )],
        Arc::new(acmecast_notify::default_registry()),
        db.clone(),
        acmecast_notify::http_client(),
    );

    let input = serde_json::json!({ "channels": ["it"] });
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
    step.execute(&mut ctx).await.expect("简讯步骤应成功");

    let requests = server.received_requests().await.unwrap_or_default();
    assert_eq!(requests.len(), 1, "选定渠道应收到一条简讯");
    let body: Value = serde_json::from_slice(&requests[0].body).expect("应为 JSON");

    // 可读出：事件类型、流水线名称、触发来源、运行结果、总耗时。
    assert_eq!(body["event"], "notify.summary");
    assert_eq!(body["title"], "运行简讯");
    assert_eq!(body["pipeline"], "e2e-summary-pipeline");
    assert_eq!(body["trigger"], "cron");
    assert_eq!(body["summary"]["result"], "success");
    assert!(
        body["summary"]["duration_secs"].as_u64().is_some_and(|s| s >= 29),
        "总耗时应取自运行历史: {}",
        body["summary"]["duration_secs"]
    );

    // 各步骤状态：申请成功、停用部署跳过、简讯自身成功。
    assert_eq!(body["summary"]["steps"][0]["type_id"], EVENT_CERT_APPLY);
    assert_eq!(body["summary"]["steps"][0]["status"], "succeeded");
    assert_eq!(body["summary"]["steps"][1]["type_id"], EVENT_CERT_DEPLOY);
    assert_eq!(body["summary"]["steps"][1]["status"], "skipped");
    assert_eq!(body["summary"]["steps"][2]["type_id"], "notify.summary");
    assert_eq!(body["summary"]["steps"][2]["status"], "succeeded");

    // 域名列表来自 cert.apply 产物。
    assert_eq!(body["domains"][0], "a.example.com");
    assert_eq!(body["domains"][1], "b.example.com");
}

#[tokio::test]
async fn step_degrades_when_pipeline_and_history_are_missing() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;

    let db = database().await;
    // 流水线与历史都不存在：素材降级，但投递不放弃。
    let step = NotifySummaryStep::new(
        vec![channel("it", &format!("{}/hook", server.uri()), &[])],
        Arc::new(acmecast_notify::default_registry()),
        db.clone(),
        acmecast_notify::http_client(),
    );

    let input = serde_json::json!({ "channels": ["it"] });
    let artifacts = Artifacts::new();
    let credentials = credential_store(&db);
    let state = MemoryState::default();
    let mut ctx = StepContext::new(999, 999, 0, &input, &artifacts, &credentials, &state);
    step.execute(&mut ctx).await.expect("降级素材也应投递");

    let requests = server.received_requests().await.unwrap_or_default();
    assert_eq!(requests.len(), 1);
    let body: Value = serde_json::from_slice(&requests[0].body).expect("应为 JSON");
    assert_eq!(body["pipeline"], "流水线 999");
    assert_eq!(body["trigger"], "unknown");
    assert_eq!(body["summary"]["steps"].as_array().map(Vec::len), Some(0));
    assert!(body.get("domains").is_none());
}
