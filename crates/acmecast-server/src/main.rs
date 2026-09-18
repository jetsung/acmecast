//! acmecast 服务端可执行文件。
//!
//! 证书守护进程：ACME 签发、流水线编排、定时续期与 REST 管理接口。
//! 真正的实现位于 `acmecast_server` 库 crate，本文件只负责启动：
//! 读环境变量 → 打开数据目录 → 迁移数据库 → 开始服务，直到收到停机信号。
//!
//! 另提供 `hash-password` 子命令：在**运行环境**（无 Rust 工具链的宿主、
//! distroless 容器内）生成管理员口令哈希，供 `ACMECAST_ADMIN_PASSWORD_HASH`
//! 使用——
//!
//! ```text
//! acmecast-server hash-password '你的口令'
//! # 容器内：docker run --rm acmecast:dev hash-password '你的口令'
//! ```

use acmecast_server::auth::AuthMode;
use acmecast_server::{AppState, assemble_router_with_runtime, serve, shutdown_signal};
use acmecast_store::migrate;
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::sync::watch;

fn main() {
    // 子命令分发：hash-password 供运行环境生成口令哈希（distroless 内可用）。
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("hash-password") {
        let password = args.get(2).cloned().unwrap_or_else(|| {
            eprintln!("用法：acmecast-server hash-password '<口令>'");
            std::process::exit(1);
        });
        let hash = acmecast_server::hash_password(&password).unwrap_or_else(|error| {
            eprintln!("生成口令哈希失败: {error}");
            std::process::exit(1);
        });
        println!("{hash}");
        return;
    }

    // 阻塞运行时由 tokio 的宏展开提供；启动失败直接退出并留下错误日志。
    let result = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("应能构建 tokio 运行时")
        .block_on(run());

    if let Err(error) = result {
        tracing::error!(%error, "服务启动失败");
        std::process::exit(1);
    }
}

async fn run() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let config = acmecast_server::config::ServerConfig::from_env();

    // 数据目录是数据库、证书文件与凭据密钥的共同前提，缺了直接建。
    tokio::fs::create_dir_all(&config.data_dir).await?;

    let db = sea_orm::Database::connect(&config.database_url()).await?;
    migrate(&db).await?;
    tracing::info!(database = %config.database_url(), "数据库就绪");

    // 鉴权状态在启动时就要说清楚：关闭是显式选择，缺失配置是错误，
    // 两者都不该让人在第一次请求被拒时才发现。
    let runtime = acmecast_server::RuntimeState::from_environment_with_steps(std::sync::Arc::new(
        acmecast_server::steps::default_steps(
            db.clone(),
            config.data_dir.clone(),
            acmecast_server::steps::default_dns_registry(),
            acmecast_server::steps::default_deploy_registry(),
        ),
    ))?;
    match &runtime.auth {
        AuthMode::Disabled => {
            tracing::warn!("鉴权已关闭（ACMECAST_AUTH_DISABLED），仅适用于本地单机场景");
        }
        AuthMode::Enforced(auth) if !auth.config.login_available() => {
            tracing::error!(
                "未配置 ACMECAST_ADMIN_PASSWORD_HASH（或 ACMECAST_ADMIN_PASSWORD_HASH_FILE）：\
                 受保护端点将拒绝所有请求，登录不可用。请生成口令哈希后重启。"
            );
        }
        AuthMode::Enforced(_) => {}
    }

    // 停机信号经 watch 广播：HTTP 服务与调度器各持一个接收端，
    // 任一先收到都开始收尾。
    let (shutdown_tx, http_shutdown) = watch::channel(false);
    tokio::spawn(async move {
        shutdown_signal().await;
        let _ = shutdown_tx.send(true);
    });

    // 调度器：重启后重新装载全部调度（9.5），随停机信号退出。
    let scheduler = acmecast_server::scheduler::spawn_scheduler(
        db.clone(),
        std::sync::Arc::clone(&runtime.step_registry),
        std::sync::Arc::clone(&runtime.credential_registry),
        runtime.cipher.as_ref(),
        http_shutdown.clone(),
    );

    let listener = TcpListener::bind(&config.listen_addr).await?;
    tracing::info!(addr = %config.listen_addr, "开始服务");

    let state = AppState { db, config };
    serve(
        listener,
        assemble_router_with_runtime(state, runtime),
        async move {
            let mut rx = http_shutdown;
            let _ = rx.changed().await;
        },
    )
    .await?;

    // HTTP 已停止接受连接；给调度器一点时间跑完手头的 tick 再退出。
    if tokio::time::timeout(Duration::from_secs(5), scheduler)
        .await
        .is_err()
    {
        tracing::warn!("调度器未在 5 秒内退出，放弃等待");
    }

    tracing::info!("服务已停止");
    Ok(())
}
