//! 服务端配置。
//!
//! 全量配置承载于单一 `config.toml`：覆盖服务监听、数据目录、数据库等字段，
//! 以及 `[[resolvers]]` 解析器扩展。合并优先级为 `builtin < config.toml < env`——
//! 文件缺省时回退内置默认值，环境变量始终覆盖文件。配置随进程启动读取一次，
//! 修改后需重启。
//!
//! 首次启动时若 `config.toml` 不存在，会自动生成带注释的模板（不覆盖已存在文件）；
//! 也可通过 `acmecast-server config init|generate` 子命令参数化生成。

use std::path::{Path, PathBuf};

use acmecast_dns::ResolverEntry;
use serde::Deserialize;

/// 默认请求体体积上限（字节）。
const DEFAULT_BODY_LIMIT_BYTES: usize = 2 * 1024 * 1024;

/// 指定 `config.toml` 路径的环境变量。
pub const CONFIG_PATH_ENV: &str = "ACMECAST_CONFIG";

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
            listen_addr: default_listen_addr(),
            data_dir: PathBuf::from(default_data_dir()),
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

fn default_listen_addr() -> String {
    "0.0.0.0:8080".to_owned()
}

fn default_data_dir() -> String {
    "data".to_owned()
}

/// `config.toml` 的完整结构：`[server]` 配置与 `[[resolvers]]` 扩展。
///
/// 缺省段经 `serde` 默认值填充，因此空文件或仅含注释的文件等同于 `AppConfig::default()`。
/// 解析器条目的结构校验在反序列化阶段完成（缺字段或类型不符将报错），
/// 端点的语义校验（如 DoH 须 `https://`）在合并时进行，非法条目跳过并告警。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct AppConfig {
    /// 服务端字段。
    #[serde(default)]
    pub server: ServerSection,
    /// 解析器扩展条目；与内置集合按 endpoint 去重。
    #[serde(default)]
    pub resolvers: Vec<ResolverEntry>,
}

/// `config.toml` 中 `[server]` 段的字段，与 [`ServerConfig`] 一一对应。
///
/// 路径类字段在文件中以字符串表达，合并时转换为 `PathBuf`。
/// 各字段的默认值与 [`ServerConfig::default`] 保持一致。
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerSection {
    /// 监听地址。
    #[serde(default = "default_listen_addr")]
    pub listen_addr: String,
    /// 数据目录。
    #[serde(default = "default_data_dir")]
    pub data_dir: String,
    /// 数据库连接串；为 `None` 时取数据目录下的 SQLite。
    #[serde(default)]
    pub database_url: Option<String>,
    /// 前端静态资源目录；为 `None` 时不托管静态资源。
    #[serde(default)]
    pub static_dir: Option<String>,
    /// 是否关闭鉴权。
    #[serde(default)]
    pub auth_disabled: bool,
    /// 请求体体积上限（字节）。
    #[serde(default = "default_body_limit_bytes")]
    pub body_limit_bytes: usize,
    /// 跳过 ACME CA 的 TLS 证书校验。
    #[serde(default)]
    pub accept_invalid_acme_certs: bool,
}

fn default_body_limit_bytes() -> usize {
    DEFAULT_BODY_LIMIT_BYTES
}

/// 与 [`ServerConfig::default`] 保持一致：serde `#[serde(default)]` 段缺省时取此值。
impl Default for ServerSection {
    fn default() -> Self {
        Self {
            listen_addr: default_listen_addr(),
            data_dir: default_data_dir(),
            database_url: None,
            static_dir: None,
            auth_disabled: false,
            body_limit_bytes: default_body_limit_bytes(),
            accept_invalid_acme_certs: false,
        }
    }
}

impl AppConfig {
    /// 从指定路径加载 `config.toml`。
    ///
    /// 文件不存在或仅含空白/注释时返回默认值（回退内置）；文件存在但解析失败时
    /// 返回错误，错误信息指明出错的键与期望类型，以便诊断。解析器条目的语义
    /// 校验（endpoint 合法性）不在此时进行，留给合并阶段按既有语义跳过告警。
    pub fn load_from_file(path: &Path) -> acmecast_core::Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let content = std::fs::read_to_string(path)?;
        if content.trim().is_empty() {
            return Ok(Self::default());
        }
        toml::from_str(&content)
            .map_err(|e| acmecast_core::Error::Config(format!("解析 config.toml 失败: {e}")))
    }
}

/// 解析 `config.toml` 的路径：`ACMECAST_CONFIG` 优先，否则取数据目录下 `config.toml`。
///
/// 数据目录在配置加载前确定，先看 `ACMECAST_DATA_DIR` 环境变量，再回退内置 `data`。
/// 文件内的 `server.data_dir` 不影响自身路径——路径必须在能读文件之前就确定。
pub fn config_path() -> PathBuf {
    if let Ok(p) = std::env::var(CONFIG_PATH_ENV)
        && !p.trim().is_empty()
    {
        return PathBuf::from(p.trim());
    }
    let data_dir = std::env::var("ACMECAST_DATA_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(default_data_dir()));
    data_dir.join("config.toml")
}

/// 解析配置文件路径：显式指定（CLI 全局 `--config`）优先，否则回退 [`config_path`]。
pub fn resolve_config_path(override_path: Option<&Path>) -> PathBuf {
    override_path
        .map(Path::to_path_buf)
        .unwrap_or_else(config_path)
}

impl ServerConfig {
    /// 仅从环境变量读取配置；未设置的项取默认值。
    pub fn from_env() -> Self {
        let mut config = Self::default();
        apply_env(&mut config);
        config
    }

    /// 从 `config.toml` 与环境变量合并配置：`builtin < config.toml < env`。
    ///
    /// 文件不存在或为空时等同于 `from_env`。文件解析失败（字段类型不匹配等）
    /// 返回错误，指明出错的键与期望类型，服务因此中止启动。
    pub fn from_env_and_file() -> acmecast_core::Result<Self> {
        Self::from_env_and_file_at(None)
    }

    /// 与 [`Self::from_env_and_file`] 相同，但允许显式指定配置文件路径
    /// （CLI 全局 `--config` 参数）；为 `None` 时回退 [`resolve_config_path`] 的解析。
    pub fn from_env_and_file_at(path: Option<&Path>) -> acmecast_core::Result<Self> {
        let path = resolve_config_path(path);
        let file = AppConfig::load_from_file(&path)?;
        let mut config = Self::default();
        apply_file(&mut config, &file);
        apply_env(&mut config);
        Ok(config)
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

/// 把 `config.toml` 的 `[server]` 段叠到运行配置上（覆盖内置默认值）。
fn apply_file(config: &mut ServerConfig, file: &AppConfig) {
    let s = &file.server;
    config.listen_addr = s.listen_addr.clone();
    config.data_dir = PathBuf::from(&s.data_dir);
    if s.database_url.is_some() {
        config.database_url = s.database_url.clone();
    }
    if s.static_dir.is_some() {
        config.static_dir = s.static_dir.as_ref().map(PathBuf::from);
    }
    config.auth_disabled = s.auth_disabled;
    config.body_limit_bytes = s.body_limit_bytes;
    config.accept_invalid_acme_certs = s.accept_invalid_acme_certs;
}

/// 把环境变量叠到运行配置上（覆盖文件值）。
fn apply_env(config: &mut ServerConfig) {
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
        config.auth_disabled = matches!(value.trim().to_lowercase().as_str(), "1" | "true" | "yes");
    }
    if let Ok(value) = std::env::var("ACMECAST_BODY_LIMIT_BYTES")
        && let Ok(limit) = value.trim().parse::<usize>()
        && limit > 0
    {
        config.body_limit_bytes = limit;
    }
}

/// 生成带注释的 `config.toml` 模板内容。
///
/// 模板含 `[server]` 全字段默认值与 `[[resolvers]]` 示例（抽样自 `acmecast-dns::builtin`）。
/// 不含任何敏感凭据；解析器示例仅取公共 DoH 端点。
#[must_use]
pub fn template_content() -> String {
    let mut out = String::new();
    out.push_str("# acmecast 配置文件\n");
    out.push_str(
        "# 缺省字段使用内置默认值；环境变量优先级高于此文件（builtin < config.toml < env）。\n",
    );
    out.push_str("# 修改后需重启服务生效。\n\n");

    out.push_str("[server]\n");
    out.push_str("# HTTP 监听地址（host:port）。\n");
    out.push_str(&format!("listen_addr = \"{}\"\n", default_listen_addr()));
    out.push_str("# 数据目录：数据库、证书与凭据密钥的共同存放点。\n");
    out.push_str(&format!("data_dir = \"{}\"\n", default_data_dir()));
    out.push_str("# 数据库连接串；留空时使用数据目录下的 SQLite。\n");
    out.push_str("# database_url = \"sqlite://data/acmecast.db?mode=rwc\"\n");
    out.push_str("# 前端静态资源目录；留空时不托管静态资源（仅 API）。\n");
    out.push_str("# static_dir = \"/static\"\n");
    out.push_str("# 是否关闭鉴权（仅限本地单机场景）。\n");
    out.push_str("auth_disabled = false\n");
    out.push_str("# 请求体体积上限（字节），超出返回 413。\n");
    out.push_str(&format!(
        "body_limit_bytes = {}\n",
        DEFAULT_BODY_LIMIT_BYTES
    ));
    out.push_str("# 跳过 ACME CA 的 TLS 证书校验，仅为自建测试 CA（pebble 等）准备。\n");
    out.push_str("accept_invalid_acme_certs = false\n\n");

    out.push_str("# DNS-01 传播检测解析器扩展（可选）。\n");
    out.push_str("# type: doh / dot / dns；dot/dns 目前仅校验，传输层尚未实现。\n");
    out.push_str("# 与内置集合按 endpoint 去重；ACMECAST_DOH_RESOLVERS=none 可跳过传播等待。\n");
    out.push_str("[[resolvers]]\n");
    out.push_str("type = \"doh\"\n");
    out.push_str("endpoint = \"https://dns.alidns.com/dns-query\"\n");
    out.push_str("# name = \"阿里 DNS\"\n\n");
    out.push_str("[[resolvers]]\n");
    out.push_str("type = \"doh\"\n");
    out.push_str("endpoint = \"https://cloudflare-dns.com/dns-query\"\n");
    out.push_str("# name = \"Cloudflare DNS\"\n");
    out
}

/// 在指定路径生成 `config.toml` 模板，不覆盖已存在文件。
///
/// 采用 `create_new` 语义：文件已存在时返回 `Ok(false)` 且不做任何改动；
/// 写入成功返回 `Ok(true)`。父目录不存在时会自动创建。返回的 `bool` 表示是否实际生成。
pub fn generate_template(path: &Path) -> std::io::Result<bool> {
    if path.exists() {
        return Ok(false);
    }
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)?;
    }
    let content = template_content();
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
    {
        Ok(mut f) => {
            use std::io::Write;
            f.write_all(content.as_bytes())?;
            Ok(true)
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(false),
        Err(e) => Err(e),
    }
}

/// 在指定路径写入 `config.toml`，`force` 为真时覆盖已存在文件。
///
/// 与 [`generate_template`] 不同，本函数面向 CLI `config init --force`：
/// 显式要求覆盖。返回的 `bool` 表示是否实际写入（已存在且未 `force` 时为 `false`）。
pub fn write_template(path: &Path, force: bool) -> std::io::Result<bool> {
    if path.exists() && !force {
        return Ok(false);
    }
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)?;
    }
    let content = template_content();
    std::fs::write(path, content)?;
    Ok(true)
}

#[cfg(test)]
// 环境变量会跨测试共享进程状态，涉及 `set_var`/`remove_var` 的用例标注 `#[serial]` 独占。
// Rust 2024 起 `set_var`/`remove_var` 为 unsafe；本模块用例均 `#[serial]` 串行，无并发访问。
#[allow(unsafe_code)]
mod tests {
    use super::*;
    use serial_test::serial;

    /// 测试目录的唯一前缀，避免并行用例相互覆盖。
    fn unique_dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("acmecast-cfg-{}-{}", name, uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    // 涉及环境变量的用例必须串行，避免互相污染。
    fn clear_server_env() {
        for k in [
            "ACMECAST_LISTEN",
            "ACMECAST_DATA_DIR",
            "ACMECAST_DATABASE_URL",
            "ACMECAST_STATIC_DIR",
            "ACMECAST_AUTH_DISABLED",
            "ACMECAST_BODY_LIMIT_BYTES",
            "ACMECAST_INSECURE_SKIP_VERIFY",
            "ACMECAST_CONFIG",
        ] {
            // SAFETY: 唯一调用者为标注 `#[serial]` 的测试用例。
            unsafe { std::env::remove_var(k) };
        }
    }

    #[test]
    #[serial]
    fn from_env_uses_defaults_when_no_env() {
        clear_server_env();
        let config = ServerConfig::from_env();
        assert_eq!(config.listen_addr, default_listen_addr());
        assert_eq!(config.data_dir, PathBuf::from(default_data_dir()));
        assert_eq!(config.body_limit_bytes, DEFAULT_BODY_LIMIT_BYTES);
    }

    #[test]
    #[serial]
    fn from_env_and_file_falls_back_when_file_missing() {
        clear_server_env();
        // 指向不存在的路径，应等同于内置默认 + env。
        unsafe { std::env::set_var("ACMECAST_CONFIG", "/tmp/__acmecast_no_such_config.toml") };
        let config = ServerConfig::from_env_and_file().unwrap();
        assert_eq!(config.listen_addr, default_listen_addr());
        unsafe { std::env::remove_var("ACMECAST_CONFIG") };
    }

    #[test]
    #[serial]
    fn from_env_and_file_at_uses_explicit_path() {
        clear_server_env();
        let dir = unique_dir("at_path");
        let path = dir.join("config.toml");
        std::fs::write(&path, "[server]\nlisten_addr = \"127.0.0.1:7001\"\n").unwrap();

        let config = ServerConfig::from_env_and_file_at(Some(&path)).unwrap();
        assert_eq!(config.listen_addr, "127.0.0.1:7001");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    #[serial]
    fn empty_file_falls_back_to_defaults() {
        clear_server_env();
        let dir = unique_dir("empty");
        let path = dir.join("config.toml");
        std::fs::write(&path, "   \n# only comments\n").unwrap();

        unsafe { std::env::set_var("ACMECAST_CONFIG", &path) };
        let config = ServerConfig::from_env_and_file().unwrap();
        assert_eq!(config.listen_addr, default_listen_addr());

        unsafe { std::env::remove_var("ACMECAST_CONFIG") };
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    #[serial]
    fn file_overrides_builtin_env_overrides_file() {
        clear_server_env();
        let dir = unique_dir("prio");
        let path = dir.join("config.toml");
        std::fs::write(
            &path,
            "[server]\nlisten_addr = \"127.0.0.1:7000\"\ndata_dir = \"./file-data\"\nbody_limit_bytes = 1024\n",
        )
        .unwrap();

        unsafe { std::env::set_var("ACMECAST_CONFIG", &path) };
        // env 覆盖文件
        unsafe { std::env::set_var("ACMECAST_LISTEN", "0.0.0.0:9999") };

        let config = ServerConfig::from_env_and_file().unwrap();
        // env 胜过文件
        assert_eq!(config.listen_addr, "0.0.0.0:9999");
        // 文件胜过内置（env 未设 data_dir）
        assert_eq!(config.data_dir, PathBuf::from("./file-data"));
        // 文件值
        assert_eq!(config.body_limit_bytes, 1024);

        unsafe { std::env::remove_var("ACMECAST_LISTEN") };
        unsafe { std::env::remove_var("ACMECAST_CONFIG") };
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    #[serial]
    fn malformed_file_reports_error() {
        clear_server_env();
        let dir = unique_dir("malformed");
        let path = dir.join("config.toml");
        std::fs::write(&path, "[server]\nlisten_addr = [unclosed\n").unwrap();

        unsafe { std::env::set_var("ACMECAST_CONFIG", &path) };
        let err = ServerConfig::from_env_and_file().unwrap_err();
        assert!(err.to_string().contains("config.toml"), "{err}");

        unsafe { std::env::remove_var("ACMECAST_CONFIG") };
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    #[serial]
    fn type_mismatch_reports_field() {
        clear_server_env();
        let dir = unique_dir("typemismatch");
        let path = dir.join("config.toml");
        // body_limit_bytes 期望 usize，给字符串
        std::fs::write(&path, "[server]\nbody_limit_bytes = \"not-a-number\"\n").unwrap();

        unsafe { std::env::set_var("ACMECAST_CONFIG", &path) };
        let err = ServerConfig::from_env_and_file().unwrap_err();
        assert!(err.to_string().contains("body_limit_bytes"), "{err}");

        unsafe { std::env::remove_var("ACMECAST_CONFIG") };
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    #[serial]
    fn resolvers_section_parses_entries() {
        clear_server_env();
        let dir = unique_dir("resolvers");
        let path = dir.join("config.toml");
        std::fs::write(
            &path,
            "[[resolvers]]\ntype = \"doh\"\nendpoint = \"https://dns.example.com/dns-query\"\nname = \"示例\"\n",
        )
        .unwrap();

        unsafe { std::env::set_var("ACMECAST_CONFIG", &path) };
        let file = AppConfig::load_from_file(&path).unwrap();
        assert_eq!(file.resolvers.len(), 1);
        assert_eq!(file.resolvers[0].kind, "doh");
        assert_eq!(
            file.resolvers[0].endpoint,
            "https://dns.example.com/dns-query"
        );
        assert_eq!(file.resolvers[0].name.as_deref(), Some("示例"));

        unsafe { std::env::remove_var("ACMECAST_CONFIG") };
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn generate_template_does_not_overwrite_existing() {
        let dir = unique_dir("noco overwrite");
        let path = dir.join("config.toml");
        std::fs::write(&path, "user content").unwrap();
        // 已存在 → 不覆盖
        let created = generate_template(&path).unwrap();
        assert!(!created, "已存在文件不应被覆盖");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "user content");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn generate_template_creates_when_missing() {
        let dir = unique_dir("create");
        let path = dir.join("config.toml");
        let created = generate_template(&path).unwrap();
        assert!(created, "缺失时应生成");
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("[server]"));
        assert!(content.contains("[[resolvers]]"));
        assert!(content.contains("listen_addr"));
        // 模板不含敏感凭据
        assert!(!content.contains("password"));
        assert!(!content.contains("secret"));
        assert!(!content.contains("token"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn write_template_force_overwrites() {
        let dir = unique_dir("force");
        let path = dir.join("config.toml");
        std::fs::write(&path, "old").unwrap();
        let written = write_template(&path, true).unwrap();
        assert!(written);
        assert!(std::fs::read_to_string(&path).unwrap().contains("[server]"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn write_template_without_force_keeps_existing() {
        let dir = unique_dir("noforce");
        let path = dir.join("config.toml");
        std::fs::write(&path, "old").unwrap();
        let written = write_template(&path, false).unwrap();
        assert!(!written);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "old");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn template_content_has_no_credentials() {
        let content = template_content();
        assert!(!content.contains("ACMECAST_ADMIN_PASSWORD_HASH"));
        assert!(!content.contains("ACMECAST_CREDENTIAL_KEY"));
        assert!(!content.contains("ACMECAST_JWT_SECRET"));
    }
}
