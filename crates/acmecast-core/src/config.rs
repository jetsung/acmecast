//! 配置装载：YAML 文件为基底，环境变量按需覆盖。
//!
//! 环境变量命名规则：`ACMECAST__<SECTION>__<KEY>`（双下划线分隔），
//! 例如 `ACMECAST__DATABASE__URL=sqlite://...`。
//! 配置文件路径可由 `ACMECAST_CONFIG` 指定，默认依次尝试
//! `./config.yaml`、`./config.yml`、`/etc/acmecast/config.yaml`。

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use crate::error::{Error, Result};

/// 环境变量前缀。
pub const ENV_PREFIX: &str = "ACMECAST__";
/// 指定配置文件路径的环境变量。
pub const CONFIG_PATH_ENV: &str = "ACMECAST_CONFIG";

/// 应用全部配置。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AppConfig {
    /// HTTP 服务与静态资源托管。
    #[serde(default)]
    pub server: ServerConfig,
    /// 数据库连接与连接池。
    #[serde(default)]
    pub database: DatabaseConfig,
    /// 证书与私钥等文件的存放位置。
    #[serde(default)]
    pub storage: StorageConfig,
    /// 凭据加密密钥、令牌签名与管理员账户。
    #[serde(default)]
    pub security: SecurityConfig,
    /// 日志级别与输出格式。
    #[serde(default)]
    pub log: LogConfig,
}

/// HTTP 服务与静态资源托管配置。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerConfig {
    /// 监听地址。
    #[serde(default = "default_bind")]
    pub bind: String,
    /// 前端静态资源目录，为空则不托管静态资源。
    #[serde(default)]
    pub web_root: Option<String>,
    /// 是否关闭鉴权中间件。默认 false；仅用于本地单机场景。
    #[serde(default)]
    pub disable_auth: bool,
    /// 请求体体积上限（字节）。
    #[serde(default = "default_body_limit")]
    pub body_limit_bytes: usize,
}

/// 数据库连接配置，按 URL scheme 区分方言。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DatabaseConfig {
    /// 连接串，`sqlite://`、`mysql://` 或 `postgres://`。
    #[serde(default = "default_database_url")]
    pub url: String,
    /// 连接池上限。
    #[serde(default = "default_max_connections")]
    pub max_connections: u32,
    /// 建连超时（秒）。
    #[serde(default = "default_connect_timeout")]
    pub connect_timeout_secs: u64,
}

/// 数据目录配置。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StorageConfig {
    /// 证书、私钥等文件的存放目录。
    #[serde(default = "default_data_dir")]
    pub data_dir: String,
}

/// 安全相关配置。
///
/// `Debug` 是手写的：三个字段要么是密钥、要么是可用于离线爆破的口令哈希，
/// 而配置对象太容易被顺手打进启动日志。用户名与有效期不是秘密，照常显示。
#[derive(Clone, Serialize, Deserialize)]
pub struct SecurityConfig {
    /// 凭据加密主密钥（base64，32 字节）。**必需**，缺省时服务拒绝启动。
    #[serde(default)]
    pub credential_key: Option<String>,
    /// JWT 签名密钥。**必需**，缺省时服务拒绝启动。
    #[serde(default)]
    pub jwt_secret: Option<String>,
    /// 访问令牌有效期（小时）。
    #[serde(default = "default_token_ttl_hours")]
    pub token_ttl_hours: i64,
    /// 管理员用户名。
    #[serde(default = "default_admin_username")]
    pub admin_username: String,
    /// 管理员口令的 Argon2 哈希（PHC 字符串）。缺省时无法登录，启动时告警。
    #[serde(default)]
    pub admin_password_hash: Option<String>,
}

impl std::fmt::Debug for SecurityConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SecurityConfig")
            .field(
                "credential_key",
                &crate::crypto::redact_presence(&self.credential_key),
            )
            .field(
                "jwt_secret",
                &crate::crypto::redact_presence(&self.jwt_secret),
            )
            .field("token_ttl_hours", &self.token_ttl_hours)
            .field("admin_username", &self.admin_username)
            .field(
                "admin_password_hash",
                &crate::crypto::redact_presence(&self.admin_password_hash),
            )
            .finish()
    }
}

/// 日志配置。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogConfig {
    /// 日志级别，如 `info`、`acmecast_cert=debug`。
    #[serde(default = "default_log_level")]
    pub level: String,
    /// 输出格式：`text` 或 `json`。
    #[serde(default = "default_log_format")]
    pub format: String,
}

fn default_bind() -> String {
    "0.0.0.0:7001".into()
}
fn default_body_limit() -> usize {
    2 * 1024 * 1024
}
fn default_database_url() -> String {
    "sqlite://./data/acmecast.db?mode=rwc".into()
}
fn default_max_connections() -> u32 {
    10
}
fn default_connect_timeout() -> u64 {
    10
}
fn default_data_dir() -> String {
    "./data".into()
}
fn default_token_ttl_hours() -> i64 {
    12
}
fn default_admin_username() -> String {
    "admin".into()
}
fn default_log_level() -> String {
    "info".into()
}
fn default_log_format() -> String {
    "text".into()
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            bind: default_bind(),
            web_root: None,
            disable_auth: false,
            body_limit_bytes: default_body_limit(),
        }
    }
}

impl Default for DatabaseConfig {
    fn default() -> Self {
        Self {
            url: default_database_url(),
            max_connections: default_max_connections(),
            connect_timeout_secs: default_connect_timeout(),
        }
    }
}

impl Default for StorageConfig {
    fn default() -> Self {
        Self {
            data_dir: default_data_dir(),
        }
    }
}

impl Default for SecurityConfig {
    fn default() -> Self {
        Self {
            credential_key: None,
            jwt_secret: None,
            token_ttl_hours: default_token_ttl_hours(),
            admin_username: default_admin_username(),
            admin_password_hash: None,
        }
    }
}

impl Default for LogConfig {
    fn default() -> Self {
        Self {
            level: default_log_level(),
            format: default_log_format(),
        }
    }
}

/// 数据库方言，由连接串 scheme 判定。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DatabaseKind {
    /// SQLite，本地文件库，开箱即用。
    Sqlite,
    /// MySQL 或 MariaDB。
    MySql,
    /// PostgreSQL。
    Postgres,
}

impl AppConfig {
    /// 从指定 YAML 文件加载配置；文件不存在时使用默认值。
    pub fn from_file(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let content = std::fs::read_to_string(path)?;
        serde_yaml::from_str(&content).map_err(Into::into)
    }

    /// 查找默认配置文件位置，返回第一个存在的文件。
    pub fn discover_file() -> Option<PathBuf> {
        if let Ok(explicit) = std::env::var(CONFIG_PATH_ENV) {
            return Some(PathBuf::from(explicit));
        }
        const CANDIDATES: [&str; 3] =
            ["./config.yaml", "./config.yml", "/etc/acmecast/config.yaml"];
        CANDIDATES
            .iter()
            .map(PathBuf::from)
            .find(|p: &PathBuf| p.exists())
    }

    /// 完整装载：读文件（若发现），再套用环境变量覆盖。
    pub fn load() -> Result<Self> {
        let mut config = match Self::discover_file() {
            Some(path) => Self::from_file(&path)?,
            None => Self::default(),
        };
        config.apply_env_overrides();
        Ok(config)
    }

    /// 套用所有已知环境变量。仅覆盖出现在环境中的项，未出现的保持原值。
    pub fn apply_env_overrides(&mut self) {
        use std::env::var;
        macro_rules! apply_str {
            ($($env:expr => $slot:expr),* $(,)?) => { $(
                if let Ok(v) = var($env) { $slot = v; }
            )* };
        }

        apply_str! {
            "ACMECAST__SERVER__BIND" => self.server.bind,
            "ACMECAST__DATABASE__URL" => self.database.url,
            "ACMECAST__STORAGE__DATA_DIR" => self.storage.data_dir,
            "ACMECAST__SECURITY__ADMIN_USERNAME" => self.security.admin_username,
            "ACMECAST__LOG__LEVEL" => self.log.level,
            "ACMECAST__LOG__FORMAT" => self.log.format,
        }

        // 单独处理 Option<String>，避免把空串写进配置。
        if let Ok(v) = var("ACMECAST__SECURITY__CREDENTIAL_KEY") {
            self.security.credential_key = Some(v);
        }
        if let Ok(v) = var("ACMECAST__SECURITY__JWT_SECRET") {
            self.security.jwt_secret = Some(v);
        }
        if let Ok(v) = var("ACMECAST__SECURITY__ADMIN_PASSWORD_HASH") {
            self.security.admin_password_hash = Some(v);
        }
        if let Ok(v) = var("ACMECAST__SERVER__WEB_ROOT") {
            self.server.web_root = Some(v);
        }
        if let Ok(v) = var("ACMECAST__SERVER__DISABLE_AUTH") {
            self.server.disable_auth = matches!(v.to_lowercase().as_str(), "1" | "true" | "yes");
        }
        if let Ok(v) = var("ACMECAST__DATABASE__MAX_CONNECTIONS")
            && let Ok(n) = v.parse()
        {
            self.database.max_connections = n;
        }
        if let Ok(v) = var("ACMECAST__SERVER__BODY_LIMIT_BYTES")
            && let Ok(n) = v.parse()
        {
            self.server.body_limit_bytes = n;
        }
    }

    /// 校验必需项。缺失凭据加密密钥或 JWT 密钥时直接失败——
    /// 明文落盘比启动失败危害大得多，因此不允许退化。
    pub fn validate(&self) -> Result<()> {
        let credential_key = self.security.credential_key.as_deref().unwrap_or("").trim();

        if credential_key.is_empty() {
            return Err(Error::Config(
                "缺少 security.credential_key：凭据需要静态存储加密，\
                 请先用 `acmecast gen-key` 生成密钥并配置；\
                 明文存储凭据是被明确禁止的，因此此处不会退化"
                    .into(),
            ));
        }

        // 密钥不仅要「有」，还必须**当场可用**：长度不对、不是合法 base64 这类问题
        // 都得在启动时就暴露，而不是拖到第一次写凭据时才炸。构造一次只为验证，
        // 实例本身不需要保留。
        crate::crypto::CredentialCipher::from_base64(credential_key)?;

        if self
            .security
            .jwt_secret
            .as_deref()
            .unwrap_or("")
            .trim()
            .is_empty()
        {
            return Err(Error::Config(
                "缺少 security.jwt_secret：令牌签发需要签名密钥".into(),
            ));
        }
        if self.database.url.trim().is_empty() {
            return Err(Error::Config("缺少 database.url".into()));
        }
        Ok(())
    }

    /// 判定数据库方言，委托给 [`DatabaseConfig::kind`]。
    pub fn database_kind(&self) -> Result<DatabaseKind> {
        self.database.kind()
    }

    /// 数据目录路径。
    pub fn data_dir(&self) -> PathBuf {
        PathBuf::from(&self.storage.data_dir)
    }
}

impl DatabaseKind {
    /// 从连接串判定方言。未识别的 scheme 返回配置错误而非运行时崩溃。
    ///
    /// SQLite 的连接串形式最多（`sqlite::memory:`、`sqlite://p?mode=rwc`、
    /// `sqlite:p`），统一按 `sqlite:` 前缀识别；其余方言按 `scheme://` 判定。
    pub fn from_url(url: &str) -> Result<Self> {
        let lower = url.to_ascii_lowercase();
        if lower.starts_with("sqlite:") {
            return Ok(Self::Sqlite);
        }

        let scheme = url
            .split("://")
            .next()
            .unwrap_or_default()
            .to_ascii_lowercase();
        match scheme.as_str() {
            "mysql" | "mariadb" => Ok(Self::MySql),
            "postgres" | "postgresql" => Ok(Self::Postgres),
            other => Err(Error::Config(format!(
                "不支持的数据库方言 `{other}`，仅支持 sqlite / mysql / postgres"
            ))),
        }
    }
}

impl DatabaseConfig {
    /// 本连接串对应的方言。
    pub fn kind(&self) -> Result<DatabaseKind> {
        DatabaseKind::from_url(&self.url)
    }
}

#[cfg(test)]
// Rust 2024 起 `std::env::set_var` / `remove_var` 为 unsafe。本模块所有用例
// 均标注 `#[serial]` 独占这些键，运行时无并发访问，故允许在此使用 unsafe。
#[allow(unsafe_code)]
mod tests {
    use super::*;
    use serial_test::serial;

    // Rust 2024 edition 起 `set_var` / `remove_var` 标记为 unsafe。
    // 本模块内所有涉及环境变量的用例均标注 `#[serial]`，运行时独占这些键，
    // 不存在数据竞争，故此处可安全单点封装。
    fn set_env(k: &str, v: &str) {
        // SAFETY: 唯一调用者为标注 `#[serial]` 的测试用例。
        unsafe { std::env::set_var(k, v) };
    }

    fn clear_env() {
        for k in [
            "ACMECAST__SERVER__BIND",
            "ACMECAST__SERVER__DISABLE_AUTH",
            "ACMECAST__SERVER__BODY_LIMIT_BYTES",
            "ACMECAST__DATABASE__URL",
            "ACMECAST__DATABASE__MAX_CONNECTIONS",
            "ACMECAST__STORAGE__DATA_DIR",
            "ACMECAST__SECURITY__CREDENTIAL_KEY",
            "ACMECAST__SECURITY__JWT_SECRET",
            "ACMECAST__SECURITY__ADMIN_PASSWORD_HASH",
            "ACMECAST__SECURITY__ADMIN_USERNAME",
            "ACMECAST__LOG__LEVEL",
            "ACMECAST__LOG__FORMAT",
            "ACMECAST__SERVER__WEB_ROOT",
        ] {
            // SAFETY: 唯一调用者为标注 `#[serial]` 的测试用例。
            unsafe { std::env::remove_var(k) };
        }
    }

    #[test]
    #[serial]
    fn defaults_are_usable() {
        clear_env();
        let config = AppConfig::default();
        assert_eq!(config.server.bind, "0.0.0.0:7001");
        assert!(!config.server.disable_auth, "鉴权默认必须开启");
        assert_eq!(config.database.max_connections, 10);
    }

    #[test]
    #[serial]
    fn env_overrides_file_values() {
        clear_env();
        set_env("ACMECAST__SERVER__BIND", "127.0.0.1:9999");
        set_env("ACMECAST__DATABASE__MAX_CONNECTIONS", "42");

        let mut config = AppConfig::default();
        config.apply_env_overrides();

        assert_eq!(config.server.bind, "127.0.0.1:9999");
        assert_eq!(config.database.max_connections, 42);
        clear_env();
    }

    #[test]
    #[serial]
    fn disable_auth_parses_boolean() {
        clear_env();
        set_env("ACMECAST__SERVER__DISABLE_AUTH", "true");
        let mut config = AppConfig::default();
        config.apply_env_overrides();
        assert!(config.server.disable_auth);
        clear_env();
    }

    #[test]
    #[serial]
    fn validate_rejects_missing_credential_key() {
        clear_env();
        let config = AppConfig::default();
        let err = config.validate().unwrap_err();
        let text = err.to_string();
        assert!(text.contains("credential_key"), "错误应指出缺少哪个配置");
        assert!(text.contains("明文"), "错误应说明不会退化为明文");
    }

    /// 构造一份其余项都合法、只有凭据密钥由参数决定的配置。
    ///
    /// 直接用结构体而不用环境变量：不起 `set_var`，也就不需要 `#[serial]`。
    fn config_with_credential_key(key: Option<&str>) -> AppConfig {
        AppConfig {
            security: SecurityConfig {
                credential_key: key.map(str::to_owned),
                jwt_secret: Some("test-jwt-secret".to_owned()),
                ..Default::default()
            },
            database: DatabaseConfig {
                url: "sqlite::memory:".to_owned(),
                ..Default::default()
            },
            ..Default::default()
        }
    }

    #[test]
    fn validate_rejects_a_blank_credential_key() {
        // 只有空白字符的密钥不是「配了密钥」，与缺失等价。
        let err = config_with_credential_key(Some("   "))
            .validate()
            .expect_err("空白密钥应被当作缺失");
        assert!(
            err.to_string().contains("credential_key"),
            "错误应指出 credential_key: {err}"
        );
    }

    #[test]
    fn validate_rejects_a_key_that_is_not_valid_base64() {
        // 非空但不是合法 base64：启动时就得失败，不能拖到写凭据那一刻。
        let err = config_with_credential_key(Some("not-a-valid-key!"))
            .validate()
            .expect_err("非法 base64 密钥应在启动校验时被拒绝");
        assert!(
            err.to_string().contains("密钥"),
            "错误应指向密钥本身: {err}"
        );
    }

    #[test]
    fn validate_rejects_a_key_of_the_wrong_length() {
        // 合法 base64，但只有 4 字节，远不足 32 字节。
        let err = config_with_credential_key(Some("YWJjZA=="))
            .validate()
            .expect_err("长度不足的密钥应在启动校验时被拒绝");
        assert!(
            err.to_string().contains("32 字节"),
            "错误应给出长度要求: {err}"
        );
    }

    #[test]
    fn validate_rejects_missing_jwt_secret() {
        // 密钥必须真实可用，否则失败会停在密钥那一步，测不到 JWT 这一条。
        let key = crate::crypto::CredentialCipher::generate_key_base64();
        let mut config = config_with_credential_key(Some(&key));
        config.security.jwt_secret = None;

        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("jwt_secret"), "{err}");
    }

    #[test]
    fn validate_accepts_complete_config() {
        let key = crate::crypto::CredentialCipher::generate_key_base64();
        config_with_credential_key(Some(&key))
            .validate()
            .expect("完整配置应通过校验");
    }

    #[test]
    #[serial]
    fn database_kind_detects_three_dialects() {
        clear_env();
        let mut config = AppConfig::default();
        for (url, want) in [
            ("sqlite://./data/a.db?mode=rwc", DatabaseKind::Sqlite),
            ("sqlite::memory:", DatabaseKind::Sqlite),
            ("sqlite:relative/a.db", DatabaseKind::Sqlite),
            ("mysql://user:pw@host/db", DatabaseKind::MySql),
            ("postgres://user:pw@host/db", DatabaseKind::Postgres),
        ] {
            config.database.url = url.into();
            assert_eq!(config.database.kind().unwrap(), want, "url = {url}");
        }
    }

    #[test]
    #[serial]
    fn database_kind_rejects_unknown_scheme() {
        clear_env();
        let mut config = AppConfig::default();
        config.database.url = "mongodb://localhost/db".into();
        assert!(matches!(config.database_kind(), Err(Error::Config(_))));
    }

    #[test]
    fn from_missing_file_falls_back_to_defaults() {
        let config = AppConfig::from_file(Path::new("/nonexistent/acmecast.yaml")).unwrap();
        assert_eq!(config.server.bind, default_bind());
    }

    #[test]
    fn yaml_is_parsed() {
        let dir = std::env::temp_dir().join(format!("acmecast-cfg-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.yaml");
        std::fs::write(
            &path,
            "server:\n  bind: 127.0.0.1:8080\ndatabase:\n  max_connections: 7\n",
        )
        .unwrap();

        let config = AppConfig::from_file(&path).unwrap();
        assert_eq!(config.server.bind, "127.0.0.1:8080");
        assert_eq!(config.database.max_connections, 7);
        assert_eq!(config.database.url, default_database_url());

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn malformed_yaml_reports_serialization_error() {
        let dir = std::env::temp_dir().join(format!("acmecast-bad-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.yaml");
        std::fs::write(&path, "server:\n  bind: [unclosed\n").unwrap();

        assert!(matches!(
            AppConfig::from_file(&path),
            Err(Error::Serialization(_))
        ));
        std::fs::remove_dir_all(&dir).ok();
    }
}
