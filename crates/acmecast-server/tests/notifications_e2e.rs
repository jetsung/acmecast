//! 5.2 端到端：流水线运行触发 webhook 通知。
//!
//! 全链路走真实 HTTP 与真实执行器：手动触发含 `cert.apply` + `cert.deploy`
//! 的流水线，断言订阅渠道收到两条消息、消息字段符合 spec；同时用一个
//! 持续失败的渠道证明投递失败不影响流水线结果。

mod support;

use std::sync::Arc;
use std::time::Duration;

use acmecast_notify::{
    ChannelConfig, DeliveryOptions, EVENT_CERT_APPLY, EVENT_CERT_DEPLOY, WebhookEventSink,
};
use acmecast_pipeline::{PipelineStep, StepContext, StepOutput, StepRegistry};
use acmecast_server::{AppState, RuntimeState, auth::AuthMode, config::ServerConfig};
use acmecast_store::migrate;
use async_trait::async_trait;
use axum::{body::Body, http::Request};
use sea_orm::{ColumnTrait, Database, DatabaseConnection, EntityTrait, QueryFilter};
use serde_json::Value;
use tower::ServiceExt;
use wiremock::matchers::method;
use wiremock::{Mock, MockServer, ResponseTemplate};

/// 总是成功的测试步骤，`type_id` 由构造注入。
#[derive(Debug)]
struct OkStep(&'static str);

#[async_trait]
impl PipelineStep for OkStep {
    fn type_id(&self) -> &'static str {
        self.0
    }

    async fn execute(&self, _ctx: &mut StepContext<'_>) -> acmecast_pipeline::Result<StepOutput> {
        Ok(StepOutput::empty())
    }
}

async fn database() -> DatabaseConnection {
    let db = Database::connect("sqlite::memory:")
        .await
        .expect("应能连接内存数据库");
    migrate(&db).await.expect("迁移应成功");
    db
}

async fn app(db: DatabaseConnection, channels: Vec<ChannelConfig>) -> axum::Router {
    let mut steps = StepRegistry::new();
    steps
        .register(OkStep(EVENT_CERT_APPLY))
        .expect("注册申请步骤应成功");
    steps
        .register(OkStep(EVENT_CERT_DEPLOY))
        .expect("注册部署步骤应成功");

    let mut runtime = RuntimeState::new(
        acmecast_server::dependencies::credential_registry(),
        Arc::new(steps),
        // run 端点要求加密器在场（凭据解析的前置条件），测试同样要给。
        Some(Arc::new(
            acmecast_core::CredentialCipher::from_base64(
                &acmecast_core::CredentialCipher::generate_key_base64(),
            )
            .expect("生成的密钥应可用"),
        )),
        AuthMode::Disabled,
    );
    runtime.notifier = Some(Arc::new(WebhookEventSink::new(
        db.clone(),
        channels,
        DeliveryOptions::default(),
    )));

    acmecast_server::assemble_router_with_runtime(
        AppState {
            db: db.clone(),
            config: ServerConfig::default(),
        },
        runtime,
    )
}

async fn send(app: &axum::Router, request: Request<Body>) -> axum::http::Response<Body> {
    app.clone().oneshot(request).await.expect("应能处理请求")
}

fn json_request(method: &str, uri: &str, body: &Value) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(body).expect("应能序列化")))
        .expect("应能构造请求")
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

#[tokio::test]
async fn pipeline_run_pushes_apply_and_deploy_messages() {
    let healthy = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&healthy)
        .await;
    // 持续失败的渠道：证明投递失败不影响流水线结果。
    let broken = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(503))
        .mount(&broken)
        .await;

    let db = database().await;
    let channels = vec![
        ChannelConfig {
            name: "ops".to_owned(),
            provider: "generic".to_owned(),
            url: format!("{}/hook", healthy.uri()),
            secret: None,
            events: vec![EVENT_CERT_APPLY.to_owned(), EVENT_CERT_DEPLOY.to_owned()],
            enabled: true,
            sign: None,
            method: "POST".to_owned(),
            headers: std::collections::BTreeMap::new(),
            body_template: None,
        },
        ChannelConfig {
            name: "down".to_owned(),
            provider: "generic".to_owned(),
            url: format!("{}/hook", broken.uri()),
            secret: None,
            events: vec![EVENT_CERT_APPLY.to_owned()],
            enabled: true,
            sign: None,
            method: "POST".to_owned(),
            headers: std::collections::BTreeMap::new(),
            body_template: None,
        },
    ];
    let app = app(db.clone(), channels).await;

    // 保存一条含两个订阅事件的流水线。
    let response = send(
        &app,
        json_request(
            "POST",
            "/api/pipelines",
            &serde_json::json!({
                "name": "e2e-pipeline",
                "steps": [
                    {
                        "type_id": EVENT_CERT_APPLY,
                        "input": { "domains": ["a.example.com"] },
                        "enabled": true
                    },
                    {
                        "type_id": EVENT_CERT_DEPLOY,
                        "input": { "target": "ssh", "config": { "host": "web.example.com" } },
                        "enabled": true
                    }
                ]
            }),
        ),
    )
    .await;
    assert_eq!(
        response.status(),
        axum::http::StatusCode::OK,
        "保存流水线应成功"
    );
    let body: Value = serde_json::from_slice(
        &axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap(),
    )
    .unwrap();
    let pipeline_id = body["data"]["id"].as_i64().expect("应返回流水线 id");

    // 手动触发运行。
    let response = send(
        &app,
        json_request(
            "POST",
            &format!("/api/pipelines/{pipeline_id}/run"),
            &serde_json::json!({}),
        ),
    )
    .await;
    assert_eq!(
        response.status(),
        axum::http::StatusCode::OK,
        "触发运行应成功"
    );

    // 运行是 spawn 旁路：轮询数据库等终态。
    use acmecast_store::entity::history;
    let mut history_status = String::new();
    for _ in 0..250 {
        if let Some(record) = history::Entity::find()
            .filter(history::Column::PipelineId.eq(pipeline_id))
            .one(&db)
            .await
            .expect("查询历史应成功")
            && record.status != "running"
        {
            history_status = record.status;
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(history_status, "success", "流水线运行应为成功");

    // 健康渠道收到两条消息：申请（含域名）与部署（含目标）。
    let requests = wait_for_requests(&healthy, 2).await;
    let messages: Vec<Value> = requests
        .iter()
        .map(|request| serde_json::from_slice(&request.body).expect("消息应为 JSON"))
        .collect();
    assert_eq!(messages[0]["event"], EVENT_CERT_APPLY);
    assert_eq!(messages[0]["pipeline"], "e2e-pipeline");
    assert_eq!(messages[0]["trigger"], "manual");
    assert_eq!(messages[0]["domains"][0], "a.example.com");
    assert_eq!(messages[1]["event"], EVENT_CERT_DEPLOY);
    assert_eq!(messages[1]["target"], "SSH web.example.com");

    // 失败渠道也收到了申请事件（最终失败只影响自身）。
    let broken_requests = wait_for_requests(&broken, 1).await;
    assert_eq!(broken_requests.len(), 1, "失败渠道应只收到订阅内的申请事件");
}
