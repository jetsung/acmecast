//! S3 客户端的出网代理支持。
//!
//! `aws-sdk-s3` 的 hyper 客户端只支持 HTTP/HTTPS 代理（经
//! `aws_smithy_http_client::proxy::ProxyConfig`），不支持 socks5——
//! socks5 用一个 reqwest 包装的自定义 [`HttpConnector`] 提供。

use std::fmt;

use aws_smithy_http_client::proxy::ProxyConfig;
use aws_smithy_http_client::tls;
use aws_smithy_http_client::{Builder, Connector};
use aws_smithy_runtime_api::client::http::{
    HttpConnector, HttpConnectorFuture, SharedHttpClient, SharedHttpConnector, http_client_fn,
};
use aws_smithy_runtime_api::client::orchestrator::{HttpRequest, HttpResponse};
use aws_smithy_runtime_api::client::result::ConnectorError;
use aws_smithy_runtime_api::http::StatusCode;
use aws_smithy_types::body::SdkBody;

/// 依据生效的代理 URL 构建 S3 的 HTTP 客户端：
///
/// - `http://` / `https://` → 默认 hyper 连接器注入 `ProxyConfig::all`；
/// - `socks5://` → reqwest 隧道自定义连接器（hyper 不支持 socks5）；
/// - `None` 或非法 URL → `None`（S3 客户端回落到默认直连）。
///
/// 供凭据连通性探测（`s3_probe`）与部署目标（`s3_put`）共用，
/// 保证两条出网路径的代理行为一致。
#[must_use]
pub fn s3_http_client(proxy_url: Option<&str>) -> Option<SharedHttpClient> {
    let url = proxy_url.map(str::trim).filter(|value| !value.is_empty())?;
    // 统一口径校验：非法 scheme / 不可解析的 URL 走直连（保存时已被拦下）。
    if acmecast_core::validate_proxy_url(url).is_err() {
        return None;
    }
    if url.starts_with("socks5") {
        let Ok(proxy) = reqwest::Proxy::all(url) else {
            return None;
        };
        let Ok(client) = reqwest::Client::builder().proxy(proxy).build() else {
            return None;
        };
        Some(http_client_fn(move |_, _| {
            SharedHttpConnector::new(Socks5TunnelConnector {
                client: client.clone(),
            })
        }))
    } else {
        let Ok(proxy) = ProxyConfig::all(url) else {
            return None;
        };
        // 顶层 Builder 的 proxy 配置走连接器层面：每次连接器选择都按同一
        // ProxyConfig 建带代理的 hyper 连接器（TLS 走项目的 rustls-aws-lc）。
        Some(
            Builder::new().build_with_connector_fn(move |_, _| {
                Connector::builder()
                    .proxy_config(proxy.clone())
                    .tls_provider(tls::Provider::Rustls(
                        tls::rustls_provider::CryptoMode::AwsLc,
                    ))
                    .build()
            }),
        )
    }
}

/// SOCKS5 隧道连接器：把 AWS SDK 的 `HttpRequest` 转成 reqwest 请求，
/// 经 socks5 代理发出，再把响应转换回 `HttpResponse`。
#[derive(Clone)]
struct Socks5TunnelConnector {
    client: reqwest::Client,
}

impl fmt::Debug for Socks5TunnelConnector {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Socks5TunnelConnector").finish()
    }
}

impl HttpConnector for Socks5TunnelConnector {
    fn call(&self, request: HttpRequest) -> HttpConnectorFuture {
        // call 本身是同步的：请求参数先全部提取出来，发送放进 future。
        let client = self.client.clone();
        let method = request.method().to_owned();
        let uri = request.uri().to_owned();
        let headers = request
            .headers()
            .iter_bytes()
            .map(|(name, value)| (name.to_owned(), value.to_owned()))
            .collect::<Vec<_>>();
        let body = request
            .body()
            .bytes()
            .map(<[u8]>::to_vec)
            .unwrap_or_default();

        HttpConnectorFuture::new_boxed(Box::pin(async move {
            let method = method
                .parse::<reqwest::Method>()
                .map_err(|e| ConnectorError::user(e.into()))?;
            let mut builder = client.request(method, &uri);
            for (name, value) in &headers {
                if let Ok(value) = std::str::from_utf8(value) {
                    builder = builder.header(name.as_str(), value);
                }
            }
            if !body.is_empty() {
                builder = builder.body(body);
            }
            let response = builder
                .send()
                .await
                .map_err(|e: reqwest::Error| ConnectorError::user(e.into()))?;
            let status = StatusCode::from(response.status());
            let bytes = response
                .bytes()
                .await
                .map_err(|e: reqwest::Error| ConnectorError::user(e.into()))?;
            let mut response = HttpResponse::new(status, SdkBody::from(bytes.to_vec()));
            // 先收集再插入：headers() 的迭代与 headers_mut() 不能同时持借用。
            let response_headers: Vec<(String, String)> = response
                .headers()
                .iter()
                .map(|(name, value)| (name.to_owned(), value.to_owned()))
                .collect();
            for (name, value) in response_headers {
                response.headers_mut().insert(name, value);
            }
            Ok(response)
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `None` / 非法 scheme / 空白 → 直连（返回 `None`）。
    #[test]
    fn no_or_invalid_proxy_yields_default_client() {
        assert!(s3_http_client(None).is_none());
        assert!(s3_http_client(Some("  ")).is_none());
        assert!(
            s3_http_client(Some("ftp://127.0.0.1:21")).is_none(),
            "非法 scheme 应回落直连"
        );
    }

    /// http/https 与 socks5 代理都构建成功（返回对应客户端）。
    #[test]
    fn proxy_urls_build_configured_clients() {
        let Some(http) = s3_http_client(Some("http://10.0.0.1:3128")) else {
            panic!("http 代理应构建成功");
        };
        let Some(https) = s3_http_client(Some("https://10.0.0.1:3129")) else {
            panic!("https 代理应构建成功");
        };
        let Some(socks) = s3_http_client(Some("socks5://10.0.0.1:1080")) else {
            panic!("socks5 代理应构建成功");
        };
        // 三个客户端都能作为 SharedHttpClient 交给 S3 配置（类型层面已保证）。
        let _ = (http, https, socks);
    }
}
