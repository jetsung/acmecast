//! 4.9 通知渠道测试端点：`POST /api/notifications/test`。
//!
//! 覆盖 spec 的三个场景：指定渠道成功、失败渠道带回原因、无启用渠道时
//! 返回可诊断结果；外加未鉴权拒绝与「指定渠道不存在」两个边界。

mod support;

use acmecast_server::auth::AuthMode;
use acmecast_server::config::ServerConfig;
use axum::{body::Body, http::Request};
use support::*;
use wiremock::matchers::method;
use wiremock::{Mock, MockServer, ResponseTemplate};

/// 配置一个指向 `url` 的启用渠道。
fn config_with_channel(name: &str, url: &str) -> ServerConfig {
    ServerConfig {
        notifications: vec![acmecast_notify::ChannelConfig {
            name: name.to_owned(),
            provider: "generic".to_owned(),
            url: url.to_owned(),
            secret: None,
            events: vec!["cert.apply".to_owned()],
            enabled: true,
            sign: None,
            method: "POST".to_owned(),
            headers: std::collections::BTreeMap::new(),
            body_template: None,
        }],
        ..ServerConfig::default()
    }
}

#[tokio::test]
async fn test_specified_channel_sends_message_and_reports_ok() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;

    let app = support::app_with(
        config_with_channel("ops", &format!("{}/hook", server.uri())),
        AuthMode::Disabled,
    )
    .await;

    let response = send(
        &app,
        json_request(
            "POST",
            "/api/notifications/test",
            &serde_json::json!({ "name": "ops" }),
        ),
    )
    .await;
    assert_eq!(response.status(), axum::http::StatusCode::OK);
    let body = json(response).await;
    assert_eq!(body["data"]["results"][0]["name"], "ops");
    assert_eq!(body["data"]["results"][0]["ok"], true);
    assert!(body["data"]["results"][0]["error"].is_null());

    let requests = server.received_requests().await.expect("应记录请求");
    let payload: serde_json::Value =
        serde_json::from_slice(&requests[0].body).expect("测试消息应为 JSON");
    assert_eq!(payload["event"], "test");
}

#[tokio::test]
async fn failing_channel_reports_reason() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(500).set_body_string("boom"))
        .mount(&server)
        .await;

    let app = support::app_with(
        config_with_channel("broken", &format!("{}/hook", server.uri())),
        AuthMode::Disabled,
    )
    .await;

    let response = send(
        &app,
        json_request(
            "POST",
            "/api/notifications/test",
            &serde_json::json!({ "name": "broken" }),
        ),
    )
    .await;
    // 渠道投递失败不是端点失败：200 里逐渠道给出失败原因。
    assert_eq!(response.status(), axum::http::StatusCode::OK);
    let body = json(response).await;
    assert_eq!(body["data"]["results"][0]["ok"], false);
    let error = body["data"]["results"][0]["error"]
        .as_str()
        .expect("应有原因");
    assert!(error.contains("500"), "{error}");
}

#[tokio::test]
async fn no_enabled_channels_returns_diagnostic() {
    let app = support::app_with(ServerConfig::default(), AuthMode::Disabled).await;

    let response = send(
        &app,
        json_request("POST", "/api/notifications/test", &serde_json::json!({})),
    )
    .await;
    assert_eq!(response.status(), axum::http::StatusCode::CONFLICT);
    let body = json(response).await;
    let message = body["error"]["message"].as_str().expect("应有错误说明");
    assert!(message.contains("没有可测试的通知渠道"), "{message}");
}

#[tokio::test]
async fn unknown_channel_name_returns_not_found() {
    let app = support::app_with(ServerConfig::default(), AuthMode::Disabled).await;

    let response = send(
        &app,
        json_request(
            "POST",
            "/api/notifications/test",
            &serde_json::json!({ "name": "missing" }),
        ),
    )
    .await;
    assert_eq!(response.status(), axum::http::StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn unauthenticated_request_is_rejected_without_sending() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;

    let app = support::app_with(
        config_with_channel("ops", &format!("{}/hook", server.uri())),
        AuthMode::enforced(auth_config()),
    )
    .await;

    let request = Request::builder()
        .method("POST")
        .uri("/api/notifications/test")
        .body(Body::empty())
        .expect("应能构造请求");
    let response = send(&app, request).await;
    assert_eq!(response.status(), axum::http::StatusCode::UNAUTHORIZED);

    let requests = server.received_requests().await.unwrap_or_default();
    assert!(requests.is_empty(), "未鉴权不应发送任何测试消息");
}
