//! 邮件渠道适配器：把统一事件消息经 SMTP 投递到收件人邮箱。
//!
//! 与 webhook 适配器共用 [`crate::message::NotificationMessage`] 素材：
//! 主题取事件标题，正文复用纯文本渲染。连接方式由配置的 [`TlsMode`]
//! 决定（隐式 TLS / STARTTLS / 明文），端口缺省随模式取 465 / 587 / 25；
//! 超时与重试沿用 [`crate::delivery`] 的统一语义，适配器只做一次投递尝试。

use async_trait::async_trait;
use lettre::{
    AsyncSmtpTransport, AsyncTransport, Tokio1Executor,
    message::Mailbox,
    message::header::ContentType,
    transport::smtp::authentication::{Credentials, Mechanism},
};

use crate::channel::{ChannelConfig, NotificationChannel};
use crate::error::{Error, Result};
use crate::message::NotificationMessage;

/// 邮件主题前缀：与 IM 渠道的文本前缀保持同一辨识度。
pub(crate) const SUBJECT_PREFIX: &str = "【acmecast】";

/// SMTP 连接的 TLS 模式。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TlsMode {
    /// 连接建立即 TLS（隐式 TLS，典型端口 465）。
    #[default]
    Implicit,
    /// 先建立明文连接，经 STARTTLS 升级（典型端口 587）。
    StartTls,
    /// 不加密（仅限内网调试）。
    None,
}

impl TlsMode {
    /// 从配置字符串解析：`implicit` / `starttls` / `none`。
    #[must_use]
    pub fn from_id(id: &str) -> Option<Self> {
        match id.trim() {
            "implicit" => Some(Self::Implicit),
            "starttls" => Some(Self::StartTls),
            "none" => Some(Self::None),
            _ => None,
        }
    }

    /// 未显式配置端口时的缺省端口。
    #[must_use]
    pub fn default_port(self) -> u16 {
        match self {
            Self::Implicit => 465,
            Self::StartTls => 587,
            Self::None => 25,
        }
    }
}

/// 邮件渠道的 SMTP 连接设置。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SmtpSettings {
    /// SMTP 服务器地址（不含端口与协议前缀）。
    pub host: String,
    /// 端口；`None` 时随 [`Self::tls`] 模式取缺省值。
    pub port: Option<u16>,
    /// 发件人地址。
    pub from: String,
    /// 收件人地址列表，至少一项。
    pub to: Vec<String>,
    /// 认证用户名；与 `password` 同时配置才启用认证。
    pub username: Option<String>,
    /// 认证密码。
    pub password: Option<String>,
    /// TLS 连接模式，缺省 [`TlsMode::Implicit`]。
    pub tls: TlsMode,
}

impl SmtpSettings {
    /// 实际生效的端口：显式配置优先，否则随 TLS 模式取缺省。
    #[must_use]
    pub fn effective_port(&self) -> u16 {
        self.port.unwrap_or(self.tls.default_port())
    }

    /// 是否启用 SMTP 认证（用户名与密码同时在场）。
    #[must_use]
    pub fn has_credentials(&self) -> bool {
        self.username.is_some() && self.password.is_some()
    }

    /// 轻量校验：与 webhook 渠道的 url 前缀检查同粒度，不追完整 RFC。
    ///
    /// # Errors
    ///
    /// host 为空、收件人列表为空、地址缺 `@` 或 `@` 两段有空段、
    /// 端口为 0、认证凭据只有一半时返回错误说明。
    pub fn validate(&self) -> std::result::Result<(), String> {
        if self.host.trim().is_empty() {
            return Err("host 不能为空".to_owned());
        }
        if let Some(port) = self.port
            && port == 0
        {
            return Err("port 不能为 0".to_owned());
        }
        if !is_emailish(&self.from) {
            return Err(format!("非法发件人地址: {}", self.from));
        }
        if self.to.is_empty() {
            return Err("to 不能为空（至少一个收件人地址）".to_owned());
        }
        for addr in &self.to {
            if !is_emailish(addr) {
                return Err(format!("非法收件人地址: {addr}"));
            }
        }
        if self.username.is_some() != self.password.is_some() {
            return Err("username 与 password 必须同时配置".to_owned());
        }
        Ok(())
    }
}

/// 邮箱地址的形态检查：含一个 `@` 且两段非空。
fn is_emailish(addr: &str) -> bool {
    let addr = addr.trim();
    match addr.split_once('@') {
        Some((local, domain)) => !local.is_empty() && !domain.is_empty(),
        None => false,
    }
}

/// 邮件渠道：经 SMTP 发送统一事件消息。
#[derive(Debug, Default)]
pub struct EmailChannel;

#[async_trait]
impl NotificationChannel for EmailChannel {
    fn provider(&self) -> &'static str {
        "email"
    }

    async fn send(
        &self,
        _client: &reqwest::Client,
        channel: &ChannelConfig,
        message: &NotificationMessage,
    ) -> Result<()> {
        let Some(settings) = &channel.smtp else {
            return Err(Error::Delivery {
                name: channel.name.clone(),
                cause: "渠道缺少 SMTP 设置（host/from/to 等）".to_owned(),
            });
        };
        let mail = build_message(settings, message).map_err(|cause| Error::Delivery {
            name: channel.name.clone(),
            cause,
        })?;
        let transport = build_transport(settings);
        transport
            .send(mail)
            .await
            .map(|_| ())
            .map_err(|error| Error::Delivery {
                name: channel.name.clone(),
                cause: format!("SMTP 投递失败: {error}"),
            })
    }
}

/// 组装邮件：主题取事件标题，正文复用统一纯文本渲染。
///
/// 收件人逐个 `to` 附加（一次会话送达全部收件人）；地址解析错误带上
/// 原文返回，便于对照配置排障。
pub(crate) fn build_message(
    settings: &SmtpSettings,
    message: &NotificationMessage,
) -> std::result::Result<lettre::message::Message, String> {
    let from: Mailbox = settings
        .from
        .trim()
        .parse()
        .map_err(|error| format!("非法发件人地址 {}: {error}", settings.from))?;
    let mut builder = lettre::message::Message::builder()
        .from(from)
        .subject(format!("{SUBJECT_PREFIX}{}", message.title));
    for addr in &settings.to {
        let mailbox: Mailbox = addr
            .trim()
            .parse()
            .map_err(|error| format!("非法收件人地址 {addr}: {error}"))?;
        builder = builder.to(mailbox);
    }
    builder
        .header(ContentType::TEXT_PLAIN)
        .body(message.render_text())
        .map_err(|error| format!("邮件构建失败: {error}"))
}

/// 按 TLS 模式构建 SMTP 传输：implicit → relay、starttls → starttls_relay、
/// none → builder_dangerous；端口与认证凭据按配置覆盖。
///
/// 构建只准备参数，不发起连接；连接、握手与会话超时挂在客户端上，
/// 上限与 webhook 投递一致（[`crate::delivery::DEFAULT_TIMEOUT`]）。
pub(crate) fn build_transport(settings: &SmtpSettings) -> AsyncSmtpTransport<Tokio1Executor> {
    let host = settings.host.trim();
    let mut builder = match settings.tls {
        TlsMode::Implicit => AsyncSmtpTransport::<Tokio1Executor>::relay(host),
        TlsMode::StartTls => AsyncSmtpTransport::<Tokio1Executor>::starttls_relay(host),
        TlsMode::None => Ok(AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(host)),
    }
    .unwrap_or_else(|error| {
        panic!("SMTP 传输参数构建失败（host={host}，tls={:?}）: {error}", settings.tls)
    });
    builder = builder
        .port(settings.effective_port())
        .authentication(vec![Mechanism::Plain, Mechanism::Login])
        .timeout(Some(crate::delivery::DEFAULT_TIMEOUT));
    if let (Some(username), Some(password)) = (&settings.username, &settings.password) {
        builder = builder.credentials(Credentials::new(username.clone(), password.clone()));
    }
    builder.build()
}

#[cfg(test)]
mod tests {
    use super::*;
    use lettre::transport::stub::AsyncStubTransport;

    /// 取出原始邮件的正文部分并做 base64 解码（lettre 对非 ASCII 正文
    /// 自动选用 base64 传输编码，断言前先还原）。
    fn decoded_body(raw: &str) -> String {
        let (_, body) = raw.split_once("\r\n\r\n").expect("头部与正文应以空行分隔");
        let compact: String = body.chars().filter(|c| !c.is_whitespace()).collect();
        use base64::Engine as _;
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(compact.as_bytes())
            .expect("正文应为 base64 编码");
        String::from_utf8(decoded).expect("正文解码后应为 UTF-8")
    }

    fn sample_message() -> NotificationMessage {
        NotificationMessage {
            event: crate::EVENT_CERT_APPLY.to_owned(),
            title: "证书申请成功".to_owned(),
            pipeline: "example-pipeline".to_owned(),
            trigger: "cron".to_owned(),
            occurred_at: chrono::Utc::now(),
            domains: vec!["a.example.com".to_owned()],
            target: None,
        }
    }

    fn settings() -> SmtpSettings {
        SmtpSettings {
            host: "smtp.example.com".to_owned(),
            port: None,
            from: "no-reply@example.com".to_owned(),
            to: vec!["ops@example.com".to_owned()],
            username: Some("no-reply@example.com".to_owned()),
            password: Some("secret".to_owned()),
            tls: TlsMode::Implicit,
        }
    }

    #[test]
    fn from_id_covers_three_modes_only() {
        assert_eq!(TlsMode::from_id("implicit"), Some(TlsMode::Implicit));
        assert_eq!(TlsMode::from_id(" starttls "), Some(TlsMode::StartTls));
        assert_eq!(TlsMode::from_id("none"), Some(TlsMode::None));
        assert_eq!(TlsMode::from_id("ssl"), None);
        assert_eq!(TlsMode::from_id(""), None);
    }

    #[test]
    fn default_port_follows_tls_mode() {
        assert_eq!(TlsMode::default(), TlsMode::Implicit);
        assert_eq!(TlsMode::Implicit.default_port(), 465);
        assert_eq!(TlsMode::StartTls.default_port(), 587);
        assert_eq!(TlsMode::None.default_port(), 25);
    }

    #[test]
    fn effective_port_prefers_explicit() {
        assert_eq!(settings().effective_port(), 465);
        let explicit = SmtpSettings {
            port: Some(2465),
            ..settings()
        };
        assert_eq!(explicit.effective_port(), 2465);
        let starttls = SmtpSettings {
            tls: TlsMode::StartTls,
            ..settings()
        };
        assert_eq!(starttls.effective_port(), 587);
    }

    #[test]
    fn credentials_need_both_halves() {
        assert!(settings().has_credentials());
        let anonymous = SmtpSettings {
            username: None,
            password: None,
            ..settings()
        };
        assert!(!anonymous.has_credentials());
        let half = SmtpSettings {
            password: None,
            ..settings()
        };
        assert!(!half.has_credentials());
    }

    #[test]
    fn validate_rejects_empty_host_and_empty_to() {
        let error = SmtpSettings {
            host: "  ".to_owned(),
            ..settings()
        }
        .validate()
        .unwrap_err();
        assert!(error.contains("host"), "{error}");

        let error = SmtpSettings {
            to: vec![],
            ..settings()
        }
        .validate()
        .unwrap_err();
        assert!(error.contains("to"), "{error}");
    }

    #[test]
    fn validate_rejects_non_emailish_addresses() {
        let error = SmtpSettings {
            from: "not-an-email".to_owned(),
            ..settings()
        }
        .validate()
        .unwrap_err();
        assert!(error.contains("发件人"), "{error}");

        let error = SmtpSettings {
            to: vec!["a@example.com".to_owned(), "@example.com".to_owned()],
            ..settings()
        }
        .validate()
        .unwrap_err();
        assert!(error.contains("收件人"), "{error}");

        let error = SmtpSettings {
            to: vec!["ops@".to_owned()],
            ..settings()
        }
        .validate()
        .unwrap_err();
        assert!(error.contains("收件人"), "{error}");
    }

    #[test]
    fn validate_rejects_zero_port_and_half_credentials() {
        let error = SmtpSettings {
            port: Some(0),
            ..settings()
        }
        .validate()
        .unwrap_err();
        assert!(error.contains("port"), "{error}");

        let error = SmtpSettings {
            password: None,
            ..settings()
        }
        .validate()
        .unwrap_err();
        assert!(error.contains("同时配置"), "{error}");
    }

    #[test]
    fn validate_accepts_anonymous_and_valid_settings() {
        let anonymous = SmtpSettings {
            username: None,
            password: None,
            ..settings()
        };
        assert!(anonymous.validate().is_ok());
        assert!(settings().validate().is_ok());
    }

    #[test]
    fn build_message_carries_subject_recipients_and_text_body() {
        let config = SmtpSettings {
            to: vec!["ops@example.com".to_owned(), "b@example.com".to_owned()],
            ..settings()
        };
        let mail = build_message(&config, &sample_message()).expect("合法素材应能构建邮件");
        let raw = String::from_utf8_lossy(&mail.formatted()).into_owned();
        assert!(raw.contains("Subject: =?utf-8?"), "中文主题应被编码: {raw}");
        assert!(raw.contains("ops@example.com"), "{raw}");
        assert!(raw.contains("b@example.com"), "{raw}");
        assert!(raw.contains("no-reply@example.com"), "{raw}");
        assert!(raw.contains("Content-Type: text/plain; charset=utf-8"), "{raw}");
        let body = decoded_body(&raw);
        assert!(body.contains("证书申请成功"), "{body}");
        assert!(body.contains("流水线：example-pipeline"), "{body}");
        assert!(body.contains("域名：a.example.com"), "{body}");
    }

    #[test]
    fn build_message_surfaces_bad_address() {
        let config = SmtpSettings {
            from: "ops@".to_owned(),
            ..settings()
        };
        let error = build_message(&config, &sample_message()).unwrap_err();
        assert!(error.contains("发件人"), "{error}");
    }

    #[test]
    fn build_transport_supports_all_tls_modes() {
        for tls in [TlsMode::Implicit, TlsMode::StartTls, TlsMode::None] {
            let config = SmtpSettings { tls, ..settings() };
            let _ = build_transport(&config);
        }
    }

    #[tokio::test]
    async fn stub_transport_delivers_assembled_message() {
        let config = SmtpSettings {
            to: vec!["ops@example.com".to_owned(), "b@example.com".to_owned()],
            ..settings()
        };
        let mail = build_message(&config, &sample_message()).expect("合法素材应能构建邮件");
        let transport = AsyncStubTransport::new_ok();
        transport.send(mail).await.expect("stub 应投递成功");

        let messages = transport.messages().await;
        assert_eq!(messages.len(), 1);
        let (envelope, raw) = &messages[0];
        assert_eq!(
            envelope.from().map(ToString::to_string).as_deref(),
            Some("no-reply@example.com")
        );
        assert_eq!(envelope.to().len(), 2, "两个收件人应都在信封中");
        let body = decoded_body(raw);
        assert!(body.contains("流水线：example-pipeline"), "{body}");
    }

    #[tokio::test]
    async fn stub_transport_error_maps_to_delivery_error() {
        let config = settings();
        let mail = build_message(&config, &sample_message()).expect("合法素材应能构建邮件");
        let transport = AsyncStubTransport::new_error();
        let error = transport.send(mail).await.unwrap_err();
        assert!(!error.to_string().is_empty());
    }
}
