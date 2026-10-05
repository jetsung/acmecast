//! 鉴权：管理员登录、访问令牌签发与校验。
//!
//! 对应 `specs/http-api/spec.md` 的「鉴权中间件」与「登录与令牌签发」：
//! 除登录与静态资源外的端点强制校验访问令牌；连续失败登录被限流；
//! 鉴权可通过配置显式关闭（默认开启，开关不得默认关闭）。
//!
//! 令牌为 HS256 签名的 JWT。管理员口令以 Argon2 PHC 字符串配置，
//! 口令明文不会出现在配置、日志或响应中；口令哈希与 JWT 密钥的
//! `Debug` 输出均被脱敏。

use std::{
    collections::HashMap,
    net::SocketAddr,
    sync::{Arc, Mutex},
    time::Duration,
};

use argon2::{Argon2, PasswordVerifier, password_hash::phc::PasswordHash};
use axum::{
    extract::{ConnectInfo, FromRequestParts, Request, State},
    http::{StatusCode, header, request::Parts},
    middleware::Next,
    response::{IntoResponse, Response},
};
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use jsonwebtoken::{Algorithm, DecodingKey, EncodingKey, Header, Validation, decode, encode};
use serde::{Deserialize, Serialize};

use crate::{HttpState, response::ApiError};

/// 默认管理员用户名。
const DEFAULT_ADMIN_USERNAME: &str = "admin";
/// 默认令牌有效期（小时）。
const DEFAULT_TOKEN_TTL_HOURS: i64 = 12;
/// 默认连续失败上限：达到后锁定该来源。
const DEFAULT_MAX_FAILURES: u32 = 5;
/// 默认失败锁定窗口（秒）。
const DEFAULT_LOCK_WINDOW_SECS: i64 = 900;

/// 鉴权配置，全部来自环境变量。
///
/// `Debug` 是手写的：口令哈希与签名密钥都可用于离线攻击，
/// 而配置对象太容易被顺手打进启动日志。用户名与有效期不是秘密，照常显示。
#[derive(Clone)]
pub struct AuthConfig {
    /// 管理员用户名。
    pub admin_username: String,
    /// 管理员口令的 Argon2 PHC 哈希；缺省时无法登录。
    admin_password_hash: Option<String>,
    /// JWT 签名密钥。
    jwt_secret: String,
    /// 访问令牌有效期。
    pub token_ttl: Duration,
    /// 连续失败上限。
    pub max_failures: u32,
    /// 失败锁定窗口。
    pub lock_window: Duration,
}

impl std::fmt::Debug for AuthConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuthConfig")
            .field("admin_username", &self.admin_username)
            .field(
                "admin_password_hash",
                &acmecast_core::redact_presence(&self.admin_password_hash),
            )
            .field("jwt_secret", &acmecast_core::REDACTED)
            .field("token_ttl", &self.token_ttl)
            .field("max_failures", &self.max_failures)
            .field("lock_window", &self.lock_window)
            .finish()
    }
}

impl AuthConfig {
    /// 用显式字段构造配置。
    #[must_use]
    pub fn new(
        admin_username: impl Into<String>,
        admin_password_hash: Option<String>,
        jwt_secret: impl Into<String>,
        token_ttl: Duration,
    ) -> Self {
        Self {
            admin_username: admin_username.into(),
            admin_password_hash,
            jwt_secret: jwt_secret.into(),
            token_ttl,
            max_failures: DEFAULT_MAX_FAILURES,
            lock_window: Duration::from_secs(DEFAULT_LOCK_WINDOW_SECS as u64),
        }
    }

    /// 从环境变量读取配置。
    ///
    /// 缺少 JWT 密钥直接失败（令牌无法签发也无需校验，属于配置错误）；
    /// 缺少口令哈希不算失败——服务照常启动，只是无人能登录，
    /// 由启动日志告警提示。
    pub fn from_environment() -> acmecast_core::Result<Self> {
        let jwt_secret = env_string("ACMECAST_JWT_SECRET").ok_or_else(|| {
            acmecast_core::Error::Config(
                "缺少 ACMECAST_JWT_SECRET：令牌签发需要签名密钥".to_owned(),
            )
        })?;
        let admin_username = env_string("ACMECAST_ADMIN_USERNAME")
            .unwrap_or_else(|| DEFAULT_ADMIN_USERNAME.to_owned());
        let admin_password_hash = resolve_admin_password_hash(
            env_string("ACMECAST_ADMIN_PASSWORD_HASH_FILE"),
            env_string("ACMECAST_ADMIN_PASSWORD_HASH"),
        )?;
        if let Some(hash) = &admin_password_hash {
            PasswordHash::new(hash).map_err(|error| {
                acmecast_core::Error::Config(format!(
                    "管理员口令哈希（来自 ACMECAST_ADMIN_PASSWORD_HASH 或 \
                     ACMECAST_ADMIN_PASSWORD_HASH_FILE）不是合法的 Argon2 PHC 字符串: {error}"
                ))
            })?;
        }
        let token_ttl_hours = std::env::var("ACMECAST_TOKEN_TTL_HOURS")
            .ok()
            .and_then(|value| value.trim().parse::<i64>().ok())
            .filter(|hours| *hours > 0)
            .unwrap_or(DEFAULT_TOKEN_TTL_HOURS);

        Ok(Self {
            admin_username,
            admin_password_hash,
            jwt_secret,
            token_ttl: Duration::from_secs(token_ttl_hours as u64 * 3600),
            max_failures: DEFAULT_MAX_FAILURES,
            lock_window: Duration::from_secs(DEFAULT_LOCK_WINDOW_SECS as u64),
        })
    }

    /// 校验管理员口令。
    ///
    /// 用户名与口令两个比较都执行（口令比较走 Argon2 的常量时间实现），
    /// 调用方据整体结果返回统一错误，不泄露用户名是否存在。
    #[must_use]
    pub fn verify_password(&self, candidate: &str) -> bool {
        let Some(hash) = self.admin_password_hash.as_deref() else {
            return false;
        };
        let Ok(parsed) = PasswordHash::new(hash) else {
            return false;
        };
        Argon2::default()
            .verify_password(candidate.as_bytes(), &parsed)
            .is_ok()
    }

    /// 是否配置了口令哈希；未配置时登录不可用。
    #[must_use]
    pub fn login_available(&self) -> bool {
        self.admin_password_hash.is_some()
    }
}

/// JWT 载荷。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenClaims {
    /// 管理员用户名。
    pub sub: String,
    /// 签发时间（Unix 秒）。
    pub iat: i64,
    /// 过期时间（Unix 秒）。
    pub exp: i64,
}

/// 鉴权运行时状态。
#[derive(Debug)]
pub struct AuthState {
    /// 鉴权配置。
    pub config: AuthConfig,
    /// 登录失败限流器。
    pub limiter: LoginLimiter,
}

impl AuthState {
    /// 用配置构造，限流策略取默认值。
    #[must_use]
    pub fn new(config: AuthConfig) -> Self {
        let policy = LimitPolicy {
            max_failures: config.max_failures,
            lock_window_secs: config.lock_window.as_secs() as i64,
        };
        Self {
            config,
            limiter: LoginLimiter::new(policy),
        }
    }

    /// 校验用户名与口令。
    ///
    /// 两个比较都完整执行——用户名错误时同样跑一遍 Argon2 校验，
    /// 耗时接近，调用方据整体结果返回统一错误即可，
    /// 不泄露用户名是否存在。
    #[must_use]
    pub fn verify_credentials(&self, username: &str, password: &str) -> bool {
        let username_ok =
            constant_time_eq(username.as_bytes(), self.config.admin_username.as_bytes());
        let password_ok = self.config.verify_password(password);
        username_ok & password_ok
    }
}

/// 常量时间字节比较：按位累积差异，不因首个不同字节提前返回。
fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.iter()
        .zip(right)
        .fold(0u8, |acc, (a, b)| acc | (a ^ b))
        == 0
}

/// 鉴权模式。
#[derive(Debug, Clone)]
pub enum AuthMode {
    /// 强制校验访问令牌（默认）。
    Enforced(Arc<AuthState>),
    /// 显式关闭（`ACMECAST_AUTH_DISABLED`），仅用于本地单机场景。
    Disabled,
}

impl AuthMode {
    /// 从环境变量构造模式；开关未显式打开时一律强制鉴权。
    pub fn from_environment() -> acmecast_core::Result<Self> {
        if env_flag("ACMECAST_AUTH_DISABLED") {
            return Ok(Self::Disabled);
        }
        Ok(Self::Enforced(Arc::new(AuthState::new(
            AuthConfig::from_environment()?,
        ))))
    }

    /// 用显式配置构造强制鉴权模式。
    #[must_use]
    pub fn enforced(config: AuthConfig) -> Self {
        Self::Enforced(Arc::new(AuthState::new(config)))
    }

    /// 鉴权是否被显式关闭。
    #[must_use]
    pub fn is_disabled(&self) -> bool {
        matches!(self, Self::Disabled)
    }

    /// 强制鉴权时的配置；关闭时返回 `None`。
    #[must_use]
    pub fn config(&self) -> Option<&AuthConfig> {
        match self {
            Self::Enforced(state) => Some(&state.config),
            Self::Disabled => None,
        }
    }
}

/// 连续失败登录的限流策略。
#[derive(Debug, Clone, Copy)]
pub struct LimitPolicy {
    /// 触发锁定前允许的连续失败次数。
    pub max_failures: u32,
    /// 触发后的锁定窗口（秒）。
    pub lock_window_secs: i64,
}

impl Default for LimitPolicy {
    fn default() -> Self {
        Self {
            max_failures: DEFAULT_MAX_FAILURES,
            lock_window_secs: DEFAULT_LOCK_WINDOW_SECS,
        }
    }
}

/// 按来源记录登录失败次数的限流器。
///
/// 计数在进程内：重启会清空，但重启本身需要运维权限，不构成爆破通道。
#[derive(Debug)]
pub struct LoginLimiter {
    policy: LimitPolicy,
    attempts: Mutex<HashMap<String, Attempt>>,
}

#[derive(Debug, Clone, Copy)]
struct Attempt {
    failures: u32,
    locked_until: Option<DateTime<Utc>>,
}

impl LoginLimiter {
    /// 用策略构造限流器。
    #[must_use]
    pub fn new(policy: LimitPolicy) -> Self {
        Self {
            policy,
            attempts: Mutex::new(HashMap::new()),
        }
    }

    /// 该来源当前是否允许尝试登录；被锁定时返回剩余秒数。
    ///
    /// 显式接收 `now`，时钟交给调用方，行为可复现。
    pub fn check(&self, key: &str, now: DateTime<Utc>) -> Result<(), i64> {
        let attempts = self.lock();
        let Some(attempt) = attempts.get(key) else {
            return Ok(());
        };
        match attempt.locked_until {
            Some(until) if until > now => Err((until - now).num_seconds().max(1)),
            _ => Ok(()),
        }
    }

    /// 记一次失败；达到上限后进入锁定窗口。
    pub fn record_failure(&self, key: &str, now: DateTime<Utc>) {
        let mut attempts = self.lock();
        let attempt = attempts.entry(key.to_owned()).or_insert(Attempt {
            failures: 0,
            locked_until: None,
        });
        // 锁定已过期则重新计数，避免一次锁定后永久拒绝。
        if attempt.locked_until.is_some_and(|until| until <= now) {
            attempt.failures = 0;
            attempt.locked_until = None;
        }
        attempt.failures += 1;
        if attempt.failures >= self.policy.max_failures {
            attempt.locked_until =
                Some(now + ChronoDuration::seconds(self.policy.lock_window_secs));
            attempt.failures = 0;
        }
    }

    /// 登录成功后清除该来源的失败记录。
    pub fn record_success(&self, key: &str) {
        self.lock().remove(key);
    }

    /// 取锁；中毒锁只影响计数，恢复数据即可。
    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, Attempt>> {
        self.attempts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// 请求来源标识：优先取反向代理写入的转发头，回退到连接地址。
///
/// 反向代理（nginx 等）部署时必须配置转发头，否则所有请求会被记成
/// 同一来源、限流会误伤正常用户；直连场景由连接地址兜底。
#[derive(Debug, Clone)]
pub struct ClientIp(pub String);

impl<S> FromRequestParts<S> for ClientIp
where
    S: Send + Sync,
{
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        Ok(Self(client_key(parts)))
    }
}

fn client_key(parts: &Parts) -> String {
    for name in ["x-forwarded-for", "x-real-ip"] {
        if let Some(value) = parts
            .headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            && let Some(first) = value
                .split(',')
                .map(str::trim)
                .find(|candidate| !candidate.is_empty())
        {
            return first.to_owned();
        }
    }
    parts
        .extensions
        .get::<ConnectInfo<SocketAddr>>()
        .map(|info| info.0.to_string())
        .unwrap_or_else(|| "unknown".to_owned())
}

/// 校验访问令牌的中间件。
///
/// 鉴权关闭时直接放行；仅 `/api/` 下的业务端点需要令牌，
/// 登录、文档与静态资源放行。
pub(crate) async fn require_token(
    State(state): State<HttpState>,
    request: Request,
    next: Next,
) -> Response {
    let AuthMode::Enforced(auth) = &state.runtime.auth else {
        return next.run(request).await;
    };
    if !requires_token(request.uri().path()) {
        return next.run(request).await;
    }
    let token = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .map(str::trim)
        .filter(|token| !token.is_empty());
    let Some(token) = token else {
        return unauthorized("缺少访问令牌");
    };
    match verify_token(&auth.config, token, Utc::now()) {
        Ok(_) => next.run(request).await,
        Err(reason) => {
            tracing::debug!(reason, "访问令牌校验失败");
            unauthorized("访问令牌无效或已过期")
        }
    }
}

/// 判断路径是否需要访问令牌。
///
/// 只有 `/api/` 下的业务端点需要：`/api/login` 是换取令牌的入口，
/// 文档端点与静态资源（SPA、Swagger UI）是公开内容。
fn requires_token(path: &str) -> bool {
    const PUBLIC: [&str; 2] = ["/api/login", "/api/openapi.json"];
    path.starts_with("/api/") && !PUBLIC.contains(&path)
}

fn unauthorized(message: &str) -> Response {
    let mut response =
        ApiError::new(StatusCode::UNAUTHORIZED, "unauthorized", message).into_response();
    response.headers_mut().insert(
        header::WWW_AUTHENTICATE,
        header::HeaderValue::from_static("Bearer"),
    );
    response
}

/// 签发访问令牌。
pub fn issue_token(config: &AuthConfig, now: DateTime<Utc>) -> acmecast_core::Result<String> {
    let claims = TokenClaims {
        sub: config.admin_username.clone(),
        iat: now.timestamp(),
        exp: (now + ChronoDuration::seconds(config.token_ttl.as_secs() as i64)).timestamp(),
    };
    encode(
        &Header::new(Algorithm::HS256),
        &claims,
        &EncodingKey::from_secret(config.jwt_secret.as_bytes()),
    )
    .map_err(|error| acmecast_core::Error::Internal(format!("令牌签发失败: {error}")))
}

/// 校验访问令牌；失败时返回可记录的原因（不返回给客户端）。
pub fn verify_token(
    config: &AuthConfig,
    token: &str,
    now: DateTime<Utc>,
) -> Result<TokenClaims, String> {
    let mut validation = Validation::new(Algorithm::HS256);
    validation.set_required_spec_claims(&["exp"]);
    validation.leeway = 0;
    let mut claims = decode::<TokenClaims>(
        token,
        &DecodingKey::from_secret(config.jwt_secret.as_bytes()),
        &validation,
    )
    .map_err(|error| error.to_string())?
    .claims;
    // 兜底校验：显式比较过期时间，不受底层库默认值变化影响。
    if claims.exp <= now.timestamp() {
        return Err("令牌已过期".to_owned());
    }
    if claims.sub != config.admin_username {
        return Err("令牌主体与当前管理员不匹配".to_owned());
    }
    claims.iat = claims.iat.min(claims.exp);
    Ok(claims)
}

fn env_string(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

fn env_flag(name: &str) -> bool {
    std::env::var(name)
        .map(|value| matches!(value.trim().to_lowercase().as_str(), "1" | "true" | "yes"))
        .unwrap_or(false)
}

/// 读取以文件路径形式提供的秘密值。
///
/// 文件不存在、或内容去掉首尾空白后为空时返回 `Ok(None)`，回退策略交给调用方；
/// 其余失败（权限不足、路径是目录、非 UTF-8 等）按配置错误上报——
/// 文件存在却读不了时静默回退会掩盖真实的配置问题。
fn read_secret_file(path: &str) -> acmecast_core::Result<Option<String>> {
    let contents = match std::fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(acmecast_core::Error::Config(format!(
                "读取 ACMECAST_ADMIN_PASSWORD_HASH_FILE 指定的文件失败（{path}）：{error}；\
                 不会回退到 ACMECAST_ADMIN_PASSWORD_HASH"
            )));
        }
    };
    let trimmed = contents.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    Ok(Some(trimmed.to_owned()))
}

/// 解析管理员口令哈希：`ACMECAST_ADMIN_PASSWORD_HASH_FILE` 指向的文件优先，
/// 文件缺失或内容为空白时回退到 `ACMECAST_ADMIN_PASSWORD_HASH`。
fn resolve_admin_password_hash(
    file_path: Option<String>,
    env_value: Option<String>,
) -> acmecast_core::Result<Option<String>> {
    match file_path {
        Some(path) => read_secret_file(&path).map(|value| value.or(env_value)),
        None => Ok(env_value),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config_with_hash(password: &str) -> AuthConfig {
        use argon2::PasswordHasher;
        let hash = Argon2::default()
            .hash_password(password.as_bytes())
            .expect("应能生成口令哈希")
            .to_string();
        AuthConfig::new(
            "admin",
            Some(hash),
            "test-secret",
            Duration::from_secs(3600),
        )
    }

    /// 建一个一次性临时目录；调用方用后自行 `remove_dir_all`。
    fn temp_dir(tag: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("acmecast-auth-{tag}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("应能建临时目录");
        dir
    }

    #[test]
    fn password_verification_accepts_only_the_right_password() {
        let config = config_with_hash("s3cret");
        assert!(config.verify_password("s3cret"));
        assert!(!config.verify_password("wrong"));
        assert!(!config.verify_password(""));
    }

    #[test]
    fn login_is_unavailable_without_a_password_hash() {
        let config = AuthConfig::new("admin", None, "test-secret", Duration::from_secs(3600));
        assert!(!config.login_available());
        assert!(!config.verify_password("anything"));
    }

    #[test]
    fn token_round_trip_and_expiry() {
        let config = config_with_hash("pw");
        let now = Utc::now();
        let token = issue_token(&config, now).expect("应能签发令牌");
        let claims = verify_token(&config, &token, now).expect("刚签发的令牌应有效");
        assert_eq!(claims.sub, "admin");

        // 令牌有效期 1 小时后：过期。
        let later = now + ChronoDuration::hours(2);
        assert!(verify_token(&config, &token, later).is_err());
    }

    #[test]
    fn token_signed_with_another_secret_is_rejected() {
        let config = config_with_hash("pw");
        let other = AuthConfig::new("admin", None, "another-secret", Duration::from_secs(3600));
        let token = issue_token(&other, Utc::now()).expect("应能签发令牌");
        assert!(verify_token(&config, &token, Utc::now()).is_err());
    }

    #[test]
    fn limiter_locks_after_repeated_failures_and_recovers_after_window() {
        let limiter = LoginLimiter::new(LimitPolicy {
            max_failures: 3,
            lock_window_secs: 60,
        });
        let now = Utc::now();
        assert!(limiter.check("1.2.3.4", now).is_ok());
        for _ in 0..3 {
            limiter.record_failure("1.2.3.4", now);
        }
        let remaining = limiter
            .check("1.2.3.4", now)
            .expect_err("达到上限后应被锁定");
        assert!(remaining > 0, "应给出剩余锁定秒数");

        // 锁定窗口过去后恢复。
        assert!(
            limiter
                .check("1.2.3.4", now + ChronoDuration::seconds(61))
                .is_ok()
        );
        // 其他来源不受影响。
        assert!(limiter.check("5.6.7.8", now).is_ok());
    }

    #[test]
    fn limiter_clears_on_success() {
        let limiter = LoginLimiter::new(LimitPolicy::default());
        let now = Utc::now();
        limiter.record_failure("1.2.3.4", now);
        limiter.record_success("1.2.3.4");
        for _ in 0..4 {
            limiter.record_failure("1.2.3.4", now);
        }
        assert!(
            limiter.check("1.2.3.4", now).is_ok(),
            "成功后计数应清零，4 次失败不应触发默认 5 次上限"
        );
    }

    #[test]
    fn token_paths_are_classified_correctly() {
        assert!(!requires_token("/healthz"));
        assert!(!requires_token("/api/login"));
        assert!(!requires_token("/api/openapi.json"));
        assert!(!requires_token("/swagger-ui/index.html"));
        assert!(!requires_token("/assets/app.js"));
        assert!(!requires_token("/"));
        assert!(requires_token("/api/pipelines"));
        assert!(requires_token("/api/certificates/1"));
    }

    #[test]
    fn debug_output_redacts_secrets() {
        let config = config_with_hash("pw");
        let text = format!("{config:?}");
        assert!(text.contains(acmecast_core::REDACTED), "应带脱敏占位符");
        assert!(!text.contains("test-secret"), "不应泄露 JWT 密钥");
        assert!(!text.contains("$argon2"), "不应泄露口令哈希");
    }

    #[test]
    fn read_secret_file_reads_and_trims_contents() {
        let dir = temp_dir("trim");
        let file = dir.join("hash");
        std::fs::write(&file, "  $argon2id$v=19$abc  \n").expect("应能写文件");

        let value = read_secret_file(file.to_str().expect("路径应为 UTF-8")).expect("读取应成功");

        assert_eq!(value.as_deref(), Some("$argon2id$v=19$abc"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn read_secret_file_treats_a_missing_file_as_absent() {
        let dir = temp_dir("missing");
        let file = dir.join("does-not-exist");

        let value =
            read_secret_file(file.to_str().expect("路径应为 UTF-8")).expect("缺文件不算错误");

        assert_eq!(value, None);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn read_secret_file_treats_blank_contents_as_absent() {
        let dir = temp_dir("blank");
        let file = dir.join("hash");
        std::fs::write(&file, "  \n\t \n").expect("应能写文件");

        let value =
            read_secret_file(file.to_str().expect("路径应为 UTF-8")).expect("空内容不算错误");

        assert_eq!(value, None);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn read_secret_file_reports_an_unreadable_path() {
        // 路径指向目录：文件「存在」但读不了，应报错而不是静默回退。
        let dir = temp_dir("directory");

        let error =
            read_secret_file(dir.to_str().expect("路径应为 UTF-8")).expect_err("读目录应报错");

        let text = error.to_string();
        assert!(
            text.contains("ACMECAST_ADMIN_PASSWORD_HASH_FILE"),
            "报错应点名文件变量：{text}"
        );
        assert!(text.contains("不会回退"), "报错应说明不回退：{text}");
        assert!(matches!(error, acmecast_core::Error::Config(_)));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn resolve_admin_password_hash_prefers_the_file() {
        let dir = temp_dir("prefer");
        let file = dir.join("hash");
        std::fs::write(&file, "file-hash").expect("应能写文件");

        let value = resolve_admin_password_hash(
            Some(file.to_str().expect("路径应为 UTF-8").to_owned()),
            Some("env-hash".to_owned()),
        )
        .expect("解析应成功");

        assert_eq!(value.as_deref(), Some("file-hash"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn resolve_admin_password_hash_falls_back_when_the_file_is_missing() {
        let dir = temp_dir("fallback-missing");
        let file = dir.join("does-not-exist");

        let value = resolve_admin_password_hash(
            Some(file.to_str().expect("路径应为 UTF-8").to_owned()),
            Some("env-hash".to_owned()),
        )
        .expect("解析应成功");

        assert_eq!(value.as_deref(), Some("env-hash"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn resolve_admin_password_hash_falls_back_when_the_file_is_blank() {
        let dir = temp_dir("fallback-blank");
        let file = dir.join("hash");
        std::fs::write(&file, " \n").expect("应能写文件");

        let value = resolve_admin_password_hash(
            Some(file.to_str().expect("路径应为 UTF-8").to_owned()),
            Some("env-hash".to_owned()),
        )
        .expect("解析应成功");

        assert_eq!(value.as_deref(), Some("env-hash"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn resolve_admin_password_hash_uses_the_environment_when_no_file_is_configured() {
        let value =
            resolve_admin_password_hash(None, Some("env-hash".to_owned())).expect("解析应成功");

        assert_eq!(value.as_deref(), Some("env-hash"));
    }

    #[test]
    fn resolve_admin_password_hash_is_none_without_any_source() {
        // 两种来源都没有不算配置错误：服务照常启动，只是无人能登录。
        let value = resolve_admin_password_hash(None, None).expect("缺哈希不应报错");

        assert_eq!(value, None);
    }

    #[test]
    fn file_sourced_hash_is_still_validated_as_phc() {
        use argon2::PasswordHasher;
        let hash = Argon2::default()
            .hash_password(b"pw")
            .expect("应能生成口令哈希")
            .to_string();
        let dir = temp_dir("phc");
        let file = dir.join("hash");
        // 模拟 `hash-password > file`：内容末尾带换行。
        std::fs::write(&file, format!("{hash}\n")).expect("应能写文件");

        let resolved = resolve_admin_password_hash(
            Some(file.to_str().expect("路径应为 UTF-8").to_owned()),
            None,
        )
        .expect("解析应成功")
        .expect("文件应给出哈希");

        assert_eq!(resolved, hash);
        assert!(
            PasswordHash::new(&resolved).is_ok(),
            "文件来源的哈希同样要过 PHC 校验"
        );
        std::fs::remove_dir_all(&dir).ok();
    }
}
