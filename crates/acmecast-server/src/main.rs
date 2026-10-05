//! acmecast 服务端可执行文件。
//!
//! 证书守护进程：ACME 签发、流水线编排、定时续期与 REST 管理接口。
//! 真正的实现位于 `acmecast_server` 库 crate，本文件只负责启动：
//! 读环境变量 → 打开数据目录 → 迁移数据库 → 开始服务，直到收到停机信号。
//!
//! 子命令（`clap` 解析，`--config/-c` 全局指定配置文件路径，缺省读
//! `ACMECAST_CONFIG`，再回退 `data_dir/config.toml`）：
//!
//! - `init [--force]`：生成带注释的 `config.toml` 模板，不覆盖已存在文件。
//! - `hash-password [--password <口令>]`：生成管理员口令哈希，省略口令时交互式输入。
//!
//! ```text
//! acmecast-server init --force
//! acmecast-server hash-password '你的口令'
//! # 容器内：docker run --rm acmecast:dev hash-password '你的口令'
//! ```

use std::io::{IsTerminal, Write};
use std::path::Path;
use std::time::Duration;

use acmecast_server::auth::AuthMode;
use acmecast_server::config::{generate_template, resolve_config_path, write_template};
use acmecast_server::{AppState, assemble_router_with_runtime, serve, shutdown_signal};
use acmecast_store::migrate;
use clap::{Parser, Subcommand};
use tokio::net::TcpListener;
use tokio::sync::watch;

/// acmecast 服务端。
#[derive(Parser)]
#[command(name = "acmecast-server")]
#[command(about = "证书守护进程：ACME 签发、流水线编排、定时续期与 REST 管理接口")]
#[command(version)]
struct Args {
    /// Configuration file path
    #[arg(short, long, env = "ACMECAST_CONFIG", global = true)]
    config: Option<String>,

    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Initialize a new config.toml file
    Init {
        /// Force overwrite if config.toml already exists
        #[arg(short, long)]
        force: bool,
    },
    /// Generate an Argon2id password hash (for ACMECAST_ADMIN_PASSWORD_HASH)
    HashPassword {
        /// Plaintext password. If omitted, you will be prompted interactively.
        #[arg(short, long)]
        password: Option<String>,
    },
}

fn main() {
    let args = Args::parse();

    if let Some(command) = args.command {
        match command {
            Commands::Init { force } => handle_init(args.config.as_deref(), force),
            Commands::HashPassword { password } => handle_hash_password(password),
        }
        return;
    }

    // 阻塞运行时由 tokio 的宏展开提供；启动失败直接退出并留下错误日志。
    let result = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("应能构建 tokio 运行时")
        .block_on(run(args.config.as_deref()));

    if let Err(error) = result {
        tracing::error!(%error, "服务启动失败");
        std::process::exit(1);
    }
}

async fn run(config_override: Option<&str>) -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    // 首次启动若 config.toml 不存在，自动生成带注释的模板（不覆盖已存在文件）；
    // 生成失败仅告警，不阻断启动——回退内置默认值仍可起一个可用实例。
    let cfg_path = resolve_config_path(config_override.map(Path::new));
    match generate_template(&cfg_path) {
        Ok(true) => tracing::info!(path = %cfg_path.display(), "已生成默认 config.toml 模板"),
        Ok(false) => {}
        Err(error) => tracing::warn!(
            path = %cfg_path.display(),
            %error,
            "生成 config.toml 模板失败，回退内置默认值"
        ),
    }

    // 合并优先级 builtin < config.toml < env；文件解析失败（字段类型不匹配等）报错中止。
    let config = acmecast_server::config::ServerConfig::from_env_and_file_at(
        config_override.map(Path::new),
    )?;

    // 数据目录是数据库、证书文件与凭据密钥的共同前提，缺了直接建。
    tokio::fs::create_dir_all(&config.data_dir).await?;

    let db = sea_orm::Database::connect(&config.database_url()).await?;
    migrate(&db).await?;
    tracing::info!(database = %config.database_url(), "数据库就绪");

    // 鉴权状态在启动时就要说清楚：关闭是显式选择，缺失配置是错误，
    // 两者都不该让人在第一次请求被拒时才发现。
    let mut runtime = acmecast_server::RuntimeState::from_environment_with_steps(
        std::sync::Arc::new(acmecast_server::steps::default_steps(
            db.clone(),
            config.data_dir.clone(),
            acmecast_server::steps::default_dns_registry(),
            acmecast_server::steps::default_deploy_registry(),
        )),
    )?;
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

    // webhook 通知：配置了启用渠道才装配订阅端；渠道数在启动日志里可见，
    // 让「我以为配了通知」这类误解在第一时间暴露。
    let enabled_channels = config
        .notifications
        .iter()
        .filter(|channel| channel.enabled)
        .count();
    if enabled_channels > 0 {
        tracing::info!(channels = enabled_channels, "webhook 通知已启用");
        runtime.notifier = Some(std::sync::Arc::new(acmecast_notify::WebhookEventSink::new(
            db.clone(),
            config.notifications.clone(),
            acmecast_notify::DeliveryOptions::default(),
        )));
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
        runtime.notifier.clone(),
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

/// `init` 子命令：在配置路径生成带注释的 `config.toml` 模板。
///
/// 已存在且未 `--force` 时报错退出；`--force` 覆盖。顺带创建数据目录
/// （`ACMECAST_DATA_DIR` 或 `data`），与模板内的相对路径约定一致。
fn handle_init(config: Option<&str>, force: bool) {
    let path = resolve_config_path(config.map(Path::new));

    if path.exists() && !force {
        eprintln!("✗ 文件 '{}' 已存在", path.display());
        eprintln!("  使用 --force 覆盖");
        std::process::exit(1);
    }

    let data_dir = std::env::var("ACMECAST_DATA_DIR").unwrap_or_else(|_| "data".to_owned());
    if !std::path::Path::new(&data_dir).exists() {
        if let Err(error) = std::fs::create_dir(&data_dir) {
            eprintln!("✗ 创建数据目录 '{data_dir}' 失败: {error}");
            std::process::exit(1);
        }
        println!("✓ 已创建数据目录 '{data_dir}'");
    }

    match write_template(&path, force) {
        Ok(true) => {
            println!("✓ 已创建 '{}'", path.display());
            println!();
            println!("后续步骤:");
            println!(
                "  1. 编辑 '{}' 并按需调整 [server] 段（listen_addr、data_dir 等）",
                path.display()
            );
            println!("  2. 设置必需的环境变量:");
            println!("     - ACMECAST_JWT_SECRET（生成：openssl rand -base64 48）");
            println!("     - ACMECAST_CREDENTIAL_KEY（生成：openssl rand -base64 32，写凭据需要）");
            println!(
                "     - ACMECAST_ADMIN_PASSWORD_HASH（生成：acmecast-server hash-password \"你的口令\"）"
            );
            println!("  3. 启动服务: acmecast-server");
        }
        Ok(false) => {
            eprintln!("✗ 文件 '{}' 已存在", path.display());
            eprintln!("  使用 --force 覆盖");
            std::process::exit(1);
        }
        Err(error) => {
            eprintln!("✗ 创建 '{}' 失败: {error}", path.display());
            std::process::exit(1);
        }
    }
}

/// `hash-password` 子命令：生成 Argon2 PHC 口令哈希；省略口令时交互式读取。
fn handle_hash_password(password: Option<String>) {
    let plaintext = match password {
        Some(p) => p,
        None => {
            if !std::io::stdin().is_terminal() {
                eprintln!("✗ 未提供口令且 stdin 不是终端");
                std::process::exit(1);
            }
            print!("Enter password: ");
            let _ = std::io::stdout().flush();
            let mut buf = String::new();
            if std::io::stdin().read_line(&mut buf).is_err() {
                eprintln!("✗ 读取口令失败");
                std::process::exit(1);
            }
            buf.trim_end().to_string()
        }
    };

    if plaintext.is_empty() {
        eprintln!("✗ 口令不能为空");
        std::process::exit(1);
    }

    match acmecast_server::hash_password(&plaintext) {
        Ok(hash) => {
            println!("{hash}");
            println!();
            println!("将上面的值设置为环境变量：ACMECAST_ADMIN_PASSWORD_HASH='{hash}'");
        }
        Err(error) => {
            eprintln!("✗ 生成口令哈希失败: {error}");
            std::process::exit(1);
        }
    }
}
