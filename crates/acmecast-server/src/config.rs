//! 服务端配置。
//!
//! 全部来自环境变量（部署环境不同端口、目录各异，配置文件留给数据目录，
//! 避免两处来源打架）。带默认值，零配置即可在本地起一个可用实例。

use std::path::PathBuf;

/// 默认请求体体积上限（字节）。
const DEFAULT_BODY_LIMIT_BYTES: usize = 2 * 1024 * 1024;

/// 服务端运行配置。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerConfig {
    /// 监听地址，默认 `0.0.0.0:8080`。
    pub listen_addr: String,
    /// 数据目录：数据库文件、证书文件与凭据密钥都落在这里。默认 `./data`。
    pub data_dir: PathBuf,
    /// 数据库连接串；缺省时取数据目录下的 SQLite 文件。
    pub database_url: Option<String>,
    /// 前端静态资源目录；为 `None` 时不托管静态资源（仅 API）。
    pub static_dir: Option<PathBuf>,
    /// 是否关闭鉴权；默认 `false`，仅用于本地单机场景。
    pub auth_disabled: bool,
    /// 请求体体积上限（字节）。
    pub body_limit_bytes: usize,
    /// 跳过 ACME CA 的 TLS 证书校验。
    ///
    /// 仅为自建内部 CA（如 pebble）准备；公网 CA 一律保持 `false`。
    pub accept_invalid_acme_certs: bool,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            listen_addr: "0.0.0.0:8080".to_owned(),
            data_dir: PathBuf::from("data"),
            database_url: None,
            static_dir: None,
            auth_disabled: false,
            body_limit_bytes: DEFAULT_BODY_LIMIT_BYTES,
            accept_invalid_acme_certs: false,
        }
    }
}

/// 读布尔环境变量（`1`/`true`/`yes`，大小写不敏感）。
fn env_flag(name: &str) -> bool {
    std::env::var(name)
        .map(|value| matches!(value.trim().to_lowercase().as_str(), "1" | "true" | "yes"))
        .unwrap_or(false)
}

impl ServerConfig {
    /// 从环境变量读取配置；未设置的项取默认值。
    pub fn from_env() -> Self {
        let mut config = Self::default();
        if let Ok(addr) = std::env::var("ACMECAST_LISTEN")
            && !addr.trim().is_empty()
        {
            config.listen_addr = addr;
        }
        if let Ok(dir) = std::env::var("ACMECAST_DATA_DIR")
            && !dir.trim().is_empty()
        {
            config.data_dir = PathBuf::from(dir);
        }
        if let Ok(url) = std::env::var("ACMECAST_DATABASE_URL")
            && !url.trim().is_empty()
        {
            config.database_url = Some(url);
        }
        if env_flag("ACMECAST_INSECURE_SKIP_VERIFY") {
            config.accept_invalid_acme_certs = true;
        }
        if let Ok(dir) = std::env::var("ACMECAST_STATIC_DIR")
            && !dir.trim().is_empty()
        {
            config.static_dir = Some(PathBuf::from(dir));
        }
        if let Ok(value) = std::env::var("ACMECAST_AUTH_DISABLED") {
            config.auth_disabled =
                matches!(value.trim().to_lowercase().as_str(), "1" | "true" | "yes");
        }
        if let Ok(value) = std::env::var("ACMECAST_BODY_LIMIT_BYTES")
            && let Ok(limit) = value.trim().parse::<usize>()
            && limit > 0
        {
            config.body_limit_bytes = limit;
        }
        config
    }

    /// 解析出数据库连接串；SQLite 文件随数据目录走。
    #[must_use]
    pub fn database_url(&self) -> String {
        self.database_url.clone().unwrap_or_else(|| {
            let db_path = self.data_dir.join("acmecast.db");
            format!("sqlite://{}?mode=rwc", db_path.display())
        })
    }
}
