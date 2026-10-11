//! 5.2 端到端：流水线运行触发 webhook 通知。
//!
//! 全链路走真实 HTTP 与真实执行器：手动触发含 `cert.apply` + `cert.deploy`
//! 的流水线，断言订阅渠道收到两条消息、消息字段符合 spec；同时用一个
//! 持续失败的渠道证明投递失败不影响流水线结果。

mod support;

use std::sync::Arc;
use std::time::Duration;

use acmecast_notify::{
    ChannelConfig, DeliveryOptions, EVENT_CERT_APPLY, EVENT_CERT_DEPLOY, SmtpSettings, TlsMode,
    WebhookEventSink,
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

#[allow(unsafe_code)]
async fn app(db: DatabaseConnection, channels: Vec<ChannelConfig>) -> axum::Router {
    unsafe {
        std::env::set_var("NO_PROXY", "127.0.0.1,localhost");
        std::env::set_var("no_proxy", "127.0.0.1,localhost");
    }
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

fn email_channel(name: &str, events: &[&str], smtp: SmtpSettings) -> ChannelConfig {
    ChannelConfig {
        name: name.to_owned(),
        provider: "email".to_owned(),
        url: String::new(),
        secret: None,
        events: events.iter().map(|e| (*e).to_owned()).collect(),
        enabled: true,
        sign: None,
        method: "POST".to_owned(),
        headers: std::collections::BTreeMap::new(),
        body_template: None,
        smtp: Some(smtp),
    }
}

fn smtp_settings(port: u16) -> SmtpSettings {
    SmtpSettings {
        host: "127.0.0.1".to_owned(),
        port: Some(port),
        from: "no-reply@example.com".to_owned(),
        to: vec!["ops@example.com".to_owned()],
        username: None,
        password: None,
        tls: TlsMode::None,
    }
}

/// 极简 SMTP 服务器：匿名、无 TLS，收集每次会话的 DATA 负载。
///
/// 只为 e2e 打通「事件 → EmailChannel → SMTP 会话」链路，不支持认证，
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

/// 创建含 cert.apply + cert.deploy 两个步骤的流水线，返回其 id。
async fn create_apply_deploy_pipeline(app: &axum::Router) -> i64 {
    let response = send(
        app,
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
    body["data"]["id"].as_i64().expect("应返回流水线 id")
}

/// 手动触发运行并轮询数据库至终态，断言成功（投递失败不影响运行结果）。
async fn run_pipeline_and_wait_success(
    app: &axum::Router,
    db: &sea_orm::DatabaseConnection,
    pipeline_id: i64,
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
    assert_eq!(history_status, "success", "流水线运行应为成功");
}

#[tokio::test]
async fn pipeline_run_pushes_apply_and_deploy_messages() {
    let _ = tracing_subscriber::fmt().with_test_writer().try_init();
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
            smtp: None,
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
            smtp: None,
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

#[tokio::test]
async fn email_channel_receives_events_and_smtp_failure_is_isolated() {
    let (smtp_addr, inbox) = smtp_server().await;
    let db = database().await;
    // 健康邮件渠道订阅两个事件；另一渠道指向不可达端口，验证投递失败
    // 与流水线结果隔离（与 webhook 失败渠道同一语义）。
    let channels = vec![
        email_channel(
            "mail-ops",
            &[EVENT_CERT_APPLY, EVENT_CERT_DEPLOY],
            smtp_settings(smtp_addr.port()),
        ),
        email_channel(
            "mail-down",
            &[EVENT_CERT_APPLY],
            smtp_settings(9),
        ),
    ];
    let app = app(db.clone(), channels).await;

    let pipeline_id = create_apply_deploy_pipeline(&app).await;
    run_pipeline_and_wait_success(&app, &db, pipeline_id).await;

    // 邮件渠道收到两封信：申请（含域名）与部署（含目标），发件与收件
    // 人按渠道配置展开。
    let messages = wait_for_messages(&inbox, 2).await;
    assert_eq!(messages.len(), 2);
    let subjects: Vec<&str> = messages
        .iter()
        .filter_map(|raw| {
            raw.lines().find(|line| line.starts_with("Subject:"))
        })
        .collect();
    assert_eq!(subjects.len(), 2, "两封邮件主题应各不相同: {messages:?}");
    for raw in &messages {
        assert!(raw.contains("From: no-reply@example.com"), "{raw}");
        assert!(raw.contains("To: ops@example.com"), "{raw}");
        assert!(raw.contains("=?utf-8?"), "中文主题应被编码: {raw}");
    }
    // 两封主题对应两个事件（证书申请成功 / 证书部署成功）。
    assert_ne!(subjects[0], subjects[1], "两个事件的邮件主题应可区分");
}
