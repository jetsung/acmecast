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
            smtp: None,
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

/// 三个已声明渠道的配置：两个启用、一个停用；url/secret/headers 塞入
/// 可辨识的取值，供脱敏断言使用。
fn config_with_three_channels() -> ServerConfig {
    let channel = |name: &str, enabled: bool| acmecast_notify::ChannelConfig {
        name: name.to_owned(),
        provider: "generic".to_owned(),
        url: format!("https://hooks.example.invalid/{name}"),
        secret: Some(format!("{name}-secret")),
        events: vec!["cert.apply".to_owned()],
        enabled,
        sign: None,
        method: "POST".to_owned(),
        headers: std::collections::BTreeMap::from([(
            "X-Api-Key".to_owned(),
            format!("{name}-key"),
        )]),
        body_template: None,
        smtp: None,
    };
    ServerConfig {
        notifications: vec![
            channel("ops", true),
            channel("audit", true),
            channel("legacy", false),
        ],
        ..ServerConfig::default()
    }
}

/// 渠道列表端点：全部已声明渠道（含停用）原样返回，凭据字段不泄露。
#[tokio::test]
async fn channels_endpoint_lists_all_declared_channels_including_disabled() {
    let app = support::app_with(config_with_three_channels(), AuthMode::Disabled).await;

    let response = send(
        &app,
        Request::get("/api/notifications/channels")
            .body(Body::empty())
            .expect("应能构造请求"),
    )
    .await;
    assert_eq!(response.status(), axum::http::StatusCode::OK);
    let body = json(response).await;

    let channels = body["data"].as_array().expect("应返回渠道数组");
    let names: Vec<&str> = channels
        .iter()
        .map(|channel| channel["name"].as_str().expect("渠道名应为字符串"))
        .collect();
    assert_eq!(
        names,
        ["ops", "audit", "legacy"],
        "含停用渠道应全部返回：{body}"
    );
    for channel in channels {
        assert!(channel["provider"].is_string(), "应有 provider：{channel}");
        assert!(channel["enabled"].is_boolean(), "应有 enabled：{channel}");
    }
    let enabled = channels
        .iter()
        .filter(|channel| channel["enabled"].as_bool() == Some(true))
        .count();
    assert_eq!(enabled, 2, "启用状态应原样反映配置：{body}");
    let legacy = channels
        .iter()
        .find(|channel| channel["name"] == "legacy")
        .expect("应含停用渠道");
    assert_eq!(
        legacy["enabled"].as_bool(),
        Some(false),
        "停用渠道应列出且标记未启用"
    );

    // 脱敏：url/secret/headers 及其取值一个都不应出现。
    let text = body.to_string();
    for needle in ["url", "secret", "headers", "X-Api-Key", "ops-key"] {
        assert!(!text.contains(needle), "响应不应包含 `{needle}`：{text}");
    }
}

/// 渠道列表端点受鉴权保护：无令牌拒绝，换取令牌后可访问。
#[tokio::test]
async fn channels_endpoint_requires_authentication() {
    let app = support::app_with(
        config_with_three_channels(),
        AuthMode::enforced(auth_config()),
    )
    .await;

    let response = send(
        &app,
        Request::get("/api/notifications/channels")
            .body(Body::empty())
            .expect("应能构造请求"),
    )
    .await;
    assert_eq!(
        response.status(),
        axum::http::StatusCode::UNAUTHORIZED,
        "未鉴权不应返回渠道列表"
    );

    let (status, body) = login(&app, ADMIN_PASSWORD).await;
    assert_eq!(status, axum::http::StatusCode::OK, "登录应成功：{body}");
    let token = body["data"]["token"].as_str().expect("登录应返回令牌");

    let response = send(
        &app,
        with_bearer(
            Request::get("/api/notifications/channels")
                .body(Body::empty())
                .expect("应能构造请求"),
            token,
        ),
    )
    .await;
    assert_eq!(response.status(), axum::http::StatusCode::OK);
    let body = json(response).await;
    assert_eq!(
        body["data"].as_array().map(Vec::len),
        Some(3),
        "带令牌应看到全部渠道：{body}"
    );
}
