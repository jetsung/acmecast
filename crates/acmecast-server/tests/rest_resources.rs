//! 10.3 REST 资源端点的最小闭环测试。

use acmecast_server::{AppState, RuntimeState, assemble_router_with_runtime};
use acmecast_store::migrate;
use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use sea_orm::Database;
use tower::ServiceExt;

/// 这些用例只关心资源端点的响应格式，鉴权交给专门的用例覆盖，
/// 这里显式关闭以免每个请求都要先换令牌。
async fn app() -> axum::Router {
    let db = Database::connect("sqlite::memory:")
        .await
        .expect("应能连接内存数据库");
    migrate(&db).await.expect("迁移应成功");
    assemble_router_with_runtime(
        AppState {
            db,
            config: acmecast_server::config::ServerConfig::default(),
        },
        RuntimeState::new(
            acmecast_server::dependencies::credential_registry(),
            acmecast_server::dependencies::step_registry(),
            None,
            acmecast_server::auth::AuthMode::Disabled,
        ),
    )
}

async fn json_body(response: axum::response::Response) -> serde_json::Value {
    let bytes = to_bytes(response.into_body(), 1024 * 1024)
        .await
        .expect("应能读取响应体");
    serde_json::from_slice(&bytes).expect("响应应是 JSON")
}

#[tokio::test]
async fn pipeline_crud_uses_uniform_data_envelope_and_returns_404() {
    let app = app().await;
    let create = Request::post("/api/pipelines")
        .header("content-type", "application/json")
        .body(Body::from(
            r#"{"name":"daily renewal","steps":[{"type_id":"test.step","input":{"value":1}}]}"#,
        ))
        .unwrap();
    let response = app.clone().oneshot(create).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = json_body(response).await;
    let id = body["data"]["id"].as_i64().expect("应返回流水线 id");

    let list = app
        .clone()
        .oneshot(
            Request::get("/api/pipelines?page=1&page_size=10")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(list.status(), StatusCode::OK);
    let list_body = json_body(list).await;
    assert_eq!(list_body["data"]["total"], 1);
    assert_eq!(list_body["data"]["items"][0]["name"], "daily renewal");

    let missing = app
        .oneshot(
            Request::get(format!("/api/pipelines/{}", id + 1))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);
    let missing_body = json_body(missing).await;
    assert_eq!(missing_body["error"]["code"], "not_found");
}

#[tokio::test]
async fn malformed_json_returns_uniform_validation_error() {
    let app = app().await;
    let response = app
        .oneshot(
            Request::post("/api/pipelines")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"name":"broken","steps":}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = json_body(response).await;
    assert_eq!(body["error"]["code"], "invalid_json");
}

/// 带加密密钥的运行装配：凭据端点没有 cipher 会直接报配置错误。
async fn app_with_cipher() -> axum::Router {
    let db = Database::connect("sqlite::memory:")
        .await
        .expect("应能连接内存数据库");
    migrate(&db).await.expect("迁移应成功");
    let cipher = acmecast_core::CredentialCipher::from_bytes(&[7u8; 32]).expect("密钥应合法");
    assemble_router_with_runtime(
        AppState {
            db,
            config: acmecast_server::config::ServerConfig::default(),
        },
        RuntimeState::new(
            acmecast_server::dependencies::credential_registry(),
            acmecast_server::dependencies::step_registry(),
            Some(std::sync::Arc::new(cipher)),
            acmecast_server::auth::AuthMode::Disabled,
        ),
    )
}

#[tokio::test]
async fn credential_detail_returns_decrypted_fields_for_editing() {
    // spec（2.4）：编辑弹窗要回填本条记录的现值——PUT 是整体替换语义，
    // GET 详情若不带字段，用户一保存就会把密钥清成空。
    let app = app_with_cipher().await;
    let create = Request::post("/api/credentials")
        .header("content-type", "application/json")
        .body(Body::from(
            r#"{"name":"cf","type_id":"cloudflare","fields":{"api_token":"  secret-tok  "}}"#,
        ))
        .unwrap();
    let response = app.clone().oneshot(create).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = json_body(response).await;
    let id = body["data"]["id"].as_i64().expect("应返回凭据 id");
    // 列表项不带字段值。
    assert!(body["data"].get("fields").is_none());

    let detail = app
        .clone()
        .oneshot(
            Request::get(format!("/api/credentials/{id}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(detail.status(), StatusCode::OK);
    let detail_body = json_body(detail).await;
    assert_eq!(detail_body["data"]["name"], "cf");
    assert_eq!(detail_body["data"]["type_id"], "cloudflare");
    assert_eq!(
        detail_body["data"]["fields"]["api_token"].as_str(),
        Some("secret-tok"),
        "详情应带回保存时已去空白的字段值"
    );

    // 列表依旧不外泄字段值。
    let list = app
        .oneshot(
            Request::get("/api/credentials?page=1&page_size=10")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let list_body = json_body(list).await;
    assert!(list_body["data"]["items"][0].get("fields").is_none());
}
