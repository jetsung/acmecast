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
use crate::http::{
    HttpRequest, HttpResponse, HttpTransport, ReqwestTransport, encode,
    inject_proxy_field_annotations, transport_for_credentials,
};
use crate::provider::{DnsProvider, TxtRecord, parse_credentials};

/// Cloudflare API 的基地址。
const API_BASE: &str = "https://api.cloudflare.com/client/v4";

/// Cloudflare 的凭据字段。
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct CloudflareCredentials {
    /// API Token，需对目标域名具备 DNS 编辑权限。
    pub api_token: String,
    /// 出网代理模式：`direct`（缺省，直连）/ `inherit`（继承全局代理，
    /// 见 `config.toml` 的 `[proxy] url` 或 `ACMECAST_PROXY_URL`）/
    /// `custom`（走下面 `proxy_url`，覆盖全局）。
    #[serde(default)]
    pub proxy_mode: Option<String>,
    /// 自定义出网代理（`proxy_mode=custom` 时生效）：支持 `socks5://`、
    /// `http://`、`https://`，留空直连；仅该提供商的 API 调用经此代理发送，
    /// 不影响 ACME 申请与通知渠道的出网路径。
    #[serde(default)]
    pub proxy_url: Option<String>,
}

/// Cloudflare。
#[derive(Debug)]
pub struct CloudflareProvider {
    http: Arc<dyn HttpTransport>,
    /// 全局出网代理（`[proxy] url` / `ACMECAST_PROXY_URL`）；仅
    /// `proxy_mode=inherit` 的凭据读它。`None` 即直连。
    global_proxy_url: Option<String>,
    /// 网络操作超时（`[network] timeout_secs`）：`proxy_mode=custom` 建
    /// reqwest 客户端时施加请求级超时。
    network_timeout: std::time::Duration,
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
        Self {
            http,
            global_proxy_url: None,
            network_timeout: acmecast_core::NETWORK_TIMEOUT,
        }
    }

    /// 设置全局出网代理与网络超时，供 `proxy_mode=inherit` 的凭据继承代理。
    #[must_use]
    pub fn with_global_proxy_and_timeout(
        mut self,
        proxy_url: Option<impl Into<String>>,
        network_timeout: std::time::Duration,
    ) -> Self {
        self.global_proxy_url = proxy_url.map(Into::into);
        self.network_timeout = network_timeout;
        self
    }

    /// 全局代理 URL；`proxy_mode=inherit` 时读它（留空/未配即直连）。
    fn global_proxy_url(&self) -> Option<&str> {
        self.global_proxy_url.as_deref()
    }

    /// 按凭据选传输层：`proxy_mode` 三态决定直连 / 继承全局 / 走凭据自定义代理。
    fn http_for(
        &self,
        credentials: &Value,
        global_proxy_url: Option<&str>,
    ) -> Result<Arc<dyn HttpTransport>> {
        transport_for_credentials(
            credentials,
            &self.http,
            global_proxy_url,
            self.network_timeout,
        )
    }

    /// 探测凭据是否可用：调 Cloudflare 的 token 自检端点。
    ///
    /// 这是凭据页「测试」按钮的探测动作——用轻量身份接口验证令牌本身，
    /// 不要求它对任何域名具备 DNS 编辑权限。
    pub async fn verify_credentials(&self, credentials: &Value) -> Result<()> {
        let http = self.http_for(credentials, self.global_proxy_url())?;
        let credentials: CloudflareCredentials = parse_credentials(credentials)?;
        let request = HttpRequest::new("GET", format!("{API_BASE}/user/tokens/verify"))
            .with_header("Authorization", format!("Bearer {}", credentials.api_token));

        let response = http.send(request).await?;
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
    async fn zone_id(
        &self,
        http: &Arc<dyn HttpTransport>,
        token: &str,
        zone: &str,
    ) -> Result<String> {
        let request = HttpRequest::new("GET", format!("{API_BASE}/zones?name={}", encode(zone)))
            .with_header("Authorization", format!("Bearer {token}"));

        let response = http.send(request).await?;
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
    async fn authorized(
        &self,
        http: &Arc<dyn HttpTransport>,
        token: &str,
        request: HttpRequest,
    ) -> Result<HttpResponse> {
        http.send(request.with_header("Authorization", format!("Bearer {token}")))
            .await
    }

    /// 列出某个名字下的全部 TXT 记录（id 与内容）。
    async fn list_records(
        &self,
        http: &Arc<dyn HttpTransport>,
        token: &str,
        zone_id: &str,
        name: &str,
    ) -> Result<Vec<(String, String)>> {
        let query = format!(
            "{API_BASE}/zones/{zone_id}/dns_records?type=TXT&name={}",
            encode(name),
        );
        let response = self
            .authorized(http, token, HttpRequest::new("GET", query))
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
        "cloud.cloudflare"
    }

    fn display_name(&self) -> &'static str {
        "Cloudflare"
    }

    fn credential_fields(&self) -> RootSchema {
        cloudflare_credentials_schema()
    }

    async fn create_txt(&self, credentials: &Value, record: &TxtRecord) -> Result<()> {
        let http = self.http_for(credentials, self.global_proxy_url())?;
        let credentials: CloudflareCredentials = parse_credentials(credentials)?;
        let zone_id = self
            .zone_id(&http, &credentials.api_token, &record.zone)
            .await?;

        let request = HttpRequest::new("POST", format!("{API_BASE}/zones/{zone_id}/dns_records"))
            .with_body(json!({
                "type": "TXT",
                "name": record.name,
                "content": record.value,
                "ttl": record.ttl,
            }));

        let response = self
            .authorized(&http, &credentials.api_token, request)
            .await?;
        ensure_success(response, "创建 TXT 记录")?;
        Ok(())
    }

    async fn find_txt(&self, credentials: &Value, record: &TxtRecord) -> Result<Vec<String>> {
        let http = self.http_for(credentials, self.global_proxy_url())?;
        let credentials: CloudflareCredentials = parse_credentials(credentials)?;
        let zone_id = self
            .zone_id(&http, &credentials.api_token, &record.zone)
            .await?;

        Ok(self
            .list_records(&http, &credentials.api_token, &zone_id, &record.name)
            .await?
            .into_iter()
            .map(|(_, content)| content)
            .collect())
    }

    async fn probe_zone(&self, credentials: &Value, zone: &str) -> Result<()> {
        let http = self.http_for(credentials, self.global_proxy_url())?;
        let credentials: CloudflareCredentials = parse_credentials(credentials)?;
        self.zone_id(&http, &credentials.api_token, zone).await?;
        Ok(())
    }

    async fn delete_txt(&self, credentials: &Value, record: &TxtRecord) -> Result<()> {
        let http = self.http_for(credentials, self.global_proxy_url())?;
        let credentials: CloudflareCredentials = parse_credentials(credentials)?;
        let zone_id = self
            .zone_id(&http, &credentials.api_token, &record.zone)
            .await?;

        // 删除要 record_id，而 trait 的入参只有记录本身；先查出来，
        // 并且**只删值与我们这条相同的**——同名但不是我们写的记录不该被替人清掉。
        let ids: Vec<String> = self
            .list_records(&http, &credentials.api_token, &zone_id, &record.name)
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
            let response = self
                .authorized(&http, &credentials.api_token, request)
                .await?;
            ensure_success(response, "删除 TXT 记录")?;
        }
        Ok(())
    }
}

/// Cloudflare 凭据字段的 schema。
///
/// serde_json 的 Map 会把 schemars 产物的 `properties` 按字母序重排——
/// 用 `x-field-order` 钉住渲染顺序：Token 在前，代理字段钉在凭据字段之后。
/// `proxy_url` 挂 `x-visible-when` 与 `proxy_mode=custom` 联动显隐。
#[must_use]
pub fn cloudflare_credentials_schema() -> RootSchema {
    let mut schema = schema_for!(CloudflareCredentials);
    schema.schema.extensions.insert(
        "x-field-order".to_owned(),
        serde_json::json!(["api_token", "proxy_mode", "proxy_url"]),
    );
    inject_proxy_field_annotations(&mut schema);
    schema
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_schema_pins_proxy_fields_after_credential_fields() {
        let rendered = serde_json::to_string(&cloudflare_credentials_schema()).unwrap();
        // Map 会把 properties 按字母序重排，布局全靠 x-field-order 钉住：
        // Token 在前，代理字段钉在凭据字段之后。
        assert!(
            rendered.contains(r#""x-field-order":["api_token","proxy_mode","proxy_url"]"#),
            "应注入声明顺序: {rendered}"
        );
        // proxy_mode 是枚举下拉：三个候选 + 中文展示名。
        assert!(
            rendered.contains(r#""enum":["direct","inherit","custom"]"#),
            "proxy_mode 应注入枚举候选: {rendered}"
        );
        for label in ["直连", "继承全局", "自定义"] {
            assert!(
                rendered.contains(label),
                "proxy_mode 应标注中文展示名 {label}: {rendered}"
            );
        }
        // proxy_url 与 proxy_mode=custom 联动显隐。
        assert!(
            rendered.contains(
                r#""x-visible-when":{"Equals":{"field":"proxy_mode","values":["custom"]}}"#
            ),
            "proxy_url 应仅在 custom 模式显隐: {rendered}"
        );
        // 前驱字段（api_token）带 x-end-row，使 proxy_mode 从新行首列开始、
        // 与 proxy_url 同行（左下拉、右输入框）。
        let value: serde_json::Value = serde_json::from_str(&rendered).unwrap();
        let api_token = &value["properties"]["api_token"];
        assert_eq!(
            api_token["x-end-row"],
            serde_json::json!(true),
            "api_token 应带 x-end-row 把 proxy_mode 推到新行: {rendered}"
        );
        // 描述写明支持的 scheme 与代理作用域。
        assert!(
            rendered.contains("socks5://") && rendered.contains("仅该提供商"),
            "proxy_url 描述应写明支持 scheme 与作用域: {rendered}"
        );
    }
}
