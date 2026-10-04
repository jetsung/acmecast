//! `WebhookEventSink` 的端到端行为：订阅过滤、跳过不触发、素材组装与降级。
//!
//! 投递走 spawn 旁路，测试以轮询 mock 收到请求为完成信号；素材断言用
//! `generic` 渠道（统一 JSON 负载字段清晰，文本格式由 message 单测覆盖）。

use std::time::Duration;

use acmecast_notify::{
    ChannelConfig, DeliveryOptions, EVENT_CERT_APPLY, EVENT_CERT_DEPLOY, WebhookEventSink,
};
use acmecast_pipeline::{EventSink, PipelineEvent};
use acmecast_store::entity::{history, pipeline, pipeline_step};
use acmecast_store::migrate;
use chrono::Utc;
use sea_orm::{ActiveModelTrait, Database, DatabaseConnection, Set};
use serde_json::Value;
use wiremock::matchers::method;
use wiremock::{Mock, MockServer, ResponseTemplate};

async fn database() -> DatabaseConnection {
    let db = Database::connect("sqlite::memory:")
        .await
        .expect("应能连上内存库");
    migrate(&db).await.expect("迁移应成功");
    db
}

/// 插入一条含 `cert.apply` 与 `cert.deploy` 步骤的流水线，返回主键。
async fn insert_pipeline(db: &DatabaseConnection) -> i64 {
    let now = Utc::now();
    let pipeline = pipeline::ActiveModel {
        name: Set("example-pipeline".to_owned()),
        enabled: Set(true),
        description: Set(None),
        created_at: Set(now),
        updated_at: Set(now),
        ..Default::default()
    }
    .insert(db)
    .await
    .expect("插入流水线应成功");
    let pipeline_id = pipeline.id;

    pipeline_step::ActiveModel {
        id: Default::default(),
        pipeline_id: Set(pipeline_id),
        order_index: Set(0),
        type_id: Set(EVENT_CERT_APPLY.to_owned()),
        input: Set(serde_json::json!({
            "domains": ["a.example.com", "b.example.com"],
            "challenge": "dns-01",
        })),
        enabled: Set(true),
    }
    .insert(db)
    .await
    .expect("插入申请步骤应成功");

    pipeline_step::ActiveModel {
        id: Default::default(),
        pipeline_id: Set(pipeline_id),
        order_index: Set(1),
        type_id: Set(EVENT_CERT_DEPLOY.to_owned()),
        input: Set(serde_json::json!({
            "target": "ssh",
            "config": { "host": "web.example.com" },
        })),
        enabled: Set(true),
    }
    .insert(db)
    .await
    .expect("插入部署步骤应成功");

    pipeline_id
}

/// 插入一条运行历史，返回主键。
async fn insert_history(db: &DatabaseConnection, pipeline_id: i64, source: &str) -> i64 {
    history::ActiveModel {
        id: Default::default(),
        pipeline_id: Set(pipeline_id),
        trigger_source: Set(source.to_owned()),
        status: Set("running".to_owned()),
        started_at: Set(Utc::now()),
        finished_at: Set(None),
        error_message: Set(None),
    }
    .insert(db)
    .await
    .expect("插入历史应成功")
    .id
}

async fn sink(db: DatabaseConnection, server_uri: &str, events: &[&str]) -> WebhookEventSink {
    WebhookEventSink::new(
        db,
        vec![ChannelConfig {
            name: "it".to_owned(),
            provider: "generic".to_owned(),
            url: format!("{server_uri}/hook"),
            secret: None,
            events: events.iter().map(|e| (*e).to_owned()).collect(),
            enabled: true,
            sign: None,
            method: "POST".to_owned(),
            headers: std::collections::BTreeMap::new(),
            body_template: None,
        }],
        DeliveryOptions::default(),
    )
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

fn step_finished(pipeline_id: i64, run_id: i64, type_id: &str, skipped: bool) -> PipelineEvent {
    PipelineEvent::StepFinished {
        pipeline_id,
        run_id,
        step_order: 0,
        type_id: type_id.to_owned(),
        skipped,
    }
}

#[tokio::test]
async fn apply_success_delivers_full_material() {
    let server = mock_server().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;

    let db = database().await;
    let pipeline_id = insert_pipeline(&db).await;
    let run_id = insert_history(&db, pipeline_id, "cron").await;

    let sink = sink(db, &server.uri(), &[EVENT_CERT_APPLY]).await;
    sink.publish(step_finished(pipeline_id, run_id, EVENT_CERT_APPLY, false))
        .await;

    let requests = wait_for_requests(&server, 1).await;
    let body: Value = serde_json::from_slice(&requests[0].body).expect("应为 JSON");
    assert_eq!(body["event"], "cert.apply");
    assert_eq!(body["title"], "证书申请成功");
    assert_eq!(body["pipeline"], "example-pipeline");
    assert_eq!(body["trigger"], "cron", "触发来源应取自运行历史");
    assert_eq!(body["domains"][0], "a.example.com");
    assert_eq!(body["domains"][1], "b.example.com");
    assert!(body.get("target").is_none(), "申请事件不应带部署目标");
}

#[tokio::test]
async fn deploy_success_includes_target() {
    let server = mock_server().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;

    let db = database().await;
    let pipeline_id = insert_pipeline(&db).await;
    let run_id = insert_history(&db, pipeline_id, "renewal").await;

    let sink = sink(db, &server.uri(), &[EVENT_CERT_DEPLOY]).await;
    sink.publish(step_finished(pipeline_id, run_id, EVENT_CERT_DEPLOY, false))
        .await;

    let requests = wait_for_requests(&server, 1).await;
    let body: Value = serde_json::from_slice(&requests[0].body).expect("应为 JSON");
    assert_eq!(body["event"], "cert.deploy");
    assert_eq!(body["title"], "证书部署成功");
    assert_eq!(body["trigger"], "renewal");
    assert_eq!(body["target"], "SSH web.example.com");
    assert_eq!(
        body["domains"][0], "a.example.com",
        "部署消息应带域名便于辨识"
    );
}

#[tokio::test]
async fn skipped_unsubscribed_and_non_step_events_never_deliver() {
    let server = mock_server().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;

    let db = database().await;
    let pipeline_id = insert_pipeline(&db).await;
    let run_id = insert_history(&db, pipeline_id, "manual").await;

    let sink = sink(db, &server.uri(), &[EVENT_CERT_APPLY]).await;
    // 步骤被停用跳过：不触发。
    sink.publish(step_finished(pipeline_id, run_id, EVENT_CERT_APPLY, true))
        .await;
    // 未订阅的事件：不触发。
    sink.publish(step_finished(pipeline_id, run_id, EVENT_CERT_DEPLOY, false))
        .await;
    // 订阅词汇之外的事件标识：不触发。
    sink.publish(step_finished(pipeline_id, run_id, "cert.store", false))
        .await;
    // 非步骤事件（开始/成功/失败）：不触发。
    sink.publish(PipelineEvent::Started {
        pipeline_id,
        run_id,
        trigger_source: "manual".to_owned(),
    })
    .await;

    assert_no_requests(&server).await;
}

#[tokio::test]
async fn missing_material_degrades_instead_of_dropping() {
    let server = mock_server().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;

    let db = database().await;
    // 流水线与历史都不存在：素材降级，但投递不放弃。
    let sink = sink(db, &server.uri(), &[EVENT_CERT_APPLY]).await;
    sink.publish(step_finished(999, 999, EVENT_CERT_APPLY, false))
        .await;

    let requests = wait_for_requests(&server, 1).await;
    let body: Value = serde_json::from_slice(&requests[0].body).expect("应为 JSON");
    assert_eq!(body["pipeline"], "流水线 999");
    assert_eq!(body["trigger"], "unknown");
    assert!(body.get("domains").is_none());
}
