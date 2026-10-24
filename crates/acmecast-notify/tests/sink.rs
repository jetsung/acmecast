//! `WebhookEventSink` 的端到端行为：运行结束投递、渠道来自凭据、空渠道不投递。
//!
//! 投递走 spawn 旁路，测试以轮询 mock 收到请求为完成信号；素材断言用
//! `generic` 渠道（统一 JSON 负载字段清晰，文本格式由 message 单测覆盖）。

use std::sync::Arc;
use std::time::Duration;

use acmecast_access::CredentialRegistry;
use acmecast_core::CredentialCipher;
use acmecast_notify::{
    DeliveryOptions, EVENT_PIPELINE_FAILED, EVENT_PIPELINE_SUCCEEDED, GenericCredentialType,
    WebhookEventSink,
};
use acmecast_pipeline::{EventSink, PipelineEvent};
use acmecast_store::entity::{credential, history, pipeline, pipeline_step};
use acmecast_store::migrate;
use chrono::Utc;
use sea_orm::{ActiveModelTrait, Database, DatabaseConnection, EntityTrait, Set};
use serde_json::Value;
use wiremock::matchers::method;
use wiremock::{Mock, MockServer, ResponseTemplate};

/// 与 [`cipher`] 使用同一密钥；测试里加解密两侧必须一致。
const CIPHER_BYTES: [u8; 32] = [7u8; 32];

fn cipher() -> Arc<CredentialCipher> {
    Arc::new(CredentialCipher::from_bytes(&CIPHER_BYTES).expect("密钥应合法"))
}

/// 只注册通知渠道类型的最小凭据注册表。
fn registry() -> Arc<CredentialRegistry> {
    let mut registry = CredentialRegistry::new();
    registry
        .register(GenericCredentialType::default())
        .expect("注册应成功");
    Arc::new(registry)
}

async fn database() -> DatabaseConnection {
    let db = Database::connect("sqlite::memory:")
        .await
        .expect("应能连上内存库");
    migrate(&db).await.expect("迁移应成功");
    db
}

/// 插入一条 `notify.generic` 凭据（指向 `url`），返回主键。
async fn insert_credential(db: &DatabaseConnection, url: &str) -> i64 {
    let fields = serde_json::json!({ "name": "it", "url": url, "method": "POST" });
    let encrypted = cipher()
        .encrypt_string(&serde_json::to_string(&fields).expect("字段应可序列化"))
        .expect("加密应成功");
    credential::ActiveModel {
        id: Default::default(),
        name: Set("it".to_owned()),
        type_id: Set("notify.generic".to_owned()),
        proxy_mode: Set("direct".to_owned()),
        encrypted_fields: Set(encrypted),
        remark: Set(String::new()),
        capabilities: Set("[]".to_owned()),
        created_at: Set(Utc::now()),
        updated_at: Set(Utc::now()),
    }
    .insert(db)
    .await
    .expect("插入凭据应成功")
    .id
}

/// 插入一条含 `cert.apply` + 停用 `cert.deploy` 步骤的流水线，返回主键。
///
/// `channels` 即 `notification_channels`；停用步骤用于验证简讯里的「跳过」。
async fn insert_pipeline(db: &DatabaseConnection, channels: &[i64]) -> i64 {
    let now = Utc::now();
    let inserted = pipeline::ActiveModel {
        id: Default::default(),
        name: Set("example-pipeline".to_owned()),
        enabled: Set(true),
        description: Set(None),
        notification_channels: Set(serde_json::to_string(channels).expect("应可序列化")),
        created_at: Set(now),
        updated_at: Set(now),
    }
    .insert(db)
    .await
    .expect("插入流水线应成功");
    let pipeline_id = inserted.id;

    let steps = [
        (
            0,
            "cert.apply",
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
            serde_json::json!({ "target": "local" }),
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

/// 插入一条已结束的运行历史（总耗时固定 65 秒），返回主键。
async fn insert_history(db: &DatabaseConnection, pipeline_id: i64, source: &str) -> i64 {
    let started_at = Utc::now() - chrono::Duration::seconds(65);
    history::ActiveModel {
        id: Default::default(),
        pipeline_id: Set(pipeline_id),
        trigger_source: Set(source.to_owned()),
        status: Set("success".to_owned()),
        started_at: Set(started_at),
        finished_at: Set(Some(started_at + chrono::Duration::seconds(65))),
        error_message: Set(None),
    }
    .insert(db)
    .await
    .expect("插入历史应成功")
    .id
}

fn sink(db: DatabaseConnection) -> WebhookEventSink {
    WebhookEventSink::new(
        db,
        registry(),
        Some(cipher()),
        DeliveryOptions::default(),
        None::<String>,
    )
}

fn succeeded_event(pipeline_id: i64, run_id: i64) -> PipelineEvent {
    PipelineEvent::Succeeded {
        pipeline_id,
        run_id,
        succeeded_steps: 1,
    }
}

fn failed_event(pipeline_id: i64, run_id: i64, step_order: i32, reason: &str) -> PipelineEvent {
    PipelineEvent::Failed {
        pipeline_id,
        run_id,
        step_order,
        type_id: "cert.apply".to_owned(),
        reason: reason.to_owned(),
    }
}

async fn wait_for_requests(server: &MockServer, expected: usize) -> Vec<wiremock::Request> {
    for _ in 0..250 {
        if let Some(requests) = server.received_requests().await
            && requests.len() >= expected
        {
            return requests;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("等待 {expected} 条请求超时");
}

async fn assert_no_requests(server: &MockServer) {
    tokio::time::sleep(Duration::from_millis(300)).await;
    let requests = server.received_requests().await.unwrap_or_default();
    assert!(requests.is_empty(), "不应产生投递: {:?}", requests.len());
}

async fn mock_server() -> MockServer {
    MockServer::start().await
}

/// 成功运行：渠道收到一条成功简讯（含步骤状态、域名与总耗时）。
#[tokio::test]
async fn succeeded_run_delivers_single_summary() {
    let server = mock_server().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;

    let db = database().await;
    let credential_id = insert_credential(&db, &format!("{}/hook", server.uri())).await;
    let pipeline_id = insert_pipeline(&db, &[credential_id]).await;
    let run_id = insert_history(&db, pipeline_id, "cron").await;

    sink(db).publish(succeeded_event(pipeline_id, run_id)).await;

    let requests = wait_for_requests(&server, 1).await;
    assert_eq!(requests.len(), 1, "成功运行只应投递一条简讯");
    let body: Value = serde_json::from_slice(&requests[0].body).expect("应为 JSON");
    assert_eq!(body["event"], EVENT_PIPELINE_SUCCEEDED);
    assert_eq!(body["title"], "流水线运行成功");
    assert_eq!(body["pipeline"], "example-pipeline");
    assert_eq!(body["trigger"], "cron", "触发来源应取自运行历史");
    assert_eq!(body["domains"][0], "a.example.com");
    assert_eq!(body["summary"]["result"], "success");
    assert_eq!(body["summary"]["duration_secs"], 65);
    // 停用的部署步骤标「跳过」，启用的申请步骤标「成功」。
    assert_eq!(body["summary"]["steps"][0]["type_id"], "cert.apply");
    assert_eq!(body["summary"]["steps"][0]["status"], "succeeded");
    assert_eq!(body["summary"]["steps"][1]["status"], "skipped");
}

/// 失败运行：渠道收到一条失败简讯（含失败步骤标识与原因）。
#[tokio::test]
async fn failed_run_delivers_failure_summary() {
    let server = mock_server().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;

    let db = database().await;
    let credential_id = insert_credential(&db, &format!("{}/hook", server.uri())).await;
    let pipeline_id = insert_pipeline(&db, &[credential_id]).await;
    let run_id = insert_history(&db, pipeline_id, "manual").await;

    sink(db)
        .publish(failed_event(pipeline_id, run_id, 0, "DNS 记录写入失败"))
        .await;

    let requests = wait_for_requests(&server, 1).await;
    assert_eq!(requests.len(), 1, "失败运行只应投递一条简讯");
    let body: Value = serde_json::from_slice(&requests[0].body).expect("应为 JSON");
    assert_eq!(body["event"], EVENT_PIPELINE_FAILED);
    assert_eq!(body["title"], "流水线运行失败");
    assert_eq!(body["trigger"], "manual");
    assert_eq!(body["failure"]["step_type"], "cert.apply");
    assert_eq!(body["failure"]["step"], 1, "序号对外从 1 起");
    assert_eq!(body["failure"]["reason"], "DNS 记录写入失败");
    assert_eq!(body["summary"]["result"], "failed");
    // 第 1 步失败：它标「失败」，之后的启用步骤标「未执行」。
    assert_eq!(body["summary"]["steps"][0]["status"], "failed");
}

/// 未选渠道：运行结束不产生任何投递。
#[tokio::test]
async fn empty_notification_channels_never_deliver() {
    let server = mock_server().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;

    let db = database().await;
    let pipeline_id = insert_pipeline(&db, &[]).await;
    let run_id = insert_history(&db, pipeline_id, "manual").await;

    sink(db).publish(succeeded_event(pipeline_id, run_id)).await;

    assert_no_requests(&server).await;
}

/// 步骤完成与开始事件不在推送词汇内：不投递。
#[tokio::test]
async fn step_and_started_events_never_deliver() {
    let server = mock_server().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;

    let db = database().await;
    let credential_id = insert_credential(&db, &format!("{}/hook", server.uri())).await;
    let pipeline_id = insert_pipeline(&db, &[credential_id]).await;
    let run_id = insert_history(&db, pipeline_id, "manual").await;

    let sink = sink(db);
    sink.publish(PipelineEvent::StepFinished {
        pipeline_id,
        run_id,
        step_order: 0,
        type_id: "cert.apply".to_owned(),
        skipped: false,
    })
    .await;
    sink.publish(PipelineEvent::Started {
        pipeline_id,
        run_id,
        trigger_source: "manual".to_owned(),
    })
    .await;

    assert_no_requests(&server).await;
}

/// 某渠道凭据已被删除：只记 WARN 并跳过，其余渠道照常投递。
#[tokio::test]
async fn missing_credential_is_skipped_but_others_continue() {
    let server = mock_server().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;

    let db = database().await;
    let credential_id = insert_credential(&db, &format!("{}/hook", server.uri())).await;
    // 9999 不存在：解析失败被跳过，不应影响有效渠道。
    let pipeline_id = insert_pipeline(&db, &[9999, credential_id]).await;
    let run_id = insert_history(&db, pipeline_id, "manual").await;

    sink(db).publish(succeeded_event(pipeline_id, run_id)).await;

    let requests = wait_for_requests(&server, 1).await;
    assert_eq!(requests.len(), 1, "有效渠道仍应收到简讯");
}

/// 凭据被删后 `notification_channels` 只剩无效 ID：不产生投递也不 panic。
#[tokio::test]
async fn all_credentials_missing_delivers_nothing() {
    let server = mock_server().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;

    let db = database().await;
    let credential_id = insert_credential(&db, &format!("{}/hook", server.uri())).await;
    let pipeline_id = insert_pipeline(&db, &[credential_id]).await;
    let run_id = insert_history(&db, pipeline_id, "manual").await;
    credential::Entity::delete_by_id(credential_id)
        .exec(&db)
        .await
        .expect("删除凭据应成功");

    sink(db).publish(succeeded_event(pipeline_id, run_id)).await;

    assert_no_requests(&server).await;
}
