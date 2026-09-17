//! 可替换的 HTTP 传输层。
//!
//! 各厂商的 API 形状各异（认证方式、请求体、响应结构），但「发一个请求、
//! 拿一个响应」是共同的。把这层抽出来，好处是厂商实现可以在**没有网络**的
//! 情况下被验证：测试注入一个按脚本应答的替身，断言请求构造得对不对、
//! 响应解析得对不对。

use std::collections::BTreeMap;

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

/// 走真实网络的传输层。
#[derive(Debug)]
pub struct ReqwestTransport {
    client: reqwest::Client,
}

impl ReqwestTransport {
    /// 建一个默认客户端。
    pub fn new() -> Result<Self> {
        let client = reqwest::Client::builder()
            .user_agent(concat!("acmecast/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|e| Error::provider(format!("无法构造 HTTP 客户端: {e}")))?;
        Ok(Self { client })
    }
}

impl Default for ReqwestTransport {
    fn default() -> Self {
        // 构造只可能因为 TLS 后端初始化失败而失败，那种情况下整个进程也跑不起来。
        Self::new().expect("HTTP 客户端应能构造")
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
            .map_err(|e| Error::provider(format!("请求 {} 失败: {e}", request.url)))?;

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
