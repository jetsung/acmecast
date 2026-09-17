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
