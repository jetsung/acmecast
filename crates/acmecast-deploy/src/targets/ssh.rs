//! SSH 远程部署。
//!
//! 通过 SSH 把证书写到远程主机，支持私钥与口令两种认证。
//!
//! **连接方式可以来自两处**：输入直填（认证材料按标识从凭据系统取），或
//! 引用一份 SSH 主机档案（[`SshHostFields`]，材料与默认值都在档案里）。
//! 两条路径在 [`resolve_input`] 合并成完整的执行配置——校验只写这一份，
//! 直填与引用都不会带着半个配置去连主机。明文材料一旦进了输入，就会跟着
//! 流水线配置与运行历史一起扩散，因此两种写法都只携带标识或档案引用。
//!
//! 连接与命令通道被抽象成 [`SshTransport`]，这样「写到哪个路径、权限是什么、
//! 重载何时执行」这些逻辑可以在没有真实 SSH 服务器的情况下被验证——
//! CI 里起一台 sshd 既不现实也不稳定。

use std::sync::Arc;

use acmecast_access::{CredentialStore, ResolvedCredential};
use async_trait::async_trait;
use schemars::schema::RootSchema;
use schemars::schema_for;
use serde::Deserialize;
use serde_json::Value;

use crate::error::{Error, Result};
use crate::ssh_host::SshHostFields;
use crate::target::{CertMaterials, DeployMode, DeployOutcome, DeploymentTarget, parse_input};
use crate::targets::local::{default_cert_mode, default_key_mode};

fn default_port() -> u16 {
    22
}

/// 部署执行时认证材料的来源。
///
/// 档案自带材料与「输入里指一个认证凭据」最终都收敛到这里，
/// 连接器不再关心材料是从哪条路来的。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SshAuthSource {
    /// 认证材料在凭据系统里：按 `kind` 决定读哪个字段，标识取值。
    Credential(SshAuth),
    /// 档案自带的私钥。
    PrivateKey(String),
    /// 档案自带的口令。
    Password(String),
}

/// 登录远程主机的方式。
///
/// 只带**凭据标识**，真正的值在执行时从凭据系统取。
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, schemars::JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SshAuth {
    /// 私钥认证：私钥 PEM 来自凭据系统。
    PrivateKey {
        /// 私钥凭据的标识。
        credential_id: i64,
    },
    /// 口令认证：口令来自凭据系统。
    Password {
        /// 口令凭据的标识。
        credential_id: i64,
    },
}

impl SshAuth {
    /// 要取的凭据标识。
    #[must_use]
    pub fn credential_id(&self) -> i64 {
        match self {
            Self::PrivateKey { credential_id } | Self::Password { credential_id } => *credential_id,
        }
    }
}

/// SSH 部署的输入。
///
/// 两种写法都合法：**引用主机档案**（顶层 `credential_id`，其余字段可选地
/// 覆盖档案默认值）或**直填**（主机、用户、认证一样不缺）。合并与校验集中在
/// [`resolve_input`]——这里的字段全部可选，不等于运行时也宽松。
#[derive(Debug, Clone, Default, Deserialize, schemars::JsonSchema)]
pub struct SshInput {
    /// SSH 主机档案凭据的标识；其连接信息、认证材料与远端默认值成为底座。
    #[serde(default)]
    pub credential_id: Option<i64>,
    /// 目标主机。
    #[serde(default)]
    pub host: Option<String>,
    /// SSH 端口。
    #[serde(default)]
    pub port: Option<u16>,
    /// 登录用户。
    #[serde(default)]
    pub user: Option<String>,
    /// 认证方式（直填时使用：材料按标识从凭据系统取）。
    #[serde(default)]
    pub auth: Option<SshAuth>,
    /// 远程的证书路径。
    #[serde(default)]
    pub cert_path: Option<String>,
    /// 远程的私钥路径。
    #[serde(default)]
    pub key_path: Option<String>,
    /// 远程证书文件的权限。
    #[serde(default)]
    pub cert_mode: Option<String>,
    /// 远程私钥文件的权限。
    #[serde(default)]
    pub key_mode: Option<String>,
    /// 写入成功后在**远程**执行的重载命令。
    #[serde(default)]
    pub reload_command: Option<String>,
}

/// 合并档案与输入后得到的完整执行配置。
///
/// 与 [`SshInput`] 的区别：这里没有 `Option`，也没有「档案」概念——
/// 连接器拿到的是什么就是什么，缺字段的报错早在合并时已经发生。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedSshConfig {
    /// 目标主机。
    pub host: String,
    /// SSH 端口。
    pub port: u16,
    /// 登录用户。
    pub user: String,
    /// 认证材料的来源。
    pub auth: SshAuthSource,
    /// 远程的证书路径。
    pub cert_path: String,
    /// 远程的私钥路径。
    pub key_path: String,
    /// 远程证书文件的权限。
    pub cert_mode: String,
    /// 远程私钥文件的权限。
    pub key_mode: String,
    /// 写入成功后在远程执行的重载命令。
    pub reload_command: Option<String>,
    /// 生效的出网代理（`socks5://` 或 `http(s)://`）；`None` 直连。
    /// 仅档案 `proxy_mode=custom` 自带 URL 时生效（deploy 链路无全局来源）。
    pub proxy_url: Option<String>,
}

/// 把部署输入与可选的主机档案合并成完整的执行配置。
///
/// 优先级：输入显式值 > 档案值 > 系统缺省（端口 22、证书 0644、私钥 0600）。
/// 远端路径与重载命令不在合并链上——它们绑定的是「主机上跑哪个服务」，
/// 档案不携带，仅取部署输入。空白输入值不算显式——它只可能是配置写错，
/// 回退到档案值比照单全收更合理。合并后仍有必填项缺失时报错指出缺哪个
/// 字段，绝不带着半个配置去连主机。
///
/// # Errors
/// 档案字段不合法、必填项缺失或认证来源缺失时返回 [`Error::InvalidInput`]。
pub fn resolve_input(
    input: &SshInput,
    profile: Option<&SshHostFields>,
    global_proxy_url: Option<&str>,
) -> Result<ResolvedSshConfig> {
    // 档案自身先过一遍校验：字段形态坏掉要指向档案里的字段，
    // 而不是合并后报「缺主机」把人往错误的方向带。
    if let Some(profile) = profile {
        profile.validate().map_err(|error| match error {
            acmecast_core::Error::Validation { field, reason } => {
                Error::invalid_input(format!("credential_id.{field}"), reason)
            }
            other => Error::Core(other),
        })?;
    }

    let missing =
        |field: &str| Error::invalid_input(field, "未提供（输入与引用的主机档案中都没有）");
    // 路径不属于档案：报错要说明「从哪补」。
    let missing_path =
        |field: &str| Error::invalid_input(field, "未提供（远端路径由部署输入提供，主机档案不含路径）");

    let host = input
        .host
        .clone()
        .or_else(|| profile.map(|profile| profile.host.clone()))
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| missing("host"))?;
    let user = input
        .user
        .clone()
        .or_else(|| profile.map(|profile| profile.user.clone()))
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| missing("user"))?;

    let auth = if let Some(auth) = &input.auth {
        SshAuthSource::Credential(auth.clone())
    } else if let Some(profile) = profile {
        profile.auth_material()?
    } else {
        return Err(Error::invalid_input(
            "auth",
            "未提供：直填认证需要 `auth` 字段，或改为引用一份 SSH 主机档案",
        ));
    };

    let cert_path = input
        .cert_path
        .clone()
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| missing_path("cert_path"))?;
    let key_path = input
        .key_path
        .clone()
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| missing_path("key_path"))?;

    Ok(ResolvedSshConfig {
        host,
        port: input
            .port
            .or(profile.map(|profile| profile.port).filter(|port| *port != 0))
            .unwrap_or_else(default_port),
        user,
        auth,
        cert_path,
        key_path,
        cert_mode: input
            .cert_mode
            .clone()
            .or_else(|| profile.and_then(|profile| profile.cert_mode.clone()))
            .unwrap_or_else(default_cert_mode),
        key_mode: input
            .key_mode
            .clone()
            .or_else(|| profile.and_then(|profile| profile.key_mode.clone()))
            .unwrap_or_else(default_key_mode),
        reload_command: input.reload_command.clone(),
        proxy_url: profile.and_then(|profile| {
            acmecast_core::effective_proxy_url(
                &serde_json::to_value(profile).expect("SshHostFields 必可序列化"),
                global_proxy_url,
            )
        }),
    })
}

/// 一个已建立的远程连接。
#[async_trait]
pub trait SshTransport: Send + Sync {
    /// 把内容写到远程路径，并设成给定权限。
    async fn write_file(&self, path: &str, content: &str, mode: &str) -> Result<()>;

    /// 执行远程命令，返回（退出码、合并输出）。
    async fn exec(&self, command: &str) -> Result<(i32, String)>;
}

/// 建立远程连接的方式。
#[async_trait]
pub trait SshConnector: Send + Sync + std::fmt::Debug {
    /// 按合并后的配置连上远程主机并完成认证。
    ///
    /// 认证材料为 [`SshAuthSource::Credential`] 时才在这里从凭据系统取出——
    /// 私钥／口令既不经过部署输入，也不需要被长时间持有。
    async fn connect(
        &self,
        config: &ResolvedSshConfig,
        credentials: &CredentialStore<'_>,
    ) -> Result<Box<dyn SshTransport>>;
}

/// SSH 远程部署。
pub struct SshTarget {
    connector: Arc<dyn SshConnector>,
    // 86 的超时/代理继承语义：全局代理与网络超时由 ServerConfig 注入，SshConnector 在 connect 时使用
    _global_proxy_url: Option<String>,
    _network_timeout: std::time::Duration,
}

impl SshTarget {
    /// 用给定的连接器装配。
    #[must_use]
    pub fn with_connector(connector: Arc<dyn SshConnector>) -> Self {
        Self { connector, _global_proxy_url: None, _network_timeout: acmecast_core::NETWORK_TIMEOUT }
    }

    /// 走真实 SSH（用 russh）。
    #[must_use]
    pub fn live() -> Self {
        Self::with_connector(Arc::new(RusshConnector::default()))
    }

    /// 供 ServerConfig 注入全局代理与网络超时（与 DNS 侧 with_global_proxy_and_timeout 同口径）。
    #[must_use]
    pub fn live_with_proxy(proxy_url: Option<&str>, network_timeout: std::time::Duration) -> Self {
        let _global_proxy_url = proxy_url.map(str::trim).filter(|s| !s.is_empty()).map(str::to_owned);
        Self { connector: Arc::new(RusshConnector { network_timeout }), _global_proxy_url, _network_timeout: network_timeout }
    }
}

impl std::fmt::Debug for SshTarget {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SshTarget")
            .field("connector", &self.connector)
            .field("_global_proxy_url", &self._global_proxy_url)
            .field("_network_timeout", &self._network_timeout)
            .finish()
    }
}

#[async_trait]
impl DeploymentTarget for SshTarget {
    fn type_id(&self) -> &'static str {
        "ssh"
    }

    fn display_name(&self) -> &'static str {
        "SSH 远程主机"
    }

    fn input_schema(&self) -> RootSchema {
        schema_for!(SshInput)
    }

    fn example_input(&self) -> Value {
        serde_json::json!({
            "credential_id": 1,
            "cert_path": "/etc/ssl/example.com.crt",
            "key_path": "/etc/ssl/example.com.key",
            "reload_command": "systemctl reload nginx"
        })
    }

    async fn deploy(
        &self,
        input: &Value,
        materials: &CertMaterials,
        credentials: &CredentialStore<'_>,
        mode: DeployMode,
    ) -> Result<DeployOutcome> {
        let input: SshInput = parse_input(input)?;

        // 引用了主机档案就先取出并解密：档案里的材料与默认值从这里来。
        let profile = match input.credential_id {
            Some(id) => Some(resolve_profile(id, credentials).await?),
            None => None,
        };
        let config = resolve_input(&input, profile.as_ref(), self._global_proxy_url.as_deref())?;

        let transport = self.connector.connect(&config, credentials).await?;

        let mut paths: Vec<String> = if mode == DeployMode::Write {
            Vec::with_capacity(2)
        } else {
            vec![config.cert_path.clone(), config.key_path.clone()]
        };

        for (path, content, file_mode) in [
            (&config.cert_path, &materials.chain_pem, &config.cert_mode),
            (&config.key_path, &materials.key_pem, &config.key_mode),
        ] {
            transport
                .write_file(path, content, file_mode)
                .await
                .map_err(|e| Error::Write {
                    path: format!("{}:{}", config.host, path),
                    reason: e.to_string(),
                })?;
            paths.push(path.clone());
        }

        let outcome = if mode == DeployMode::SkipWrite {
            DeployOutcome::skipped(paths)
        } else {
            DeployOutcome::written(paths)
        };

        match config.reload_command.as_deref() {
            Some(command) => {
                let (exit_code, output) = transport.exec(command).await?;
                if exit_code != 0 {
                    return Err(Error::Reload {
                        command: command.to_owned(),
                        exit_code: Some(exit_code),
                        output,
                    });
                }
                Ok(outcome.with_reload_output(output))
            }
            None => Ok(outcome),
        }
    }
}

/// 取出输入引用的 SSH 主机档案。
async fn resolve_profile(
    id: i64,
    credentials: &CredentialStore<'_>,
) -> Result<SshHostFields> {
    let resolved = credentials.resolve(id).await.map_err(|e| {
        Error::Remote(format!("取 SSH 主机档案 `{id}` 失败: {e}"))
    })?;
    resolved.as_fields().map_err(|e| {
        Error::invalid_input(
            "credential_id",
            format!("SSH 主机档案 `{id}` 的字段不合法: {e}"),
        )
    })
}

/// 建立 SSH 会话：配了代理就先开隧道流，再走 `connect_stream`；
/// 未配置则直连。两种路径共用同一份客户端配置与回调。
async fn connect_ssh(
    settings: Arc<russh::client::Config>,
    host: &str,
    port: u16,
    proxy_url: Option<&str>,
    timeout: std::time::Duration,
) -> Result<russh::client::Handle<ClientHandler>> {
    if let Some(proxy_url) = proxy_url {
        let stream = open_proxy_stream(proxy_url, host, port, timeout).await?;
        return tokio::time::timeout(timeout, russh::client::connect_stream(settings, stream, ClientHandler))
            .await
            .map_err(|_| Error::Remote(format!("连接 {host}:{port} 超时（{} 秒无响应，经代理 {}）", timeout.as_secs(), acmecast_core::proxy_log_target(proxy_url))))?
            .map_err(|e| Error::Remote(e.to_string()));
    }
    tokio::time::timeout(timeout, russh::client::connect(settings, (host, port), ClientHandler))
        .await
        .map_err(|_| Error::Remote(format!("连接 {host}:{port} 超时（{} 秒无响应）", timeout.as_secs())))?
        .map_err(|e| Error::Remote(e.to_string()))
}

/// 走 russh 的连接器。
#[derive(Debug)]
pub struct RusshConnector {
    network_timeout: std::time::Duration,
}

impl Default for RusshConnector {
    fn default() -> Self {
        Self { network_timeout: acmecast_core::NETWORK_TIMEOUT }
    }
}

/// 经代理隧道建立到目标主机的 TCP 流：
///
/// - `socks5://` → RFC 1928 握手（无认证）+ CONNECT 请求；
/// - `http://` / `https://` → HTTP CONNECT 隧道。
///
/// 返回的流直接交给 `russh::client::connect_stream` 建立 SSH 会话。
/// 代理 URL 的 scheme/可解析性已在保存时由 `validate_proxy_url` 拦下，
/// 这里对无法解析的输入直接报 `Remote` 错误。
async fn open_proxy_stream(
    proxy_url: &str,
    host: &str,
    port: u16,
    timeout: std::time::Duration,
) -> Result<tokio::net::TcpStream> {
    let fut = async {
    let parsed = url::Url::parse(proxy_url)
        .map_err(|e| Error::Remote(format!("代理 URL 非法: {e}")))?;
    let (proxy_host, proxy_port) = match parsed.host_str() {
        Some(h) if !h.is_empty() => (
            h.to_owned(),
            parsed.port().unwrap_or(match parsed.scheme() {
                "http" => 80,
                _ => 1080,
            }),
        ),
        _ => {
            return Err(Error::Remote(format!(
                "代理 URL 缺少主机: {}",
                acmecast_core::proxy_log_target(proxy_url)
            )))
        }
    };
    let stream = tokio::net::TcpStream::connect((proxy_host.as_str(), proxy_port))
        .await
        .map_err(|e| {
            Error::Remote(format!(
                "连接代理 {} 失败: {e}",
                acmecast_core::proxy_log_target(proxy_url)
            ))
        })?;
    match parsed.scheme() {
        "socks5" => socks5_connect(stream, host, port).await,
        _ => http_connect(stream, host, port).await,
    }
    };
    tokio::time::timeout(timeout, fut)
        .await
        .map_err(|_| Error::Remote(format!("代理 {} 到 {}:{} 握手超时（{} 秒无响应）", acmecast_core::proxy_log_target(proxy_url), host, port, timeout.as_secs())))?
}

/// SOCKS5（RFC 1928）握手 + CONNECT：无认证，支持域名（ATYP=3）与 IPv4（ATYP=1）。
async fn socks5_connect(
    mut stream: tokio::net::TcpStream,
    host: &str,
    port: u16,
) -> Result<tokio::net::TcpStream> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    // 方法选择：只支持无认证。
    stream.write_all(&[0x05, 0x01, 0x00]).await.map_err(|e| {
        Error::Remote(format!("SOCKS5 隧道握手失败: {e}"))
    })?;
    let mut greeting = [0u8; 2];
    stream.read_exact(&mut greeting).await.map_err(|e| {
        Error::Remote(format!("SOCKS5 隧道握手失败: {e}"))
    })?;
    if greeting[0] != 0x05 || greeting[1] != 0x00 {
        return Err(Error::Remote(format!(
            "SOCKS5 代理拒绝了无认证方法（回复 {:02x} {:02x}）",
            greeting[0], greeting[1]
        )));
    }

    // CONNECT 请求：IPv4 字面量用 ATYP=1，其余一律按域名（ATYP=3）。
    let mut request = vec![0x05, 0x01, 0x00];
    if let Ok(ip) = host.parse::<std::net::Ipv4Addr>() {
        request.push(0x01);
        request.extend_from_slice(&ip.octets());
    } else {
        if host.len() > 255 {
            return Err(Error::Remote("目标主机名过长，SOCKS5 域名寻址不支持".to_owned()));
        }
        request.push(0x03);
        request.push(host.len() as u8);
        request.extend_from_slice(host.as_bytes());
    }
    request.extend_from_slice(&port.to_be_bytes());
    stream.write_all(&request).await.map_err(|e| {
        Error::Remote(format!("SOCKS5 隧道请求失败: {e}"))
    })?;

    // 响应：VER REP RSV ATYP BND ADDR BND PORT。
    let mut head = [0u8; 4];
    stream.read_exact(&mut head).await.map_err(|e| {
        Error::Remote(format!("SOCKS5 隧道响应读取失败: {e}"))
    })?;
    if head[0] != 0x05 {
        return Err(Error::Remote(format!(
            "SOCKS5 代理返回了非 SOCKS5 版本 {:02x}",
            head[0]
        )));
    }
    if head[1] != 0x00 {
        return Err(Error::Remote(format!(
            "SOCKS5 代理拒绝连接（错误码 {:02x}）",
            head[1]
        )));
    }
    // 跳过绑定的地址与端口（长度取决于 ATYP）。
    let skip = match head[3] {
        0x01 => 4 + 2, // IPv4 + 端口
        0x04 => 16 + 2, // IPv6 + 端口
        0x03 => {
            let mut len = [0u8; 1];
            stream
                .read_exact(&mut len)
                .await
                .map_err(|e| Error::Remote(format!("SOCKS5 隧道响应读取失败: {e}")))?;
            len[0] as usize + 2
        }
        other => {
            return Err(Error::Remote(format!(
                "SOCKS5 代理返回了未知寻址类型 {:02x}",
                other
            )))
        }
    };
    let mut rest = vec![0u8; skip];
    stream.read_exact(&mut rest).await.map_err(|e| {
        Error::Remote(format!("SOCKS5 隧道响应读取失败: {e}"))
    })?;
    Ok(stream)
}

/// HTTP CONNECT 隧道：发 `CONNECT host:port`，确认 `2xx` 后流即为隧道。
async fn http_connect(
    mut stream: tokio::net::TcpStream,
    host: &str,
    port: u16,
) -> Result<tokio::net::TcpStream> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let request = format!("CONNECT {host}:{port} HTTP/1.1\r\nHost: {host}:{port}\r\n\r\n");
    stream.write_all(request.as_bytes()).await.map_err(|e| {
        Error::Remote(format!("HTTP CONNECT 请求失败: {e}"))
    })?;

    // 逐字节读响应头直到 `\r\n\r\n` 结束：CONNECT 2xx 按规范无 body，
    // 响应头之后的第一个字节就是 SSH banner——绝不能多读。
    let mut byte = [0u8; 1];
    let mut seen: [u8; 4] = [0; 4];
    let mut header = Vec::new();
    let mut index = 0usize;
    loop {
        stream.read_exact(&mut byte).await.map_err(|e| {
            Error::Remote(format!("HTTP 代理响应读取失败: {e}"))
        })?;
        let slot = index % 4;
        seen[slot] = byte[0];
        header.push(byte[0]);
        // 按循环序取最近 4 个字节比较（槽位 i%4 写入后，最近四字节是
        // (i+1)%4, (i+2)%4, (i+3)%4, i%4 依次排开）。
        let last4 = [
            seen[(slot + 1) % 4],
            seen[(slot + 2) % 4],
            seen[(slot + 3) % 4],
            seen[slot],
        ];
        if last4 == *b"\r\n\r\n" {
            break;
        }
        index += 1;
        if header.len() > 8192 {
            return Err(Error::Remote("HTTP 代理响应头过长，格式非法".to_owned()));
        }
    }
    // 状态行是响应头的第一行：`HTTP/1.x 2xx ...`。
    let status_line = String::from_utf8_lossy(&header);
    if !status_line.split("\r\n").next().unwrap_or("").contains(" 2") {
        return Err(Error::Remote(format!(
            "HTTP 代理拒绝连接（响应: {}）",
            status_line.lines().next().unwrap_or("").trim()
        )));
    }
    Ok(stream)
}

#[async_trait]
impl SshConnector for RusshConnector {
    async fn connect(
        &self,
        config: &ResolvedSshConfig,
        credentials: &CredentialStore<'_>,
    ) -> Result<Box<dyn SshTransport>> {
        let settings = Arc::new(russh::client::Config::default());
        let session = connect_ssh(settings, &config.host, config.port, config.proxy_url.as_deref(), self.network_timeout)
            .await
            .map_err(|e| {
                Error::Remote(format!("连接 {}:{} 失败: {e}", config.host, config.port))
            })?;
        let mut session = session;

        // 三种材料来源各读各的：凭据缺哪个字段就报哪个，比一句「认证失败」有用。
        // 认证也受网络超时约束（与 connect_ssh 同口径，超时展示目标主机）。
        let auth_fut = async {
        match &config.auth {
            SshAuthSource::Credential(auth) => {
                let credential = resolve_auth_credential(auth, credentials).await?;
                match auth {
                    SshAuth::PrivateKey { .. } => {
                        let pem = credential_field(&credential, "private_key")?;
                        authenticate_with_key(&mut session, &config.user, &pem).await?;
                    }
                    SshAuth::Password { .. } => {
                        let password = credential_field(&credential, "password")?;
                        authenticate_with_password(&mut session, &config.user, &password).await?;
                    }
                }
            }
            SshAuthSource::PrivateKey(pem) => {
                authenticate_with_key(&mut session, &config.user, pem).await?;
            }
            SshAuthSource::Password(password) => {
                authenticate_with_password(&mut session, &config.user, password).await?;
            }
        }
        Ok::<(), Error>(())
        };
        tokio::time::timeout(self.network_timeout, auth_fut)
            .await
            .map_err(|_| Error::Remote(format!("认证 {}@{}:{} 超时（{} 秒无响应）", config.user, config.host, config.port, self.network_timeout.as_secs())))?
            .map_err(|e| Error::Remote(format!("认证 {}@{}:{} 失败: {e}", config.user, config.host, config.port)))?;

        Ok(Box::new(RusshTransport { session, network_timeout: self.network_timeout }))
    }
}

/// 取出直填认证要用的凭据。
async fn resolve_auth_credential(
    auth: &SshAuth,
    credentials: &CredentialStore<'_>,
) -> Result<ResolvedCredential> {
    let id = auth.credential_id();
    credentials.resolve(id).await.map_err(|e| {
        Error::Remote(format!(
            "取 {} 认证用的凭据 `{id}` 失败: {e}",
            match auth {
                SshAuth::PrivateKey { .. } => "私钥",
                SshAuth::Password { .. } => "口令",
            }
        ))
    })
}

/// 从凭据里取一个字段值。
fn credential_field(credential: &ResolvedCredential, field: &str) -> Result<String> {
    credential
        .fields
        .get(field)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| {
            Error::invalid_input(
                "auth",
                format!("凭据 `{}` 里没有 `{field}` 字段", credential.id),
            )
        })
}

/// 用私钥完成认证。
async fn authenticate_with_key(
    session: &mut russh::client::Handle<ClientHandler>,
    user: &str,
    pem: &str,
) -> Result<()> {
    let key = parse_private_key(pem)?;
    let result = session
        .authenticate_publickey(user, key)
        .await
        .map_err(|e| Error::Remote(format!("私钥认证失败: {e}")))?;
    ensure_authenticated(result)
}

/// 用口令完成认证。
async fn authenticate_with_password(
    session: &mut russh::client::Handle<ClientHandler>,
    user: &str,
    password: &str,
) -> Result<()> {
    let result = session
        .authenticate_password(user, password)
        .await
        .map_err(|e| Error::Remote(format!("口令认证失败: {e}")))?;
    ensure_authenticated(result)
}

/// 把认证结果判成成功或失败。
fn ensure_authenticated(result: russh::client::AuthResult) -> Result<()> {
    if result.success() {
        Ok(())
    } else {
        Err(Error::Remote("认证被远程主机拒绝".to_owned()))
    }
}

/// 解析 OpenSSH 格式的私钥 PEM。
fn parse_private_key(pem: &str) -> Result<russh::keys::PrivateKeyWithHashAlg> {
    use russh::keys::{PrivateKey, PrivateKeyWithHashAlg};

    let key = PrivateKey::from_openssh(pem.as_bytes())
        .map_err(|e| Error::Remote(format!("私钥无法解析（应为 OpenSSH 或 PEM 格式）: {e}")))?;

    Ok(PrivateKeyWithHashAlg::new(Arc::new(key), None))
}

/// 连通性探测的连接超时。
///
/// 凭据测试是用户手动触发的交互动作，挂在那里等一个不存在的主机
/// 比干脆失败更糟。
const PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// 对一份 SSH 主机档案发起一次真实 SSH 连接与认证，判定档案是否可用。
///
/// 供凭据的连通性测试使用：字段校验先行（缺材料不发起任何网络请求），
/// 然后带超时连上主机完成认证。成功即返回；失败原因**未脱敏**——
/// 调用方（凭据类型）要先用 [`SshHostFields`] 的字段值过一遍脱敏再呈现。
pub async fn probe_host(fields: &SshHostFields, global_proxy_url: Option<&str>) -> Result<()> {
    fields.validate().map_err(|error| match error {
        acmecast_core::Error::Validation { field, reason } => {
            Error::invalid_input(field, reason)
        }
        other => Error::Core(other),
    })?;

    let settings = Arc::new(russh::client::Config::default());
    let proxy_url = acmecast_core::effective_proxy_url(
        &serde_json::to_value(fields).expect("SshHostFields 必可序列化"),
        global_proxy_url,
    );
    let connect = connect_ssh(settings, &fields.host, fields.port, proxy_url.as_deref(), PROBE_TIMEOUT);
    let mut session = tokio::time::timeout(PROBE_TIMEOUT, connect)
        .await
        .map_err(|_| {
            Error::Remote(format!(
                "连接 {}:{} 超时（10 秒无响应）",
                fields.host, fields.port
            ))
        })?
        .map_err(|e| {
            Error::Remote(format!("连接 {}:{} 失败: {e}", fields.host, fields.port))
        })?;

    // 校验已保证材料恰好其一，这里按剩下的那个走。
    if let Some(pem) = fields.private_key.as_deref() {
        authenticate_with_key(&mut session, &fields.user, pem).await?;
    } else if let Some(password) = fields.password.as_deref() {
        authenticate_with_password(&mut session, &fields.user, password).await?;
    }

    // Handle 落地即关闭连接——探测只关心「能不能连上并认证」。
    Ok(())
}

/// russh 的回调：这里只做主机密钥的处置决定。
#[derive(Debug)]
struct ClientHandler;

impl russh::client::Handler for ClientHandler {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        server_public_key: &russh::keys::PublicKeyOrCertificate,
    ) -> std::result::Result<bool, Self::Error> {
        // 首版尚未接入 known_hosts，因此接受任何主机密钥。
        // 这意味着存在中间人风险，留一条 warn 让它在日志里可被发现。
        tracing::warn!(
            fingerprint = %server_public_key
                .public_key()
                .fingerprint(russh::keys::HashAlg::Sha256),
            "未校验 SSH 主机指纹（known_hosts 支持尚未实现），接受该主机"
        );
        Ok(true)
    }
}

/// 已认证的 russh 会话。
struct RusshTransport {
    session: russh::client::Handle<ClientHandler>,
    network_timeout: std::time::Duration,
}

/// 把一段文本转成带引号的 shell 参数。
fn shell_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', "'\\''"))
}

#[async_trait]
impl SshTransport for RusshTransport {
    async fn write_file(&self, path: &str, content: &str, mode: &str) -> Result<()> {
        // 与本地部署同构：先写临时文件再原子替换。`trap` 保证任何一步失败时
        // 临时产物都被清掉，而目标文件保持原样——远端文件同样可能正被 web 服务器读着。
        //
        // 通过 stdin 灌进 `cat`，避免把整份证书塞进命令行参数——
        // 参数长度有限制，而且会出现在远端的 ps 输出里。
        let temp = format!("{path}.acmecast-{}.tmp", uuid::Uuid::new_v4().simple());
        let script = format!(
            "set -e\n\
             trap 'rm -f {temp}' 0\n\
             cat > {temp} <<'ACMECAST_EOF'\n{content}\nACMECAST_EOF\n\
             chmod {mode} {temp}\n\
             mv {temp} {path}\n\
             trap - 0\n",
            temp = shell_quote(&temp),
            path = shell_quote(path),
            content = content,
            mode = shell_quote(mode),
        );

        let fut = run_channel(&self.session, &script);
        let (exit_code, output) = tokio::time::timeout(self.network_timeout, fut)
            .await
            .map_err(|_| Error::Remote(format!("写入 {path} 超时（{} 秒无响应）", self.network_timeout.as_secs())))?
            .map_err(|e| Error::Remote(format!("写入 {path} 失败: {e}")))?;
        if exit_code != 0 {
            return Err(Error::Write {
                path: path.to_owned(),
                reason: format!("远端写入失败（退出码 {exit_code}）: {output}"),
            });
        }
        Ok(())
    }

    async fn exec(&self, command: &str) -> Result<(i32, String)> {
        let fut = run_channel(&self.session, command);
        tokio::time::timeout(self.network_timeout, fut)
            .await
            .map_err(|_| Error::Remote(format!("执行命令超时（{} 秒无响应，命令: {}）", self.network_timeout.as_secs(), command.chars().take(80).collect::<String>())))?
            .map_err(|e| Error::Remote(format!("执行命令失败: {e}")))
    }
}

/// 开一个会话通道执行脚本，返回退出码与合并输出。
async fn run_channel(
    session: &russh::client::Handle<ClientHandler>,
    script: &str,
) -> Result<(i32, String)> {
    let mut channel = session
        .channel_open_session()
        .await
        .map_err(|e| Error::Remote(format!("打开会话通道失败: {e}")))?;

    channel
        .exec(true, script)
        .await
        .map_err(|e| Error::Remote(format!("执行命令失败: {e}")))?;

    let mut collected = Vec::new();
    let mut exit_code = 0_i32;

    while let Some(message) = channel.wait().await {
        match message {
            russh::ChannelMsg::Data { data, .. } | russh::ChannelMsg::ExtendedData { data, .. } => {
                collected.extend_from_slice(&data);
            }
            russh::ChannelMsg::ExitStatus { exit_status } => {
                exit_code = exit_status as i32;
                break;
            }
            russh::ChannelMsg::Eof => break,
            _ => {}
        }
    }

    channel
        .close()
        .await
        .map_err(|e| Error::Remote(format!("关闭通道失败: {e}")))?;

    let output = String::from_utf8_lossy(&collected).into_owned();
    Ok((
        exit_code,
        if output.trim().is_empty() {
            "(无输出)".to_owned()
        } else {
            output
        },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn socks5_tunnel_handshakes_with_an_in_process_proxy() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        // 进程内假 SOCKS5：应答方法选择与 CONNECT（域名寻址）。
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy_addr = listener.local_addr().unwrap();
        let proxy = tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut greeting = [0u8; 3];
            sock.read_exact(&mut greeting).await.unwrap();
            assert_eq!(&greeting, &[0x05, 0x01, 0x00]);
            sock.write_all(&[0x05, 0x00]).await.unwrap();
            // CONNECT 请求头：VER CMD RSV ATYP（再按 ATYP 读地址与端口）。
            let mut head = [0u8; 5];
            sock.read_exact(&mut head).await.unwrap();
            assert_eq!(&head, &[0x05, 0x01, 0x00, 0x03, 18]); // 域名寻址 + 长度
            let mut host_bytes = vec![0u8; head[4] as usize];
            sock.read_exact(&mut host_bytes).await.unwrap();
            assert_eq!(
                String::from_utf8_lossy(&host_bytes),
                "target.example.com",
                "隧道请求必须带上目标主机"
            );
            let mut port_bytes = [0u8; 2];
            sock.read_exact(&mut port_bytes).await.unwrap();
            assert_eq!(u16::from_be_bytes(port_bytes), 2222, "隧道请求必须带上目标端口");
            // REP=0 成功，绑定地址用 0.0.0.0:0。
            sock.write_all(&[0x05, 0x00, 0x00, 0x01, 0, 0, 0, 0, 0, 0]).await.unwrap();
        });

        let stream = open_proxy_stream(
            &format!("socks5://{proxy_addr}"),
            "target.example.com",
            2222,
            std::time::Duration::from_secs(5),
        )
        .await
        .expect("SOCKS5 隧道应建立成功");
        // 隧道流可继续收发：SSH banner 会直接流过这条流。
        assert!(stream.peer_addr().is_ok());
        proxy.await.unwrap();
    }

    #[tokio::test]
    async fn http_connect_tunnel_passes_through_on_2xx() {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy_addr = listener.local_addr().unwrap();
        let proxy = tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut reader = tokio::io::BufReader::new(&mut sock);
            let mut request = String::new();
            reader.read_line(&mut request).await.unwrap();
            assert!(
                request.starts_with("CONNECT target.example.com:2222 HTTP/1.1"),
                "CONNECT 请求必须带上目标地址: {request:?}"
            );
            // 排空剩余请求头（Host 行 + 空行），否则退出时内核缓冲里有
            // 未读数据会发 RST，把已写出的响应冲掉。
            let mut rest = String::new();
            reader.read_line(&mut rest).await.unwrap();
            reader.read_line(&mut rest).await.unwrap();
            sock.write_all(b"HTTP/1.1 200 Connection established\r\n\r\n")
                .await
                .unwrap();
        });

        let stream = open_proxy_stream(
            &format!("http://{proxy_addr}"),
            "target.example.com",
            2222,
            std::time::Duration::from_secs(5),
        )
        .await
        .expect("HTTP CONNECT 隧道应建立成功");
        assert!(stream.peer_addr().is_ok());
        proxy.await.unwrap();
    }

    #[tokio::test]
    async fn http_connect_tunnel_rejects_non_2xx() {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy_addr = listener.local_addr().unwrap();
        let proxy = tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut reader = tokio::io::BufReader::new(&mut sock);
            let mut line = String::new();
            reader.read_line(&mut line).await.unwrap();
            reader.read_line(&mut line).await.unwrap();
            reader.read_line(&mut line).await.unwrap();
            sock.write_all(b"HTTP/1.1 403 Forbidden\r\n\r\n").await.unwrap();
        });

        let result = open_proxy_stream(
            &format!("http://{proxy_addr}"),
            "target.example.com",
            2222,
            std::time::Duration::from_secs(5),
        )
        .await;
        let Err(e) = result else {
            panic!("非 2xx 应拒绝隧道: {result:?}");
        };
        assert!(
            e.to_string().contains("403"),
            "错误信息应带响应: {e}"
        );
        proxy.await.unwrap();
    }

    #[test]
    fn resolved_config_carrying_a_custom_proxy_passes_it_through() {
        let input = SshInput {
            cert_path: Some("/etc/ssl/example.com.crt".to_owned()),
            key_path: Some("/etc/ssl/example.com.key".to_owned()),
            ..SshInput::default()
        };
        let profile = SshHostFields {
            proxy_mode: Some("custom".to_owned()),
            proxy_url: Some("socks5://10.0.0.1:1080".to_owned()),
            ..test_profile()
        };
        let resolved = resolve_input(&input, Some(&profile), None).expect("应合并成功");
        assert_eq!(
            resolved.proxy_url.as_deref(),
            Some("socks5://10.0.0.1:1080"),
            "custom 自带 URL 应进入执行配置"
        );

        // direct / inherit（deploy 链路无全局来源）→ 直连。
        for mode in ["direct", "inherit"] {
            let profile = SshHostFields {
                proxy_mode: Some(mode.to_owned()),
                proxy_url: Some("socks5://10.0.0.1:1080".to_owned()),
                ..test_profile()
            };
            let resolved = resolve_input(&input, Some(&profile), None).expect("应合并成功");
            assert_eq!(resolved.proxy_url, None, "{mode} 应直连");
        }
    }

    fn test_profile() -> SshHostFields {
        SshHostFields {
            host: "target.example.com".to_owned(),
            port: 22,
            user: "deploy".to_owned(),
            private_key: None,
            password: Some("secret".to_owned()),
            cert_mode: None,
            key_mode: None,
            proxy_mode: None,
            proxy_url: None,
        }
    }

    #[test]
    fn both_auth_kinds_carry_only_a_credential_id() {
        let key = serde_json::from_value::<SshAuth>(serde_json::json!({
            "kind": "private_key", "credential_id": 7
        }))
        .expect("应能解析");
        assert_eq!(key.credential_id(), 7);

        let password = serde_json::from_value::<SshAuth>(serde_json::json!({
            "kind": "password", "credential_id": 9
        }))
        .expect("应能解析");
        assert_eq!(password.credential_id(), 9);
    }

    #[test]
    fn no_credential_material_lives_in_the_input_definition() {
        // 这一条是在防回归：一旦有人把 private_key / password 直接做成输入字段，
        // 明文就会随流水线配置与运行历史扩散。
        //
        // 只看属性名，不看整个 Schema 文本——`private_key`／`password` 会作为
        // 认证方式的 `kind` 枚举值出现，那是合法的，不能一概而论。
        let schema = schema_for!(SshInput);
        let properties = &schema.schema.object.as_ref().expect("应是对象").properties;

        assert!(
            !properties.contains_key("private_key"),
            "私钥不该成为输入字段: {:?}",
            properties.keys().collect::<Vec<_>>()
        );
        assert!(!properties.contains_key("password"), "口令不该成为输入字段");
        assert!(properties.contains_key("auth"), "认证方式应在");
        assert!(properties.contains_key("credential_id"), "档案引用应在");
    }

    #[test]
    fn shell_quoting_handles_quotes() {
        assert_eq!(shell_quote("plain"), "'plain'");
        // 内部的单引号按 POSIX 的方式闭合再转义。
        assert_eq!(shell_quote("it's"), "'it'\\''s'");
    }

    /// 一份只填了必填项的档案，供合并测试复用。
    ///
    /// 端口 2222 与证书权限 0644 用于断言「输入未给时回退档案值」。
    fn profile() -> SshHostFields {
        SshHostFields {
            host: "web-1.example.com".to_owned(),
            port: 2222,
            user: "deploy".to_owned(),
            private_key: Some("PRIVATE-KEY".to_owned()),
            password: None,
            cert_mode: Some("0644".to_owned()),
            key_mode: None,
            proxy_mode: None,
            proxy_url: None,
        }
    }

    #[test]
    fn a_profile_with_input_paths_resolves_into_a_full_config() {
        let input: SshInput = serde_json::from_value(serde_json::json!({
            "credential_id": 1,
            "cert_path": "/srv/ssl/site.crt",
            "key_path": "/srv/ssl/site.key",
        }))
        .expect("应能解析");
        let config = resolve_input(&input, Some(&profile()), None).expect("档案应补齐连接与认证");

        assert_eq!(config.host, "web-1.example.com");
        assert_eq!(config.user, "deploy");
        assert_eq!(config.port, 2222, "输入未给端口时回退档案值");
        assert_eq!(config.cert_path, "/srv/ssl/site.crt");
        assert_eq!(config.cert_mode, "0644", "输入未给权限时回退档案值");
        assert_eq!(config.key_mode, "0600", "档案未给权限时用系统缺省");
        assert!(
            config.reload_command.is_none(),
            "重载命令不属于档案，只来自部署输入"
        );
        match config.auth {
            SshAuthSource::PrivateKey(key) => assert_eq!(key, "PRIVATE-KEY"),
            other => panic!("期望档案自带私钥，实际 {other:?}"),
        }
    }

    #[test]
    fn a_profile_reference_without_paths_names_the_missing_field() {
        let input: SshInput = serde_json::from_value(serde_json::json!({
            "credential_id": 1,
        }))
        .expect("应能解析");
        let err = resolve_input(&input, Some(&profile()), None).expect_err("档案不含路径应报错");

        let text = err.to_string();
        assert!(text.contains("cert_path"), "应指出缺证书路径: {text}");
        assert!(
            text.contains("部署输入"),
            "报错要说明路径从哪补: {text}"
        );
    }

    #[test]
    fn explicit_input_values_win_over_profile_values() {
        let input: SshInput = serde_json::from_value(serde_json::json!({
            "credential_id": 1,
            "port": 2223,
            "cert_path": "/override/site.crt",
            "key_path": "/override/site.key",
            "cert_mode": "0600",
            "reload_command": "systemctl reload caddy",
        }))
        .expect("应能解析");
        let config = resolve_input(&input, Some(&profile()), None).expect("合并应成功");

        assert_eq!(config.port, 2223, "输入显式端口应覆盖档案");
        assert_eq!(config.cert_path, "/override/site.crt");
        assert_eq!(config.key_path, "/override/site.key");
        assert_eq!(config.cert_mode, "0600", "输入权限应覆盖档案默认值");
        assert_eq!(config.reload_command.as_deref(), Some("systemctl reload caddy"));
        assert_eq!(config.host, "web-1.example.com", "未覆盖的字段回到档案");
    }

    #[test]
    fn a_direct_fill_input_without_a_profile_still_resolves() {
        let input: SshInput = serde_json::from_value(serde_json::json!({
            "host": "10.0.0.9",
            "user": "root",
            "auth": { "kind": "password", "credential_id": 3 },
            "cert_path": "/c.pem",
            "key_path": "/k.pem",
        }))
        .expect("应能解析");
        let config = resolve_input(&input, None, None).expect("直填应完整");

        assert_eq!(config.host, "10.0.0.9");
        assert_eq!(config.port, 22);
        match config.auth {
            SshAuthSource::Credential(auth) => assert_eq!(auth.credential_id(), 3),
            other => panic!("期望凭据引用，实际 {other:?}"),
        }
    }

    #[test]
    fn missing_fields_are_named_after_the_merge() {
        let input: SshInput = serde_json::from_value(serde_json::json!({})).expect("应能解析");
        let err = resolve_input(&input, None, None).expect_err("既无档案也无直填应报错");
        assert!(
            err.to_string().contains("host"),
            "应指出缺主机: {err}",
        );

        // 逐项补齐到只剩认证缺失：报错要跟上进度，指向 auth。
        let input: SshInput = serde_json::from_value(serde_json::json!({
            "host": "10.0.0.9",
            "user": "root",
            "cert_path": "/c.pem",
            "key_path": "/k.pem",
        }))
        .expect("应能解析");
        let err = resolve_input(&input, None, None).expect_err("缺认证应报错");
        assert!(err.to_string().contains("auth"), "{err}");

        let input: SshInput = serde_json::from_value(serde_json::json!({
            "credential_id": 1,
        }))
        .expect("应能解析");
        let mut broken = profile();
        broken.private_key = None;
        let err = resolve_input(&input, Some(&broken), None).expect_err("档案缺认证材料应报错");
        assert!(err.to_string().contains("credential_id.private_key"), "{err}");
    }

    #[test]
    fn a_broken_profile_is_reported_against_its_own_fields() {
        let input: SshInput = serde_json::from_value(serde_json::json!({
            "credential_id": 1,
        }))
        .expect("应能解析");
        let mut broken = profile();
        broken.private_key = None;
        let err = resolve_input(&input, Some(&broken), None).expect_err("档案缺认证材料应报错");
        let text = err.to_string();
        assert!(text.contains("credential_id.private_key"), "{err}");
    }

    /// 一份指向本地保留端口、带标记材料的档案，供探测测试复用。
    fn probe_fields() -> SshHostFields {
        SshHostFields {
            host: "127.0.0.1".to_owned(),
            port: 1,
            user: "probe-user".to_owned(),
            private_key: Some("SECRET-KEY-MATERIAL".to_owned()),
            password: None,
            cert_mode: None,
            key_mode: None,
            proxy_mode: None,
            proxy_url: None,
        }
    }

    #[tokio::test]
    async fn a_probe_refuses_to_run_with_missing_materials() {
        let mut fields = probe_fields();
        fields.private_key = None;
        let err = probe_host(&fields, None)
            .await
            .expect_err("缺认证材料的探测应立即失败");

        // 校验先行：不该发起任何网络请求，也不该变成一句模糊的「连接失败」。
        assert!(matches!(err, Error::InvalidInput { .. }), "{err:?}");
        assert!(err.to_string().contains("private_key"), "{err}");
    }

    #[tokio::test]
    async fn an_unreachable_host_reports_the_target() {
        // 本地保留端口：连接被立即拒绝，测试不依赖网络与超时。
        let err = probe_host(&probe_fields(), None)
            .await
            .expect_err("保留端口应连接失败");
        let text = err.to_string();
        assert!(text.contains("127.0.0.1:1"), "应含连接目标: {text}");
    }

    #[tokio::test]
    async fn probe_errors_never_carry_the_materials() {
        // 连接失败路径：错误文本来自连接器本身，材料只进过字段结构体。
        let text = probe_host(&probe_fields(), None)
            .await
            .expect_err("应连接失败")
            .to_string();
        assert!(!text.contains("SECRET-KEY-MATERIAL"), "{text}");
        assert!(!text.contains("probe-user"), "用户名也不是材料，但一并别露: {text}");
    }
}

#[cfg(test)]
mod schema_print {
    #[test]
    fn print() {
        println!("{}", serde_json::to_string_pretty(&schemars::schema_for!(super::SshInput)).unwrap());
    }
}
