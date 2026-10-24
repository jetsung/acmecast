//! 同步腾讯云 SSL 证书列表端点（部署表单「证书 ID」下拉的数据来源）。
//!
//! 传输层替身经 `RuntimeState.tencent_http` 注入（生产为 `None` 走真实网络）。
//! 两条主线：`cloud.tencent` 凭据拉到全量证书选项（label 口径与 spec 一致、
//! 不带 SearchKey、走凭据站点端点）；非 `cloud.tencent` 凭据在任何云端调用
//! 之前以可读输入错误失败（错误口径与部署路径同一实现）。

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use acmecast_dns::{HttpRequest, HttpResponse, HttpTransport};
use acmecast_server::{AppState, RuntimeState, assemble_router_with_runtime};
use acmecast_store::migrate;
use async_trait::async_trait;
use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use sea_orm::Database;
use tower::ServiceExt;

/// 按脚本依次应答的传输层替身（与 deploy 侧测试同构）。
#[derive(Debug, Default)]
struct ScriptedTransport {
    responses: Mutex<VecDeque<HttpResponse>>,
    requests: Mutex<Vec<HttpRequest>>,
}

impl ScriptedTransport {
    fn new(responses: Vec<HttpResponse>) -> Arc<Self> {
        Arc::new(Self {
            responses: Mutex::new(responses.into()),
            requests: Mutex::new(Vec::new()),
        })
    }

    fn requests(&self) -> Vec<HttpRequest> {
        self.requests.lock().expect("锁不应中毒").clone()
    }
}

#[async_trait]
impl HttpTransport for ScriptedTransport {
    async fn send(&self, request: HttpRequest) -> acmecast_dns::Result<HttpResponse> {
        self.requests.lock().expect("锁不应中毒").push(request);
        self.responses
            .lock()
            .expect("锁不应中毒")
            .pop_front()
            .ok_or_else(|| acmecast_dns::Error::provider("脚本里没有更多响应了".to_owned()))
    }
}

/// `DescribeCertificates` 的一页：`certs` 为 (域名, 证书 ID, 可选别名)。
fn describe_page(total: u64, certs: &[(&str, &str, Option<&str>)]) -> HttpResponse {
    let items: Vec<serde_json::Value> = certs
        .iter()
        .map(|(domain, id, alias)| {
            let mut item = serde_json::json!({
                "CertificateId": id,
                "Domain": domain,
                "CertificateType": "SVR",
            });
            if let Some(alias) = alias {
                item["Alias"] = serde_json::json!(alias);
            }
            item
        })
        .collect();
    HttpResponse::new(
        200,
        serde_json::json!({"Response": {"TotalCount": total, "Certificates": items, "RequestId": "req"}})
            .to_string(),
    )
}

/// 装配带加密密钥与替身传输层的路由（鉴权关闭，专注端点语义）。
async fn app_with_mock(transport: Arc<dyn HttpTransport>) -> axum::Router {
    let db = Database::connect("sqlite::memory:")
        .await
        .expect("应能连接内存数据库");
    migrate(&db).await.expect("迁移应成功");
    let cipher =
        Arc::new(acmecast_core::CredentialCipher::from_bytes(&[7u8; 32]).expect("密钥应合法"));
    let mut runtime = RuntimeState::new(
        acmecast_server::dependencies::credential_registry(),
        acmecast_server::dependencies::step_registry(),
        Some(cipher),
        acmecast_server::auth::AuthMode::Disabled,
    );
    runtime.tencent_http = Some(transport);
    assemble_router_with_runtime(
        AppState {
            db,
            config: acmecast_server::config::ServerConfig::default(),
        },
        runtime,
    )
}

async fn json_body(response: axum::response::Response) -> serde_json::Value {
    let bytes = to_bytes(response.into_body(), 1024 * 1024)
        .await
        .expect("应能读取响应体");
    serde_json::from_slice(&bytes).expect("响应应是 JSON")
}

/// 经创建端点插一条凭据（走真实校验与加密链路），返回主键。
async fn create_credential(app: &axum::Router, type_id: &str, fields: serde_json::Value) -> i64 {
    let create = Request::post("/api/credentials")
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::json!({ "name": "腾讯云凭据", "type_id": type_id, "fields": fields })
                .to_string(),
        ))
        .unwrap();
    let response = app.clone().oneshot(create).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK, "凭据应能创建");
    json_body(response).await["data"]["id"]
        .as_i64()
        .expect("应返回凭据 id")
}

#[tokio::test]
async fn sync_returns_certificates_with_declared_labels() {
    let transport = ScriptedTransport::new(vec![describe_page(
        2,
        &[
            ("idev.top", "a9ZwitUs", Some("idev")),
            ("example.com", "CERTB", None),
        ],
    )]);
    let app = app_with_mock(Arc::clone(&transport) as Arc<dyn HttpTransport>).await;
    let id = create_credential(
        &app,
        "cloud.tencent",
        serde_json::json!({ "secret_id": "AKIDexample", "secret_key": "secret-key" }),
    )
    .await;

    let response = app
        .clone()
        .oneshot(
            Request::get(format!("/api/credentials/{id}/tencent-ssl-certificates"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let body = json_body(response).await;
    let options = body["data"].as_array().expect("应为选项数组");
    assert_eq!(options.len(), 2);
    assert_eq!(options[0]["value"], "a9ZwitUs");
    assert_eq!(
        options[0]["label"], "a9ZwitUs (idev.top - idev)",
        "有别名：证书ID (域名 - 别名)"
    );
    assert_eq!(options[1]["value"], "CERTB");
    assert_eq!(
        options[1]["label"], "CERTB (example.com)",
        "无别名：证书ID (域名)"
    );

    // 同步是全量拉取：不带 SearchKey；凭据缺省国内站，请求发往国内端点。
    let requests = transport.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].url, "https://ssl.tencentcloudapi.com/");
    let payload = requests[0].body.as_ref().expect("查询应有请求体");
    assert!(
        payload.get("SearchKey").is_none(),
        "同步端点不应携带 SearchKey"
    );
    assert_eq!(payload["CertificateType"], "SVR");
}

#[tokio::test]
async fn sync_rejects_non_tencent_credential_without_requests() {
    // 空脚本：任何云端请求都会因脚本耗尽而报错——若发生即失败暴露。
    let transport = ScriptedTransport::new(vec![]);
    let app = app_with_mock(Arc::clone(&transport) as Arc<dyn HttpTransport>).await;
    let id = create_credential(
        &app,
        "cloud.cloudflare",
        serde_json::json!({ "api_token": "cf-token" }),
    )
    .await;

    let response = app
        .clone()
        .oneshot(
            Request::get(format!("/api/credentials/{id}/tencent-ssl-certificates"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(
        response.status(),
        StatusCode::BAD_REQUEST,
        "非腾讯云凭据应被拒绝"
    );
    let body = json_body(response).await;
    let message = body["error"]["message"]
        .as_str()
        .expect("错误应携带可读消息");
    assert!(
        message.contains("可用于 SSL 部署"),
        "错误口径应与部署路径同一实现: {message}"
    );
    assert_eq!(body["error"]["field"], "credential_id");
    assert!(
        transport.requests().is_empty(),
        "被拒绝时不得发起任何云端调用"
    );
}
