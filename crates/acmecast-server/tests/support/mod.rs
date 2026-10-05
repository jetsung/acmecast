//! 集成测试共用的装配工具。
//!
//! 每个用例都要起一个「数据库已迁移、依赖已注入」的路由，重复写在各个
//! 测试文件里只会让签名变更时到处补丁。这里集中一处。

#![allow(dead_code, unreachable_pub)]

use std::{path::PathBuf, sync::Arc, time::Duration};

use acmecast_access::CredentialRegistry;
use acmecast_pipeline::StepRegistry;
use acmecast_server::{
    AppState, RuntimeState,
    auth::{AuthConfig, AuthMode},
    config::ServerConfig,
};
use acmecast_store::migrate;
use axum::{
    Router,
    body::Body,
    http::{Request, Response},
};
use sea_orm::{Database, DatabaseConnection};
use tower::ServiceExt;

/// 测试用管理员口令。
pub const ADMIN_PASSWORD: &str = "correct-horse-battery-staple";
/// 测试用 JWT 签名密钥。
pub const JWT_SECRET: &str = "integration-test-secret";

/// 一个临时的数据目录，随结构体析构而删除。
#[derive(Debug)]
pub struct TempDir {
    path: PathBuf,
}

impl TempDir {
    /// 在系统临时目录下建一个唯一子目录。
    pub fn new(tag: &str) -> Self {
        // 不引 tempfile：需要的只是「唯一且会清理」，uuid 已在依赖里。
        let path = std::env::temp_dir().join(format!("acmecast-{tag}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&path).expect("应能创建临时目录");
        Self { path }
    }

    /// 目录路径。
    #[must_use]
    pub fn path(&self) -> &std::path::Path {
        &self.path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// 生成一个 Argon2 PHC 口令哈希。
#[must_use]
pub fn password_hash(password: &str) -> String {
    use argon2::{Argon2, PasswordHasher};
    Argon2::default()
        .hash_password(password.as_bytes())
        .expect("应能生成口令哈希")
        .to_string()
}

/// 测试用鉴权配置：口令为 [`ADMIN_PASSWORD`]，令牌有效期 1 小时。
#[must_use]
pub fn auth_config() -> AuthConfig {
    AuthConfig::new(
        "admin",
        Some(password_hash(ADMIN_PASSWORD)),
        JWT_SECRET,
        Duration::from_secs(3600),
    )
}

/// 用给定配置装配路由与内存数据库。
///
/// `config` 决定静态目录与体积上限，`auth` 决定是否鉴权；
/// 数据库固定用内存 SQLite，用例之间互不干扰。
pub async fn app_with(config: ServerConfig, auth: AuthMode) -> Router {
    let db = Database::connect("sqlite::memory:")
        .await
        .expect("应能连接内存数据库");
    migrate(&db).await.expect("迁移应成功");
    acmecast_server::assemble_router_with_runtime(
        AppState { db, config },
        RuntimeState::new(
            acmecast_server::dependencies::credential_registry(),
            acmecast_server::dependencies::step_registry(),
            None,
            auth,
        ),
    )
}

/// 用完整依赖（自定义加密器与步骤注册表）装配路由。
///
/// 吊销等需要解密凭据的端点用得上；`cipher` 为 `None` 时凭据写入会被拒绝。
pub async fn app_with_deps(
    config: ServerConfig,
    auth: AuthMode,
    cipher: Option<Arc<acmecast_core::CredentialCipher>>,
    credential_registry: Arc<CredentialRegistry>,
    steps: Arc<StepRegistry>,
) -> Router {
    let db = Database::connect("sqlite::memory:")
        .await
        .expect("应能连接内存数据库");
    migrate(&db).await.expect("迁移应成功");
    router_with_db(db, config, auth, cipher, credential_registry, steps).await
}

/// 用调用方持有的数据库装配路由——需要先往库里写状态的测试（如经
/// 启动器首签）用得上。
pub async fn router_with_db(
    db: DatabaseConnection,
    config: ServerConfig,
    auth: AuthMode,
    cipher: Option<Arc<acmecast_core::CredentialCipher>>,
    credential_registry: Arc<CredentialRegistry>,
    steps: Arc<StepRegistry>,
) -> Router {
    acmecast_server::assemble_router_with_runtime(
        AppState { db, config },
        RuntimeState::new(credential_registry, steps, cipher, auth),
    )
}

/// 装配一个强制鉴权、默认配置的路由。
pub async fn authenticated_app() -> Router {
    app_with(ServerConfig::default(), AuthMode::enforced(auth_config())).await
}

/// 装配一个关闭鉴权、默认配置的路由。
pub async fn open_app() -> Router {
    app_with(ServerConfig::default(), AuthMode::Disabled).await
}

/// 直接向路由发一个请求。
pub async fn send(app: &Router, request: Request<Body>) -> Response<Body> {
    app.clone().oneshot(request).await.expect("应能处理请求")
}

/// 读取响应体为 JSON。
pub async fn json(response: Response<Body>) -> serde_json::Value {
    let bytes = axum::body::to_bytes(response.into_body(), 8 * 1024 * 1024)
        .await
        .expect("应能读取响应体");
    serde_json::from_slice(&bytes).expect("响应应为 JSON")
}

/// 构造带 JSON 请求体的请求。
pub fn json_request(method: &str, uri: &str, body: &serde_json::Value) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::to_vec(body).expect("应能序列化请求体"),
        ))
        .expect("应能构造请求")
}

/// 登录并取回访问令牌。
pub async fn login(app: &Router, password: &str) -> (axum::http::StatusCode, serde_json::Value) {
    let request = json_request(
        "POST",
        "/api/login",
        &serde_json::json!({ "username": "admin", "password": password }),
    );
    let response = send(app, request).await;
    let status = response.status();
    (status, json(response).await)
}

/// 给请求装上 Bearer 令牌。
#[must_use]
pub fn with_bearer(mut request: Request<Body>, token: &str) -> Request<Body> {
    request.headers_mut().insert(
        axum::http::header::AUTHORIZATION,
        format!("Bearer {token}")
            .parse()
            .expect("令牌应是合法头部值"),
    );
    request
}

/// 生成一份自签证书（叶子 + 根）与对应私钥。
#[must_use]
pub fn test_certificate() -> (String, String) {
    use rcgen::{BasicConstraints, CertificateParams, DnType, IsCa, KeyPair, SanType};

    let ca_key = KeyPair::generate().expect("应能生成 CA 密钥");
    let mut ca_params = CertificateParams::default();
    ca_params
        .distinguished_name
        .push(DnType::CommonName, "acmecast Test Root CA");
    ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    let ca_cert = ca_params.self_signed(&ca_key).expect("应能自签 CA");

    let leaf_key = KeyPair::generate().expect("应能生成叶子密钥");
    let mut leaf_params = CertificateParams::default();
    leaf_params
        .distinguished_name
        .push(DnType::CommonName, "download.example.com");
    leaf_params.subject_alt_names =
        vec![SanType::DnsName("download.example.com".try_into().unwrap())];
    let issuer = rcgen::Issuer::new(ca_params, ca_key);
    let leaf_cert = leaf_params
        .signed_by(&leaf_key, &issuer)
        .expect("应能签发叶子证书");

    (
        format!("{}{}", leaf_cert.pem(), ca_cert.pem()),
        leaf_key.serialize_pem(),
    )
}

/// 在一个临时数据目录里放一份证书与私钥，返回目录、两条相对路径与证书链。
///
/// 走 `FileStore::write_certificate` 而非自己拼路径：库里存的相对路径
/// 就是它算出来的，测试应当和生产用同一套规则。
pub async fn certificate_fixture() -> (TempDir, String, String, String) {
    let dir = TempDir::new("cert");
    let (chain_pem, key_pem) = test_certificate();
    let store = acmecast_store::FileStore::open(dir.path())
        .await
        .expect("应能打开文件仓库");
    let paths = store
        .write_certificate(
            "download-example-com",
            "download.example.com",
            &chain_pem,
            &key_pem,
        )
        .await
        .expect("应能写入证书");
    (dir, paths.cert_pem, paths.key_pem, chain_pem)
}
