//! 10.4 OpenAPI 文档、10.8 请求体限制与下载响应头、10.9 请求追踪标识。
//!
//! 三个任务的共同点是「约定」而非「业务」：文档要与路由同步，超限要
//! 明确拒绝，下载要带齐响应头，日志要能按请求串起来。
//!
//! 本文件的用例全部标了 `#[serial]`，包括不使用订阅器的那几个。原因在 10.9：
//! 那两个用例用线程本地的 `subscriber::set_default` 捕获日志，而同时并发的其他
//! 用例会命中同一批 callsite，tracing 的全局 callsite 兴趣缓存与最大级别会短暂
//! 不一致，日志行偶发丢失——实测 CPU 竞争下约 12% 概率只剩 1 行，断言 `>= 2` 就挂。
//! 只串行那两个用例没用（实测仍 4/40），必须整个文件串行（30 次压测 0 失败）。
//! 代价可忽略：本文件全部用例加起来只跑约 0.3 秒。

mod support;

use std::sync::{Arc, Mutex};

use acmecast_server::openapi::ApiDoc;
use axum::{
    body::Body,
    http::{Request, StatusCode, header},
};
use serial_test::serial;
use support::*;
use utoipa::OpenApi;

// ---- 10.4 OpenAPI 文档 ----

/// 文档必须覆盖的 API 端点清单，与 `handlers::routes()` 一一对应。
///
/// `/api/openapi.json` 与 `/swagger-ui` 由 `SwaggerUi` 挂载、与文档本身
/// 同源生成，不在此列。这份清单就是「端点变更后文档同步」的守护：
/// 新增路由而忘记补文档时，这里会失败提醒。
const KNOWN_API_PATHS: [&str; 22] = [
    "/api/login",
    "/api/pipelines",
    "/api/pipelines/{id}",
    "/api/pipelines/{id}/histories",
    "/api/pipelines/{id}/run",
    "/api/certificates",
    "/api/certificates/{id}",
    "/api/certificates/{id}/download",
    "/api/certificates/{id}/revoke",
    "/api/credentials",
    "/api/credentials/{id}",
    "/api/credentials/{id}/test",
    "/api/credential-types",
    "/api/notifications/test",
    "/api/tasks",
    "/api/tasks/{type_id}/schema",
    "/api/histories",
    "/api/histories/{id}",
    "/api/histories/{id}/logs",
    "/api/schedules",
    "/api/schedules/trigger-logs",
    "/healthz",
];

#[test]
#[serial]
fn openapi_document_covers_every_api_endpoint() {
    let doc = ApiDoc::openapi();
    let paths = doc.paths.paths.keys().cloned().collect::<Vec<_>>();

    for path in KNOWN_API_PATHS {
        assert!(
            paths.iter().any(|documented| documented == path),
            "文档缺少端点 {path}；已覆盖：{paths:?}"
        );
    }
    assert_eq!(
        paths.len(),
        KNOWN_API_PATHS.len(),
        "文档里出现了清单之外的端点，请把 KNOWN_API_PATHS 与 handlers::routes() 对齐"
    );
}

#[test]
#[serial]
fn openapi_document_declares_bearer_auth() {
    let doc = ApiDoc::openapi();
    let components = doc.components.as_ref().expect("文档应声明 components");
    let scheme = components
        .security_schemes
        .get("bearerAuth")
        .expect("应声明 bearerAuth 安全方案");
    let utoipa::openapi::security::SecurityScheme::Http(http) = scheme else {
        panic!("bearerAuth 应是 HTTP 方案");
    };
    assert!(matches!(
        http.scheme,
        utoipa::openapi::security::HttpAuthScheme::Bearer
    ));
    assert_eq!(http.bearer_format.as_deref(), Some("JWT"));
}

#[test]
#[serial]
fn openapi_document_marks_protected_endpoints() {
    let doc = ApiDoc::openapi();

    // 登录是换取令牌的入口，不应要求令牌。
    let login = doc.paths.paths.get("/api/login").expect("应有登录端点");
    assert!(
        login.post.as_ref().is_some_and(|op| op.security.is_none()),
        "登录端点不应声明 security"
    );

    // 业务端点都要求 Bearer。
    let pipelines = doc
        .paths
        .paths
        .get("/api/pipelines")
        .expect("应有流水线端点");
    for (method, op) in [("get", &pipelines.get), ("post", &pipelines.post)] {
        let op = op.as_ref().unwrap_or_else(|| panic!("{method} 应有文档"));
        let security = op.security.as_ref().expect("{method} 应声明 security");
        let rendered = serde_json::to_string(security).expect("security 应能序列化");
        assert!(
            rendered.contains("bearerAuth"),
            "{method} 应引用 bearerAuth，实际为 {rendered}"
        );
    }
}

// ---- 10.8 请求体体积限制 ----

#[tokio::test]
#[serial]
async fn oversized_request_bodies_are_rejected_with_413() {
    let config = acmecast_server::config::ServerConfig {
        body_limit_bytes: 64,
        ..Default::default()
    };
    let app = app_with(config, acmecast_server::auth::AuthMode::Disabled).await;

    let oversized = serde_json::json!({
        "name": "这条流水线名称远超 64 字节上限，用于触发请求体体积限制",
        "steps": [{ "type_id": "noop", "input": {}, "enabled": true }]
    });
    let response = send(&app, json_request("POST", "/api/pipelines", &oversized)).await;

    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    let body = json(response).await;
    assert_eq!(
        body["error"]["code"], "payload_too_large",
        "超限应是明确的错误码而非 JSON 解析失败：{body}"
    );
}

// ---- 10.8 二进制下载响应头 ----

/// 装配带一条已入库证书的路由；返回路由与数据目录（必须保活，证书文件在里）。
async fn app_with_downloadable_certificate() -> (axum::Router, TempDir) {
    // 证书文件落在临时数据目录，配置指向它，下载端点才读得到。
    let fixture = certificate_fixture().await;
    let (cert_pem_path, key_pem_path) = (fixture.1.clone(), fixture.2.clone());
    let config = acmecast_server::config::ServerConfig {
        data_dir: fixture.0.path().to_path_buf(),
        ..Default::default()
    };
    let app = app_with(config, acmecast_server::auth::AuthMode::Disabled).await;

    // 经 REST 入库一条证书记录，与生产写入路径一致。
    let response = send(
        &app,
        json_request(
            "POST",
            "/api/certificates",
            &serde_json::json!({
                "domains": ["download.example.com"],
                "cert_pem_path": cert_pem_path,
                "key_pem_path": key_pem_path,
                "fingerprint": "sha256:download-test",
                "not_before": "2026-01-01T00:00:00Z",
                "not_after": "2027-01-01T00:00:00Z"
            }),
        ),
    )
    .await;
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "证书记录应能创建：{}",
        serde_json::to_string(&json(response).await).unwrap()
    );
    (app, fixture.0)
}

#[tokio::test]
#[serial]
async fn pem_download_carries_filename_and_content_type() {
    let (app, _data_dir) = app_with_downloadable_certificate().await;

    let response = send(
        &app,
        Request::get("/api/certificates/1/download")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);

    let disposition = response
        .headers()
        .get(header::CONTENT_DISPOSITION)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_owned();
    assert!(
        disposition.contains("attachment") && disposition.contains("download.example.com.pem"),
        "应带附件文件名，实际为 `{disposition}`"
    );
    let content_type = response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    assert!(
        content_type.contains("x-pem-file") || content_type.contains("pem"),
        "PEM 应带证书内容类型，实际为 `{content_type}`"
    );
}

#[tokio::test]
#[serial]
async fn pfx_download_is_binary_with_keystore_content_type() {
    let (app, _data_dir) = app_with_downloadable_certificate().await;

    let response = send(
        &app,
        Request::get("/api/certificates/1/download?format=pfx&password=secret")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);

    let content_type = response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_owned();
    assert!(
        content_type.contains("pkcs12"),
        "PFX 应带二进制密钥库类型，实际为 `{content_type}`"
    );
    let disposition = response
        .headers()
        .get(header::CONTENT_DISPOSITION)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_owned();
    assert!(
        disposition.contains("download.example.com.pfx"),
        "PFX 文件名应带 .pfx 后缀，实际为 `{disposition}`"
    );
    let bytes = axum::body::to_bytes(response.into_body(), 64 * 1024)
        .await
        .expect("应能读取响应体");
    assert!(!bytes.is_empty(), "PFX 响应体不应为空");
}

// ---- 10.9 请求追踪标识 ----

#[derive(Clone, Default)]
struct LogCapture(Arc<Mutex<Vec<u8>>>);

impl LogCapture {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.0.lock().expect("日志捕获锁不会中毒")).into_owned()
    }
}

impl std::io::Write for LogCapture {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        self.0
            .lock()
            .expect("日志捕获锁不会中毒")
            .extend_from_slice(buffer);
        Ok(buffer.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'writer> tracing_subscriber::fmt::MakeWriter<'writer> for LogCapture {
    type Writer = LogCapture;

    fn make_writer(&'writer self) -> Self::Writer {
        self.clone()
    }
}

#[tokio::test]
#[serial]
async fn all_log_lines_of_a_request_share_the_request_id() {
    let capture = LogCapture::default();
    let subscriber = tracing_subscriber::fmt()
        .with_ansi(false)
        .with_max_level(tracing::Level::DEBUG)
        .with_writer(capture.clone())
        .finish();
    let _guard = tracing::subscriber::set_default(subscriber);

    let app = open_app().await;
    // 客户端自带标识：网关场景下标识由上游生成，服务端应保留而不是重造。
    let response = send(
        &app,
        Request::get("/healthz")
            .header("x-request-id", "trace-abc-123")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    // 标识回写到响应头，客户端可据此排查。
    assert_eq!(
        response
            .headers()
            .get("x-request-id")
            .and_then(|value| value.to_str().ok()),
        Some("trace-abc-123"),
        "响应应回写请求的追踪标识"
    );

    let logs = capture.text();
    let request_id_lines = logs
        .lines()
        .filter(|line| line.contains("request_id") && line.contains("trace-abc-123"))
        .count();
    assert!(
        request_id_lines >= 2,
        "同一次请求应产生多条共享同一标识的日志，实际捕获：\n{logs}"
    );
}

#[tokio::test]
#[serial]
async fn requests_without_a_client_id_get_a_generated_one() {
    let capture = LogCapture::default();
    let subscriber = tracing_subscriber::fmt()
        .with_ansi(false)
        .with_max_level(tracing::Level::DEBUG)
        .with_writer(capture.clone())
        .finish();
    let _guard = tracing::subscriber::set_default(subscriber);

    let app = open_app().await;
    let response = send(&app, Request::get("/healthz").body(Body::empty()).unwrap()).await;
    assert_eq!(response.status(), StatusCode::OK);

    let generated = response
        .headers()
        .get("x-request-id")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_owned();
    assert!(!generated.is_empty(), "服务端应为无标识的请求生成一个");
    assert_ne!(generated, "unknown", "生成的标识不应是占位符");

    let logs = capture.text();
    assert!(
        logs.contains(&generated),
        "日志里应携带生成的标识 {generated}：\n{}",
        logs
    );
}
