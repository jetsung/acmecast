//! 可替换的 HTTP 传输层。
//!
//! 各厂商的 API 形状各异（认证方式、请求体、响应结构），但「发一个请求、
//! 拿一个响应」是共同的。把这层抽出来，好处是厂商实现可以在**没有网络**的
//! 情况下被验证：测试注入一个按脚本应答的替身，断言请求构造得对不对、
//! 响应解析得对不对。

use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;

use crate::error::{Error, Result};

/// 一次 HTTP 请求。
#[derive(Debug, Clone, PartialEq)]
pub struct HttpRequest {
    /// 方法，如 `GET` / `POST` / `DELETE`。
    pub method: String,
    /// 完整 URL（含查询串）。
    pub url: String,
    /// 请求头。用 `BTreeMap` 让顺序稳定，测试断言时不会因为乱序而假失败。
    pub headers: BTreeMap<String, String>,
    /// JSON 请求体；无体时为 `None`。
    pub body: Option<Value>,
}

impl HttpRequest {
    /// 建一个请求。
    #[must_use]
    pub fn new(method: impl Into<String>, url: impl Into<String>) -> Self {
        Self {
            method: method.into(),
            url: url.into(),
            headers: BTreeMap::new(),
            body: None,
        }
    }

    /// 追加一个请求头。
    #[must_use]
    pub fn with_header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.insert(name.into(), value.into());
        self
    }

    /// 带上 JSON 请求体。
    #[must_use]
    pub fn with_body(mut self, body: Value) -> Self {
        self.body = Some(body);
        self
    }
}

/// 一次 HTTP 响应。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpResponse {
    /// 状态码。
    pub status: u16,
    /// 响应体文本。
    ///
    /// 存文本而非 JSON：厂商在出错时未必回 JSON（网关返回的 HTML 错误页很常见），
    /// 解析失败时把原文保留下来，报错才有信息量。
    pub body: String,
}

impl HttpResponse {
    /// 建一个响应。
    #[must_use]
    pub fn new(status: u16, body: impl Into<String>) -> Self {
        Self {
            status,
            body: body.into(),
        }
    }

    /// 状态码是否表示成功。
    #[must_use]
    pub fn is_success(&self) -> bool {
        (200..300).contains(&self.status)
    }

    /// 把响应体解析成 JSON。
    ///
    /// 解析失败时报出的错误会带上响应体片段——排查时那段文本往往就是答案。
    pub fn json(&self) -> Result<Value> {
        serde_json::from_str(&self.body).map_err(|e| {
            Error::provider(format!(
                "响应不是合法 JSON（{}）：{}",
                e,
                truncate(&self.body, 200)
            ))
        })
    }
}

/// 按 RFC 3986 的 unreserved 集合做最小化百分号编码。
///
/// 只编码必要字符：记录名与摘要里出现的 `-`、`_`、`.` 都保持原样，
/// 免得测试断言里全是难以阅读的 `%2D`。
#[must_use]
pub fn encode(value: &str) -> String {
    value
        .bytes()
        .map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                char::from(byte).to_string()
            }
            _ => format!("%{byte:02X}"),
        })
        .collect()
}

/// 把长文本截断，避免把整个响应体塞进错误信息。
fn truncate(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        return text.to_owned();
    }
    let head: String = text.chars().take(limit).collect();
    format!("{head}…（共 {} 字符）", text.chars().count())
}

/// HTTP 传输层。
#[async_trait]
pub trait HttpTransport: Send + Sync + std::fmt::Debug {
    /// 发出一个请求。
    async fn send(&self, request: HttpRequest) -> Result<HttpResponse>;
}

// 代理原语已下沉共享 crate：凭据级三态、URL 校验与 schema 注入由
// 所有凭据类型（DNS 提供商、部署目标、ACME 账号）共用，口径单一来源。
// 这里 re-export 以兼容既有 `acmecast_dns::` 路径。
pub use acmecast_core::{
    ProxyMode, inject_proxy_field_annotations, proxy_log_target, proxy_url_from_credentials,
    validate_proxy_url,
};

/// 按凭据选传输层。
///
/// `proxy_mode=direct` 直连；`inherit` 读 `global`（全局代理，非空则走代理，
/// 未配仍直连）；`custom` 读凭据 `proxy_url`（校验后走代理）。
/// 三种模式都未取到代理时沿用 `fallback`（默认直连，或测试注入的伪传输层）。
///
/// `timeout` 施加到新建的 reqwest 客户端上（请求级超时），与部署链路的
/// `[network] timeout_secs` 同一口径；`fallback` 自身的超时在装配时已定。
pub fn transport_for_credentials(
    credentials: &Value,
    fallback: &Arc<dyn HttpTransport>,
    global: Option<&str>,
    timeout: std::time::Duration,
) -> Result<Arc<dyn HttpTransport>> {
    // custom 分支先取 owned 值（`proxy_url_from_credentials` 返回 `Option<String>`），
    // 另两个分支是借来的 `&str`，因此这里统一收成 owned。
    let proxy_url: Option<String> = match ProxyMode::from_credentials(credentials) {
        ProxyMode::Direct => None,
        ProxyMode::Inherit => global
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned),
        ProxyMode::Custom => proxy_url_from_credentials(credentials),
    };
    match proxy_url.as_deref() {
        Some(url) => Ok(Arc::new(ReqwestTransport::with_proxy(url, timeout)?)),
        // 三态都没取到代理（含 custom 但凭据 proxy_url 留空）→ 直连。
        // custom 留空本该在保存时就被 validate_proxy_url_field 拦下；这里
        // 直连而非报错，是为了让存量脏数据不至于让整条流水线崩在传输层。
        None => Ok(Arc::clone(fallback)),
    }
}

/// 走真实网络的传输层。
#[derive(Debug)]
pub struct ReqwestTransport {
    client: reqwest::Client,
    /// 代理主机（`host:port`，不含用户名密码）：请求失败时写进错误信息，
    /// 让「代理不可达」一眼可辨，而不是伪装成凭据无效。
    proxy_host: Option<String>,
}

impl ReqwestTransport {
    /// 建一个默认客户端（直连）。
    pub fn new(timeout: std::time::Duration) -> Result<Self> {
        let client = reqwest::Client::builder()
            .user_agent(concat!("acmecast/", env!("CARGO_PKG_VERSION")))
            .timeout(timeout)
            .build()
            .map_err(|e| Error::provider(format!("无法构造 HTTP 客户端: {e}")))?;
        Ok(Self {
            client,
            proxy_host: None,
        })
    }

    /// 建一个走指定代理的客户端。
    ///
    /// 支持 `socks5://`、`http://`、`https://`（`socks` feature 已启用）；
    /// 其余 scheme 或无法解析的 URL 被拒绝。超时从请求发送后开始计算。
    pub fn with_proxy(proxy_url: &str, timeout: std::time::Duration) -> Result<Self> {
        validate_proxy_url(proxy_url)?;
        let parsed = reqwest::Url::parse(proxy_url).expect("validate_proxy_url 已确保可解析");
        let proxy = reqwest::Proxy::all(proxy_url)
            .map_err(|e| Error::provider(format!("代理 URL `{proxy_url}` 不合法: {e}")))?;
        let client = reqwest::Client::builder()
            .user_agent(concat!("acmecast/", env!("CARGO_PKG_VERSION")))
            .timeout(timeout)
            .proxy(proxy)
            .build()
            .map_err(|e| Error::provider(format!("无法构造带代理的 HTTP 客户端: {e}")))?;
        let host = match parsed.port() {
            Some(port) => format!("{}:{port}", parsed.host_str().unwrap_or_default()),
            None => parsed.host_str().unwrap_or_default().to_owned(),
        };
        Ok(Self {
            client,
            proxy_host: Some(host),
        })
    }
}

impl Default for ReqwestTransport {
    fn default() -> Self {
        // 构造只可能因为 TLS 后端初始化失败而失败，那种情况下整个进程也跑不起来。
        Self::new(acmecast_core::NETWORK_TIMEOUT).expect("HTTP 客户端应能构造")
    }
}

#[async_trait]
impl HttpTransport for ReqwestTransport {
    async fn send(&self, request: HttpRequest) -> Result<HttpResponse> {
        let method = reqwest::Method::from_bytes(request.method.as_bytes())
            .map_err(|e| Error::provider(format!("HTTP 方法 `{}` 不合法: {e}", request.method)))?;

        let mut builder = self.client.request(method, &request.url);
        for (name, value) in &request.headers {
            builder = builder.header(name, value);
        }
        if let Some(body) = &request.body {
            builder = builder.json(body);
        }

        let response = builder
            .send()
            .await
            .map_err(|e| {
                let via = self
                    .proxy_host
                    .as_ref()
                    .map(|host| format!("（经代理 {host}）"))
                    .unwrap_or_default();
                Error::provider(format!("请求 {} 失败{via}: {e}", request.url))
            })?;

        let status = response.status().as_u16();
        let body = response
            .text()
            .await
            .map_err(|e| Error::provider(format!("读取响应失败: {e}")))?;

        Ok(HttpResponse { status, body })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_proxy_transport_is_built_from_a_valid_url() {
        for url in [
            "socks5://127.0.0.1:7890",
            "http://127.0.0.1:8080",
            "https://proxy.example.com:443",
        ] {
            ReqwestTransport::with_proxy(url, acmecast_core::NETWORK_TIMEOUT).unwrap_or_else(|e| panic!("{url} 应能构建: {e}"));
        }
    }

    #[test]
    fn an_invalid_proxy_url_is_rejected() {
        assert!(ReqwestTransport::with_proxy("not-a-url", acmecast_core::NETWORK_TIMEOUT).is_err());
        assert!(ReqwestTransport::with_proxy("ftp://127.0.0.1:21", acmecast_core::NETWORK_TIMEOUT).is_err());
    }

    #[test]
    fn credentials_with_a_proxy_url_resolve_to_proxy_transport() {
        let credentials = serde_json::json!({ "proxy_url": "socks5://127.0.0.1:7890" });
        assert_eq!(
            proxy_url_from_credentials(&credentials).as_deref(),
            Some("socks5://127.0.0.1:7890")
        );
        // 构建成功即选择了代理路径（非法 URL 会在构建时报错）。
        let fallback: Arc<dyn HttpTransport> = Arc::new(ReqwestTransport::default());
        transport_for_credentials(&credentials, &fallback, None, acmecast_core::NETWORK_TIMEOUT).expect("带代理应能构建 transport");
    }

    #[test]
    fn credentials_without_a_proxy_field_resolve_to_direct_connection() {
        let fallback: Arc<dyn HttpTransport> = Arc::new(ReqwestTransport::default());
        for credentials in [
            serde_json::json!({}),                 // 存量凭据缺字段
            serde_json::json!({ "proxy_url": "" }), // 留空
            serde_json::json!({ "proxy_url": "  " }),
        ] {
            assert_eq!(proxy_url_from_credentials(&credentials), None);
            // 返回的是 fallback 本身（直连），且不会因代理构建失败报错。
            transport_for_credentials(&credentials, &fallback, None, acmecast_core::NETWORK_TIMEOUT).expect("直连应直接复用注入层");
        }
    }

    /// 三态：direct 不继承全局、inherit 继承全局、custom 覆盖全局。
    #[test]
    fn proxy_mode_selects_the_transport_per_branch() {
        let global = Some("socks5://127.0.0.1:7890");
        let fallback: Arc<dyn HttpTransport> = Arc::new(ReqwestTransport::default());
        let timeout = acmecast_core::NETWORK_TIMEOUT;

        // direct：即使全局已配也直连（复用注入层）。
        assert_eq!(
            ProxyMode::from_credentials(&serde_json::json!({ "proxy_mode": "direct" })),
            ProxyMode::Direct
        );
        transport_for_credentials(
            &serde_json::json!({ "proxy_mode": "direct" }),
            &fallback,
            global,
            timeout,
        )
        .expect("direct 应直连");

        // inherit：全局已配 → 经代理；全局未配/留空 → 直连。
        assert_eq!(
            ProxyMode::from_credentials(&serde_json::json!({ "proxy_mode": "inherit" })),
            ProxyMode::Inherit
        );
        transport_for_credentials(
            &serde_json::json!({ "proxy_mode": "inherit" }),
            &fallback,
            global,
            timeout,
        )
        .expect("inherit 已配全局应能构建代理 transport");
        transport_for_credentials(
            &serde_json::json!({ "proxy_mode": "inherit" }),
            &fallback,
            None,
            timeout,
        )
        .expect("inherit 未配全局应直连");
        transport_for_credentials(
            &serde_json::json!({ "proxy_mode": "inherit" }),
            &fallback,
            Some("   "),
            timeout,
        )
        .expect("inherit 全局留空应直连");

        // custom：读凭据 proxy_url，覆盖全局。
        assert_eq!(
            ProxyMode::from_credentials(&serde_json::json!({
                "proxy_mode": "custom",
                "proxy_url": "http://other.proxy:3128"
            })),
            ProxyMode::Custom
        );
        transport_for_credentials(
            &serde_json::json!({
                "proxy_mode": "custom",
                "proxy_url": "http://other.proxy:3128"
            }),
            &fallback,
            global,
            timeout,
        )
        .expect("custom 应走凭据代理");
        // custom 但 proxy_url 留空：保存校验会拦；传输层退化为直连，
        // 不让存量脏数据把流水线崩在传输层。
        transport_for_credentials(
            &serde_json::json!({ "proxy_mode": "custom" }),
            &fallback,
            global,
            timeout,
        )
        .expect("custom 缺 proxy_url 应退化为直连而非报错");
    }

    /// 存量凭据：无 `proxy_mode` 时按旧 `proxy_url` 有无归一化。
    #[test]
    fn legacy_credentials_without_proxy_mode_are_normalized_on_read() {
        // 旧 proxy_url 有值 → custom + 该值。
        assert_eq!(
            ProxyMode::from_credentials(
                &serde_json::json!({ "proxy_url": "socks5://127.0.0.1:7890" })
            ),
            ProxyMode::Custom
        );
        // 无两字段，或 proxy_url 为空/空白 → direct。
        for credentials in [
            serde_json::json!({}),
            serde_json::json!({ "proxy_url": "" }),
            serde_json::json!({ "proxy_url": "  " }),
            serde_json::json!({ "api_token": "t" }),
        ] {
            assert_eq!(
                ProxyMode::from_credentials(&credentials),
                ProxyMode::Direct,
                "{credentials} 应归一化为 direct"
            );
        }
    }

    #[test]
    fn a_request_can_be_built_up() {
        let request = HttpRequest::new("POST", "https://api.example.com/x")
            .with_header("Authorization", "Bearer token")
            .with_body(serde_json::json!({ "a": 1 }));

        assert_eq!(request.method, "POST");
        assert_eq!(request.headers["Authorization"], "Bearer token");
        assert_eq!(request.body.unwrap()["a"], 1);
    }

    #[test]
    fn success_is_decided_by_the_status_range() {
        assert!(HttpResponse::new(200, "").is_success());
        assert!(HttpResponse::new(204, "").is_success());
        assert!(!HttpResponse::new(400, "").is_success());
        assert!(!HttpResponse::new(500, "").is_success());
    }

    #[test]
    fn a_json_body_parses() {
        let response = HttpResponse::new(200, r#"{"ok":true}"#);
        assert_eq!(response.json().unwrap()["ok"], true);
    }

    #[test]
    fn a_non_json_body_is_reported_with_context() {
        // 网关返回 HTML 错误页很常见，报错得带上原文才有信息量。
        let response = HttpResponse::new(502, "<html>Bad Gateway</html>");
        let err = response.json().expect_err("非 JSON 应报错");
        assert!(err.to_string().contains("Bad Gateway"), "{err}");
    }

    #[test]
    fn a_long_body_is_truncated_in_the_error() {
        let response = HttpResponse::new(200, "x".repeat(500));
        let err = response.json().expect_err("应报错");
        let text = err.to_string();
        assert!(text.contains('…'), "长文本应被截断: {text}");
        assert!(text.len() < 400, "截断后不该还这么长: {}", text.len());
    }
}
