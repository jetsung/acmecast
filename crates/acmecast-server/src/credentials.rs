//! 内置 DNS 提供商与 SSH 主机的凭据类型。
//!
//! 字段定义直接复用领域 crate 里的结构体，避免「结构体改了、
//! schema 没改」的漂移。注册进凭据注册表后，前端就能创建这类凭据，
//! 流水线里的 `dns_credential_id` / SSH 档案引用下拉也才有可选项。
//!
//! 连通性测试：SSH 主机档案有一次真实的探测动作（连上主机认证一次）；
//! DNS 提供商沿用 trait 默认的 `NotTestable`——一次有效探测需要具体
//! zone，这里不猜一个域名去试。

use std::sync::Arc;

use acmecast_access::{ConnectivityOutcome, CredentialType};
use acmecast_deploy::{SshHostFields, probe_host, ssh_host_fields_schema};
use acmecast_dns::providers::aliyun::{AliyunCredentials, aliyun_credentials_schema};
use acmecast_dns::providers::cloudflare::{CloudflareCredentials, cloudflare_credentials_schema};
use acmecast_dns::providers::tencent::{TencentCredentials, tencent_credentials_schema};
use acmecast_dns::{
    AliyunProvider, CloudflareProvider, HttpTransport, ReqwestTransport, TencentEoProvider,
    TencentProvider,
};
use schemars::schema::RootSchema;

/// Cloudflare DNS 凭据类型（字段：`api_token`）。
///
/// `Default` 走真实网络；测试用 [`Self::with_transport`] 注入替身，
/// 让「可用 / 不可用」两条分支都能离线验证。
#[derive(Debug, Default)]
pub struct CloudflareCredentialType {
    /// 测试用传输层替身。
    transport: Option<Arc<dyn HttpTransport>>,
    /// 全局出网代理（`[proxy] url` / `ACMECAST_PROXY_URL`）；供
    /// `proxy_mode=inherit` 的凭据在连通性测试里继承。
    global_proxy_url: Option<String>,
}

impl CloudflareCredentialType {
    /// 注入传输层替身，供测试用。
    #[must_use]
    pub fn with_transport(transport: Arc<dyn HttpTransport>) -> Self {
        Self {
            transport: Some(transport),
            global_proxy_url: None,
        }
    }

    /// 设置全局出网代理，供 `proxy_mode=inherit` 的凭据继承。
    #[must_use]
    pub fn with_global_proxy(mut self, proxy_url: Option<impl Into<String>>) -> Self {
        self.global_proxy_url = proxy_url.map(Into::into);
        self
    }

    fn provider(&self) -> CloudflareProvider {
        let transport: Arc<dyn HttpTransport> = match &self.transport {
            Some(transport) => Arc::clone(transport),
            None => Arc::new(ReqwestTransport::default()),
        };
        CloudflareProvider::with_transport(transport).with_global_proxy(self.global_proxy_url.clone())
    }
}

#[async_trait::async_trait]
impl CredentialType for CloudflareCredentialType {
    fn type_id(&self) -> &'static str {
        // 与 `CloudflareProvider::type_id` 对齐：cert.apply 的凭据 `type_id` 即提供商标识。
        "cloud.cloudflare"
    }

    fn display_name(&self) -> &'static str {
        // 能力标签已表达用途，展示名不再用括号标注使用范围。
        "Cloudflare"
    }

    fn fields_schema(&self) -> RootSchema {
        // 走提供商的 schema 注入函数：proxy_mode 的 x-enum-labels 与
        // proxy_url 的 x-visible-when 联动都在那里统一挂上。
        cloudflare_credentials_schema()
    }

    fn validate(&self, fields: &serde_json::Value) -> acmecast_core::Result<()> {
        let parsed: CloudflareCredentials =
            serde_json::from_value(fields.clone()).map_err(|error| {
                acmecast_core::Error::validation("api_token", format!("字段不合法: {error}"))
            })?;
        if parsed.api_token.trim().is_empty() {
            return Err(acmecast_core::Error::validation(
                "api_token",
                "API Token 不能为空白",
            ));
        }
        validate_proxy_url_field(fields)
    }

    async fn test_connectivity(&self, fields: &serde_json::Value) -> ConnectivityOutcome {
        match self.provider().verify_credentials(fields).await {
            Ok(()) => ConnectivityOutcome::Ok,
            Err(error) => ConnectivityOutcome::Unavailable {
                // 外部报错可能回显请求里的密钥，先脱敏再交给用户。
                reason: self.redact(fields, &error.to_string()),
            },
        }
    }
}

/// 阿里云 DNS 凭据类型（字段：`access_key_id` / `access_key_secret`）。
#[derive(Debug, Default)]
pub struct AliyunCredentialType {
    /// 测试用传输层替身。
    transport: Option<Arc<dyn HttpTransport>>,
    /// 全局出网代理（`[proxy] url` / `ACMECAST_PROXY_URL`）；供
    /// `proxy_mode=inherit` 的凭据在连通性测试里继承。
    global_proxy_url: Option<String>,
}

impl AliyunCredentialType {
    /// 注入传输层替身，供测试用。
    #[must_use]
    pub fn with_transport(transport: Arc<dyn HttpTransport>) -> Self {
        Self {
            transport: Some(transport),
            global_proxy_url: None,
        }
    }

    /// 设置全局出网代理，供 `proxy_mode=inherit` 的凭据继承。
    #[must_use]
    pub fn with_global_proxy(mut self, proxy_url: Option<impl Into<String>>) -> Self {
        self.global_proxy_url = proxy_url.map(Into::into);
        self
    }

    fn provider(&self) -> AliyunProvider {
        let transport: Arc<dyn HttpTransport> = match &self.transport {
            Some(transport) => Arc::clone(transport),
            None => Arc::new(ReqwestTransport::default()),
        };
        AliyunProvider::with_transport(transport).with_global_proxy(self.global_proxy_url.clone())
    }
}

#[async_trait::async_trait]
impl CredentialType for AliyunCredentialType {
    fn type_id(&self) -> &'static str {
        // 与 `AliyunProvider::type_id` 对齐。
        "cloud.aliyun"
    }

    fn display_name(&self) -> &'static str {
        // 能力标签已表达用途，展示名不再用括号标注使用范围。
        "阿里云"
    }

    fn fields_schema(&self) -> RootSchema {
        aliyun_credentials_schema()
    }

    fn validate(&self, fields: &serde_json::Value) -> acmecast_core::Result<()> {
        let parsed: AliyunCredentials =
            serde_json::from_value(fields.clone()).map_err(|error| {
                acmecast_core::Error::validation("access_key_id", format!("字段不合法: {error}"))
            })?;
        if parsed.access_key_id.trim().is_empty() {
            return Err(acmecast_core::Error::validation(
                "access_key_id",
                "AccessKey ID 不能为空白",
            ));
        }
        if parsed.access_key_secret.trim().is_empty() {
            return Err(acmecast_core::Error::validation(
                "access_key_secret",
                "AccessKey Secret 不能为空白",
            ));
        }
        validate_proxy_url_field(fields)
    }

    async fn test_connectivity(&self, fields: &serde_json::Value) -> ConnectivityOutcome {
        match self.provider().verify_credentials(fields).await {
            Ok(()) => ConnectivityOutcome::Ok,
            Err(error) => ConnectivityOutcome::Unavailable {
                reason: self.redact(fields, &error.to_string()),
            },
        }
    }
}

/// 校验凭据的代理字段：`proxy_mode=custom` 时要求 `proxy_url` 非空且
/// scheme 合法（`socks5://` / `http://` / `https://` 且可解析），非法定位到
/// `proxy_url`；`direct` / `inherit` 放行（`proxy_url` 可空）。
///
/// 口径与提供商侧 `transport_for_credentials` 一致（同一校验函数），
/// 保证「保存时拦得住、运行期不炸」。存量凭据无 `proxy_mode` 时由
/// [`acmecast_dns::ProxyMode::from_credentials`] 按旧 `proxy_url` 有无归一化。
fn validate_proxy_url_field(fields: &serde_json::Value) -> acmecast_core::Result<()> {
    // 单一口径在 core：保存入口与各凭据类型 validate 共用同一实现。
    acmecast_core::validate_proxy_field(fields)
}

/// 腾讯云体系的凭据字段校验：云解析与 EdgeOne 共用一份字段定义，
/// 两个凭据类型只是标识不同，校验规则完全一致。
fn validate_tencent_fields(fields: &serde_json::Value) -> acmecast_core::Result<()> {
    let parsed: TencentCredentials = serde_json::from_value(fields.clone()).map_err(|error| {
        acmecast_core::Error::validation("secret_id", format!("字段不合法: {error}"))
    })?;
    if parsed.secret_id.trim().is_empty() {
        return Err(acmecast_core::Error::validation(
            "secret_id",
            "SecretId 不能为空白",
        ));
    }
    if parsed.secret_key.trim().is_empty() {
        return Err(acmecast_core::Error::validation(
            "secret_key",
            "SecretKey 不能为空白",
        ));
    }
    validate_proxy_url_field(fields)
}

/// 腾讯云凭据类型（字段：`secret_id` / `secret_key` / `account_site`；同一套账号密钥同时服务 DNS 提供商与 SSL 证书部署）。
#[derive(Debug, Default)]
pub struct TencentCredentialType {
    /// 测试用传输层替身。
    transport: Option<Arc<dyn HttpTransport>>,
    /// 全局出网代理（`[proxy] url` / `ACMECAST_PROXY_URL`）；供
    /// `proxy_mode=inherit` 的凭据在连通性测试里继承。
    global_proxy_url: Option<String>,
}

impl TencentCredentialType {
    /// 注入传输层替身，供测试用。
    #[must_use]
    pub fn with_transport(transport: Arc<dyn HttpTransport>) -> Self {
        Self {
            transport: Some(transport),
            global_proxy_url: None,
        }
    }

    /// 设置全局出网代理，供 `proxy_mode=inherit` 的凭据继承。
    #[must_use]
    pub fn with_global_proxy(mut self, proxy_url: Option<impl Into<String>>) -> Self {
        self.global_proxy_url = proxy_url.map(Into::into);
        self
    }

    fn provider(&self) -> TencentProvider {
        let transport: Arc<dyn HttpTransport> = match &self.transport {
            Some(transport) => Arc::clone(transport),
            None => Arc::new(ReqwestTransport::default()),
        };
        TencentProvider::with_transport(transport).with_global_proxy(self.global_proxy_url.clone())
    }
}

#[async_trait::async_trait]
impl CredentialType for TencentCredentialType {
    fn type_id(&self) -> &'static str {
        // 与 `TencentProvider::type_id` 对齐：cert.apply 的凭据 `type_id` 即提供商标识。
        "cloud.tencent"
    }

    fn display_name(&self) -> &'static str {
        // 能力标签已表达用途（含 DNS 与 SSL 部署），展示名不再用括号标注使用范围。
        "腾讯云"
    }

    fn fields_schema(&self) -> RootSchema {
        tencent_credentials_schema()
    }

    fn validate(&self, fields: &serde_json::Value) -> acmecast_core::Result<()> {
        validate_tencent_fields(fields)
    }

    async fn test_connectivity(&self, fields: &serde_json::Value) -> ConnectivityOutcome {
        match self.provider().verify_credentials(fields).await {
            Ok(()) => ConnectivityOutcome::Ok,
            Err(error) => ConnectivityOutcome::Unavailable {
                reason: self.redact(fields, &error.to_string()),
            },
        }
    }
}

/// 腾讯云 EdgeOne DNS 凭据类型（字段与腾讯云 DNS 相同，标识独立）。
#[derive(Debug, Default)]
pub struct TencentEoCredentialType {
    /// 测试用传输层替身。
    transport: Option<Arc<dyn HttpTransport>>,
    /// 全局出网代理（`[proxy] url` / `ACMECAST_PROXY_URL`）；供
    /// `proxy_mode=inherit` 的凭据在连通性测试里继承。
    global_proxy_url: Option<String>,
}

impl TencentEoCredentialType {
    /// 注入传输层替身，供测试用。
    #[must_use]
    pub fn with_transport(transport: Arc<dyn HttpTransport>) -> Self {
        Self {
            transport: Some(transport),
            global_proxy_url: None,
        }
    }

    /// 设置全局出网代理，供 `proxy_mode=inherit` 的凭据继承。
    #[must_use]
    pub fn with_global_proxy(mut self, proxy_url: Option<impl Into<String>>) -> Self {
        self.global_proxy_url = proxy_url.map(Into::into);
        self
    }

    fn provider(&self) -> TencentEoProvider {
        let transport: Arc<dyn HttpTransport> = match &self.transport {
            Some(transport) => Arc::clone(transport),
            None => Arc::new(ReqwestTransport::default()),
        };
        TencentEoProvider::with_transport(transport).with_global_proxy(self.global_proxy_url.clone())
    }
}

#[async_trait::async_trait]
impl CredentialType for TencentEoCredentialType {
    fn type_id(&self) -> &'static str {
        // 与 `TencentEoProvider::type_id` 对齐。
        "dns.tencent-eo"
    }

    fn display_name(&self) -> &'static str {
        // 能力标签已表达用途，展示名不再用括号标注使用范围。
        "腾讯云 EdgeOne"
    }

    fn fields_schema(&self) -> RootSchema {
        tencent_credentials_schema()
    }

    fn validate(&self, fields: &serde_json::Value) -> acmecast_core::Result<()> {
        validate_tencent_fields(fields)
    }

    async fn test_connectivity(&self, fields: &serde_json::Value) -> ConnectivityOutcome {
        match self.provider().verify_credentials(fields).await {
            Ok(()) => ConnectivityOutcome::Ok,
            Err(error) => ConnectivityOutcome::Unavailable {
                reason: self.redact(fields, &error.to_string()),
            },
        }
    }
}

/// SSH 主机凭据类型（SSH 主机档案：主机、用户、认证材料与远端默认值）。
#[derive(Debug, Default)]
pub struct SshHostCredentialType {
    /// 全局出网代理（`[proxy] url` / `ACMECAST_PROXY_URL`）；供
    /// `proxy_mode=inherit` 的凭据在连通性测试里继承。
    global_proxy_url: Option<String>,
}

impl SshHostCredentialType {
    /// 设置全局出网代理（`[proxy] url` / `ACMECAST_PROXY_URL`）。
    #[must_use]
    pub fn with_global_proxy(mut self, proxy_url: Option<impl Into<String>>) -> Self {
        self.global_proxy_url = proxy_url.map(Into::into);
        self
    }
}

#[async_trait::async_trait]
impl CredentialType for SshHostCredentialType {
    fn type_id(&self) -> &'static str {
        // 与 SSH 部署目标的 type_id 对齐：`cert.deploy` 的 SSH 配置引用它。
        "deploy.ssh"
    }

    fn display_name(&self) -> &'static str {
        // 能力标签已表达用途，展示名不再用括号标注使用范围。
        "SSH 主机"
    }

    fn fields_schema(&self) -> RootSchema {
        // `private_key` 的多行标注（`x-multiline`）在这份 schema 里统一注入。
        ssh_host_fields_schema()
    }

    fn validate(&self, fields: &serde_json::Value) -> acmecast_core::Result<()> {
        let parsed: SshHostFields = serde_json::from_value(fields.clone()).map_err(|error| {
            acmecast_core::Error::validation("fields", format!("字段不合法: {error}"))
        })?;
        parsed.validate()
    }

    async fn test_connectivity(&self, fields: &serde_json::Value) -> ConnectivityOutcome {
        let Ok(parsed) = serde_json::from_value::<SshHostFields>(fields.clone()) else {
            return ConnectivityOutcome::Unavailable {
                reason: "档案字段不合法，无法测试".to_owned(),
            };
        };
        // 探测的报错文本可能回显请求里的字段值，先过一遍脱敏再呈现。
        match probe_host(&parsed, self.global_proxy_url.as_deref()).await {
            Ok(()) => ConnectivityOutcome::Ok,
            Err(error) => ConnectivityOutcome::Unavailable {
                reason: self.redact(fields, &error.to_string()),
            },
        }
    }

    /// 只抹认证材料。
    ///
    /// 默认实现会把**所有**字段值原样出现的地方抹掉——host 也会中招，
    /// 「连不上哪台」就看不出来了。host/port/user 是用户刚填的连接信息，
    /// 不是秘密；真正要挡住的只有私钥、口令与代理 URL——后者允许
    /// `socks5://user:pass@host:port` 形态，内嵌的凭据不能进错误信息。
    fn redact(&self, fields: &serde_json::Value, message: &str) -> String {
        let mut redacted = message.to_owned();
        for secret in [
            fields
                .get("private_key")
                .and_then(serde_json::Value::as_str),
            fields.get("password").and_then(serde_json::Value::as_str),
            fields.get("proxy_url").and_then(serde_json::Value::as_str),
        ]
        .into_iter()
        .flatten()
        {
            // 太短的值不做替换：否则会把正常文本里的常见词一并抹掉。
            if secret.len() >= 4 {
                redacted = redacted.replace(secret, "***");
            }
        }
        redacted
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 按给定的响应体应答的传输层替身：探测的「可用 / 不可用」两条
    /// 分支都能在无外网的情况下验证。
    #[derive(Debug)]
    struct FakeTransport {
        body: &'static str,
    }

    #[async_trait::async_trait]
    impl HttpTransport for FakeTransport {
        async fn send(
            &self,
            _request: acmecast_dns::HttpRequest,
        ) -> acmecast_dns::Result<acmecast_dns::HttpResponse> {
            Ok(acmecast_dns::HttpResponse::new(200, self.body))
        }
    }

    /// 有效凭据时腾讯云返回的成功形态。
    const STS_OK: &str = r#"{"Response":{"AccountId":"1234","RequestId":"req-1"}}"#;
    /// 无效凭据时腾讯云返回的业务错误形态。
    const AUTH_FAILURE: &str = r#"{"Response":{"Error":{"Code":"AuthFailure.SecretIdNotFound","Message":"The SecretId does not exist"},"RequestId":"req-1"}}"#;
    /// 无效凭据时 Cloudflare 返回的错误形态。
    const CF_INVALID: &str = r#"{"success":false,"errors":[{"message":"Invalid request headers"}]}"#;
    /// 有效凭据时 Cloudflare 返回的成功形态。
    const CF_ACTIVE: &str = r#"{"success":true,"result":{"status":"active"}}"#;

    /// 类型标识必须与 DNS 提供商的 type_id 一致，否则凭据 `type_id` 推导的提供商对不上。
    #[test]
    fn type_ids_match_dns_providers() {
        assert_eq!(CloudflareCredentialType::default().type_id(), "cloud.cloudflare");
        assert_eq!(AliyunCredentialType::default().type_id(), "cloud.aliyun");
        assert_eq!(TencentCredentialType::default().type_id(), "cloud.tencent");
        assert_eq!(TencentEoCredentialType::default().type_id(), "dns.tencent-eo");
    }

    #[test]
    fn display_names_are_short_labels() {
        // 展示名去括号：用途由列表能力标签表达，下拉只显示短名称。
        assert_eq!(TencentCredentialType::default().display_name(), "腾讯云");
        assert_eq!(
            TencentEoCredentialType::default().display_name(),
            "腾讯云 EdgeOne"
        );
    }

    /// 端口 1 上没有代理服务，连接立即被拒——用来在无外网依赖下
    /// 验证「代理不可达」的错误归因。
    #[tokio::test]
    async fn connectivity_failure_through_a_dead_proxy_names_the_proxy() {
        let cloudflare = CloudflareCredentialType::default();
        let fields = serde_json::json!({
            "api_token": "cf-token",
            "proxy_mode": "custom",
            "proxy_url": "socks5://user:supersecret@127.0.0.1:1",
        });
        let acmecast_access::ConnectivityOutcome::Unavailable { reason } =
            cloudflare.test_connectivity(&fields).await
        else {
            panic!("代理不可达时应报不可用");
        };
        // 错误归因到代理主机，而不是伪装成凭据无效。
        assert!(
            reason.contains("经代理 127.0.0.1:1"),
            "错误应指向代理地址: {reason}"
        );
        // 代理 URL 内嵌的凭据经 redact 脱敏，不出现在错误信息里。
        assert!(!reason.contains("supersecret"), "内嵌凭据应被脱敏: {reason}");
    }

    /// 连通性测试走与正式调用相同的 `proxy_mode` transport：
    /// `inherit` 模式按全局代理构建，`direct` 模式不走代理。
    #[tokio::test]
    async fn connectivity_follows_the_credential_proxy_mode() {
        let cloudflare = CloudflareCredentialType::default();
        // direct：全局代理已配也不该经代理——直连会因无网络而失败，
        // 但错误里不该出现代理主机（无代理主机可指认）。
        let outcome = cloudflare
            .test_connectivity(&serde_json::json!({
                "api_token": "cf-token",
                "proxy_mode": "direct",
                "proxy_url": "socks5://user:supersecret@127.0.0.1:1",
            }))
            .await;
        let acmecast_access::ConnectivityOutcome::Unavailable { reason } = outcome else {
            panic!("直连应失败并给出原因");
        };
        assert!(
            !reason.contains("经代理"),
            "direct 模式不该出现代理归因: {reason}"
        );
        assert!(!reason.contains("supersecret"), "内嵌凭据应被脱敏: {reason}");
    }

    /// 保存校验：`proxy_mode=custom` 时校验 `proxy_url`，
    /// `direct` / `inherit` 放行；错误定位到 `proxy_url`。
    #[test]
    fn proxy_fields_are_validated_per_proxy_mode() {
        let cloudflare = CloudflareCredentialType::default();
        let validate = |fields: serde_json::Value| cloudflare.validate(&fields);

        // custom + 三种合法 scheme 均通过。
        for url in [
            "socks5://127.0.0.1:7890",
            "http://127.0.0.1:8080",
            "https://proxy.example.com",
        ] {
            validate(serde_json::json!({
                "api_token": "t", "proxy_mode": "custom", "proxy_url": url
            }))
            .unwrap_or_else(|e| panic!("{url} 应通过校验: {e}"));
        }

        // custom + 非法 scheme / 不可解析 → 拒绝并定位到 proxy_url。
        for url in ["ftp://127.0.0.1:21", "not-a-url"] {
            let error = validate(serde_json::json!({
                "api_token": "t", "proxy_mode": "custom", "proxy_url": url
            }))
            .expect_err("非法代理 URL 应被拒绝");
            let acmecast_core::Error::Validation { field, .. } = &error else {
                panic!("错误应定位到字段: {error:?}");
            };
            assert_eq!(field, "proxy_url", "错误应定位到 proxy_url: {error:?}");
            assert!(
                error.to_string().contains("socks5"),
                "应提示支持的 scheme: {error}"
            );
        }

        // custom 但 proxy_url 留空 → 拒绝（矛盾配置不能放行）。
        validate(serde_json::json!({
            "api_token": "t", "proxy_mode": "custom", "proxy_url": ""
        }))
        .expect_err("custom 缺 proxy_url 应被拒绝");

        // direct / inherit 放行（proxy_url 可空，配了也不拦）。
        for mode in ["direct", "inherit"] {
            validate(serde_json::json!({ "api_token": "t", "proxy_mode": mode }))
                .unwrap_or_else(|e| panic!("{mode} 应放行: {e}"));
        }
        validate(serde_json::json!({
            "api_token": "t", "proxy_mode": "direct", "proxy_url": "ftp://127.0.0.1:21"
        }))
        .expect("direct 不读 proxy_url，配了也不该拦");

        // 存量凭据（无 proxy_mode）：旧 proxy_url 有值 → custom 语义，同样校验。
        validate(serde_json::json!({
            "api_token": "t", "proxy_url": "ftp://127.0.0.1:21"
        }))
        .expect_err("存量旧 proxy_url 非法应被拒绝");
        validate(serde_json::json!({ "api_token": "t" })).expect("存量缺字段应视为 direct");
    }

    #[tokio::test]
    async fn tencent_probe_reports_ok_or_a_redacted_reason() {
        let ok = TencentCredentialType::with_transport(Arc::new(FakeTransport { body: STS_OK }));
        assert!(
            ok.test_connectivity(&serde_json::json!({
                "secret_id": "AKID", "secret_key": "tencent-secret", "account_site": "intl"
            }))
            .await
            .is_ok(),
            "有效凭据应判定可用"
        );

        let broken =
            TencentCredentialType::with_transport(Arc::new(FakeTransport { body: AUTH_FAILURE }));
        let outcome = broken
            .test_connectivity(&serde_json::json!({
                "secret_id": "AKID", "secret_key": "tencent-secret", "account_site": "intl"
            }))
            .await;
        assert!(!outcome.is_ok(), "无效凭据不应判定可用");
        let reason = outcome.reason().expect("不可用应给出原因");
        assert!(reason.contains("AuthFailure.SecretIdNotFound"), "{reason}");
        assert!(!reason.contains("tencent-secret"), "原因不得回显密钥: {reason}");
    }

    #[tokio::test]
    async fn cloudflare_probe_reports_ok_or_a_redacted_reason() {
        let ok = CloudflareCredentialType::with_transport(Arc::new(FakeTransport {
            body: CF_ACTIVE,
        }));
        assert!(
            ok.test_connectivity(&serde_json::json!({ "api_token": "cf-token" }))
                .await
                .is_ok(),
            "有效令牌应判定可用"
        );

        let broken = CloudflareCredentialType::with_transport(Arc::new(FakeTransport {
            body: CF_INVALID,
        }));
        let outcome = broken
            .test_connectivity(&serde_json::json!({ "api_token": "cf-token" }))
            .await;
        let reason = outcome.reason().expect("不可用应给出原因");
        assert!(reason.contains("Invalid request headers"), "{reason}");
        assert!(!reason.contains("cf-token"), "原因不得回显令牌: {reason}");
    }

    /// SSH 档案的标识必须与 SSH 部署目标一致，前端据此对齐语义。
    #[test]
    fn the_ssh_host_type_id_aligns_with_the_deploy_target() {
        assert_eq!(SshHostCredentialType::default().type_id(), "deploy.ssh");
        assert_eq!(SshHostCredentialType::default().display_name(), "SSH 主机");
    }

    #[test]
    fn cloudflare_requires_a_non_blank_token() {
        let err = CloudflareCredentialType::default()
            .validate(&serde_json::json!({ "api_token": "   " }))
            .expect_err("空白 token 应被拒绝");
        match err {
            acmecast_core::Error::Validation { field, .. } => assert_eq!(field, "api_token"),
            other => panic!("期望 Validation，实际 {other:?}"),
        }

        CloudflareCredentialType::default()
            .validate(&serde_json::json!({ "api_token": "token" }))
            .expect("非空 token 应通过");
    }

    #[test]
    fn aliyun_requires_both_halves() {
        AliyunCredentialType::default()
            .validate(&serde_json::json!({ "access_key_id": "id" }))
            .expect_err("缺 secret 应被拒绝");
        let err = AliyunCredentialType::default()
            .validate(&serde_json::json!({ "access_key_id": "id", "access_key_secret": " " }))
            .expect_err("空白 secret 应被拒绝");
        match err {
            acmecast_core::Error::Validation { field, .. } => {
                assert_eq!(field, "access_key_secret")
            }
            other => panic!("期望 Validation，实际 {other:?}"),
        }

        AliyunCredentialType::default()
            .validate(&serde_json::json!({ "access_key_id": "id", "access_key_secret": "secret" }))
            .expect("两半齐全应通过");
    }

    #[test]
    fn tencent_requires_both_halves_and_a_known_site() {
        TencentCredentialType::default()
            .validate(&serde_json::json!({ "secret_id": "id" }))
            .expect_err("缺 secret 应被拒绝");
        let err = TencentCredentialType::default()
            .validate(&serde_json::json!({ "secret_id": "id", "secret_key": " " }))
            .expect_err("空白 secret 应被拒绝");
        match err {
            acmecast_core::Error::Validation { field, .. } => assert_eq!(field, "secret_key"),
            other => panic!("期望 Validation，实际 {other:?}"),
        }

        // account_site 有默认值：缺省即国内站。
        TencentCredentialType::default()
            .validate(&serde_json::json!({ "secret_id": "id", "secret_key": "secret" }))
            .expect("缺省站点应通过");
        TencentCredentialType::default()
            .validate(&serde_json::json!({
                "secret_id": "id", "secret_key": "secret", "account_site": "intl"
            }))
            .expect("国际站应通过");

        let err = TencentCredentialType::default()
            .validate(&serde_json::json!({
                "secret_id": "id", "secret_key": "secret", "account_site": "eu"
            }))
            .expect_err("未知站点应被拒绝");
        // serde 的原始信息会指明合法取值（expected `cn` or `intl`）。
        assert!(
            err.to_string().contains("expected `cn` or `intl`"),
            "报错应指明合法取值: {err}"
        );
    }

    #[test]
    fn the_edgeone_credential_type_shares_the_field_validation() {
        // 与腾讯云 DNS 共用字段定义：标识独立，校验规则相同。
        TencentEoCredentialType::default()
            .validate(&serde_json::json!({ "secret_id": " ", "secret_key": "secret" }))
            .expect_err("空白 SecretId 应被拒绝");
        TencentEoCredentialType::default()
            .validate(&serde_json::json!({ "secret_id": "id", "secret_key": "secret" }))
            .expect("合法字段应通过");
    }

    #[test]
    fn a_valid_proxy_url_passes_validation() {
        // 合法三种 scheme 均放行（腾讯系校验即代表三处 DNS 凭据类型的口径）。
        for proxy in ["socks5://127.0.0.1:7890", "http://proxy.local:8080", "https://p.example.com"] {
            TencentCredentialType::default()
                .validate(&serde_json::json!({
                    "secret_id": "id", "secret_key": "secret", "proxy_url": proxy,
                }))
                .unwrap_or_else(|e| panic!("{proxy} 应通过校验: {e}"));
        }
    }

    #[test]
    fn an_invalid_proxy_url_is_rejected_and_located() {
        for proxy in ["ftp://127.0.0.1:21", "not-a-url"] {
            let error = TencentCredentialType::default()
                .validate(&serde_json::json!({
                    "secret_id": "id", "secret_key": "secret", "proxy_url": proxy,
                }))
                .expect_err("非法代理应被拒绝");
            let acmecast_core::Error::Validation { field, reason } = &error else {
                panic!("应为校验错误: {error}");
            };
            assert_eq!(field, "proxy_url", "错误应定位到 proxy_url 字段: {error}");
            assert!(reason.contains("socks5"), "应提示支持的 scheme: {error}");
        }
    }

    #[test]
    fn an_absent_or_blank_proxy_url_is_direct_connection() {
        for fields in [
            serde_json::json!({ "secret_id": "id", "secret_key": "secret" }),
            serde_json::json!({ "secret_id": "id", "secret_key": "secret", "proxy_url": "" }),
            serde_json::json!({ "secret_id": "id", "secret_key": "secret", "proxy_url": "  " }),
        ] {
            TencentCredentialType::default()
                .validate(&fields)
                .expect("缺字段或留空应视为直连放行");
        }
    }

    /// 一份可直接创建的档案字段。
    fn profile_fields() -> serde_json::Value {
        serde_json::json!({
            "host": "web-1.example.com",
            "user": "deploy",
            "private_key": "PRIVATE-KEY-MATERIAL",
        })
    }

    #[test]
    fn the_ssh_host_type_reuses_the_field_validation() {
        SshHostCredentialType::default()
            .validate(&profile_fields())
            .expect("完整档案应通过");

        let mut broken = profile_fields();
        broken["private_key"] = serde_json::Value::Null;
        let err = SshHostCredentialType::default()
            .validate(&broken)
            .expect_err("缺认证材料应被拒绝");
        match err {
            acmecast_core::Error::Validation { field, .. } => assert_eq!(field, "private_key"),
            other => panic!("期望 Validation，实际 {other:?}"),
        }

        let mut broken = profile_fields();
        broken["host"] = serde_json::json!("   ");
        let err = SshHostCredentialType::default()
            .validate(&broken)
            .expect_err("空白主机应被拒绝");
        match err {
            acmecast_core::Error::Validation { field, .. } => assert_eq!(field, "host"),
            other => panic!("期望 Validation，实际 {other:?}"),
        }

        let mut broken = profile_fields();
        broken["password"] = serde_json::json!("pw");
        SshHostCredentialType::default()
            .validate(&broken)
            .expect_err("两种认证材料同给应被拒绝");
    }

    #[test]
    fn the_ssh_host_schema_marks_the_private_key_multiline() {
        let rendered =
            serde_json::to_value(SshHostCredentialType::default().fields_schema()).expect("Schema 应可序列化");
        assert_eq!(
            rendered["properties"]["private_key"]["x-multiline"],
            serde_json::Value::Bool(true)
        );
    }

    #[test]
    fn the_ssh_host_schema_carries_only_profile_fields_with_form_defaults() {
        let rendered =
            serde_json::to_value(SshHostCredentialType::default().fields_schema()).expect("Schema 应可序列化");
        let properties = rendered["properties"]
            .as_object()
            .expect("schema 应有 properties");
        let mut names: Vec<_> = properties.keys().map(String::as_str).collect();
        names.sort_unstable();
        assert_eq!(
            names,
            vec![
                "cert_mode",
                "host",
                "key_mode",
                "password",
                "port",
                "private_key",
                "proxy_mode",
                "proxy_url",
                "user"
            ],
            "档案 schema 只应含档案字段（路径与重载命令属部署输入）: {names:?}"
        );
        for banned in ["cert_path", "key_path", "reload_command"] {
            assert!(
                !properties.contains_key(banned),
                "凭据 schema 不得出现部署字段 {banned}"
            );
        }
        // 表单缺省值随 schema 带出，供 SchemaForm 预填。
        assert_eq!(rendered["properties"]["user"]["default"], "root");
        assert_eq!(rendered["properties"]["cert_mode"]["default"], "0644");
        assert_eq!(rendered["properties"]["key_mode"]["default"], "0600");
    }

    #[tokio::test]
    async fn ssh_host_connectivity_reports_unreachable_targets_without_the_materials() {
        let mut fields = profile_fields();
        fields["host"] = serde_json::json!("127.0.0.1");
        fields["port"] = serde_json::json!(1);

        let outcome = SshHostCredentialType::default().test_connectivity(&fields).await;
        let ConnectivityOutcome::Unavailable { reason } = outcome else {
            panic!("不可达主机应报不可用，实际 {outcome:?}");
        };
        // host 不是秘密，保留它用户才知道测的是哪台；材料必须被抹掉。
        assert!(reason.contains("127.0.0.1:1"), "{reason}");
        assert!(
            !reason.contains("PRIVATE-KEY-MATERIAL"),
            "脱敏后的原因不得含认证材料: {reason}"
        );
    }

    #[test]
    fn the_ssh_host_redact_only_hides_auth_materials() {
        let fields = serde_json::json!({
            "host": "web-1.example.com",
            "user": "deploy",
            "password": "S3CRET-PASSWORD",
        });
        let message = "连接 web-1.example.com 失败，请求携 S3CRET-PASSWORD";
        let redacted = SshHostCredentialType::default().redact(&fields, message);
        assert!(redacted.contains("web-1.example.com"), "{redacted}");
        assert!(!redacted.contains("S3CRET-PASSWORD"), "{redacted}");
    }

    #[tokio::test]
    async fn ssh_host_connectivity_rejects_malformed_fields() {
        let outcome = SshHostCredentialType::default()
            .test_connectivity(&serde_json::json!({ "host": 1 }))
            .await;
        assert!(
            matches!(outcome, ConnectivityOutcome::Unavailable { .. }),
            "形态坏掉的字段应报不可用: {outcome:?}"
        );
    }
}
