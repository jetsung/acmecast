//! SMTP 代理隧道：`socks5` / `http` / `https` 三种代理协议。
//!
//! lettre 0.11 的 SMTP 传输不带代理支持，但公开了注入自定义流的通道：
//! `AsyncSmtpConnection::connect_with_transport(Box<dyn AsyncTokioStream>, &ClientId)`。
//! 本模块在代理之上建隧道流，必要时在流上包装 TLS（隐式 TLS 与 `https://`
//! 代理的「TLS 到代理」），最后交由 lettre 完成 EHLO/STARTTLS/AUTH/投递。

use std::{
    fmt, io,
    net::SocketAddr,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};

use lettre::{
    message::Message,
    transport::smtp::{
        authentication::{Credentials, Mechanism},
        client::{AsyncSmtpConnection, TlsParameters},
        extension::ClientId,
    },
};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

use lettre::transport::smtp::client::AsyncTokioStream;

use crate::delivery::DEFAULT_TIMEOUT;
use crate::email::{SmtpSettings, TlsMode};

/// 把任意 `AsyncRead+AsyncWrite` 包装为 lettre 的 `AsyncTokioStream`。
#[derive(Debug)]
struct TokioStream<S>(S);

impl<S> AsyncRead for TokioStream<S>
where
    S: AsyncRead + Unpin,
{
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.0).poll_read(cx, buf)
    }
}

impl<S> AsyncWrite for TokioStream<S>
where
    S: AsyncWrite + Unpin,
{
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.0).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.0).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.0).poll_shutdown(cx)
    }
}

impl<S> AsyncTokioStream for TokioStream<S>
where
    S: AsyncRead + AsyncWrite + Send + Sync + Unpin + fmt::Debug,
{
    fn peer_addr(&self) -> io::Result<SocketAddr> {
        Ok(SocketAddr::from(([0, 0, 0, 0], 0)))
    }
}

/// `AsyncRead+AsyncWrite+Debug+Send+Sync+Unpin` 的对象安全别名，方便 `Box<dyn _>`。
trait AsyncWriteReadDebug: AsyncRead + AsyncWrite + Send + Sync + Unpin + fmt::Debug {}
impl<T> AsyncWriteReadDebug for T where T: AsyncRead + AsyncWrite + Send + Sync + Unpin + fmt::Debug {}

/// SMTP 投递的工作超时——隧道建立 + 会话整体计入 [`DEFAULT_TIMEOUT`]。
async fn with_proxy_timeout<T>(
    proxy_url: &str,
    fut: impl std::future::Future<Output = std::result::Result<T, String>>,
) -> std::result::Result<T, String> {
    match tokio::time::timeout(DEFAULT_TIMEOUT, fut).await {
        Ok(result) => result,
        Err(_) => Err(format!(
            "经代理 {} 连接超时（{} 秒内未完成）",
            acmecast_core::proxy_log_target(proxy_url),
            DEFAULT_TIMEOUT.as_secs()
        )),
    }
}

/// 经代理隧道投递已构建的邮件.
///
/// `proxy_url` 为已归一化的有效代理 URL。成功返回 `Ok(())`，失败返回
/// 已含代理主机（剥 userinfo）的错误原因。
pub async fn send_via_proxy(
    settings: &SmtpSettings,
    proxy_url: &str,
    mail: Message,
) -> std::result::Result<(), String> {
    let proxy_for_timeout = proxy_url.to_owned();
    let proxy_for_inner = proxy_url.to_owned();
    let settings = settings.clone();
    with_proxy_timeout(&proxy_for_timeout.clone(), async move {
        send_via_proxy_inner(&settings, &proxy_for_inner, mail).await
    })
    .await
}

async fn send_via_proxy_inner(
    settings: &SmtpSettings,
    proxy_url: &str,
    mail: Message,
) -> std::result::Result<(), String> {
    let host = settings.host.trim().to_owned();
    let port = settings.effective_port();
    let tunnel = connect_through_proxy(proxy_url, &host, port)
        .await
        .map_err(|cause| format!("代理隧道建立失败: {cause}"))?;

    match settings.tls {
        TlsMode::Implicit => {
            // 连接后立即 TLS：先在隧道流上做 TLS 握手，再交 lettre EHLO。
            let tls_stream = tls_wrap(tunnel, &host).await.map_err(|cause| {
                format!(
                    "经代理 {} 隐式 TLS 握手失败: {cause}",
                    acmecast_core::proxy_log_target(proxy_url)
                )
            })?;
            let boxed: Box<dyn AsyncTokioStream> = Box::new(TokioStream(tls_stream));
            smtp_session(settings, boxed, mail).await
        }
        TlsMode::StartTls | TlsMode::None => {
            let boxed: Box<dyn AsyncTokioStream> = Box::new(TokioStream(tunnel));
            let mut conn = AsyncSmtpConnection::connect_with_transport(boxed, &ClientId::default())
                .await
                .map_err(|error| format!("SMTP 连接建立失败: {error}"))?;
            if settings.tls == TlsMode::StartTls {
                let params = TlsParameters::new_rustls(host.clone())
                    .map_err(|error| format!("TLS 参数构建失败: {error}"))?;
                conn.starttls(params, &ClientId::default())
                    .await
                    .map_err(|error| format!("STARTTLS 失败: {error}"))?;
            }
            smtp_auth_and_send(settings, &mut conn, mail).await
        }
    }
}

async fn smtp_session(
    settings: &SmtpSettings,
    stream: Box<dyn AsyncTokioStream>,
    mail: Message,
) -> std::result::Result<(), String> {
    let mut conn = AsyncSmtpConnection::connect_with_transport(stream, &ClientId::default())
        .await
        .map_err(|error| format!("SMTP 连接建立失败: {error}"))?;
    smtp_auth_and_send(settings, &mut conn, mail).await
}

async fn smtp_auth_and_send(
    settings: &SmtpSettings,
    conn: &mut AsyncSmtpConnection,
    mail: Message,
) -> std::result::Result<(), String> {
    if let (Some(username), Some(password)) = (&settings.username, &settings.password) {
        conn.auth(
            &[Mechanism::Plain, Mechanism::Login],
            &Credentials::new(username.clone(), password.clone()),
        )
        .await
        .map_err(|error| format!("SMTP 认证失败: {error}"))?;
    }
    let envelope = mail.envelope().clone();
    let bytes = mail.formatted();
    conn.send(&envelope, &bytes)
        .await
        .map_err(|error| format!("SMTP 投递失败: {error}"))?;
    Ok(())
}

/// 在代理 URL 与目标之间建隧道，返回可用于 SMTP 会话的流。
async fn connect_through_proxy(
    proxy_url: &str,
    target_host: &str,
    target_port: u16,
) -> std::result::Result<Box<dyn AsyncWriteReadDebug>, String> {
    let parsed = url::Url::parse(proxy_url).map_err(|_| {
        format!(
            "代理 URL `{}` 无法解析",
            acmecast_core::proxy_log_target(proxy_url)
        )
    })?;
    match parsed.scheme() {
        "socks5" => connect_via_socks5(&parsed, target_host, target_port).await,
        "http" => connect_via_http(&parsed, target_host, target_port, false).await,
        "https" => connect_via_http(&parsed, target_host, target_port, true).await,
        other => Err(format!("不支持的代理 scheme `{other}`")),
    }
}

/// SOCKS5 隧道。
async fn connect_via_socks5(
    parsed: &url::Url,
    target_host: &str,
    target_port: u16,
) -> std::result::Result<Box<dyn AsyncWriteReadDebug>, String> {
    let proxy_host = parsed
        .host_str()
        .ok_or_else(|| "代理 URL 缺少 host".to_owned())?;
    let proxy_port = parsed.port().unwrap_or(1080);
    let proxy_addr = format!("{proxy_host}:{proxy_port}");
    let username = parsed.username().to_owned();
    let password = parsed.password().unwrap_or("").to_owned();
    let target = (target_host.to_owned(), target_port);
    let stream: Box<dyn AsyncWriteReadDebug> = if username.is_empty() {
        let s = tokio_socks::tcp::Socks5Stream::connect(proxy_addr.as_str(), target)
            .await
            .map_err(|error| format!("{error}"))?;
        Box::new(s)
    } else {
        let s = tokio_socks::tcp::Socks5Stream::connect_with_password(
            proxy_addr.as_str(),
            target,
            username.as_str(),
            password.as_str(),
        )
        .await
        .map_err(|error| format!("{error}"))?;
        Box::new(s)
    };
    Ok(stream)
}

/// HTTP/HTTPS CONNECT 隧道.
///
/// `secure_to_proxy` 为真时先对代理做 TLS（`https://` scheme）。
async fn connect_via_http(
    parsed: &url::Url,
    target_host: &str,
    target_port: u16,
    secure_to_proxy: bool,
) -> std::result::Result<Box<dyn AsyncWriteReadDebug>, String> {
    let proxy_host = parsed
        .host_str()
        .ok_or_else(|| "代理 URL 缺少 host".to_owned())?;
    let proxy_host_owned = proxy_host.to_owned();
    let proxy_port = parsed
        .port()
        .unwrap_or(if secure_to_proxy { 443 } else { 8080 });
    let proxy_addr = format!("{proxy_host_owned}:{proxy_port}");

    let tcp = tokio::net::TcpStream::connect(&proxy_addr)
        .await
        .map_err(|error| format!("连接代理 {proxy_host_owned}:{proxy_port} 失败: {error}"))?;

    // https 代理：先 TLS 到代理本身。
    let mut stream: Box<dyn AsyncWriteReadDebug> = if secure_to_proxy {
        let tls = tls_wrap(
            Box::new(tcp) as Box<dyn AsyncWriteReadDebug>,
            &proxy_host_owned,
        )
        .await
        .map_err(|cause| {
            format!(
                "到代理 {} 的 TLS 失败: {cause}",
                acmecast_core::proxy_log_target(parsed.as_str())
            )
        })?;
        Box::new(tls)
    } else {
        Box::new(tcp)
    };

    // CONNECT 请求。
    let mut request = format!(
        "CONNECT {target_host}:{target_port} HTTP/1.1\r\nHost: {target_host}:{target_port}\r\n"
    );
    if !parsed.username().is_empty() {
        let password = parsed.password().unwrap_or("");
        let creds = format!("{}:{password}", parsed.username());
        let encoded =
            base64::Engine::encode(&base64::engine::general_purpose::STANDARD, creds.as_bytes());
        request.push_str(&format!("Proxy-Authorization: Basic {encoded}\r\n"));
    }
    request.push_str("\r\n");

    use tokio::io::AsyncWriteExt as _;
    stream
        .write_all(request.as_bytes())
        .await
        .map_err(|error| format!("发送 CONNECT 请求失败: {error}"))?;
    stream
        .flush()
        .await
        .map_err(|error| format!("CONNECT 刷新失败: {error}"))?;

    // 读响应头直到 `\r\n\r\n`。
    use tokio::io::AsyncReadExt as _;
    let mut header = Vec::new();
    let mut buf = [0u8; 1];
    loop {
        stream
            .read_exact(&mut buf)
            .await
            .map_err(|error| format!("读取代理响应失败: {error}"))?;
        header.push(buf[0]);
        if header.ends_with(b"\r\n\r\n") {
            break;
        }
        if header.len() > 8192 {
            return Err("代理响应头过长".to_owned());
        }
    }
    let header_text = String::from_utf8_lossy(&header);
    if !is_connect_success(&header_text) {
        let snippet = header_text.chars().take(200).collect::<String>();
        return Err(format!("代理拒绝 CONNECT：{snippet}"));
    }
    Ok(stream)
}

fn is_connect_success(header: &str) -> bool {
    // 首行如 `HTTP/1.1 200 Connection Established` 或 `HTTP/1.0 200 OK`
    if let Some(first_line) = header.lines().next() {
        let parts: Vec<&str> = first_line.split_whitespace().collect();
        if parts.len() >= 2 && parts[1].starts_with('2') {
            return true;
        }
    }
    false
}

/// 在给定流上做 rustls 客户端 TLS 握手，返回 `TlsStream`。
async fn tls_wrap<S>(
    stream: S,
    server_name: &str,
) -> std::result::Result<tokio_rustls::client::TlsStream<S>, String>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + Sync + fmt::Debug + 'static,
{
    let mut roots = tokio_rustls::rustls::RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let provider = Arc::new(tokio_rustls::rustls::crypto::ring::default_provider());
    let config = tokio_rustls::rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|error| format!("TLS 配置失败: {error}"))?
        .with_root_certificates(roots)
        .with_no_client_auth();
    let connector = tokio_rustls::TlsConnector::from(Arc::new(config));
    let server_name: tokio_rustls::rustls::pki_types::ServerName<'static> = server_name
        .to_owned()
        .try_into()
        .map_err(|_| format!("非法 TLS 服务器名: {server_name}"))?;
    connector
        .connect(server_name, stream)
        .await
        .map_err(|error| format!("TLS 握手失败: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connect_success_parses_status_line() {
        assert!(is_connect_success(
            "HTTP/1.1 200 Connection Established\r\n\r\n"
        ));
        assert!(is_connect_success(
            "HTTP/1.0 200 OK\r\nServer: squid\r\n\r\n"
        ));
        assert!(!is_connect_success(
            "HTTP/1.1 407 Proxy Authentication Required\r\n\r\n"
        ));
        assert!(!is_connect_success("HTTP/1.1 500 error\r\n\r\n"));
    }
}
