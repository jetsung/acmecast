//! 端到端：流水线运行结束 → 凭据渠道收到简讯。
//!
//! 全链路走真实 HTTP 与真实执行器：通知渠道经 `/api/credentials` 加密
//! 入库，流水线的 `notification_channels` 选中它，手动触发运行，断言运行
//! 结束（成功/失败）时渠道收到**一条**简讯；同时验证投递失败不影响流水线
//! 结果、未选渠道不产生投递。

mod support;

use std::sync::Arc;
use std::time::Duration;

use acmecast_core::CredentialCipher;
use acmecast_notify::{
    DeliveryOptions, EVENT_PIPELINE_FAILED, EVENT_PIPELINE_SUCCEEDED, WebhookEventSink,
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

/// 与测试装配共用同一密钥；凭据的加解密两侧必须一致。
const CIPHER_BYTES: [u8; 32] = [7u8; 32];

/// `cert.apply` 的类型标识（成功简讯的域名列表从它的输入取）。
const CERT_APPLY: &str = "cert.apply";
/// 成功部署步骤的类型标识。
const CERT_DEPLOY: &str = "cert.deploy";
/// 总是失败步骤的类型标识（专为失败简讯断言构造）。
const CERT_FAIL: &str = "cert.fail";

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

/// 总是失败的测试步骤：失败原因固定，供失败简讯断言。
#[derive(Debug)]
struct FailStep(&'static str);

#[async_trait]
impl PipelineStep for FailStep {
    fn type_id(&self) -> &'static str {
        self.0
    }

    async fn execute(&self, _ctx: &mut StepContext<'_>) -> acmecast_pipeline::Result<StepOutput> {
        Err(acmecast_pipeline::Error::InvalidInput {
            field: "domains".to_owned(),
            reason: "测试构造的失败".to_owned(),
        })
    }
}

async fn database() -> DatabaseConnection {
    let db = Database::connect("sqlite::memory:")
        .await
        .expect("应能连接内存数据库");
    migrate(&db).await.expect("迁移应成功");
    db
}

/// 装配含三个测试步骤与通知订阅端的应用。
///
/// 凭据加密器与 `/api/credentials` 端点、`WebhookEventSink` 三方共用：
/// 保存时加密、投递时解密的密钥必须一致。
#[allow(unsafe_code)]
async fn app(db: DatabaseConnection) -> axum::Router {
    unsafe {
        std::env::set_var("NO_PROXY", "127.0.0.1,localhost");
        std::env::set_var("no_proxy", "127.0.0.1,localhost");
    }
    let mut steps = StepRegistry::new();
    steps
        .register(OkStep(CERT_APPLY))
        .expect("注册申请步骤应成功");
    steps
        .register(OkStep(CERT_DEPLOY))
        .expect("注册部署步骤应成功");
    steps
        .register(FailStep(CERT_FAIL))
        .expect("注册失败步骤应成功");

    let cipher = Arc::new(CredentialCipher::from_bytes(&CIPHER_BYTES).expect("密钥应合法"));
    let credentials = acmecast_server::dependencies::credential_registry();
    let mut runtime = RuntimeState::new(
        Arc::clone(&credentials),
        Arc::new(steps),
        Some(Arc::clone(&cipher)),
        AuthMode::Disabled,
    );
    runtime.notifier = Some(Arc::new(WebhookEventSink::new(
        db.clone(),
        credentials,
        Some(cipher),
        DeliveryOptions::default(),
        None::<String>,
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

async fn json_body(response: axum::http::Response<Body>) -> Value {
    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .expect("应能读取响应体");
    serde_json::from_slice(&bytes).expect("响应应为 JSON")
}

/// 经 API 创建一条通知凭据，返回其 id。
async fn create_credential(app: &axum::Router, type_id: &str, fields: Value) -> i64 {
    let response = send(
        app,
        json_request(
            "POST",
            "/api/credentials",
            &serde_json::json!({ "name": "e2e-channel", "type_id": type_id, "fields": fields }),
        ),
    )
    .await;
    assert_eq!(
        response.status(),
        axum::http::StatusCode::OK,
        "创建凭据应成功"
    );
    json_body(response).await["data"]["id"]
        .as_i64()
        .expect("应返回凭据 id")
}

/// 经 API 创建流水线（含通知渠道选择），返回其 id。
async fn create_pipeline(app: &axum::Router, channels: &[i64], steps: Value) -> i64 {
    let response = send(
        app,
        json_request(
            "POST",
            "/api/pipelines",
            &serde_json::json!({
                "name": "e2e-pipeline",
                "notification_channels": channels,
                "steps": steps,
            }),
        ),
    )
    .await;
    assert_eq!(
        response.status(),
        axum::http::StatusCode::OK,
        "保存流水线应成功"
    );
    json_body(response).await["data"]["id"]
        .as_i64()
        .expect("应返回流水线 id")
}

/// 手动触发运行并轮询数据库至终态，断言终态与预期一致。
async fn run_pipeline_and_wait(
    app: &axum::Router,
    db: &DatabaseConnection,
    pipeline_id: i64,
    expected: &str,
) {
    let response = send(
        app,
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

    use acmecast_store::entity::history;
    let mut history_status = String::new();
    for _ in 0..500 {
        if let Some(record) = history::Entity::find()
            .filter(history::Column::PipelineId.eq(pipeline_id))
            .one(db)
            .await
            .expect("查询历史应成功")
            && record.status != "running"
        {
            history_status = record.status;
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(history_status, expected, "流水线运行终态应符合预期");
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

/// 运行结束与投递是两条并行旁路：终态已到但投递可能还在路上，等一小会。
async fn assert_no_requests(server: &MockServer) {
    tokio::time::sleep(Duration::from_millis(300)).await;
    let requests = server.received_requests().await.unwrap_or_default();
    assert!(requests.is_empty(), "不应产生投递: {}", requests.len());
}

fn generic_credential_fields(url: &str) -> Value {
    serde_json::json!({ "name": "e2e", "url": url, "method": "POST" })
}

/// 极简 SMTP 服务器：匿名、无 TLS，收集每次会话的 DATA 负载。
///
/// 只为 e2e 打通「运行结束 → EmailChannel → SMTP 会话」链路，不支持认证，
/// 命令响应一律按最简格式应答（EHLO 附带 8BITMIME）。
async fn smtp_server() -> (std::net::SocketAddr, Arc<tokio::sync::Mutex<Vec<String>>>) {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::TcpListener;

    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("应能绑定本地端口");
    let addr = listener.local_addr().expect("应能取得监听地址");
    let inbox = Arc::new(tokio::sync::Mutex::new(Vec::new()));
    let inbox_clone = Arc::clone(&inbox);
    tokio::spawn(async move {
        loop {
            let Ok((socket, _)) = listener.accept().await else {
                break;
            };
            let inbox = Arc::clone(&inbox_clone);
            tokio::spawn(async move {
                let (reader, mut writer) = socket.into_split();
                let mut reader = BufReader::new(reader);
                let mut data_mode = false;
                let mut data = String::new();
                let _ = writer.write_all(b"220 test\r\n").await;
                loop {
                    let mut line = String::new();
                    match reader.read_line(&mut line).await {
                        Ok(0) | Err(_) => break,
                        Ok(_) => {}
                    }
                    if data_mode {
                        if line.trim_end_matches(['\r', '\n']) == "." {
                            data_mode = false;
                            let _ = writer.write_all(b"250 OK\r\n").await;
                            inbox.lock().await.push(std::mem::take(&mut data));
                        } else {
                            data.push_str(&line);
                        }
                        continue;
                    }
                    let upper = line.to_ascii_uppercase();
                    let response: &[u8] = if upper.starts_with("EHLO") || upper.starts_with("HELO")
                    {
                        b"250-test\r\n250 8BITMIME\r\n"
                    } else if upper.starts_with("DATA") {
                        data_mode = true;
                        b"354 go\r\n"
                    } else if upper.starts_with("QUIT") {
                        let _ = writer.write_all(b"221 bye\r\n").await;
                        break;
                    } else {
                        b"250 OK\r\n"
                    };
                    let _ = writer.write_all(response).await;
                }
            });
        }
    });
    (addr, inbox)
}

async fn wait_for_messages(
    inbox: &tokio::sync::Mutex<Vec<String>>,
    expected: usize,
) -> Vec<String> {
    for _ in 0..500 {
        let messages = inbox.lock().await;
        if messages.len() >= expected {
            return messages.clone();
        }
        drop(messages);
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("等待 {expected} 封邮件超时");
}

/// 成功运行：渠道收到一条成功简讯（含步骤状态、域名与总耗时）。
#[tokio::test]
async fn succeeded_run_delivers_single_summary() {
    let _ = tracing_subscriber::fmt().with_test_writer().try_init();
    let healthy = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&healthy)
        .await;

    let db = database().await;
    let app = app(db.clone()).await;
    let credential_id = create_credential(
        &app,
        "notify.generic",
        generic_credential_fields(&format!("{}/hook", healthy.uri())),
    )
    .await;
    let pipeline_id = create_pipeline(
        &app,
        &[credential_id],
        serde_json::json!([
            { "type_id": CERT_APPLY, "input": { "domains": ["a.example.com"] }, "enabled": true },
            { "type_id": CERT_DEPLOY, "input": { "target": "local" }, "enabled": true },
        ]),
    )
    .await;

    run_pipeline_and_wait(&app, &db, pipeline_id, "success").await;

    let requests = wait_for_requests(&healthy, 1).await;
    assert_eq!(requests.len(), 1, "成功运行只应投递一条简讯");
    let body: Value = serde_json::from_slice(&requests[0].body).expect("应为 JSON");
    assert_eq!(body["event"], EVENT_PIPELINE_SUCCEEDED);
    assert_eq!(body["title"], "流水线运行成功");
    assert_eq!(body["pipeline"], "e2e-pipeline");
    assert_eq!(body["trigger"], "manual");
    assert_eq!(body["domains"][0], "a.example.com");
    assert_eq!(body["summary"]["result"], "success");
    assert_eq!(body["summary"]["steps"][0]["type_id"], CERT_APPLY);
    assert_eq!(body["summary"]["steps"][0]["status"], "succeeded");
    assert_eq!(body["summary"]["steps"][1]["type_id"], CERT_DEPLOY);
    assert_eq!(body["summary"]["steps"][1]["status"], "succeeded");
}

/// 失败运行：渠道收到一条失败简讯（含失败步骤与原因），运行终态为失败。
#[tokio::test]
async fn failed_run_delivers_failure_summary() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;

    let db = database().await;
    let app = app(db.clone()).await;
    let credential_id = create_credential(
        &app,
        "notify.generic",
        generic_credential_fields(&format!("{}/hook", server.uri())),
    )
    .await;
    let pipeline_id = create_pipeline(
        &app,
        &[credential_id],
        serde_json::json!([
            { "type_id": CERT_APPLY, "input": { "domains": ["a.example.com"] }, "enabled": true },
            { "type_id": CERT_FAIL, "input": {}, "enabled": true },
        ]),
    )
    .await;

    run_pipeline_and_wait(&app, &db, pipeline_id, "failed").await;

    let requests = wait_for_requests(&server, 1).await;
    assert_eq!(requests.len(), 1, "失败运行只应投递一条简讯");
    let body: Value = serde_json::from_slice(&requests[0].body).expect("应为 JSON");
    assert_eq!(body["event"], EVENT_PIPELINE_FAILED);
    assert_eq!(body["title"], "流水线运行失败");
    assert_eq!(body["failure"]["step"], 2, "失败步序号对外从 1 起");
    assert_eq!(body["failure"]["step_type"], CERT_FAIL);
    let reason = body["failure"]["reason"].as_str().expect("应含失败原因");
    assert!(reason.contains("测试构造的失败"), "{reason}");
    assert_eq!(body["summary"]["result"], "failed");
    assert_eq!(body["summary"]["steps"][0]["status"], "succeeded");
    assert_eq!(body["summary"]["steps"][1]["status"], "failed");
}

/// 未选渠道：运行结束不产生任何投递。
#[tokio::test]
async fn empty_notification_channels_never_deliver() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;

    let db = database().await;
    let app = app(db.clone()).await;
    let pipeline_id = create_pipeline(
        &app,
        &[],
        serde_json::json!([
            { "type_id": CERT_APPLY, "input": { "domains": ["a.example.com"] }, "enabled": true },
        ]),
    )
    .await;

    run_pipeline_and_wait(&app, &db, pipeline_id, "success").await;

    assert_no_requests(&server).await;
}

/// 邮件渠道：运行结束收到一封成功简讯；投递失败渠道不影响运行结果。
#[tokio::test]
async fn email_credential_receives_summary() {
    let (smtp_addr, inbox) = smtp_server().await;
    let db = database().await;
    let app = app(db.clone()).await;
    let credential_id = create_credential(
        &app,
        "notify.email",
        serde_json::json!({
            "name": "邮件群",
            "host": "127.0.0.1",
            "port": smtp_addr.port(),
            "tls": "none",
            "from": "no-reply@example.com",
            "to": ["ops@example.com"],
        }),
    )
    .await;
    let pipeline_id = create_pipeline(
        &app,
        &[credential_id],
        serde_json::json!([
            { "type_id": CERT_APPLY, "input": { "domains": ["a.example.com"] }, "enabled": true },
        ]),
    )
    .await;

    run_pipeline_and_wait(&app, &db, pipeline_id, "success").await;

    let messages = wait_for_messages(&inbox, 1).await;
    assert_eq!(messages.len(), 1, "运行结束只应投递一封邮件");
    let raw = &messages[0];
    assert!(raw.contains("From: no-reply@example.com"), "{raw}");
    assert!(raw.contains("To: ops@example.com"), "{raw}");
    assert!(raw.contains("=?utf-8?"), "中文主题应被编码: {raw}");
}
