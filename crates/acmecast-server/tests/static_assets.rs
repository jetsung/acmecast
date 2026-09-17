//! 10.7 静态资源托管与 SPA 回退。
//!
//! 关键约束来自 spec 的「静态资源托管」：静态文件按原样送达；
//! 未匹配的非 API 路径回退到前端入口 HTML，使客户端路由能接管；
//! API 前缀不参与回退，应返回结构化 404。

mod support;

use axum::{
    body::Body,
    http::{Request, StatusCode, header},
};
use support::*;

/// 造一个静态目录：入口 HTML、一个 JS 产物、一张图片。
fn static_site() -> TempDir {
    let dir = TempDir::new("static");
    std::fs::write(
        dir.path().join("index.html"),
        "<!doctype html><title>acmecast</title><div id=\"app\"></div>",
    )
    .expect("应能写入入口 HTML");
    std::fs::create_dir_all(dir.path().join("assets")).expect("应能创建资源目录");
    std::fs::write(dir.path().join("assets/app.js"), "console.log('acmecast');")
        .expect("应能写入 JS 产物");
    std::fs::write(dir.path().join("assets/logo.svg"), "<svg/>").expect("应能写入图片");
    dir
}

fn config_with_static(dir: &TempDir) -> acmecast_server::config::ServerConfig {
    acmecast_server::config::ServerConfig {
        static_dir: Some(dir.path().to_path_buf()),
        ..Default::default()
    }
}

#[tokio::test]
async fn static_assets_are_served_with_their_content_types() {
    let site = static_site();
    let app = app_with(
        config_with_static(&site),
        acmecast_server::auth::AuthMode::Disabled,
    )
    .await;

    let response = send(
        &app,
        Request::get("/assets/app.js").body(Body::empty()).unwrap(),
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
        content_type.contains("javascript"),
        "JS 产物应带脚本类型，实际为 `{content_type}`"
    );

    let response = send(
        &app,
        Request::get("/assets/logo.svg")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let content_type = response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    assert!(
        content_type.contains("svg"),
        "SVG 应带图片类型，实际为 `{content_type}`"
    );
}

#[tokio::test]
async fn unmatched_paths_fall_back_to_the_spa_entry_html() {
    let site = static_site();
    let app = app_with(
        config_with_static(&site),
        acmecast_server::auth::AuthMode::Disabled,
    )
    .await;

    // 客户端路由的深链：服务端没有这个文件，但必须交出入口 HTML。
    for uri in [
        "/",
        "/pipelines",
        "/certificates/42",
        "/settings/profile",
        "/some/deep/path",
    ] {
        let response = send(&app, Request::get(uri).body(Body::empty()).unwrap()).await;
        let status = response.status();
        let content_type = response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_owned();
        let bytes = axum::body::to_bytes(response.into_body(), 64 * 1024)
            .await
            .expect("应能读取响应体");
        let html = String::from_utf8_lossy(&bytes).into_owned();
        assert_eq!(
            status,
            StatusCode::OK,
            "{uri} 应回退到入口 HTML，实际 {status}，体：{html}"
        );
        assert!(
            content_type.contains("text/html"),
            "{uri} 应返回 HTML，实际为 `{content_type}`"
        );
        assert!(
            html.contains("id=\"app\""),
            "{uri} 应返回入口 HTML 本身，实际：{html}"
        );
    }
}

#[tokio::test]
async fn unknown_api_paths_do_not_fall_back_to_html() {
    let site = static_site();
    let app = app_with(
        config_with_static(&site),
        acmecast_server::auth::AuthMode::Disabled,
    )
    .await;

    // API 前缀必须返回结构化错误：前端按错误码分支，拿到 HTML 会解析失败。
    let response = send(
        &app,
        Request::get("/api/no-such-endpoint")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let body = json(response).await;
    assert_eq!(body["error"]["code"], "not_found");
}

#[tokio::test]
async fn without_a_static_dir_only_the_api_is_served() {
    // 未配置静态目录时不应假装有前端：返回结构化 404 更诚实。
    let app = app_with(
        acmecast_server::config::ServerConfig::default(),
        acmecast_server::auth::AuthMode::Disabled,
    )
    .await;

    let response = send(
        &app,
        Request::get("/pipelines").body(Body::empty()).unwrap(),
    )
    .await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let body = json(response).await;
    assert_eq!(body["error"]["code"], "not_found");
}
