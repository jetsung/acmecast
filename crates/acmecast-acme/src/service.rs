//! ACME 服务门面。
//!
//! [`AcmeService`] 是上层唯一需要接触的类型：它持有底层账号与 HTTP 客户端，
//! 对外只暴露项目自己的类型。底层 `instant-acme` 的类型不出现在任何公开签名里。

use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use bytes::Bytes;
use http_body_util::BodyExt;
use instant_acme::{
    Account, BytesBody, BytesResponse, HttpClient, Identifier, NewOrder, Order, RetryPolicy,
    RevocationRequest,
};
use reqwest::Client as ReqwestClient;

use crate::account::{AccountCredentials, EstablishAccountInput};
use crate::error::{AcmeError, Result};
use crate::order::{PendingOrder, retry_policy};

/// 默认的等待 CA 确认超时。
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(120);
/// 默认首次重试延迟。
const DEFAULT_INITIAL_DELAY: Duration = Duration::from_millis(500);

/// 等待 CA 确认的重试配置。
///
/// 对应 spec 中「轮询采用指数退避且总时长可配置」的要求。
#[derive(Debug, Clone, Copy)]
pub struct RetryConfig {
    /// 首次重试前的延迟。
    pub initial_delay: Duration,
    /// 每次重试后延迟的放大倍数。
    pub backoff: f32,
    /// 总超时；超过后轮询以超时错误结束。
    pub timeout: Duration,
}

impl Default for RetryConfig {
    fn default() -> Self {
        Self {
            initial_delay: DEFAULT_INITIAL_DELAY,
            backoff: 2.0,
            timeout: DEFAULT_TIMEOUT,
        }
    }
}

impl RetryConfig {
    fn to_policy(self) -> RetryPolicy {
        retry_policy(self.initial_delay, self.backoff, self.timeout)
    }
}

/// 代理配置。
///
/// 未配置时直连；配置了则所有 ACME 请求经该代理发出。
/// 这让无法直连公网的环境仍能完成验证。
#[derive(Debug, Clone, Default)]
pub struct ProxyConfig {
    /// HTTP 代理，形如 `http://host:port`。
    pub http: Option<String>,
    /// HTTPS 代理。
    pub https: Option<String>,
    /// SOCKS5 代理（同时覆盖 http/https）。
    pub socks5: Option<String>,
    /// 跳过服务器证书校验。
    ///
    /// 仅用于连接自签测试 CA（如 pebble）；生产 CA 一律 `false`，
    /// 关闭校验等于放弃对 CA 身份的确认。
    pub accept_invalid_certs: bool,
}

impl ProxyConfig {
    /// 是否配置了任一代理。
    #[must_use]
    pub fn is_enabled(&self) -> bool {
        self.http.is_some() || self.https.is_some() || self.socks5.is_some()
    }
}

/// 一次性的 HTTP 传输层。
///
/// 存在的意义是让**测试**注入进程内替身（见 `testing::MockAcme`）；生产路径不需要它，
/// [`AcmeService`] 默认走 reqwest。
///
/// 之所以要多这层包装而不是直接暴露底层 trait：底层是第三方库的 `HttpClient`，
/// 按本 crate 的防腐层约定不该越过边界。调用方写 `Transport::new(mock)` 即可，
/// 不必知道它背后是谁的 trait——泛型约束留在本 crate 内部解析。
pub struct Transport(Box<dyn HttpClient>);

impl Transport {
    /// 把任意传输实现装箱。
    pub fn new<H>(client: H) -> Self
    where
        H: HttpClient + 'static,
    {
        Self(Box::new(client))
    }

    /// 交给底层客户端消费。
    pub(crate) fn into_inner(self) -> Box<dyn HttpClient> {
        self.0
    }
}

impl std::fmt::Debug for Transport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // 不打印底层对象：它不是给用户看的，也不该出现在日志里。
        f.write_str("Transport")
    }
}

/// ACME 服务。
pub struct AcmeService {
    account: Account,
    retry: RetryPolicy,
}

impl std::fmt::Debug for AcmeService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // 账号内含私钥，绝不参与格式化输出。
        f.debug_struct("AcmeService")
            .field("account", &"<redacted>")
            .finish()
    }
}

impl AcmeService {
    /// 建立账号并连上 CA。
    ///
    /// 三种模式（注册 / 绑定 / 复用）由 [`EstablishAccountInput::mode`] 决定。
    pub async fn establish(
        input: &EstablishAccountInput<'_>,
        proxy: Option<&ProxyConfig>,
    ) -> Result<(Self, AccountCredentials)> {
        let http = build_http_client(proxy)?;
        let (account, credentials) = crate::account::establish(input, Some(http)).await?;
        Ok((
            Self {
                account,
                retry: default_retry_policy(),
            },
            credentials,
        ))
    }

    /// 用调用方提供的传输层建立账号。
    ///
    /// 生产路径走 [`AcmeService::establish`]，它会依据代理配置自建客户端；
    /// 本方法供测试注入 mock 服务器使用。
    pub async fn establish_with_transport(
        input: &EstablishAccountInput<'_>,
        transport: Transport,
    ) -> Result<Self> {
        let (account, _credentials) =
            crate::account::establish(input, Some(transport.into_inner())).await?;
        Ok(Self {
            account,
            retry: default_retry_policy(),
        })
    }

    /// 用已持久化的凭据与指定的传输层连接 CA。
    ///
    /// 与 [`AcmeService::from_credentials`] 同义，只是换成调用方给的传输层——
    /// 供测试注入 mock，无需让被测代码知道它连着谁。
    pub async fn from_credentials_with_transport(
        credentials: &AccountCredentials,
        transport: Transport,
    ) -> Result<Self> {
        let inner = credentials.to_inner()?;
        let account = Account::builder_with_http(transport.into_inner())
            .from_credentials(inner)
            .await
            .map_err(|e| AcmeError::Account(format!("恢复账号失败: {e}")))?;

        Ok(Self {
            account,
            retry: default_retry_policy(),
        })
    }

    /// 用已持久化的凭据连接 CA，不产生任何注册请求。
    pub async fn from_credentials(
        credentials: &AccountCredentials,
        proxy: Option<&ProxyConfig>,
    ) -> Result<Self> {
        let http = build_http_client(proxy)?;
        let inner = credentials.to_inner()?;
        let account = Account::builder_with_http(http)
            .from_credentials(inner)
            .await
            .map_err(|e| AcmeError::Account(format!("恢复账号失败: {e}")))?;

        Ok(Self {
            account,
            retry: default_retry_policy(),
        })
    }

    /// 为一组域名创建订单。
    ///
    /// 域名中的通配符会被保留为独立标识符（`*.example.com`），
    /// CA 侧据此判定只能走 DNS-01。
    pub async fn new_order(&self, domains: &[String]) -> Result<PendingOrder> {
        if domains.is_empty() {
            return Err(AcmeError::Order("订单域名列表不能为空".to_owned()));
        }

        // RFC 8555 §7.1.3：通配符以 `*.` 前缀的完整值作为 DNS identifier 下单；
        // 只有 CA 返回的**授权对象**才剥前缀（identifier 为 base domain + wildcard
        // 标记）。下单时剥掉前缀会造出一张非通配订单，finalize 时 CSR 的
        // `*.example.com` SAN 与订单标识符对不上，CA 直接拒绝。
        let identifiers: Vec<Identifier> = domains
            .iter()
            .map(|d| Identifier::Dns(d.trim().to_owned()))
            .collect();

        let order: Order = self
            .account
            .new_order(&NewOrder::new(&identifiers))
            .await
            .map_err(|e| AcmeError::Order(format!("创建订单失败: {e}")))?;

        Ok(PendingOrder::new(order, self.retry))
    }

    /// 为一组域名创建订单，并指定 CA 提供的证书 profile（preferred chain）。
    pub async fn new_order_with_profile(
        &self,
        domains: &[String],
        profile: &str,
    ) -> Result<PendingOrder> {
        let identifiers: Vec<Identifier> = domains
            .iter()
            .map(|d| Identifier::Dns(d.trim().to_owned()))
            .collect();

        let order: Order = self
            .account
            .new_order(&NewOrder::new(&identifiers).profile(profile))
            .await
            .map_err(|e| AcmeError::Order(format!("创建订单失败（profile={profile}）: {e}")))?;

        Ok(PendingOrder::new(order, self.retry))
    }

    /// 吊销证书。
    ///
    /// `cert_der` 为 DER 编码的待吊销证书。签名使用**本服务持有的账号**完成——
    /// 调用方必须提供与签发时一致的账号凭据，本方法不会去反查流水线。
    pub async fn revoke(&self, cert_der: &[u8]) -> Result<()> {
        let certificate = rustls_pki_types::CertificateDer::from(cert_der.to_vec());
        self.account
            .revoke(&RevocationRequest {
                certificate: &certificate,
                reason: None,
            })
            .await
            .map_err(|e| AcmeError::Revocation(format!("{e}")))
    }

    /// 指定原因吊销证书。
    pub async fn revoke_with_reason(
        &self,
        cert_der: &[u8],
        reason: instant_acme::RevocationReason,
    ) -> Result<()> {
        let certificate = rustls_pki_types::CertificateDer::from(cert_der.to_vec());
        self.account
            .revoke(&RevocationRequest {
                certificate: &certificate,
                reason: Some(reason),
            })
            .await
            .map_err(|e| AcmeError::Revocation(format!("{e}")))
    }

    /// 把 RFC 5280 的吊销原因代码映射为协议枚举；越界返回 `None`。
    #[must_use]
    pub fn revocation_reason(code: u8) -> Option<instant_acme::RevocationReason> {
        use instant_acme::RevocationReason as R;
        match code {
            0 => Some(R::Unspecified),
            1 => Some(R::KeyCompromise),
            2 => Some(R::CaCompromise),
            3 => Some(R::AffiliationChanged),
            4 => Some(R::Superseded),
            5 => Some(R::CessationOfOperation),
            6 => Some(R::CertificateHold),
            8 => Some(R::RemoveFromCrl),
            9 => Some(R::PrivilegeWithdrawn),
            10 => Some(R::AaCompromise),
            _ => None,
        }
    }

    /// 覆盖默认的重试配置。
    ///
    /// 主要用于测试缩短等待时间，以及为内网 CA 放宽超时。
    #[must_use]
    pub fn with_retry_config(mut self, config: RetryConfig) -> Self {
        self.retry = config.to_policy();
        self
    }

    /// 本服务用于签名的账号 ID。
    #[must_use]
    pub fn account_id(&self) -> &str {
        self.account.id()
    }
}

fn default_retry_policy() -> RetryPolicy {
    RetryConfig::default().to_policy()
}

/// 依据代理配置构造 HTTP 客户端。
///
/// 即使未配置代理也要自建客户端：默认的 hyper 客户端不支持代理注入，
/// 统一走这里可让两条路径行为一致。
fn build_http_client(proxy: Option<&ProxyConfig>) -> Result<Box<dyn HttpClient>> {
    let mut builder = ReqwestClient::builder().use_rustls_tls();

    if let Some(config) = proxy {
        // 仅在显式配置时关闭证书校验：自签测试 CA（pebble）的场景。
        if config.accept_invalid_certs {
            builder = builder.danger_accept_invalid_certs(true);
        }
        if let Some(socks) = &config.socks5 {
            builder = builder.proxy(
                reqwest::Proxy::all(socks)
                    .map_err(|e| AcmeError::Transport(format!("SOCKS5 代理地址非法: {e}")))?,
            );
        }
        if let Some(http_proxy) = &config.http {
            builder = builder.proxy(
                reqwest::Proxy::http(http_proxy)
                    .map_err(|e| AcmeError::Transport(format!("HTTP 代理地址非法: {e}")))?,
            );
        }
        if let Some(https_proxy) = &config.https {
            builder = builder.proxy(
                reqwest::Proxy::https(https_proxy)
                    .map_err(|e| AcmeError::Transport(format!("HTTPS 代理地址非法: {e}")))?,
            );
        }
    }

    let client = builder
        .build()
        .map_err(|e| AcmeError::Transport(format!("构造 HTTP 客户端失败: {e}")))?;

    Ok(Box::new(ReqwestHttpClient { client }))
}

/// 基于 reqwest 的 ACME HTTP 客户端实现。
struct ReqwestHttpClient {
    client: ReqwestClient,
}

impl HttpClient for ReqwestHttpClient {
    fn request(
        &self,
        req: http::Request<instant_acme::BodyWrapper<Bytes>>,
    ) -> Pin<Box<dyn Future<Output = std::result::Result<BytesResponse, instant_acme::Error>> + Send>>
    {
        let client = self.client.clone();
        Box::pin(async move {
            let (parts, body) = req.into_parts();
            let payload = body
                .collect()
                .await
                .map_err(|e| instant_acme::Error::Other(Box::new(e)))?
                .to_bytes();

            let method = reqwest::Method::from_bytes(parts.method.as_str().as_bytes())
                .map_err(|e| instant_acme::Error::Other(Box::new(e)))?;
            let url = parts.uri.to_string();

            let mut request = client.request(method, &url);
            for (name, value) in parts.headers.iter() {
                request = request.header(name.as_str(), value.as_bytes());
            }

            let response = request
                .body(payload.to_vec())
                .send()
                .await
                .map_err(|e| instant_acme::Error::Other(Box::new(e)))?;

            let status = response.status().as_u16();
            let headers = response.headers().clone();
            let body_bytes = response
                .bytes()
                .await
                .map_err(|e| instant_acme::Error::Other(Box::new(e)))?;

            let mut builder = http::Response::builder().status(status);
            for (name, value) in headers.iter() {
                builder = builder.header(name.as_str(), value.as_bytes());
            }
            let http_response = builder
                .body(body_bytes)
                .map_err(instant_acme::Error::Http)?;

            let (resp_parts, resp_body) = http_response.into_parts();
            Ok(BytesResponse {
                parts: resp_parts,
                body: Box::new(resp_body) as Box<dyn BytesBody>,
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn new_order_keeps_wildcard_prefix_in_identifier() {
        // RFC 8555 §7.1.3：通配符下单时 identifier 必须带 `*.` 前缀，与 CSR 的
        // SAN 一致；剥掉前缀会让 finalize 被以「CSR does not specify same
        // identifiers as Order」拒绝。
        let mock = MockAcme::new();
        let service = service_on(&mock).await;
        service.new_order(&domains()).await.unwrap();

        let payload = mock.payload_for("/new-order").expect("应记录到下单请求");
        assert!(
            payload.contains("*.example.com"),
            "下单请求必须保留通配符前缀，实际: {payload}"
        );
    }

    #[test]
    fn proxy_config_disabled_by_default() {
        assert!(!ProxyConfig::default().is_enabled());
    }

    #[test]
    fn proxy_config_detects_any_proxy() {
        assert!(
            ProxyConfig {
                socks5: Some("socks5://127.0.0.1:1080".to_owned()),
                ..Default::default()
            }
            .is_enabled()
        );
    }

    #[test]
    fn invalid_proxy_address_is_rejected() {
        let config = ProxyConfig {
            http: Some("not a url".to_owned()),
            ..Default::default()
        };
        match build_http_client(Some(&config)) {
            Ok(_) => panic!("非法代理地址应被拒绝"),
            Err(err) => assert!(err.to_string().contains("代理"), "{err}"),
        }
    }

    #[tokio::test]
    async fn direct_client_builds_without_proxy() {
        assert!(build_http_client(None).is_ok());
    }

    #[tokio::test]
    async fn socks_proxy_client_builds() {
        let config = ProxyConfig {
            socks5: Some("socks5://127.0.0.1:1080".to_owned()),
            ..Default::default()
        };
        assert!(build_http_client(Some(&config)).is_ok());
    }

    #[test]
    fn retry_config_defaults_are_sane() {
        let config = RetryConfig::default();
        assert!(config.timeout >= config.initial_delay);
        assert!(config.backoff >= 1.0);
    }

    #[test]
    fn retry_config_is_overridable() {
        let config = RetryConfig {
            initial_delay: Duration::from_millis(5),
            backoff: 1.0,
            timeout: Duration::from_millis(50),
        };
        assert_eq!(config.timeout, Duration::from_millis(50));
    }

    // ---- 以下用例通过 mock ACME 服务器验证 spec 3.7 / 3.8 ----

    use crate::account::AccountMode;
    use crate::testing::{BASE, MockAcme};

    /// 建立一个连到 mock 的服务。
    async fn service_on(mock: &MockAcme) -> AcmeService {
        let directory_url = format!("{BASE}/directory");
        let input = EstablishAccountInput {
            directory_url: &directory_url,
            mode: AccountMode::Create {
                contacts: vec!["mailto:ops@example.com".to_owned()],
            },
            external_account: None,
        };
        AcmeService::establish_with_transport(&input, Transport::new(mock.clone()))
            .await
            .expect("mock 上建立账号应成功")
            .with_retry_config(RetryConfig {
                initial_delay: Duration::from_millis(1),
                backoff: 1.0,
                timeout: Duration::from_millis(300),
            })
    }

    fn domains() -> Vec<String> {
        vec!["example.com".to_owned(), "*.example.com".to_owned()]
    }

    // ---- 3.7 badNonce 自动重试 ----

    #[tokio::test]
    async fn bad_nonce_is_retried_and_eventually_succeeds() {
        // 前两次 POST 返回 badNonce，第三次应成功。
        let mock = MockAcme::new().with_bad_nonce(2);
        let _service = service_on(&mock).await;

        assert_eq!(
            mock.count_for("/new-account"),
            3,
            "badNonce 应触发重试：共 3 次尝试（2 次失败 + 1 次成功）"
        );
    }

    #[tokio::test]
    async fn bad_nonce_retries_are_bounded() {
        // 一直返回 badNonce 时不得无限重试。
        let mock = MockAcme::new().with_bad_nonce(100);
        let directory_url = format!("{BASE}/directory");
        let input = EstablishAccountInput {
            directory_url: &directory_url,
            mode: AccountMode::Create { contacts: vec![] },
            external_account: None,
        };

        let result =
            AcmeService::establish_with_transport(&input, Transport::new(mock.clone())).await;
        assert!(result.is_err(), "重试耗尽后应报错");

        // 底层库的重试上限为 3 次尝试。
        assert_eq!(
            mock.count_for("/new-account"),
            3,
            "重试次数必须有上限，实际 {}",
            mock.count_for("/new-account")
        );
    }

    // ---- 3.7 invalid 与超时的错误详情 ----

    #[tokio::test]
    async fn invalid_authorization_is_surfaced_with_status() {
        let mock = MockAcme::new().with_authz_status("invalid");
        let service = service_on(&mock).await;

        let mut order = service
            .new_order(&["example.com".to_owned()])
            .await
            .unwrap();
        let authz = order.authorizations().await.unwrap();

        assert!(!authz.is_empty());
        assert!(
            authz
                .iter()
                .any(|a| matches!(a.state, crate::order::AuthorizationStateKind::Invalid)),
            "CA 返回的 invalid 授权应被如实映射，实际: {:?}",
            authz.iter().map(|a| a.state).collect::<Vec<_>>()
        );
    }

    #[tokio::test]
    async fn timeout_is_reported_when_order_stuck_in_processing() {
        // 订单永远停在 processing，轮询应以超时结束。
        let mock = MockAcme::new()
            .with_order_status_after_finalize("processing")
            .with_processing_refreshes(usize::MAX);
        let service = service_on(&mock).await;

        let mut order = service.new_order(&domains()).await.unwrap();
        let err = order
            .finalize_and_download(&[0x30, 0x00])
            .await
            .expect_err("订单卡在 processing 时应超时");

        let text = err.to_string();
        assert!(
            matches!(err, AcmeError::Timeout(_)),
            "应返回超时错误，实际: {text}"
        );
        assert!(text.contains("超时"), "{text}");
    }

    // ---- 3.8 CSR 提交与证书链下载 ----

    #[tokio::test]
    async fn finalize_downloads_the_full_chain() {
        let chain = "-----BEGIN CERTIFICATE-----\nLEAF\n-----END CERTIFICATE-----\n\
                     -----BEGIN CERTIFICATE-----\nINTERMEDIATE\n-----END CERTIFICATE-----\n";
        let mock = MockAcme::new()
            .with_order_status_after_finalize("valid")
            .with_cert_pem(chain);
        let service = service_on(&mock).await;

        let mut order = service.new_order(&domains()).await.unwrap();
        let pem = order.finalize_and_download(&[0x30, 0x00]).await.unwrap();

        assert_eq!(
            pem.matches("BEGIN CERTIFICATE").count(),
            2,
            "应下载到含中间证书的完整链: {pem}"
        );
        assert_eq!(mock.count_for("/cert/1"), 1, "证书应只下载一次");
    }

    #[tokio::test]
    async fn csr_is_submitted_exactly_once_across_polling() {
        // 订单在 processing 停留若干次刷新后才 valid。
        let mock = MockAcme::new()
            .with_order_status_after_finalize("processing")
            .with_processing_refreshes(2);
        let service = service_on(&mock).await;

        let mut order = service.new_order(&domains()).await.unwrap();
        order.finalize_and_download(&[0x30, 0x00]).await.unwrap();

        assert_eq!(mock.finalize_calls(), 1, "轮询期间不得重复提交 CSR");
        assert!(
            mock.count_for("/order/1") >= 2,
            "processing 状态应触发刷新轮询，实际 {} 次",
            mock.count_for("/order/1")
        );
    }

    // ---- 3.7 挑战应答 ----

    #[tokio::test]
    async fn challenge_can_be_marked_ready() {
        let mock = MockAcme::new();
        let service = service_on(&mock).await;

        let mut order = service.new_order(&domains()).await.unwrap();
        let authz = order.authorizations().await.unwrap();
        let identifier = authz[0].identifier.clone();

        order
            .set_challenge_ready(&identifier, crate::order::ChallengeKind::Dns01)
            .await
            .expect("通知 CA 校验应成功");

        assert_eq!(mock.count_for("/chall/1"), 1);
    }

    #[tokio::test]
    async fn unknown_identifier_is_rejected_before_contacting_ca() {
        let mock = MockAcme::new();
        let service = service_on(&mock).await;

        let mut order = service.new_order(&domains()).await.unwrap();
        let err = order
            .set_challenge_ready("not-in-order.test", crate::order::ChallengeKind::Dns01)
            .await
            .expect_err("不存在的域名应报错");

        assert!(err.to_string().contains("找不到"), "{err}");
        assert_eq!(mock.count_for("/chall/1"), 0, "不应发出挑战请求");
    }

    #[tokio::test]
    async fn order_without_domains_is_rejected_locally() {
        let mock = MockAcme::new();
        let service = service_on(&mock).await;

        let err = service.new_order(&[]).await.expect_err("空域名应被拒绝");
        assert!(err.to_string().contains("不能为空"), "{err}");
        assert_eq!(mock.count_for("/new-order"), 0, "不应发出订单请求");
    }

    // ---- 3.7 挑战应答材料（spec：HTTP-01 提供令牌与键值授权串，DNS-01 提供 TXT 值）----

    #[tokio::test]
    async fn dns_challenge_provides_txt_value() {
        let mock = MockAcme::new();
        let service = service_on(&mock).await;

        let mut order = service.new_order(&domains()).await.unwrap();
        let materials = order
            .challenge_materials("example.com", crate::order::ChallengeKind::Dns01)
            .await
            .expect("应取得 DNS-01 应答材料");

        assert!(
            !materials.key_authorization.is_empty(),
            "键值授权串不能为空——它是要投放的实际内容"
        );
        assert!(!materials.dns_txt_value.is_empty(), "DNS TXT 值不能为空");
    }

    #[tokio::test]
    async fn dns_txt_value_matches_independently_computed_sha256() {
        // 独立按 RFC 8555 §8.4 复算：TXT 值 = base64url_nopad(sha256(key_authorization))。
        // 这验证的是计算结果本身，而不是「调用了同一个函数」。
        use base64::Engine;
        use sha2::{Digest, Sha256};

        let mock = MockAcme::new();
        let service = service_on(&mock).await;

        let mut order = service.new_order(&domains()).await.unwrap();
        let materials = order
            .challenge_materials("example.com", crate::order::ChallengeKind::Dns01)
            .await
            .unwrap();

        let expected = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(Sha256::digest(materials.key_authorization.as_bytes()));

        assert_eq!(
            materials.dns_txt_value, expected,
            "DNS TXT 值应为键值授权串的 SHA-256 摘要"
        );

        // SHA-256 的 base64url 无填充编码固定为 43 个字符。
        assert_eq!(materials.dns_txt_value.len(), 43);
        assert!(
            !materials.dns_txt_value.contains('='),
            "TXT 值不应带 base64 填充"
        );
    }

    #[tokio::test]
    async fn key_authorization_is_token_dot_key_thumbprint() {
        // 形状应为 `{token}.{账号密钥指纹}`，即至少包含一个点且以 token 开头。
        let mock = MockAcme::new();
        let service = service_on(&mock).await;

        let mut order = service.new_order(&domains()).await.unwrap();
        let authz = order.authorizations().await.unwrap();
        let dns_token = authz
            .iter()
            .find(|a| a.identifier == "example.com")
            .and_then(|a| a.challenge(crate::order::ChallengeKind::Dns01))
            .map(|c| c.token.clone())
            .expect("example.com 应有 dns-01 挑战");

        // 重新下单取材料（authorizations 已消费掉本轮的句柄）。
        let mut order = service.new_order(&domains()).await.unwrap();
        let materials = order
            .challenge_materials("example.com", crate::order::ChallengeKind::Dns01)
            .await
            .unwrap();

        assert!(
            materials
                .key_authorization
                .starts_with(&format!("{dns_token}.")),
            "键值授权串应以 `{{token}}.` 开头，实际: {}",
            materials.key_authorization
        );
        assert!(
            materials.key_authorization.contains('.'),
            "应包含 token 与指纹的分隔点"
        );
    }

    #[tokio::test]
    async fn http_challenge_provides_key_authorization() {
        let mock = MockAcme::new();
        let service = service_on(&mock).await;

        let mut order = service.new_order(&domains()).await.unwrap();
        let materials = order
            .challenge_materials("wildcard.example.com", crate::order::ChallengeKind::Http01)
            .await
            .expect("应取得 HTTP-01 应答材料");

        assert!(!materials.key_authorization.is_empty());
        // HTTP-01 与 DNS-01 共用同一份键值授权串，区别只在投放位置。
        assert_eq!(
            materials.dns_txt_value.len(),
            43,
            "TXT 值同样可被算出，只是 HTTP-01 用不到"
        );
    }

    #[tokio::test]
    async fn challenge_materials_reject_unsupported_kind() {
        // `example.com` 的授权只提供 dns-01。
        let mock = MockAcme::new();
        let service = service_on(&mock).await;

        let mut order = service.new_order(&domains()).await.unwrap();
        let err = order
            .challenge_materials("example.com", crate::order::ChallengeKind::Http01)
            .await
            .expect_err("该域名不支持 http-01，应报错");

        let text = err.to_string();
        assert!(text.contains("http-01"), "错误应指明缺少哪种挑战: {text}");
        assert!(text.contains("example.com"), "错误应指明是哪个域名: {text}");
    }

    #[tokio::test]
    async fn challenge_materials_reject_unknown_identifier() {
        let mock = MockAcme::new();
        let service = service_on(&mock).await;

        let mut order = service.new_order(&domains()).await.unwrap();
        let err = order
            .challenge_materials("not-in-order.test", crate::order::ChallengeKind::Dns01)
            .await
            .expect_err("订单中不存在的域名应报错");

        assert!(err.to_string().contains("找不到"), "{err}");
    }

    #[tokio::test]
    async fn challenge_info_is_listed_without_account_key() {
        // 基本挑战信息（类型/令牌/路径）无需账号密钥，应在 authorizations 里直接可见。
        let mock = MockAcme::new();
        let service = service_on(&mock).await;

        let mut order = service.new_order(&domains()).await.unwrap();
        let authz = order.authorizations().await.unwrap();

        let dns = authz
            .iter()
            .find(|a| a.identifier == "example.com")
            .and_then(|a| a.challenge(crate::order::ChallengeKind::Dns01))
            .expect("应有 dns-01 挑战");

        assert_eq!(dns.token, "mock-token");
        assert_eq!(dns.http_path, "/.well-known/acme-challenge/mock-token");
        assert_eq!(
            crate::order::ChallengeInfo::dns_record_name("example.com"),
            "_acme-challenge.example.com"
        );
    }

    #[tokio::test]
    async fn wildcard_is_passed_as_plain_identifier() {
        let mock = MockAcme::new().with_authz_status("valid");
        let service = service_on(&mock).await;

        let mut order = service.new_order(&domains()).await.unwrap();
        let authz = order.authorizations().await.unwrap();

        // 命中 valid 授权时规格要求跳过验证——此处确认状态被如实反映。
        assert!(authz.iter().all(|a| a.is_valid()));
    }
}
