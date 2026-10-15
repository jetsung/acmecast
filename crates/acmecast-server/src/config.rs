//! 服务端配置。
//!
//! 全量配置承载于单一 `config.toml`：覆盖服务监听、数据目录、数据库等字段，
//! 以及 `[[resolvers]]` 解析器扩展。合并优先级为 `builtin < config.toml < env`——
//! 文件缺省时回退内置默认值，环境变量始终覆盖文件。配置随进程启动读取一次，
//! 修改后需重启。
//!
//! 首次启动时若 `config.toml` 不存在，会自动生成带注释的模板（不覆盖已存在文件）；
//! 也可通过 `acmecast-server config init|generate` 子命令参数化生成。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use acmecast_dns::ResolverEntry;
use acmecast_notify::{ChannelConfig, SignKind};
use serde::Deserialize;

/// 默认请求体体积上限（字节）。
const DEFAULT_BODY_LIMIT_BYTES: usize = 2 * 1024 * 1024;

/// 指定 `config.toml` 路径的环境变量。
pub const CONFIG_PATH_ENV: &str = "ACMECAST_CONFIG";

/// 全局出网代理的环境变量键名；覆盖 `config.toml` 的 `[proxy] url`。
///
/// 与凭据级 `proxy_mode=inherit`、DoH 解析器代理回退共用同一份取值。
/// 原 `ACMECAST_DOH_PROXY_URL`（仅 DoH 专用）已废弃合并到此处。
pub const PROXY_URL_ENV: &str = "ACMECAST_PROXY_URL";

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
    /// webhook 通知渠道。
    ///
    /// 来自 `[[notifications]]` 段并已通过合并校验；`enabled = false` 的
    /// 渠道保留在列表里但不投递（装配与测试端点各自过滤）。
    pub notifications: Vec<ChannelConfig>,
    /// DNS-01 传播等待策略；来自 `[propagation]` 段与环境变量的合并。
    pub propagation: PropagationConfig,
    /// `config.toml` 的 `[[resolvers]]` 条目；传播等待的阶段 2/3
    /// 用它扩展内置解析器集合。
    pub resolvers: Vec<ResolverEntry>,
    /// 全局出网代理 URL；来自 `[proxy] url` 与环境变量 `ACMECAST_PROXY_URL`
    /// 的合并（`builtin < config.toml < env`）。`None` 即直连——不给任何
    /// 渠道注入代理，也不隐式读取 `HTTPS_PROXY` 之类的标准变量。
    pub proxy_url: Option<String>,
    /// 流水线网络操作（SSH 连接/传输、S3/WebDAV 上传、腾讯云与 DNS API）的
    /// 超时时长；来自 `[network]` 段与环境变量 `ACMECAST_NETWORK_TIMEOUT_SECS`
    /// 的合并（`builtin < config.toml < env`）。
    pub network_timeout: std::time::Duration,
    /// Redis / Valkey 缓存连接串（可选）；缺省时禁用缓存。
    pub redis_url: Option<String>,
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
            notifications: Vec::new(),
            propagation: PropagationConfig {
                timeout_secs: 300,
                interval_secs: 5,
            },
            resolvers: Vec::new(),
            proxy_url: None,
            network_timeout: default_network_timeout(),
            redis_url: None,
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

/// `config.toml` 的完整结构：`[server]` 配置、`[[resolvers]]` 与
/// `[[notifications]]` 扩展。
///
/// 缺省段经 `serde` 默认值填充，因此空文件或仅含注释的文件等同于 `AppConfig::default()`。
/// 解析器条目的结构校验在反序列化阶段完成（缺字段或类型不符将报错），
/// 端点的语义校验（如 DoH 须 `https://`）与通知渠道的字段校验在合并时进行，
/// 非法条目跳过并告警。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct AppConfig {
    /// 服务端字段。
    #[serde(default)]
    pub server: ServerSection,
    /// DNS-01 传播等待策略；缺省字段经 serde 默认值填充。
    #[serde(default)]
    pub propagation: PropagationSection,
    /// 解析器扩展条目；与内置集合按 endpoint 去重。
    #[serde(default)]
    pub resolvers: Vec<ResolverEntry>,
    /// 全局出网代理；缺省段即直连。与凭据级 `proxy_mode=inherit`
    /// 及 DoH 解析器回退共用同一份配置。
    #[serde(default)]
    pub proxy: ProxySection,
    /// 通知渠道条目；字段合法性在合并阶段校验。
    #[serde(default)]
    pub notifications: Vec<NotificationEntry>,
    /// 流水线网络操作超时；缺省段即内置默认值。
    #[serde(default)]
    pub network: NetworkSection,
}

/// `config.toml` 中 `[network]` 段的字段：流水线网络操作超时。
///
/// 与 `[propagation]` 同构；约束 SSH 部署连接与传输、S3/WebDAV 上传、
/// 腾讯云 SSL 与 DNS 提供商的 API 调用。
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NetworkSection {
    /// 网络操作超时（秒）。须为正整数，`0` 在启动合并时被拒绝。
    #[serde(default = "default_network_timeout_secs")]
    pub timeout_secs: u64,
}

impl Default for NetworkSection {
    fn default() -> Self {
        Self {
            timeout_secs: default_network_timeout_secs(),
        }
    }
}

fn default_network_timeout_secs() -> u64 {
    30
}

fn default_network_timeout() -> std::time::Duration {
    std::time::Duration::from_secs(default_network_timeout_secs())
}

/// `config.toml` 中 `[proxy]` 段的字段：全局出网代理。
///
/// 独立成段而非塞进 `[server]`：代理语义与服务端基础配置（监听、数据目录）
/// 正交，且它的消费者跨 DNS 凭据与 DoH 解析器两处。
/// 取值口径与凭据级 `proxy_url` 完全一致（同一 [`acmecast_dns::validate_proxy_url`]）。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProxySection {
    /// 全局代理 URL：`socks5://` / `http://` / `https://` 且可解析。
    /// 留空或本段缺省即直连，不给任何渠道注入代理。
    #[serde(default)]
    pub url: Option<String>,
}

/// `config.toml` 中 `[propagation]` 段的字段：DNS-01 传播等待策略。
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PropagationSection {
    /// 传播等待总超时（秒）。商有 DNS（EdgeOne 等）从控制面到权威 NS 的
    /// 同步延迟可达数分钟，慢同步环境建议放宽到 600；配置过小（如 10）
    /// 会让挑战几乎必然超时——等待期记录始终存在且退出时必清理，
    /// 偏大没有残留风险，偏小只有失败风险。
    #[serde(default = "default_propagation_timeout_secs")]
    pub timeout_secs: u64,
    /// 每轮查询之间的间隔（秒）。
    #[serde(default = "default_propagation_interval_secs")]
    pub interval_secs: u64,
}

impl Default for PropagationSection {
    fn default() -> Self {
        Self {
            timeout_secs: default_propagation_timeout_secs(),
            interval_secs: default_propagation_interval_secs(),
        }
    }
}

fn default_propagation_timeout_secs() -> u64 {
    300
}

fn default_propagation_interval_secs() -> u64 {
    5
}

/// 运行时的 DNS-01 传播等待策略（由 `[propagation]` 段与环境变量合并得出）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PropagationConfig {
    /// 传播等待总超时（秒）。
    pub timeout_secs: u64,
    /// 每轮查询之间的间隔（秒）。
    pub interval_secs: u64,
}

/// `config.toml` 中一条 `[[notifications]]` 通知渠道声明。
///
/// 结构校验（缺字段/类型不符）在反序列化阶段报错；语义校验
/// （provider 是否内置、事件是否可识别、渠道类型专属字段）在合并阶段进行。
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NotificationEntry {
    /// 渠道名，用于日志与测试端点定位，须在渠道列表中唯一。
    pub name: String,
    /// 适配器类型标识：`feishu` / `dingtalk` / `generic` / `email`。
    pub provider: String,
    /// 机器人 webhook 地址。webhook 渠道（`feishu` / `dingtalk` / `generic`）
    /// 必填；`email` 渠道不支持该字段。
    #[serde(default)]
    pub url: Option<String>,
    /// 签名密钥；平台支持签名校验时填写。
    #[serde(default)]
    pub secret: Option<String>,
    /// 订阅的事件标识列表（`cert.apply` / `cert.deploy`）。
    #[serde(default)]
    pub events: Vec<String>,
    /// 是否启用；缺省 `true`。
    #[serde(default)]
    pub enabled: Option<bool>,
    /// 签名方式：`feishu` / `dingtalk`，缺省不签名。签名算法写死在代码
    /// 中（飞书进请求体、钉钉进地址查询参数），这里只指定方式标识。
    /// 仅 `generic` 渠道可用——专用适配器按 `secret` 是否配置自动签名。
    #[serde(default)]
    pub sign: Option<String>,
    /// 请求方法：`POST` / `PUT` / `PATCH`，缺省 `POST`。仅 `generic` 渠道可用。
    #[serde(default)]
    pub method: Option<String>,
    /// 额外静态请求头（如自建端点的 `X-API-Key`）。仅 `generic` 渠道可用。
    #[serde(default)]
    pub headers: Option<BTreeMap<String, String>>,
    /// 自定义请求体模板，占位符 `{{var}}` 填充事件素材。仅 `generic`
    /// 渠道可用；缺省按签名方式选平台 text 格式，无签名时为统一事件负载。
    #[serde(default)]
    pub body_template: Option<String>,
    /// SMTP 服务器地址。仅 `email` 渠道必填。
    #[serde(default)]
    pub host: Option<String>,
    /// SMTP 端口；缺省随 `tls` 模式（`implicit`→465、`starttls`→587、
    /// `none`→25）。仅 `email` 渠道可用。
    #[serde(default)]
    pub port: Option<u16>,
    /// 发件人地址。仅 `email` 渠道必填。
    #[serde(default)]
    pub from: Option<String>,
    /// 收件人地址列表，至少一项。仅 `email` 渠道可用。
    #[serde(default)]
    pub to: Option<Vec<String>>,
    /// SMTP 认证用户名；与 `password` 同时配置才启用认证。
    #[serde(default)]
    pub username: Option<String>,
    /// SMTP 认证密码。
    #[serde(default)]
    pub password: Option<String>,
    /// TLS 连接模式：`implicit`（隐式 TLS，缺省）/ `starttls` / `none`。
    /// 仅 `email` 渠道可用。
    #[serde(default)]
    pub tls: Option<String>,
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
    /// Redis / Valkey 缓存连接串。
    #[serde(default)]
    pub redis_url: Option<String>,
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
            redis_url: None,
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
    ///
    /// `ACMECAST_PROXY_URL` 非法时返回错误（拒绝启动），其余字段同前。
    pub fn from_env() -> acmecast_core::Result<Self> {
        let mut config = Self::default();
        apply_env(&mut config)?;
        Ok(config)
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
        apply_file(&mut config, &file)?;
        apply_env(&mut config)?;
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
///
/// `[proxy] url` 与 env `ACMECAST_PROXY_URL` 的非法值在此返回 [`acmecast_core::Error::Config`]，
/// 由调用方拒绝启动——代理值错在第一次出网时才暴露，报出来只会像「网络不通」。
fn apply_file(config: &mut ServerConfig, file: &AppConfig) -> acmecast_core::Result<()> {
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
    config.notifications = resolve_notifications(&file.notifications);
    config.propagation = PropagationConfig {
        timeout_secs: file.propagation.timeout_secs,
        interval_secs: file.propagation.interval_secs,
    };
    config.resolvers = file.resolvers.clone();
    // 网络操作超时：须为正整数，0 在启动时拒绝（与 env 同一口径）。
    if file.network.timeout_secs == 0 {
        return Err(acmecast_core::Error::Config(
            "[network] timeout_secs 不合法: 期望正整数（秒）".to_owned(),
        ));
    }
    config.network_timeout = std::time::Duration::from_secs(file.network.timeout_secs);
    // 全局代理：非空值才覆盖（留空即维持直连），并当场校验——文件值非法
    // 要让服务拒绝启动，而不是等第一次出网才以「代理连不上」的形式炸开。
    if let Some(url) = normalized_proxy_url(file.proxy.url.as_deref()) {
        acmecast_dns::validate_proxy_url(&url)
            .map_err(|e| acmecast_core::Error::Config(format!("[proxy] url 不合法: {e}")))?;
        config.proxy_url = Some(url);
    }
    if s.redis_url.is_some() {
        config.redis_url = s.redis_url.clone();
    }
    Ok(())
}

/// 归一化 `[proxy] url` / `ACMECAST_PROXY_URL` 的取值：去首尾空白，
/// 空串或纯空白归一为 `None`（视为直连，不做覆盖）。
fn normalized_proxy_url(raw: Option<&str>) -> Option<String> {
    raw.map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

/// 校验并转换 `[[notifications]]` 条目；非法条目跳过并告警。
///
/// 与 `[[resolvers]]` 的语义一致：单个渠道的配置错误不让服务拒绝启动，
/// 但必须让管理员在启动日志里看到它被跳过了。
fn resolve_notifications(entries: &[NotificationEntry]) -> Vec<ChannelConfig> {
    let registry = acmecast_notify::default_registry();
    let mut channels = Vec::new();
    let mut names = std::collections::HashSet::new();
    for entry in entries {
        match validate_notification(entry, &registry) {
            Ok(config) => {
                if !names.insert(config.name.clone()) {
                    tracing::warn!(
                        channel = %entry.name,
                        "通知渠道重名，后者已跳过（渠道名用于日志与测试端点定位，须唯一）"
                    );
                    continue;
                }
                channels.push(config);
            }
            Err(reason) => {
                tracing::warn!(channel = %entry.name, %reason, "通知渠道配置非法，已跳过");
            }
        }
    }
    channels
}

/// 校验单条通知渠道声明。
fn validate_notification(
    entry: &NotificationEntry,
    registry: &acmecast_notify::ChannelRegistry,
) -> Result<ChannelConfig, String> {
    if entry.name.trim().is_empty() {
        return Err("name 不能为空".to_owned());
    }
    if registry.get(&entry.provider).is_none() {
        return Err(format!(
            "未知 provider: {}（可用：{}）",
            entry.provider,
            registry.type_ids().join("、")
        ));
    }
    let is_email = entry.provider == "email";
    let url = if is_email {
        if entry.url.is_some() {
            return Err("provider = email 不支持 url 字段（webhook 地址是 webhook 渠道专属）".to_owned());
        }
        String::new()
    } else {
        match entry.url.as_deref().map(str::trim) {
            None => return Err("url 不能为空（webhook 渠道必填）".to_owned()),
            Some(url) if url.starts_with("https://") || url.starts_with("http://") => {
                url.to_owned()
            }
            Some(_) => return Err("url 必须为 http(s) 地址".to_owned()),
        }
    };
    if entry.events.is_empty() {
        return Err("events 不能为空（可订阅：cert.apply、cert.deploy）".to_owned());
    }
    for event in &entry.events {
        if acmecast_notify::event_title(event).is_none() {
            return Err(format!(
                "未知订阅事件: {event}（可用：{cert_apply}、{cert_deploy}）",
                cert_apply = acmecast_notify::EVENT_CERT_APPLY,
                cert_deploy = acmecast_notify::EVENT_CERT_DEPLOY,
            ));
        }
    }

    // 扩展字段（签名方式、方法、请求头、模板）只在统一请求方案（generic）里生效，
    // 专用适配器的请求形态写死，配置了反而造成「配了不生效」的困惑，直接拒绝。
    let extras = [
        ("sign", entry.sign.is_some()),
        ("method", entry.method.is_some()),
        ("headers", entry.headers.is_some()),
        ("body_template", entry.body_template.is_some()),
    ];
    if entry.provider != "generic"
        && let Some((field, _)) = extras.iter().find(|(_, present)| *present)
    {
        return Err(format!(
            "provider = {provider} 不支持 {field} 字段（扩展字段仅 generic 渠道可用）",
            provider = entry.provider
        ));
    }
    let sign = match &entry.sign {
        None => None,
        Some(id) => match SignKind::from_id(id.trim()) {
            Some(kind) => Some(kind),
            None => {
                return Err(format!(
                    "未知签名方式: {id}（可用：{}）",
                    acmecast_notify::SIGN_KINDS.join("、")
                ));
            }
        },
    };
    let method = match &entry.method {
        None => "POST".to_owned(),
        Some(value) => {
            let normalized = value.trim().to_ascii_uppercase();
            if !acmecast_notify::ALLOWED_METHODS.contains(&normalized.as_str()) {
                return Err(format!(
                    "method 仅支持 {}（收到 {value:?}）",
                    acmecast_notify::ALLOWED_METHODS.join(" / ")
                ));
            }
            normalized
        }
    };

    // SMTP 字段仅 email 渠道可用；email 渠道的必填项（host/from/to）与
    // 缺省值（端口随 tls 模式）由 SmtpSettings 统一校验，错误说明尽量
    // 与配置文件字段名对应，管理员照着改就行。
    let smtp = if is_email {
        let tls = match &entry.tls {
            None => acmecast_notify::TlsMode::Implicit,
            Some(id) => acmecast_notify::TlsMode::from_id(id).ok_or_else(|| {
                format!("未知 tls 模式: {id}（可用：implicit、starttls、none，缺省 implicit）")
            })?,
        };
        let settings = acmecast_notify::SmtpSettings {
            host: entry.host.clone().unwrap_or_default(),
            port: entry.port,
            from: entry.from.clone().unwrap_or_default(),
            to: entry.to.clone().unwrap_or_default(),
            username: entry.username.clone(),
            password: entry.password.clone(),
            tls,
        };
        settings.validate()?;
        Some(settings)
    } else {
        let smtp_fields = [
            ("host", entry.host.is_some()),
            ("port", entry.port.is_some()),
            ("from", entry.from.is_some()),
            ("to", entry.to.is_some()),
            ("username", entry.username.is_some()),
            ("password", entry.password.is_some()),
            ("tls", entry.tls.is_some()),
        ];
        if let Some((field, _)) = smtp_fields.iter().find(|(_, present)| *present) {
            return Err(format!(
                "provider = {provider} 不支持 {field} 字段（SMTP 字段仅 email 渠道可用）",
                provider = entry.provider
            ));
        }
        None
    };

    let config = ChannelConfig {
        name: entry.name.clone(),
        provider: entry.provider.clone(),
        url,
        secret: entry.secret.clone(),
        events: entry.events.clone(),
        // 缺省不启用：渠道配置写好默认是「待命」，显式 enabled = true 才
        // 开始对外投递——新增渠道不会在管理员确认前就往外发消息。
        enabled: entry.enabled.unwrap_or(false),
        sign,
        method,
        headers: entry.headers.clone().unwrap_or_default(),
        body_template: entry.body_template.clone(),
        smtp,
    };
    acmecast_notify::validate_channel(&config)?;
    Ok(config)
}

/// 把环境变量叠到运行配置上（覆盖文件值）。
///
/// `ACMECAST_PROXY_URL` 的非法值在此返回 [`acmecast_core::Error::Config`]（与
/// `[proxy] url` 同一口径），拒绝启动而非留到第一次出网才炸。
fn apply_env(config: &mut ServerConfig) -> acmecast_core::Result<()> {
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
    if let Ok(value) = std::env::var("ACMECAST_PROPAGATION_TIMEOUT_SECS")
        && let Ok(secs) = value.trim().parse::<u64>()
        && secs > 0
    {
        config.propagation.timeout_secs = secs;
    }
    if let Ok(value) = std::env::var("ACMECAST_PROPAGATION_INTERVAL_SECS")
        && let Ok(secs) = value.trim().parse::<u64>()
        && secs > 0
    {
        config.propagation.interval_secs = secs;
    }
    // 网络操作超时 env 覆盖文件值：已设置时必须是可解析的正整数，否则拒绝启动
    //（比 [propagation] 的静默忽略更严——写错的超时值等到第一次挂起才暴露代价太高）。
    if let Ok(value) = std::env::var("ACMECAST_NETWORK_TIMEOUT_SECS") {
        match value.trim().parse::<u64>() {
            Ok(secs) if secs > 0 => {
                config.network_timeout = std::time::Duration::from_secs(secs);
            }
            _ => {
                return Err(acmecast_core::Error::Config(
                    "ACMECAST_NETWORK_TIMEOUT_SECS 不合法: 期望正整数（秒）".to_owned(),
                ));
            }
        }
    }
    if let Some(redis_url) = resolve_redis_url_from_env() {
        config.redis_url = Some(redis_url);
    }
    // 全局代理 env 覆盖文件值：非空才覆盖，非法同样拒绝启动（与文件值同一口径）。
    // 刻意不读 `HTTPS_PROXY` / `ALL_PROXY` 等标准变量——那会波及所有未配代理的凭据。
    if let Some(url) = normalized_proxy_url(std::env::var(PROXY_URL_ENV).ok().as_deref()) {
        acmecast_dns::validate_proxy_url(&url)
            .map_err(|e| acmecast_core::Error::Config(format!("{PROXY_URL_ENV} 不合法: {e}")))?;
        config.proxy_url = Some(url);
    }
    Ok(())
}

/// 按优先级从环境变量解析 Redis / Valkey 缓存地址：
/// `ACMECAST_REDIS_URL` > `ACMECAST_VALKEY_URL` > `REDIS_URL`。
/// 遇到空串回退到后续别名，全部为空或未设则返回 `None`。
pub(crate) fn resolve_redis_url_from_env() -> Option<String> {
    resolve_redis_url_with(|key| std::env::var(key).ok())
}

pub(crate) fn resolve_redis_url_with<F>(lookup: F) -> Option<String>
where
    F: Fn(&str) -> Option<String>,
{
    for key in ["ACMECAST_REDIS_URL", "ACMECAST_VALKEY_URL", "REDIS_URL"] {
        if let Some(value) = lookup(key) {
            let trimmed = value.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.to_owned());
            }
        }
    }
    None
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

    out.push_str("# DNS-01 传播等待策略（可选）。\n");
    out.push_str("# timeout_secs：总超时。商有 DNS（EdgeOne 等）从控制面到权威 NS 的\n");
    out.push_str("#   同步延迟可达数分钟，慢同步环境建议放宽到 600；偏大无残留风险\n");
    out.push_str("#   （等待期记录始终存在、退出必清理），偏小只有失败风险。\n");
    out.push_str("# interval_secs：每轮查询之间的间隔。\n");
    out.push_str("# 环境变量 ACMECAST_PROPAGATION_TIMEOUT_SECS / ACMECAST_PROPAGATION_INTERVAL_SECS 可覆盖。\n");
    out.push_str("[propagation]\n");
    out.push_str("timeout_secs = 300\n");
    out.push_str("interval_secs = 5\n\n");

    out.push_str("# 流水线网络操作超时（可选）：约束 SSH 部署连接与传输、S3/WebDAV\n");
    out.push_str("# 上传、腾讯云 SSL 与 DNS 提供商的 API 调用——每次连接/请求在此时长内\n");
    out.push_str("# 完成或以超时失败，避免远端无响应时步骤永久挂起。须为正整数。\n");
    out.push_str("# 环境变量 ACMECAST_NETWORK_TIMEOUT_SECS 可覆盖。\n");
    out.push_str("[network]\n");
    out.push_str("timeout_secs = 30\n\n");

    out.push_str("# 全局出网代理（可选）。\n");
    out.push_str("# 支持 socks5:// / http:// / https://，取值须可解析；留空或本段缺省即直连，\n");
    out.push_str("# 不给任何渠道注入代理。本配置供两处共用：凭据页 proxy_mode=inherit 的凭据\n");
    out.push_str("# 继承，以及传播 DoH 解析器直连失败后的代理自动回退。\n");
    out.push_str("# 环境变量 ACMECAST_PROXY_URL 可覆盖本文件取值（builtin < config.toml < env）。\n");
    out.push_str("[proxy]\n");
    out.push_str("# url = \"socks5://127.0.0.1:7890\"\n\n");

    out.push_str("# DNS-01 传播检测解析器扩展（可选）。\n");
    out.push_str("# type: doh / dot / dns；dot/dns 目前仅校验，传输层尚未实现。\n");
    out.push_str("# 与内置集合按 endpoint 去重；ACMECAST_DOH_RESOLVERS=none 可跳过传播等待。\n");
    out.push_str("# 腾讯云 DNSPod 在国内可达性优先，推荐与阿里/Cloudflare 组合使用。\n");
    out.push_str("[[resolvers]]\n");
    out.push_str("type = \"doh\"\n");
    out.push_str("endpoint = \"https://dns.alidns.com/dns-query\"\n");
    out.push_str("# name = \"阿里 DNS\"\n\n");
    out.push_str("[[resolvers]]\n");
    out.push_str("type = \"doh\"\n");
    out.push_str("endpoint = \"https://doh.pub/dns-query\"\n");
    out.push_str("# name = \"腾讯 DNSPod\"\n\n");
    out.push_str("[[resolvers]]\n");
    out.push_str("type = \"doh\"\n");
    out.push_str("endpoint = \"https://1.12.12.12/dns-query\"\n");
    out.push_str("# name = \"腾讯 DNSPod（IP 直连）\"\n\n");
    out.push_str("[[resolvers]]\n");
    out.push_str("type = \"doh\"\n");
    out.push_str("endpoint = \"https://cloudflare-dns.com/dns-query\"\n");
    out.push_str("# name = \"Cloudflare DNS\"\n\n");

    out.push_str("# 通知渠道（可选）：证书申请/部署成功后推送到 IM 群机器人或邮箱。\n");
    out.push_str("# provider: feishu（飞书）/ dingtalk（钉钉）/ generic（统一请求方案）/\n");
    out.push_str("#          email（SMTP 邮件）。\n");
    out.push_str("# generic 可配 sign（feishu/dingtalk 签名方式）、method、headers、\n");
    out.push_str("# body_template（占位符如 {{title}}、{{domains}}）。\n");
    out.push_str("# events 可订阅: cert.apply（申请成功）、cert.deploy（部署成功）。\n");
    out.push_str("# 渠道缺省不启用（enabled 缺省 false）：确认配置无误后显式写\n");
    out.push_str("# enabled = true 开始投递；修改后需重启生效。\n");
    out.push_str("# [[notifications]]\n");
    out.push_str("# name = \"ops-feishu\"\n");
    out.push_str("# provider = \"feishu\"\n");
    out.push_str("# url = \"https://open.feishu.cn/open-apis/bot/v2/hook/xxxxxxxx\"\n");
    out.push_str("# secret = \"机器人启用签名校验时填写（可选）\"\n");
    out.push_str("# events = [\"cert.apply\", \"cert.deploy\"]\n");
    out.push_str("# enabled = true                      # 缺省 false，须显式开启\n\n");
    out.push_str("# 自定义 webhook 示例：指定签名方式与请求体模板。\n");
    out.push_str("# [[notifications]]\n");
    out.push_str("# name = \"ops-custom\"\n");
    out.push_str("# provider = \"generic\"\n");
    out.push_str("# url = \"https://ops.example.com/api/events\"\n");
    out.push_str("# secret = \"sk-xxx\"\n");
    out.push_str("# sign = \"feishu\"                  # 或 dingtalk；缺省不签名\n");
    out.push_str("# method = \"POST\"                  # POST / PUT / PATCH\n");
    out.push_str("# headers = { \"X-API-Key\" = \"sk-xxx\" }\n");
    out.push_str("# body_template = '{\"text\": \"{{title}}｜{{pipeline}}｜{{domains}}\"}'\n");
    out.push_str("# enabled = true                      # 缺省 false，须显式开启\n");
    out.push_str("# events = [\"cert.apply\", \"cert.deploy\"]\n");
    out.push_str("# 邮件通知示例：事件以纯文本邮件送达每个收件人（主题含事件标题）。\n");
    out.push_str("# [[notifications]]\n");
    out.push_str("# name = \"ops-email\"\n");
    out.push_str("# provider = \"email\"\n");
    out.push_str("# host = \"smtp.exmail.qq.com\"      # SMTP 服务器，不含端口\n");
    out.push_str("# port = 465                        # 可选；缺省随 tls：implicit→465 / starttls→587 / none→25\n");
    out.push_str("# tls = \"implicit\"                 # implicit（缺省）/ starttls / none\n");
    out.push_str("# from = \"no-reply@example.com\"\n");
    out.push_str("# to = [\"ops@example.com\"]         # 收件人列表，可为多个\n");
    out.push_str("# username = \"no-reply@example.com\"  # 与 password 同时配置才启用认证\n");
    out.push_str("# password = \"xxx\"\n");
    out.push_str("# enabled = true                      # 缺省 false，须显式开启\n");
    out.push_str("# events = [\"cert.apply\", \"cert.deploy\"]\n");
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
            "ACMECAST_PROPAGATION_TIMEOUT_SECS",
            "ACMECAST_PROPAGATION_INTERVAL_SECS",
            "ACMECAST_PROXY_URL",
        ] {
            // SAFETY: 唯一调用者为标注 `#[serial]` 的测试用例。
            unsafe { std::env::remove_var(k) };
        }
    }

    #[test]
    #[serial]
    fn from_env_uses_defaults_when_no_env() {
        clear_server_env();
        let config = ServerConfig::from_env().expect("env 应可应用");
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
    fn propagation_file_overrides_builtin_env_overrides_file() {
        clear_server_env();
        let dir = unique_dir("propagation");
        let path = dir.join("config.toml");
        std::fs::write(
            &path,
            "[propagation]\ntimeout_secs = 600\ninterval_secs = 10\n",
        )
        .unwrap();

        unsafe { std::env::set_var("ACMECAST_CONFIG", &path) };
        let config = ServerConfig::from_env_and_file().unwrap();
        // 文件胜过内置
        assert_eq!(config.propagation.timeout_secs, 600);
        assert_eq!(config.propagation.interval_secs, 10);

        // env 胜过文件
        unsafe { std::env::set_var("ACMECAST_PROPAGATION_TIMEOUT_SECS", "900") };
        let config = ServerConfig::from_env_and_file().unwrap();
        assert_eq!(config.propagation.timeout_secs, 900);
        assert_eq!(config.propagation.interval_secs, 10);

        unsafe { std::env::remove_var("ACMECAST_CONFIG") };
        unsafe { std::env::remove_var("ACMECAST_PROPAGATION_TIMEOUT_SECS") };
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn propagation_defaults_are_documented_values() {
        let config = ServerConfig::default();
        assert_eq!(config.propagation.timeout_secs, 300);
        assert_eq!(config.propagation.interval_secs, 5);
    }

    // ---- 网络操作超时（[network] 段 / ACMECAST_NETWORK_TIMEOUT_SECS）----

    /// 合并优先级 `builtin < config.toml < env`：文件覆盖内置 30 秒，
    /// env 再覆盖文件值。
    #[test]
    #[serial]
    fn network_file_overrides_builtin_env_overrides_file() {
        clear_server_env();
        let dir = unique_dir("network");
        let path = dir.join("config.toml");
        std::fs::write(&path, "[network]\ntimeout_secs = 60\n").unwrap();

        unsafe { std::env::set_var("ACMECAST_CONFIG", &path) };
        let config = ServerConfig::from_env_and_file().unwrap();
        // 文件胜过内置
        assert_eq!(config.network_timeout, std::time::Duration::from_secs(60));

        // env 胜过文件
        unsafe { std::env::set_var("ACMECAST_NETWORK_TIMEOUT_SECS", "15") };
        let config = ServerConfig::from_env_and_file().unwrap();
        assert_eq!(config.network_timeout, std::time::Duration::from_secs(15));

        unsafe { std::env::remove_var("ACMECAST_NETWORK_TIMEOUT_SECS") };
        unsafe { std::env::remove_var("ACMECAST_CONFIG") };
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 未配置时默认 30 秒。
    #[test]
    fn network_defaults_are_documented_values() {
        let config = ServerConfig::default();
        assert_eq!(config.network_timeout, std::time::Duration::from_secs(30));
    }

    /// 非法值拒绝启动：文件 `timeout_secs = 0`、env 非正整数或非数字都报错。
    #[test]
    #[serial]
    fn invalid_network_timeout_is_rejected_at_startup() {
        clear_server_env();
        let dir = unique_dir("network-invalid");
        let path = dir.join("config.toml");
        unsafe { std::env::set_var("ACMECAST_CONFIG", &path) };

        // 文件 0 值拒绝启动。
        std::fs::write(&path, "[network]\ntimeout_secs = 0\n").unwrap();
        let err = ServerConfig::from_env_and_file().expect_err("文件 0 值应拒绝启动");
        assert!(err.to_string().contains("正整数"), "{err}");

        // env 非法值同样拒绝启动（文件合法，排除文件先报错的干扰）。
        std::fs::write(&path, "[network]\ntimeout_secs = 60\n").unwrap();
        for value in ["-5", "0", "abc"] {
            unsafe { std::env::set_var("ACMECAST_NETWORK_TIMEOUT_SECS", value) };
            let err = ServerConfig::from_env_and_file()
                .expect_err("env 非法值应拒绝启动");
            assert!(
                err.to_string().contains("ACMECAST_NETWORK_TIMEOUT_SECS"),
                "{err}"
            );
        }

        unsafe { std::env::remove_var("ACMECAST_NETWORK_TIMEOUT_SECS") };
        unsafe { std::env::remove_var("ACMECAST_CONFIG") };
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 自动生成的模板包含带注释的 `[network]` 段与默认值。
    #[test]
    fn template_contains_network_section() {
        let content = template_content();
        assert!(content.contains("[network]"), "{content}");
        assert!(content.contains("timeout_secs = 30"), "{content}");
        assert!(
            content.contains("ACMECAST_NETWORK_TIMEOUT_SECS"),
            "{content}"
        );
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
    fn notifications_section_parses_and_resolves() {
        let dir = unique_dir("notif");
        let path = dir.join("config.toml");
        std::fs::write(
            &path,
            "[[notifications]]\nname = \"ops\"\nprovider = \"feishu\"\nurl = \"https://open.feishu.cn/hook/x\"\nevents = [\"cert.apply\", \"cert.deploy\"]\n",
        )
        .unwrap();

        let file = AppConfig::load_from_file(&path).unwrap();
        assert_eq!(file.notifications.len(), 1);
        assert_eq!(file.notifications[0].name, "ops");

        let config = ServerConfig::default();
        let mut merged = config;
        merged.notifications = resolve_notifications(&file.notifications);
        assert_eq!(merged.notifications.len(), 1);
        assert_eq!(merged.notifications[0].provider, "feishu");
        assert!(!merged.notifications[0].enabled, "enabled 缺省应为 false");
        assert!(merged.notifications[0].secret.is_none());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn notification_extras_parse_and_validate() {
        let dir = unique_dir("notif-extra");
        let path = dir.join("config.toml");
        std::fs::write(
            &path,
            concat!(
                "[[notifications]]\n",
                "name = \"custom\"\n",
                "provider = \"generic\"\n",
                "url = \"https://ops.example.com/api/events\"\n",
                "secret = \"sk-xxx\"\n",
                "sign = \"feishu\"\n",
                "method = \"put\"\n",
                "headers = { \"X-API-Key\" = \"sk-123\" }\n",
                "body_template = \"{\\\"text\\\": \\\"{{title}}\\\"}\"\n",
                "events = [\"cert.apply\"]\n",
            ),
        )
        .unwrap();

        let file = AppConfig::load_from_file(&path).unwrap();
        assert_eq!(file.notifications.len(), 1);

        let channels = resolve_notifications(&file.notifications);
        assert_eq!(channels.len(), 1, "合法的扩展字段应通过: {channels:?}");
        let channel = &channels[0];
        assert_eq!(channel.sign, Some(SignKind::Feishu));
        assert_eq!(channel.method, "PUT", "method 应归一化为大写");
        assert_eq!(
            channel.headers.get("X-API-Key").map(String::as_str),
            Some("sk-123")
        );
        assert_eq!(
            channel.body_template.as_deref(),
            Some("{\"text\": \"{{title}}\"}")
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn invalid_notification_extras_are_skipped() {
        let base = |name: &str, extra: &str| {
            format!(
                "name = \"{name}\"\nprovider = \"generic\"\nurl = \"https://ops.example.com/hook\"\nsecret = \"sk\"\nevents = [\"cert.apply\"]\n{extra}"
            )
        };
        let entries: Vec<NotificationEntry> = [
            base("bad-sign", "sign = \"hmac\"\n"),
            base("bad-method", "method = \"GET\"\n"),
            base("bad-placeholder", "body_template = \"{{nope}}\"\n"),
            base(
                "feishu-sign-plain-body",
                "sign = \"feishu\"\nbody_template = \"plain {{title}}\"\n",
            ),
        ]
        .into_iter()
        .map(|toml| toml::from_str(&toml).expect("测试条目应为合法 TOML"))
        .collect();
        // 配了签名方式却没给 secret 的条目单独构造。
        let sign_without_secret: NotificationEntry = toml::from_str(concat!(
            "name = \"sign-without-secret\"\n",
            "provider = \"generic\"\n",
            "url = \"https://ops.example.com/hook\"\n",
            "events = [\"cert.apply\"]\n",
            "sign = \"feishu\"\n",
        ))
        .unwrap();
        let entries = [entries, vec![sign_without_secret]].concat();
        let channels = resolve_notifications(&entries);
        assert!(channels.is_empty(), "非法扩展字段应全部跳过: {channels:?}");

        // 扩展字段仅 generic 渠道可用。
        let toml = concat!(
            "name = \"misuse\"\n",
            "provider = \"dingtalk\"\n",
            "url = \"https://oapi.dingtalk.com/robot/send?x=1\"\n",
            "events = [\"cert.apply\"]\n",
            "body_template = \"{}\"\n",
        );
        let entry: NotificationEntry = toml::from_str(toml).unwrap();
        let channels = resolve_notifications(&[entry]);
        assert!(
            channels.is_empty(),
            "专用适配器配扩展字段应跳过: {channels:?}"
        );
    }

    /// 构造一条可变的 email 渠道声明：缺省字段齐全，用例按需覆盖。
    fn email_entry(mutate: impl FnOnce(&mut NotificationEntry)) -> NotificationEntry {
        let mut entry = NotificationEntry {
            name: "ops-email".to_owned(),
            provider: "email".to_owned(),
            url: None,
            secret: None,
            events: vec!["cert.apply".to_owned()],
            enabled: None,
            sign: None,
            method: None,
            headers: None,
            body_template: None,
            host: Some("smtp.example.com".to_owned()),
            port: None,
            from: Some("no-reply@example.com".to_owned()),
            to: Some(vec!["ops@example.com".to_owned()]),
            username: None,
            password: None,
            tls: None,
        };
        mutate(&mut entry);
        entry
    }

    #[test]
    fn email_channel_parses_and_resolves() {
        let dir = unique_dir("notif-email");
        let path = dir.join("config.toml");
        std::fs::write(
            &path,
            concat!(
                "[[notifications]]\n",
                "name = \"ops-email\"\n",
                "provider = \"email\"\n",
                "host = \"smtp.exmail.qq.com\"\n",
                "from = \"no-reply@example.com\"\n",
                "to = [\"ops@example.com\", \"b@example.com\"]\n",
                "username = \"no-reply@example.com\"\n",
                "password = \"secret\"\n",
                "events = [\"cert.apply\", \"cert.deploy\"]\n",
            ),
        )
        .unwrap();
        let file = AppConfig::load_from_file(&path).unwrap();
        assert_eq!(file.notifications.len(), 1);

        let channels = resolve_notifications(&file.notifications);
        assert_eq!(channels.len(), 1);
        let channel = &channels[0];
        assert_eq!(channel.provider, "email");
        assert!(channel.url.is_empty(), "email 渠道不携带 webhook 地址");
        assert!(!channel.enabled, "enabled 缺省应为 false");
        let smtp = channel.smtp.as_ref().expect("email 渠道应携带 SMTP 设置");
        assert_eq!(smtp.host, "smtp.exmail.qq.com");
        assert_eq!(smtp.tls, acmecast_notify::TlsMode::Implicit, "tls 缺省 implicit");
        assert_eq!(smtp.effective_port(), 465, "端口缺省随 tls 模式");
        assert_eq!(smtp.to.len(), 2, "收件人列表应完整保留");
        assert!(smtp.has_credentials(), "username + password 应启用认证");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn email_channel_tls_mode_drives_default_port() {
        for (tls, port) in [
            (acmecast_notify::TlsMode::Implicit, 465),
            (acmecast_notify::TlsMode::StartTls, 587),
            (acmecast_notify::TlsMode::None, 25),
        ] {
            let entry = email_entry(|e| {
                e.tls = Some(match tls {
                    acmecast_notify::TlsMode::Implicit => "implicit",
                    acmecast_notify::TlsMode::StartTls => "starttls",
                    acmecast_notify::TlsMode::None => "none",
                }
                .to_owned());
            });
            let channel = validate_notification(&entry, &acmecast_notify::default_registry())
                .expect("合法 email 条目应通过");
            let smtp = channel.smtp.as_ref().expect("email 渠道应携带 SMTP 设置");
            assert_eq!(smtp.tls, tls);
            assert_eq!(smtp.effective_port(), port, "tls={tls:?} 缺省端口");
        }
    }

    #[test]
    fn invalid_email_entries_are_skipped() {
        let entries = vec![
            // 缺 host（email 渠道必填）。
            email_entry(|e| e.host = None),
            // 缺 from。
            email_entry(|e| e.from = None),
            // 收件人列表为空。
            email_entry(|e| e.to = Some(vec![])),
            // 收件人地址缺 @。
            email_entry(|e| e.to = Some(vec!["not-an-email".to_owned()])),
            // 未知 tls 模式。
            email_entry(|e| e.tls = Some("ssl".to_owned())),
            // 认证凭据只配了一半。
            email_entry(|e| e.username = Some("no-reply@example.com".to_owned())),
            // email 渠道不支持 webhook 地址。
            email_entry(|e| e.url = Some("https://hooks.example.com/x".to_owned())),
            // email 渠道不支持 generic 扩展字段。
            email_entry(|e| e.body_template = Some("{{title}}".to_owned())),
        ];
        let channels = resolve_notifications(&entries);
        assert!(channels.is_empty(), "非法 email 条目应全部跳过: {channels:?}");
    }

    #[test]
    fn webhook_channel_rejects_smtp_fields() {
        let entries = vec![NotificationEntry {
            name: "feishu-with-host".to_owned(),
            provider: "feishu".to_owned(),
            url: Some("https://open.feishu.cn/hook/x".to_owned()),
            secret: None,
            events: vec!["cert.apply".to_owned()],
            enabled: None,
            sign: None,
            method: None,
            headers: None,
            body_template: None,
            host: Some("smtp.example.com".to_owned()),
            port: None,
            from: None,
            to: None,
            username: None,
            password: None,
            tls: None,
        }];
        let channels = resolve_notifications(&entries);
        assert!(channels.is_empty(), "webhook 渠道配 SMTP 字段应跳过: {channels:?}");
    }

    #[test]
    fn unknown_provider_hint_lists_email() {
        let entry = NotificationEntry {
            name: "x".to_owned(),
            provider: "mail".to_owned(),
            url: Some("https://hooks.example.com/x".to_owned()),
            secret: None,
            events: vec!["cert.apply".to_owned()],
            enabled: None,
            sign: None,
            method: None,
            headers: None,
            body_template: None,
            host: None,
            port: None,
            from: None,
            to: None,
            username: None,
            password: None,
            tls: None,
        };
        let error = validate_notification(&entry, &acmecast_notify::default_registry()).unwrap_err();
        assert!(error.contains("email"), "未知 provider 提示应列出 email: {error}");
    }

    #[test]
    fn invalid_notification_entries_are_skipped() {
        // 三类非法条目：缺 url 形态、未知 provider、未知事件——各自跳过并保留有效条目。
        let entries = vec![
            NotificationEntry {
                name: "bad-url".to_owned(),
                provider: "feishu".to_owned(),
                url: Some("not-a-url".to_owned()),
                secret: None,
                events: vec!["cert.apply".to_owned()],
                enabled: None,
                sign: None,
                method: None,
                headers: None,
                body_template: None,
                host: None,
                port: None,
                from: None,
                to: None,
                username: None,
                password: None,
                tls: None,
            },
            NotificationEntry {
                name: "bad-provider".to_owned(),
                provider: "slack".to_owned(),
                url: Some("https://hooks.example.com/x".to_owned()),
                secret: None,
                events: vec!["cert.apply".to_owned()],
                enabled: None,
                sign: None,
                method: None,
                headers: None,
                body_template: None,
                host: None,
                port: None,
                from: None,
                to: None,
                username: None,
                password: None,
                tls: None,
            },
            NotificationEntry {
                name: "bad-event".to_owned(),
                provider: "dingtalk".to_owned(),
                url: Some("https://oapi.dingtalk.com/robot/send".to_owned()),
                secret: None,
                events: vec!["pipeline.failed".to_owned()],
                enabled: None,
                sign: None,
                method: None,
                headers: None,
                body_template: None,
                host: None,
                port: None,
                from: None,
                to: None,
                username: None,
                password: None,
                tls: None,
            },
            NotificationEntry {
                name: "no-events".to_owned(),
                provider: "generic".to_owned(),
                url: Some("https://example.com/hook".to_owned()),
                secret: None,
                events: vec![],
                enabled: None,
                sign: None,
                method: None,
                headers: None,
                body_template: None,
                host: None,
                port: None,
                from: None,
                to: None,
                username: None,
                password: None,
                tls: None,
            },
            NotificationEntry {
                name: "good".to_owned(),
                provider: "dingtalk".to_owned(),
                url: Some("https://oapi.dingtalk.com/robot/send?access_token=x".to_owned()),
                secret: Some("SECxxx".to_owned()),
                events: vec!["cert.deploy".to_owned()],
                enabled: Some(false),
                sign: None,
                method: None,
                headers: None,
                body_template: None,
                host: None,
                port: None,
                from: None,
                to: None,
                username: None,
                password: None,
                tls: None,
            },
        ];
        let channels = resolve_notifications(&entries);
        assert_eq!(channels.len(), 1, "只应保留合法条目: {channels:?}");
        assert_eq!(channels[0].name, "good");
        // enabled = false 的合法渠道保留在配置里（由装配与测试端点各自过滤）。
        assert!(!channels[0].enabled);
    }

    #[test]
    fn duplicate_notification_names_are_skipped() {
        let entries = vec![
            NotificationEntry {
                name: "same".to_owned(),
                provider: "feishu".to_owned(),
                url: Some("https://a.example.com/hook".to_owned()),
                secret: None,
                events: vec!["cert.apply".to_owned()],
                enabled: None,
                sign: None,
                method: None,
                headers: None,
                body_template: None,
                host: None,
                port: None,
                from: None,
                to: None,
                username: None,
                password: None,
                tls: None,
            },
            NotificationEntry {
                name: "same".to_owned(),
                provider: "generic".to_owned(),
                url: Some("https://b.example.com/hook".to_owned()),
                secret: None,
                events: vec!["cert.apply".to_owned()],
                enabled: None,
                sign: None,
                method: None,
                headers: None,
                body_template: None,
                host: None,
                port: None,
                from: None,
                to: None,
                username: None,
                password: None,
                tls: None,
            },
        ];
        let channels = resolve_notifications(&entries);
        assert_eq!(channels.len(), 1, "重名渠道只保留先到的: {channels:?}");
    }

    #[test]
    fn unknown_notification_field_rejected_at_parse() {
        let dir = unique_dir("notif-unknown");
        let path = dir.join("config.toml");
        std::fs::write(
            &path,
            "[[notifications]]\nname = \"x\"\nprovider = \"feishu\"\nwebhook = \"https://a.example.com\"\nevents = [\"cert.apply\"]\n",
        )
        .unwrap();
        let error = AppConfig::load_from_file(&path).unwrap_err();
        assert!(error.to_string().contains("webhook"), "{error}");
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
        // 模板不含真实敏感凭据：secret / password 之类字段只允许出现在
        // 注释示例行里（email 渠道的注释示例用占位值）。
        for line in content.lines() {
            let trimmed = line.trim_start();
            if trimmed.starts_with('#') {
                continue;
            }
            assert!(
                !trimmed.contains("secret") && !trimmed.contains("password"),
                "未注释行不应出现敏感字段: {line}"
            );
        }
        assert!(!content.contains("token"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn template_contains_propagation_section() {
        let content = template_content();
        assert!(content.contains("[propagation]"), "{content}");
        assert!(content.contains("timeout_secs = 300"), "{content}");
        assert!(content.contains("interval_secs = 5"), "{content}");
    }

    // ---- 全局代理（[proxy] 段 / ACMECAST_PROXY_URL）----

    /// `[proxy]` 段反序列化：合法 url 成功、缺段为 `None`、未知键被拒。
    #[test]
    fn proxy_section_deserialization() {
        let file: AppConfig = toml::from_str("[proxy]\nurl = \"socks5://127.0.0.1:7890\"\n").unwrap();
        assert_eq!(file.proxy.url.as_deref(), Some("socks5://127.0.0.1:7890"));

        let file: AppConfig = toml::from_str("[server]\nlisten_addr = \"127.0.0.1:8080\"\n").unwrap();
        assert_eq!(file.proxy.url, None, "缺段应视为未配置");

        let err = toml::from_str::<AppConfig>("[proxy]\nurll = \"socks5://127.0.0.1:7890\"\n")
            .expect_err("deny_unknown_fields 应拒绝未知键");
        assert!(
            err.to_string().contains("urll"),
            "报错应指出未知键: {err}"
        );
    }

    /// 合并优先级 `builtin < config.toml < env`：内置直连，文件可设，
    /// env 非空才覆盖文件。
    #[test]
    #[serial]
    fn proxy_merge_priority_is_builtin_file_then_env() {
        clear_server_env();
        let dir = unique_dir("proxy-merge");
        let path = dir.join("config.toml");

        // 1. 内置：无文件、无 env → 直连。
        assert_eq!(ServerConfig::default().proxy_url, None);

        // 2. 文件胜过内置。
        std::fs::write(&path, "[proxy]\nurl = \"socks5://127.0.0.1:7890\"\n").unwrap();
        let config = ServerConfig::from_env_and_file_at(Some(&path)).unwrap();
        assert_eq!(config.proxy_url.as_deref(), Some("socks5://127.0.0.1:7890"));

        // 3. env 胜过文件。
        unsafe { std::env::set_var(PROXY_URL_ENV, "http://proxy.local:8080") };
        let config = ServerConfig::from_env_and_file_at(Some(&path)).unwrap();
        assert_eq!(config.proxy_url.as_deref(), Some("http://proxy.local:8080"));

        // 4. env 留空不覆盖文件值。
        unsafe { std::env::set_var(PROXY_URL_ENV, "   ") };
        let config = ServerConfig::from_env_and_file_at(Some(&path)).unwrap();
        assert_eq!(config.proxy_url.as_deref(), Some("socks5://127.0.0.1:7890"));

        // 5. 文件 url 留空 → 维持直连。
        unsafe { std::env::remove_var(PROXY_URL_ENV) };
        std::fs::write(&path, "[proxy]\nurl = \"\"\n").unwrap();
        let config = ServerConfig::from_env_and_file_at(Some(&path)).unwrap();
        assert_eq!(config.proxy_url, None, "留空应维持直连");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 非法取值（文件与 env 两条路径）都在启动时拒绝，错误提示支持的 scheme。
    #[test]
    #[serial]
    fn invalid_proxy_url_is_rejected_at_startup() {
        clear_server_env();
        let dir = unique_dir("proxy-invalid");
        let path = dir.join("config.toml");

        for bad in ["ftp://127.0.0.1:21", "not-a-url"] {
            std::fs::write(&path, format!("[proxy]\nurl = \"{bad}\"\n")).unwrap();
            let err = ServerConfig::from_env_and_file_at(Some(&path))
                .expect_err("文件非法代理值应拒绝启动");
            assert!(
                err.to_string().contains("socks5"),
                "应提示支持的 scheme: {err}"
            );

            std::fs::write(&path, "[proxy]\nurl = \"socks5://127.0.0.1:7890\"\n").unwrap();
            unsafe { std::env::set_var(PROXY_URL_ENV, bad) };
            let err = ServerConfig::from_env_and_file_at(Some(&path))
                .expect_err("env 非法代理值应拒绝启动");
            assert!(
                err.to_string().contains("socks5"),
                "应提示支持的 scheme: {err}"
            );
            unsafe { std::env::remove_var(PROXY_URL_ENV) };
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 生成模板含 `[proxy]` 段（注释形态：不预置一个会让所有渠道走代理的值），
    /// 且模板可被解析器读回。
    #[test]
    fn template_contains_proxy_section() {
        let content = template_content();
        assert!(content.contains("[proxy]"), "{content}");
        assert!(
            content.contains("# url = \"socks5://127.0.0.1:7890\""),
            "示例 url 应整段注释: {content}"
        );
        let dir = unique_dir("tpl-proxy");
        let path = dir.join("config.toml");
        std::fs::write(&path, &content).unwrap();
        let file = AppConfig::load_from_file(&path).unwrap();
        assert_eq!(file.proxy.url, None, "注释示例不应让模板自带代理");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn template_contains_notifications_example() {
        let content = template_content();
        assert!(content.contains("[[notifications]]"), "{content}");
        // 示例必须整段注释：生成的模板不应自带一个会告警/投递的渠道。
        for line in content.lines() {
            if line.contains("notifications") || line.contains("feishu") {
                assert!(
                    line.trim_start().starts_with('#') || line.starts_with("# "),
                    "通知示例行应注释: {line}"
                );
            }
        }
        // 模板可被解析器读取且不产生渠道（含 deny_unknown_fields 字段形态正确）。
        let dir = unique_dir("tpl-parse");
        let path = dir.join("config.toml");
        std::fs::write(&path, &content).unwrap();
        let file = AppConfig::load_from_file(&path).unwrap();
        assert!(file.notifications.is_empty());
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

    #[test]
    fn server_config_default_redis_url_is_none() {
        let config = ServerConfig::default();
        assert_eq!(config.redis_url, None);
    }

    #[test]
    fn redis_url_alias_priority_and_empty_fallback() {
        // 1. 全部设置时，ACMECAST_REDIS_URL 优先级最高
        let url = resolve_redis_url_with(|key| match key {
            "ACMECAST_REDIS_URL" => Some("redis://redis:6379".to_owned()),
            "ACMECAST_VALKEY_URL" => Some("redis://valkey:6379".to_owned()),
            "REDIS_URL" => Some("redis://standard:6379".to_owned()),
            _ => None,
        });
        assert_eq!(url.as_deref(), Some("redis://redis:6379"));

        // 2. ACMECAST_REDIS_URL 为空串时，回退到 ACMECAST_VALKEY_URL
        let url = resolve_redis_url_with(|key| match key {
            "ACMECAST_REDIS_URL" => Some("   ".to_owned()),
            "ACMECAST_VALKEY_URL" => Some("redis://valkey:6379".to_owned()),
            "REDIS_URL" => Some("redis://standard:6379".to_owned()),
            _ => None,
        });
        assert_eq!(url.as_deref(), Some("redis://valkey:6379"));

        // 3. 前两者均为空串时，回退到 REDIS_URL
        let url = resolve_redis_url_with(|key| match key {
            "ACMECAST_REDIS_URL" => Some("".to_owned()),
            "ACMECAST_VALKEY_URL" => Some(" ".to_owned()),
            "REDIS_URL" => Some("redis://standard:6379".to_owned()),
            _ => None,
        });
        assert_eq!(url.as_deref(), Some("redis://standard:6379"));

        // 4. 全部为空白字符或未设时，回退为 None
        let url = resolve_redis_url_with(|key| match key {
            "ACMECAST_REDIS_URL" => Some("  ".to_owned()),
            "ACMECAST_VALKEY_URL" => None,
            "REDIS_URL" => Some("".to_owned()),
            _ => None,
        });
        assert_eq!(url, None);

        let url = resolve_redis_url_with(|_| None);
        assert_eq!(url, None);
    }

    #[test]
    #[serial]
    fn env_redis_url_integration() {
        unsafe {
            std::env::remove_var("ACMECAST_REDIS_URL");
            std::env::remove_var("ACMECAST_VALKEY_URL");
            std::env::remove_var("REDIS_URL");
        }

        let mut config = ServerConfig::default();
        apply_env(&mut config).expect("env 应可应用");
        assert_eq!(config.redis_url, None);

        unsafe {
            std::env::set_var("ACMECAST_VALKEY_URL", "redis://valkey.test:6379");
        }
        let mut config = ServerConfig::default();
        apply_env(&mut config).expect("env 应可应用");
        assert_eq!(config.redis_url.as_deref(), Some("redis://valkey.test:6379"));

        unsafe {
            std::env::set_var("ACMECAST_REDIS_URL", "redis://redis.test:6379");
        }
        let mut config = ServerConfig::default();
        apply_env(&mut config).expect("env 应可应用");
        assert_eq!(config.redis_url.as_deref(), Some("redis://redis.test:6379"));

        unsafe {
            std::env::remove_var("ACMECAST_REDIS_URL");
            std::env::remove_var("ACMECAST_VALKEY_URL");
            std::env::remove_var("REDIS_URL");
        }
    }
}
