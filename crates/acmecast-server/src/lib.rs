//! acmecast 服务端：HTTP 装配、优雅停机与请求处理。
//!
//! 对应 `specs/http-api/spec.md`。库层只提供可测试的装配函数与停机语义；
//! 环境变量读取与进程入口在 `main.rs`。

pub mod auth;
pub mod config;
pub mod credentials;
mod handlers;
pub mod openapi;
pub mod response;
mod run;
pub mod scheduler;
pub mod steps;

/// 生成管理员口令的 Argon2 PHC 哈希（`hash-password` 子命令与开发示例共用）。
///
/// 格式与 `ACMECAST_ADMIN_PASSWORD_HASH` 的解析端完全一致；错误仅在
/// 哈希无法生成时出现。
pub fn hash_password(password: &str) -> acmecast_core::Result<String> {
    use argon2::{Argon2, PasswordHasher};

    // 盐由实现自动生成并编入 PHC 字符串（与 auth.rs 的校验端同一 API）。
    let hash = Argon2::default()
        .hash_password(password.as_bytes())
        .map_err(|e| acmecast_core::Error::Config(format!("生成口令哈希失败: {e}")))?;
    Ok(hash.to_string())
}

use std::{future::Future, sync::Arc, time::Duration};

use acmecast_access::CredentialRegistry;
use acmecast_core::CredentialCipher;
use acmecast_pipeline::StepRegistry;
use axum::{
    Router,
    body::Body,
    extract::{DefaultBodyLimit, Request, State},
    http::{HeaderName, StatusCode},
    middleware,
    response::{IntoResponse, Response},
    routing::get,
};
use tokio::net::TcpListener;
use tower::ServiceExt;
use tower_http::{
    request_id::{MakeRequestUuid, PropagateRequestIdLayer, SetRequestIdLayer},
    services::{ServeDir, ServeFile},
    trace::TraceLayer,
};
use utoipa::OpenApi;
use utoipa_swagger_ui::SwaggerUi;

use crate::{auth::AuthMode, config::ServerConfig, response::ApiError};

/// 请求追踪标识的头部名称。
pub const REQUEST_ID_HEADER: &str = "x-request-id";

/// 请求处理共享状态。
///
/// axum 要求 state 可克隆；`DatabaseConnection` 内部是连接池句柄，
/// 克隆只复制引用，不复制连接。
#[derive(Debug, Clone)]
pub struct AppState {
    /// 数据库连接；各仓储与引擎都从这里借。
    pub db: sea_orm::DatabaseConnection,
    /// 服务端配置。
    pub config: ServerConfig,
}

/// HTTP 层运行时依赖。
///
/// 注册表与加密器在启动时一次性装配，并通过 `Arc` 在各请求间共享。
/// 自定义装配函数主要供测试和嵌入式调用方注入替身；普通进程使用
/// [`RuntimeState::from_environment`] 即可。
#[derive(Debug, Clone)]
pub struct RuntimeState {
    /// 已注册的凭据类型。
    pub credential_registry: Arc<CredentialRegistry>,
    /// 已注册的流水线任务类型。
    pub step_registry: Arc<StepRegistry>,
    /// 凭据静态加密器；未配置密钥时为 `None`，保存凭据的端点会拒绝操作。
    pub cipher: Option<Arc<CredentialCipher>>,
    /// 鉴权模式。
    pub auth: AuthMode,
    /// webhook 通知订阅端；未配置通知渠道时为 `None`，流水线运行不产生投递。
    pub notifier: Option<Arc<acmecast_notify::WebhookEventSink>>,
}

impl RuntimeState {
    /// 用显式依赖构造运行时状态。
    ///
    /// 通知订阅端缺省为 `None`；配置了通知渠道的调用方在构造后直接为
    /// 公开字段 [`RuntimeState::notifier`] 赋值。
    #[must_use]
    pub fn new(
        credential_registry: Arc<CredentialRegistry>,
        step_registry: Arc<StepRegistry>,
        cipher: Option<Arc<CredentialCipher>>,
        auth: AuthMode,
    ) -> Self {
        Self {
            credential_registry,
            step_registry,
            cipher,
            auth,
            notifier: None,
        }
    }

    /// 从环境变量构造默认运行时状态。
    ///
    /// 支持 `ACMECAST__SECURITY__CREDENTIAL_KEY` 与简写的
    /// `ACMECAST_CREDENTIAL_KEY`。密钥非法时不让服务悄悄退化为明文，
    /// 而是记录错误并保留 `None`，后续凭据写入会明确失败。
    ///
    /// 鉴权配置缺失（如未设 JWT 密钥）属于配置错误，直接失败——
    /// 令牌无法校验的服务不该悄悄放行请求。
    pub fn from_environment() -> acmecast_core::Result<Self> {
        Self::from_environment_with_steps(handlers::default_step_registry())
    }

    /// 与 [`RuntimeState::from_environment`] 相同，但用调用方装配的步骤注册表。
    ///
    /// 生产路径传 [`crate::steps::default_steps`] 的产物——步骤需要数据库与
    /// 数据目录，只能在连接建立后装配；测试可以注入自定义步骤。
    pub fn from_environment_with_steps(
        step_registry: Arc<StepRegistry>,
    ) -> acmecast_core::Result<Self> {
        let credential_registry = handlers::default_credential_registry();
        let key = std::env::var("ACMECAST__SECURITY__CREDENTIAL_KEY")
            .or_else(|_| std::env::var("ACMECAST_CREDENTIAL_KEY"));
        let cipher = key
            .ok()
            .and_then(|key| match CredentialCipher::from_base64(&key) {
                Ok(cipher) => Some(Arc::new(cipher)),
                Err(error) => {
                    tracing::error!(%error, "凭据加密密钥无效，凭据写入将被拒绝");
                    None
                }
            });
        Ok(Self::new(
            credential_registry,
            step_registry,
            cipher,
            AuthMode::from_environment()?,
        ))
    }
}

/// 请求 handler 使用的完整状态。
#[derive(Debug, Clone)]
pub(crate) struct HttpState {
    /// 应用基础状态。
    pub(crate) app: AppState,
    /// HTTP 运行时依赖。
    pub(crate) runtime: RuntimeState,
}

/// 默认依赖装配的公开入口，供集成测试复用。
///
/// 生产路径走 [`RuntimeState::from_environment_with_steps`] 注入完整步骤
/// 注册表；这里的步骤注册表是空的——多数 HTTP 测试不执行流水线，
/// 需要执行链路的测试自行装配。
pub mod dependencies {
    use std::sync::Arc;

    use acmecast_access::CredentialRegistry;
    use acmecast_pipeline::StepRegistry;

    /// 默认凭据注册表（含内置 ACME 账号类型）。
    #[must_use]
    pub fn credential_registry() -> Arc<CredentialRegistry> {
        crate::handlers::default_credential_registry()
    }

    /// 空步骤注册表；需要执行流水线的测试请用 [`crate::steps::default_steps`]。
    #[must_use]
    pub fn step_registry() -> Arc<StepRegistry> {
        crate::handlers::default_step_registry()
    }
}

/// 装配全部路由。
///
/// 健康检查不做鉴权也不计入业务端点——它是编排系统判断进程死活的探针，
/// 依赖鉴权会让「服务活着但配置坏了」无法与「服务挂了」区分。
pub fn assemble_router(state: AppState) -> acmecast_core::Result<Router> {
    Ok(assemble_router_with_runtime(
        state,
        RuntimeState::from_environment()?,
    ))
}

/// 使用显式运行时依赖装配路由。
///
/// 中间件栈（由外到内）：
/// 1. `SetRequestIdLayer` 为每个请求生成或保留 `x-request-id`；
/// 2. `PropagateRequestIdLayer` 把它回写到响应头，客户端可据此排查；
/// 3. `TraceLayer` 建立带该标识的 span，请求内的日志都挂在这个 span 下；
/// 4. `DefaultBodyLimit` 限制请求体体积，超出直接 413；
/// 5. 鉴权中间件校验访问令牌。
pub fn assemble_router_with_runtime(state: AppState, runtime: RuntimeState) -> Router {
    let body_limit = state.config.body_limit_bytes;
    let http_state = HttpState {
        app: state,
        runtime,
    };
    Router::new()
        .route("/healthz", get(healthz))
        .merge(handlers::routes())
        .merge(SwaggerUi::new("/swagger-ui").url("/api/openapi.json", openapi::ApiDoc::openapi()))
        .fallback(fallback_handler)
        .with_state(http_state.clone())
        .layer(middleware::from_fn_with_state(
            http_state,
            auth::require_token,
        ))
        .layer(DefaultBodyLimit::max(body_limit))
        .layer(
            TraceLayer::new_for_http()
                .make_span_with(make_span)
                .on_request(on_request)
                .on_response(on_response),
        )
        .layer(PropagateRequestIdLayer::new(HeaderName::from_static(
            REQUEST_ID_HEADER,
        )))
        .layer(SetRequestIdLayer::new(
            HeaderName::from_static(REQUEST_ID_HEADER),
            MakeRequestUuid,
        ))
}

/// 健康检查探针。
async fn healthz() -> &'static str {
    "ok"
}

/// 未匹配路径的兜底。
///
/// API 前缀返回结构化 404（前端拿得到错误码）；其余路径交给静态资源，
/// 未命中文件时回退到 SPA 入口 HTML，使客户端路由可接管。
async fn fallback_handler(State(state): State<HttpState>, request: Request) -> Response {
    let path = request.uri().path().to_owned();
    if path.starts_with("/api/") {
        return ApiError::new(
            StatusCode::NOT_FOUND,
            "not_found",
            format!("接口不存在: {path}"),
        )
        .into_response();
    }
    let Some(static_dir) = state.app.config.static_dir.clone() else {
        return ApiError::new(
            StatusCode::NOT_FOUND,
            "not_found",
            format!("路径不存在: {path}"),
        )
        .into_response();
    };
    let index = static_dir.join("index.html");
    // 用 `fallback` 而不是 `not_found_service`：后者会把回退响应的状态码
    // 强制改写为 404（tower-http 为「自定义 404 页面」设计的语义），而 SPA
    // 深链回退必须以 200 交出入口 HTML，客户端路由才能接管。
    let service = ServeDir::new(&static_dir).fallback(ServeFile::new(index));
    match service.oneshot(request).await {
        Ok(response) => response.map(Body::new).into_response(),
        Err(never) => match never {},
    }
}

/// 建立带追踪标识的请求 span。
fn make_span(request: &Request) -> tracing::Span {
    let request_id = request
        .headers()
        .get(REQUEST_ID_HEADER)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("unknown");
    tracing::info_span!(
        "http",
        request_id = %request_id,
        method = %request.method(),
        path = %request.uri().path(),
    )
}

/// 请求进入时记录一行带标识的日志。
///
/// 显式覆盖 tower-http 的默认 `on_request`（那是 `tower_http::trace` 的一条 DEBUG 日志）：
/// 「同一次请求的日志共享同一标识」这件事应当由应用自己保证，测试里才不必依赖
/// 中间件的内部日志——否则 tower-http 哪天不再默认打这行，断言就会静默失去覆盖。
fn on_request(_request: &Request, span: &tracing::Span) {
    tracing::info!(parent: span, "请求开始");
}

/// 请求完成时记录一行带标识与耗时的日志。
fn on_response(response: &Response, latency: Duration, span: &tracing::Span) {
    tracing::info!(
        parent: span,
        status = response.status().as_u16(),
        latency_ms = latency.as_millis() as u64,
        "请求完成"
    );
}

/// 在给定监听套接字上服务，直到 `shutdown` future 完成。
///
/// 收到停机信号后**停止接受新连接**，但已接受的请求会继续执行至完成
/// （axum 的 graceful shutdown 语义）。真正「拒绝新连接」发生在
/// listener 被丢弃之后——`connect` 将得到连接拒绝错误。
pub async fn serve(
    listener: TcpListener,
    router: Router,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> std::io::Result<()> {
    axum::serve(listener, router)
        .with_graceful_shutdown(shutdown)
        .await
}

/// 组装停机信号：Ctrl-C 或 SIGTERM（Unix）任一先到即触发。
pub async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };

    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut stream) => {
                stream.recv().await;
            }
            Err(error) => {
                tracing::error!(%error, "无法注册 SIGTERM 处理器，停机只能靠 Ctrl-C");
                std::future::pending::<()>().await;
            }
        }
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = ctrl_c => tracing::info!("收到 Ctrl-C，开始优雅停机"),
        () = terminate => tracing::info!("收到 SIGTERM，开始优雅停机"),
    }
}
