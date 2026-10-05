//! 10.5 鉴权中间件与 10.6 登录端点、令牌签发。
//!
//! 用例覆盖 spec 中的三条关键约束：
//! 1. 除登录与静态资源外全部端点强制鉴权，缺失/过期/伪造令牌一律 401；
//! 2. 401 发生后业务逻辑不得执行（用数据库副作用反证）；
//! 3. 登录失败响应不泄露用户名是否存在，连续失败触发限流。

mod support;

use acmecast_server::auth::AuthMode;
use axum::{
    body::Body,
    http::{Request, StatusCode, header},
};
use support::*;

#[tokio::test]
async fn protected_endpoints_reject_requests_without_a_token() {
    let app = authenticated_app().await;

    for (method, uri) in [
        ("GET", "/api/pipelines"),
        ("POST", "/api/pipelines"),
        ("GET", "/api/certificates"),
        ("GET", "/api/credentials"),
        ("GET", "/api/histories"),
    ] {
        let request = Request::builder()
            .method(method)
            .uri(uri)
            .header("content-type", "application/json")
            .body(Body::from("{}"))
            .unwrap();
        let response = send(&app, request).await;
        assert_eq!(
            response.status(),
            StatusCode::UNAUTHORIZED,
            "{method} {uri} 缺少令牌时应 401"
        );
        assert_eq!(
            response.headers().get(header::WWW_AUTHENTICATE).unwrap(),
            "Bearer",
            "应告知客户端用哪种认证方案"
        );
        let body = json(response).await;
        assert_eq!(body["error"]["code"], "unauthorized");
    }
}

#[tokio::test]
async fn unauthorized_request_does_not_touch_business_state() {
    let app = authenticated_app().await;

    // 未鉴权地创建流水线：必须被挡在 handler 之前。
    let request = json_request(
        "POST",
        "/api/pipelines",
        &serde_json::json!({
            "name": "不该被创建",
            "steps": [{"type_id": "test.step", "input": {}}]
        }),
    );
    let response = send(&app, request).await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

    // 换令牌再列一次：若集合为空，说明上一步确实没落到数据库。
    let (_, login_body) = login(&app, ADMIN_PASSWORD).await;
    let token = login_body["data"]["token"].as_str().expect("应返回令牌");
    let list = with_bearer(
        Request::get("/api/pipelines").body(Body::empty()).unwrap(),
        token,
    );
    let response = send(&app, list).await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = json(response).await;
    assert_eq!(body["data"]["total"], 0, "未鉴权的写请求不应产生任何数据");
}

#[tokio::test]
async fn malformed_and_forged_tokens_are_rejected() {
    let app = authenticated_app().await;

    for token in [
        "",                             // 空令牌
        "not-a-jwt",                    // 结构不对
        "eyJhbGciOiJIUzI1NiJ9.e30.xxx", // 签名乱写
    ] {
        let request = Request::get("/api/pipelines").body(Body::empty()).unwrap();
        let request = if token.is_empty() {
            request
        } else {
            with_bearer(request, token)
        };
        let response = send(&app, request).await;
        assert_eq!(
            response.status(),
            StatusCode::UNAUTHORIZED,
            "令牌 `{token}` 应被拒绝"
        );
    }
}

#[tokio::test]
async fn expired_token_is_rejected() {
    use chrono::{Duration as ChronoDuration, Utc};

    // 用负的有效期签发一个「出生即过期」的令牌——比等待更可靠。
    let expired_config = acmecast_server::auth::AuthConfig::new(
        "admin",
        Some(password_hash(ADMIN_PASSWORD)),
        JWT_SECRET,
        // Duration 不接受负数，改为把签发时间推到过去。
        std::time::Duration::from_secs(60),
    );
    let token =
        acmecast_server::auth::issue_token(&expired_config, Utc::now() - ChronoDuration::hours(2))
            .expect("应能签发令牌");

    let app = app_with(
        acmecast_server::config::ServerConfig::default(),
        AuthMode::enforced(expired_config),
    )
    .await;
    let request = with_bearer(
        Request::get("/api/pipelines").body(Body::empty()).unwrap(),
        &token,
    );
    let response = send(&app, request).await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let body = json(response).await;
    assert_eq!(body["error"]["code"], "unauthorized");
}

#[tokio::test]
async fn public_endpoints_stay_reachable_without_a_token() {
    let app = authenticated_app().await;

    // 健康检查：编排系统的探针，不该依赖鉴权。
    let response = send(&app, Request::get("/healthz").body(Body::empty()).unwrap()).await;
    assert_eq!(response.status(), StatusCode::OK);

    // OpenAPI 文档与 Swagger UI：公开内容。
    for uri in ["/api/openapi.json", "/swagger-ui/"] {
        let response = send(&app, Request::get(uri).body(Body::empty()).unwrap()).await;
        assert_eq!(
            response.status(),
            StatusCode::OK,
            "{uri} 应无需令牌即可访问"
        );
    }
}

#[tokio::test]
async fn correct_credentials_issue_a_usable_token() {
    let app = authenticated_app().await;

    let (status, body) = login(&app, ADMIN_PASSWORD).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"]["token_type"], "Bearer");
    assert_eq!(body["data"]["expires_in"], 3600);
    let token = body["data"]["token"].as_str().expect("应返回令牌");

    // 拿这个令牌访问受保护端点。
    let request = with_bearer(
        Request::get("/api/pipelines").body(Body::empty()).unwrap(),
        token,
    );
    let response = send(&app, request).await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = json(response).await;
    assert_eq!(body["data"]["total"], 0, "应返回空列表而非错误");
}

#[tokio::test]
async fn wrong_credentials_do_not_reveal_whether_the_username_exists() {
    let app = authenticated_app().await;

    // 存在的用户名 + 错口令。
    let (wrong_password_status, wrong_password_body) = login(&app, "definitely-wrong").await;
    // 不存在的用户名 + 任意口令。
    let request = json_request(
        "POST",
        "/api/login",
        &serde_json::json!({ "username": "no-such-user", "password": "whatever" }),
    );
    let response = send(&app, request).await;
    let nonexistent_status = response.status();
    let nonexistent_body = json(response).await;

    assert_eq!(wrong_password_status, StatusCode::UNAUTHORIZED);
    assert_eq!(nonexistent_status, StatusCode::UNAUTHORIZED);
    assert_eq!(
        wrong_password_body["error"]["code"], nonexistent_body["error"]["code"],
        "两种失败的错误码应一致"
    );
    assert_eq!(
        wrong_password_body["error"]["message"], nonexistent_body["error"]["message"],
        "两种失败的消息应一致，不泄露用户名是否存在"
    );
}

#[tokio::test]
async fn repeated_failures_lock_the_source_but_not_other_sources() {
    let app = authenticated_app().await;

    // 默认策略是连续 5 次失败后锁定。
    let mut last_status = StatusCode::UNAUTHORIZED;
    for _ in 0..6 {
        let request = json_request(
            "POST",
            "/api/login",
            &serde_json::json!({ "username": "admin", "password": "wrong" }),
        );
        // 固定来源，模拟同一客户端反复尝试。
        let mut request = request;
        request
            .headers_mut()
            .insert("x-forwarded-for", "203.0.113.7".parse().unwrap());
        let response = send(&app, request).await;
        last_status = response.status();
    }
    assert_eq!(
        last_status,
        StatusCode::TOO_MANY_REQUESTS,
        "连续失败达到上限后应被限流"
    );

    // 同一来源即使给出正确口令也被拒——这正是限流的意义。
    let mut request = json_request(
        "POST",
        "/api/login",
        &serde_json::json!({ "username": "admin", "password": ADMIN_PASSWORD }),
    );
    request
        .headers_mut()
        .insert("x-forwarded-for", "203.0.113.7".parse().unwrap());
    let response = send(&app, request).await;
    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    let body = json(response).await;
    assert_eq!(body["error"]["code"], "too_many_attempts");

    // 其他来源不受影响。
    let mut request = json_request(
        "POST",
        "/api/login",
        &serde_json::json!({ "username": "admin", "password": ADMIN_PASSWORD }),
    );
    request
        .headers_mut()
        .insert("x-forwarded-for", "198.51.100.9".parse().unwrap());
    let response = send(&app, request).await;
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "限流应按来源隔离，不该误伤其他客户端"
    );
}

#[tokio::test]
async fn login_is_reported_as_unavailable_when_no_password_hash_is_configured() {
    // 有 JWT 密钥但没配口令哈希：服务能起，但没人能登录。
    let config = acmecast_server::auth::AuthConfig::new(
        "admin",
        None,
        JWT_SECRET,
        std::time::Duration::from_secs(3600),
    );
    let app = app_with(
        acmecast_server::config::ServerConfig::default(),
        AuthMode::enforced(config),
    )
    .await;

    let (status, body) = login(&app, "anything").await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(
        body["error"]["code"], "configuration_error",
        "配置缺失应报成配置错误，而不是「口令错误」"
    );
}

#[tokio::test]
async fn auth_can_be_disabled_for_local_use() {
    let app = open_app().await;

    // 关闭鉴权后无需令牌即可访问。
    let response = send(
        &app,
        Request::get("/api/pipelines").body(Body::empty()).unwrap(),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);

    // 登录端点明确说明鉴权已关闭，而不是返回一个用不上的令牌。
    let (status, body) = login(&app, ADMIN_PASSWORD).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["code"], "auth_disabled");
}
