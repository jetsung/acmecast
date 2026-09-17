//! 10.1 优雅停机：收到停机信号后，已接受的请求执行至完成，新连接被拒绝。

use std::time::Duration;

use acmecast_server::serve;
use axum::Router;
use axum::routing::get;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::watch;

/// 用裸 TCP 发一个 HTTP/1.1 请求并读回响应。
///
/// 不引入 HTTP 客户端的原因：这里要精确控制「请求已发出、连接已建立」
/// 与「收到响应」的时序，客户端库的连接池与重试会把它搅浑。
async fn raw_request(addr: std::net::SocketAddr, target: &str) -> std::io::Result<String> {
    let mut stream = TcpStream::connect(addr).await?;
    let request = format!("GET {target} HTTP/1.1\r\nHost: test\r\nConnection: close\r\n\r\n");
    stream.write_all(request.as_bytes()).await?;
    stream.flush().await?;

    let mut buffer = Vec::new();
    stream.read_to_end(&mut buffer).await?;
    Ok(String::from_utf8_lossy(&buffer).into_owned())
}

#[tokio::test]
async fn shutdown_lets_in_flight_requests_finish_and_rejects_new_connections() {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("应能绑定临时端口");
    let addr = listener.local_addr().expect("应能读取端口");

    // 慢 handler：响应中途触发停机，验证它仍会执行完。
    async fn slow() -> &'static str {
        tokio::time::sleep(Duration::from_millis(300)).await;
        "done"
    }
    let router = Router::new().route("/slow", get(slow));

    let (shutdown_tx, mut shutdown_rx) = watch::channel(false);
    let server = tokio::spawn(serve(listener, router, async move {
        let _ = shutdown_rx.changed().await;
    }));

    // 请求先飞出去（连接已建立、请求已到达），再触发停机。
    let client = tokio::spawn(raw_request(addr, "/slow"));
    tokio::time::sleep(Duration::from_millis(100)).await;
    let _ = shutdown_tx.send(true);

    let response = client
        .await
        .expect("请求任务不应 panic")
        .expect("已接受的请求应执行至完成");
    assert!(
        response.starts_with("HTTP/1.1 200 OK"),
        "在途请求应得到完整响应：{response}"
    );
    assert!(response.ends_with("done"), "响应体应完整：{response}");

    // 服务退出：listener 已被丢弃。
    tokio::time::timeout(Duration::from_secs(5), server)
        .await
        .expect("停机应在超时前完成")
        .expect("serve 任务不应 panic")
        .expect("serve 不应报错");

    // 新连接被拒绝。
    let rejected = TcpStream::connect(addr).await;
    assert!(rejected.is_err(), "停机后新连接应被拒绝，但连接成功了");
}

#[tokio::test]
async fn healthz_responds_when_assembled() {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("应能绑定临时端口");
    let addr = listener.local_addr().expect("应能读取端口");

    let state = acmecast_server::AppState {
        db: sea_orm::Database::connect("sqlite::memory:")
            .await
            .expect("应能连上内存库"),
        config: acmecast_server::config::ServerConfig::default(),
    };
    let router = acmecast_server::assemble_router_with_runtime(
        state,
        acmecast_server::RuntimeState::new(
            acmecast_server::dependencies::credential_registry(),
            acmecast_server::dependencies::step_registry(),
            None,
            acmecast_server::auth::AuthMode::Disabled,
        ),
    );

    let (_shutdown_tx, mut shutdown_rx) = watch::channel(false);
    // 服务只在测试进程结束时随运行时退出；本用例只关心请求路径。
    let _server = tokio::spawn(serve(listener, router, async move {
        let _ = shutdown_rx.changed().await;
    }));

    let response = raw_request(addr, "/healthz").await.expect("健康检查应可达");
    assert!(
        response.starts_with("HTTP/1.1 200 OK"),
        "健康检查应返回 200：{response}"
    );
    assert!(response.ends_with("ok"), "健康检查应返回 ok：{response}");
}
