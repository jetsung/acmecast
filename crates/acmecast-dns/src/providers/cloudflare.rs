//! Cloudflare DNS 提供商。
//!
//! 用的是 API Token 方式（而非早已不推荐的 Global API Key）：Token 可以
//! 限定到具体域名与权限，泄漏的爆炸半径小得多。
//!
//! # API 文档
//!
//! - 创建 DNS 记录：
//!   <https://developers.cloudflare.com/api/resources/dns/subresources/records/methods/create/>

use std::sync::Arc;

use async_trait::async_trait;
use schemars::schema::RootSchema;
use schemars::schema_for;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::error::{Error, Result};
use crate::http::{HttpRequest, HttpResponse, HttpTransport, ReqwestTransport, encode};
use crate::provider::{DnsProvider, TxtRecord, parse_credentials};

/// Cloudflare API 的基地址。
const API_BASE: &str = "https://api.cloudflare.com/client/v4";

/// Cloudflare 的凭据字段。
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct CloudflareCredentials {
    /// API Token，需对目标域名具备 DNS 编辑权限。
    pub api_token: String,
}

/// Cloudflare。
#[derive(Debug)]
pub struct CloudflareProvider {
    http: Arc<dyn HttpTransport>,
}

impl CloudflareProvider {
    /// 走真实网络。
    #[must_use]
    pub fn new() -> Self {
        Self::with_transport(Arc::new(ReqwestTransport::default()))
    }

    /// 注入传输层，供测试用。
    #[must_use]
    pub fn with_transport(http: Arc<dyn HttpTransport>) -> Self {
        Self { http }
    }

    /// 探测凭据是否可用：调 Cloudflare 的 token 自检端点。
    ///
    /// 这是凭据页「测试」按钮的探测动作——用轻量身份接口验证令牌本身，
    /// 不要求它对任何域名具备 DNS 编辑权限。
    pub async fn verify_credentials(&self, credentials: &Value) -> Result<()> {
        let credentials: CloudflareCredentials = parse_credentials(credentials)?;
        let request = HttpRequest::new("GET", format!("{API_BASE}/user/tokens/verify"))
            .with_header("Authorization", format!("Bearer {}", credentials.api_token));

        let response = self.http.send(request).await?;
        let body = ensure_success(response, "验证令牌")?;

        // 自检端点对无效令牌也可能回 200（`success: false` + errors），
        // 所以「HTTP 成功」之外还要看令牌状态是不是 active。
        let status = body["result"]["status"].as_str().unwrap_or("");
        if status == "active" {
            return Ok(());
        }
        Err(Error::provider(format!(
            "Cloudflare 令牌未激活（status: {status}）"
        )))
    }

    /// 按域名换 zone_id。
    async fn zone_id(&self, token: &str, zone: &str) -> Result<String> {
        let request = HttpRequest::new("GET", format!("{API_BASE}/zones?name={}", encode(zone)))
            .with_header("Authorization", format!("Bearer {token}"));

        let response = self.http.send(request).await?;
        let body = ensure_success(response, "查询域名")?;

        body["result"]
            .as_array()
            .and_then(|zones| zones.first())
            .and_then(|zone| zone["id"].as_str())
            .map(str::to_owned)
            .ok_or_else(|| {
                Error::provider(format!(
                    "Cloudflare 上找不到域名 `{zone}`——请确认 token 有该域名的权限"
                ))
            })
    }

    /// 发一个带认证的请求。
    async fn authorized(&self, token: &str, request: HttpRequest) -> Result<HttpResponse> {
        self.http
            .send(request.with_header("Authorization", format!("Bearer {token}")))
            .await
    }

    /// 列出某个名字下的全部 TXT 记录（id 与内容）。
    async fn list_records(
        &self,
        token: &str,
        zone_id: &str,
        name: &str,
    ) -> Result<Vec<(String, String)>> {
        let query = format!(
            "{API_BASE}/zones/{zone_id}/dns_records?type=TXT&name={}",
            encode(name),
        );
        let response = self
            .authorized(token, HttpRequest::new("GET", query))
            .await?;
        let body = ensure_success(response, "查询 TXT 记录")?;

        Ok(body["result"]
            .as_array()
            .map(|records| {
                records
                    .iter()
                    .filter_map(|record| {
                        Some((
                            record["id"].as_str()?.to_owned(),
                            record["content"].as_str()?.to_owned(),
                        ))
                    })
                    .collect()
            })
            .unwrap_or_default())
    }
}

impl Default for CloudflareProvider {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl DnsProvider for CloudflareProvider {
    fn type_id(&self) -> &'static str {
        "cloudflare"
    }

    fn display_name(&self) -> &'static str {
        "Cloudflare"
    }

    fn credential_fields(&self) -> RootSchema {
        schema_for!(CloudflareCredentials)
    }

    async fn create_txt(&self, credentials: &Value, record: &TxtRecord) -> Result<()> {
        let credentials: CloudflareCredentials = parse_credentials(credentials)?;
        let zone_id = self.zone_id(&credentials.api_token, &record.zone).await?;

        let request = HttpRequest::new("POST", format!("{API_BASE}/zones/{zone_id}/dns_records"))
            .with_body(json!({
                "type": "TXT",
                "name": record.name,
                "content": record.value,
                "ttl": record.ttl,
            }));

        let response = self.authorized(&credentials.api_token, request).await?;
        ensure_success(response, "创建 TXT 记录")?;
        Ok(())
    }

    async fn find_txt(&self, credentials: &Value, record: &TxtRecord) -> Result<Vec<String>> {
        let credentials: CloudflareCredentials = parse_credentials(credentials)?;
        let zone_id = self.zone_id(&credentials.api_token, &record.zone).await?;

        Ok(self
            .list_records(&credentials.api_token, &zone_id, &record.name)
            .await?
            .into_iter()
            .map(|(_, content)| content)
            .collect())
    }

    async fn delete_txt(&self, credentials: &Value, record: &TxtRecord) -> Result<()> {
        let credentials: CloudflareCredentials = parse_credentials(credentials)?;
        let zone_id = self.zone_id(&credentials.api_token, &record.zone).await?;

        // 删除要 record_id，而 trait 的入参只有记录本身；先查出来，
        // 并且**只删值与我们这条相同的**——同名但不是我们写的记录不该被替人清掉。
        let ids: Vec<String> = self
            .list_records(&credentials.api_token, &zone_id, &record.name)
            .await?
            .into_iter()
            .filter(|(_, content)| content == &record.value)
            .map(|(id, _)| id)
            .collect();

        // 一条都没找到也算成功：删除是幂等的，清理路径上不该为「它本来就不在」报错。
        for id in ids {
            let request = HttpRequest::new(
                "DELETE",
                format!("{API_BASE}/zones/{zone_id}/dns_records/{id}"),
            );
            let response = self.authorized(&credentials.api_token, request).await?;
            ensure_success(response, "删除 TXT 记录")?;
        }
        Ok(())
    }
}

/// 校验响应成功，并解析出 JSON。
///
/// 失败时摘出 Cloudflare 的 `errors[].message`：它的报错比一个光秃秃的 403
/// 有用得多（比如「token 缺少 DNS:Edit 权限」）。出错一般伴随 4xx，但
/// `success: false` 也可能出现在 200 响应里（如 token 自检对无效令牌），
/// 两处都判断才不会把业务失败误判成成功。
fn ensure_success(response: HttpResponse, what: &str) -> Result<Value> {
    let body = response
        .json()
        .unwrap_or_else(|_| json!({ "raw": &response.body }));

    let business_failed = body["success"].as_bool() == Some(false);
    if response.is_success() && !business_failed {
        return Ok(body);
    }

    let detail = body["errors"]
        .as_array()
        .map(|errors| {
            errors
                .iter()
                .filter_map(|error| error["message"].as_str())
                .collect::<Vec<_>>()
                .join("；")
        })
        .filter(|text| !text.is_empty())
        .unwrap_or_else(|| body.to_string());

    Err(Error::provider(format!(
        "Cloudflare {what}失败（HTTP {}）：{detail}",
        response.status
    )))
}
