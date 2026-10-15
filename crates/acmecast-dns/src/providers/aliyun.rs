//! 阿里云 DNS 提供商。
//!
//! 用的是阿里云的 RPC 风格 API（`Action` 参数分派、HMAC-SHA1 签名），
//! 而非它的新版 OpenAPI——前者不需要额外 SDK，签名逻辑三十行就能写清。
//!
//! # API 文档
//!
//! - 添加解析记录（AddDomainRecord）：
//!   <https://help.aliyun.com/zh/dns/api-alidns-2015-01-09-adddomainrecord>

use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use base64::Engine as _;
use hmac::{Hmac, Mac};
use schemars::schema::RootSchema;
use schemars::schema_for;
use serde::Deserialize;
use serde_json::{Value, json};
use sha1::Sha1;

use crate::error::{Error, Result};
use crate::http::{
    HttpRequest, HttpResponse, HttpTransport, ReqwestTransport, encode, inject_proxy_field_annotations,
    transport_for_credentials,
};
use crate::provider::{DnsProvider, TxtRecord, parse_credentials};

type HmacSha1 = Hmac<Sha1>;

/// API 端点。
const API_ENDPOINT: &str = "https://alidns.aliyuncs.com/";
/// 阿里云 DNS 的 API 版本。
const API_VERSION: &str = "2015-01-09";
/// AddDomainRecord 接受的 TTL 区间（秒）。上层惯用的 60/120s 在阿里云
/// 会直接 400（QuotaExceeded.TTL——名字骗人，其实是参数校验）。
const TTL_MIN: u32 = 600;
const TTL_MAX: u32 = 86400;

/// 阿里云的凭据字段。
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct AliyunCredentials {
    /// AccessKey ID。
    pub access_key_id: String,
    /// AccessKey Secret。
    pub access_key_secret: String,
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

/// 阿里云 DNS。
#[derive(Debug)]
pub struct AliyunProvider {
    http: Arc<dyn HttpTransport>,
    /// 全局出网代理（`[proxy] url` / `ACMECAST_PROXY_URL`）；仅
    /// `proxy_mode=inherit` 的凭据读它。`None` 即直连。
    global_proxy_url: Option<String>,
    /// 网络操作超时（`[network] timeout_secs`）：`proxy_mode=custom` 建
    /// reqwest 客户端时施加请求级超时。
    network_timeout: std::time::Duration,
}

impl AliyunProvider {
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

    /// 发一次 RPC 调用（DNS 服务端点）。
    async fn call(
        &self,
        http: &Arc<dyn HttpTransport>,
        credentials: &AliyunCredentials,
        action: &str,
        extra: BTreeMap<String, String>,
    ) -> Result<Value> {
        self.call_service(http, credentials, API_ENDPOINT, API_VERSION, action, extra)
            .await
    }

    /// 发一次 RPC 调用，端点与服务版本由调用方给——
    /// DNS 与 STS（凭据探测的 `GetCallerIdentity`）共用同一套签名。
    async fn call_service(
        &self,
        http: &Arc<dyn HttpTransport>,
        credentials: &AliyunCredentials,
        endpoint: &str,
        version: &str,
        action: &str,
        extra: BTreeMap<String, String>,
    ) -> Result<Value> {
        let mut params = BTreeMap::from([
            ("AccessKeyId".to_owned(), credentials.access_key_id.clone()),
            ("Action".to_owned(), action.to_owned()),
            ("Format".to_owned(), "JSON".to_owned()),
            ("SignatureMethod".to_owned(), "HMAC-SHA1".to_owned()),
            ("SignatureNonce".to_owned(), nonce()),
            ("SignatureVersion".to_owned(), "1.0".to_owned()),
            ("Timestamp".to_owned(), timestamp()),
            ("Version".to_owned(), version.to_owned()),
        ]);
        params.extend(extra);

        let signature = sign(&params, &credentials.access_key_secret)?;
        params.insert("Signature".to_owned(), signature);

        let query = params
            .iter()
            .map(|(key, value)| format!("{}={}", encode(key), encode(value)))
            .collect::<Vec<_>>()
            .join("&");

        let request = HttpRequest::new("GET", format!("{endpoint}?{query}"));
        let response = http.send(request).await?;

        ensure_success(response, action)
    }

    /// 探测凭据是否可用：调 STS 的 `GetCallerIdentity`。
    ///
    /// 这是凭据页「测试」按钮的探测动作——只做身份辨识，
    /// 不依赖任何 DNS 业务权限，子账号默认可调。
    pub async fn verify_credentials(&self, credentials: &Value) -> Result<()> {
        let http = self.http_for(credentials, self.global_proxy_url())?;
        let credentials: AliyunCredentials = parse_credentials(credentials)?;
        let body = self
            .call_service(
                &http,
                &credentials,
                "https://sts.aliyuncs.com/",
                "2015-04-01",
                "GetCallerIdentity",
                BTreeMap::new(),
            )
            .await?;

        // 鉴权失败通常伴随 HTTP 4xx（ensure_success 已摘 Code/Message），
        // 这里再顶层的看一次：个别网关路径会把错误包在 HTTP 200 里。
        if let Some(code) = body["Code"].as_str() {
            let message = body["Message"].as_str().unwrap_or("");
            return Err(Error::provider(format!(
                "阿里云 GetCallerIdentity 失败：{code}: {message}"
            )));
        }
        Ok(())
    }

    /// 列出某个主机记录下的全部 TXT 记录（RecordId 与值）。
    async fn list_records(
        &self,
        http: &Arc<dyn HttpTransport>,
        credentials: &AliyunCredentials,
        record: &TxtRecord,
    ) -> Result<Vec<(String, String)>> {
        let query = BTreeMap::from([
            ("DomainName".to_owned(), record.zone.clone()),
            ("RRKeyWord".to_owned(), record.relative_name()?),
            ("TypeKeyWord".to_owned(), "TXT".to_owned()),
        ]);
        let body = self
            .call(http, credentials, "DescribeDomainRecords", query)
            .await?;

        Ok(body["DomainRecords"]["Record"]
            .as_array()
            .map(|records| {
                records
                    .iter()
                    .filter_map(|record| {
                        Some((
                            record["RecordId"].as_str()?.to_owned(),
                            record["Value"].as_str()?.to_owned(),
                        ))
                    })
                    .collect()
            })
            .unwrap_or_default())
    }
}

impl Default for AliyunProvider {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl DnsProvider for AliyunProvider {
    fn type_id(&self) -> &'static str {
        "cloud.aliyun"
    }

    fn display_name(&self) -> &'static str {
        "阿里云 DNS"
    }

    fn credential_fields(&self) -> RootSchema {
        aliyun_credentials_schema()
    }

    async fn create_txt(&self, credentials: &Value, record: &TxtRecord) -> Result<()> {
        let http = self.http_for(credentials, self.global_proxy_url())?;
        let credentials: AliyunCredentials = parse_credentials(credentials)?;

        let extra = BTreeMap::from([
            ("DomainName".to_owned(), record.zone.clone()),
            ("RR".to_owned(), record.relative_name()?),
            ("Type".to_owned(), "TXT".to_owned()),
            ("Value".to_owned(), record.value.clone()),
            (
                "TTL".to_owned(),
                record.ttl.clamp(TTL_MIN, TTL_MAX).to_string(),
            ),
        ]);

        self.call(&http, &credentials, "AddDomainRecord", extra)
            .await?;
        Ok(())
    }

    async fn find_txt(&self, credentials: &Value, record: &TxtRecord) -> Result<Vec<String>> {
        let http = self.http_for(credentials, self.global_proxy_url())?;
        let credentials: AliyunCredentials = parse_credentials(credentials)?;

        Ok(self
            .list_records(&http, &credentials, record)
            .await?
            .into_iter()
            .map(|(_, value)| value)
            .collect())
    }

    async fn probe_zone(&self, credentials: &Value, zone: &str) -> Result<()> {
        let http = self.http_for(credentials, self.global_proxy_url())?;
        let credentials: AliyunCredentials = parse_credentials(credentials)?;
        // DescribeDomain 对不存在的域名直接报错，存在即托管——无需读响应内容。
        self.call(
            &http,
            &credentials,
            "DescribeDomain",
            BTreeMap::from([("DomainName".to_owned(), zone.to_owned())]),
        )
        .await?;
        Ok(())
    }

    async fn delete_txt(&self, credentials: &Value, record: &TxtRecord) -> Result<()> {
        let http = self.http_for(credentials, self.global_proxy_url())?;
        let credentials: AliyunCredentials = parse_credentials(credentials)?;

        // 删除要 RecordId，而 trait 的入参只有记录本身；先查出来，
        // 并且**只删值与我们这条相同的**——同名但不是我们写的记录不该被替人清掉。
        let ids: Vec<String> = self
            .list_records(&http, &credentials, record)
            .await?
            .into_iter()
            .filter(|(_, value)| value == &record.value)
            .map(|(id, _)| id)
            .collect();

        // 一条都没找到也算成功：删除是幂等的，清理路径上不该为「它本来就不在」报错。
        for id in ids {
            let extra = BTreeMap::from([("RecordId".to_owned(), id)]);
            self.call(&http, &credentials, "DeleteDomainRecord", extra)
                .await?;
        }
        Ok(())
    }
}

/// 阿里云凭据字段的 schema。
///
/// serde_json 的 Map 会把 schemars 产物的 `properties` 按字母序重排——
/// 用 `x-field-order` 钉住渲染顺序：密钥对在前，代理字段钉在凭据字段之后。
/// `proxy_url` 挂 `x-visible-when` 与 `proxy_mode=custom` 联动显隐。
#[must_use]
pub fn aliyun_credentials_schema() -> RootSchema {
    let mut schema = schema_for!(AliyunCredentials);
    schema.schema.extensions.insert(
        "x-field-order".to_owned(),
        serde_json::json!(["access_key_id", "access_key_secret", "proxy_mode", "proxy_url"]),
    );
    inject_proxy_field_annotations(&mut schema);
    schema
}

/// 按阿里云 RPC 风格签名。
///
/// 步骤（见其「签名机制」文档）：
/// 1. 参数按名字典序排列，拼成 `k=v&k=v`，键与值都做百分号编码；
/// 2. `StringToSign = "GET&%2F&" + 上一步的串**再编码一次**`；
/// 3. 以 `AccessKeySecret + "&"` 为密钥算 HMAC-SHA1；
/// 4. 结果 Base64 即 Signature。
///
/// 第 2 步里的 `%2F` 是 `/` 的编码结果，**不能**被二次编码成 `%252F`。
fn sign(params: &BTreeMap<String, String>, access_key_secret: &str) -> Result<String> {
    let canonical = params
        .iter()
        .map(|(key, value)| format!("{}={}", encode(key), encode(value)))
        .collect::<Vec<_>>()
        .join("&");

    let string_to_sign = format!("GET&%2F&{}", encode(&canonical));

    let mut mac = HmacSha1::new_from_slice(format!("{access_key_secret}&").as_bytes())
        .map_err(|e| Error::provider(format!("无法初始化签名密钥: {e}")))?;
    mac.update(string_to_sign.as_bytes());

    Ok(base64::engine::general_purpose::STANDARD.encode(mac.finalize().into_bytes()))
}

/// 阿里云要求的 ISO8601 UTC 时间戳。
fn timestamp() -> String {
    chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

/// 每次请求都要带一个不重复的随机串。
fn nonce() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

/// 校验响应成功，并解析出 JSON。
fn ensure_success(response: HttpResponse, action: &str) -> Result<Value> {
    let body = response
        .json()
        .unwrap_or_else(|_| json!({ "raw": &response.body }));

    if response.is_success() {
        return Ok(body);
    }

    // 阿里云的报错在 `Code` 与 `Message` 里，比一个光秃秃的状态码有用得多。
    let detail = match (body["Code"].as_str(), body["Message"].as_str()) {
        (Some(code), Some(message)) => format!("{code}: {message}"),
        _ => body.to_string(),
    };

    Err(Error::provider(format!(
        "阿里云 {action} 失败（HTTP {}）：{detail}",
        response.status
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_schema_pins_proxy_fields_after_credential_fields() {
        let rendered = serde_json::to_string(&aliyun_credentials_schema()).unwrap();
        // Map 会把 properties 按字母序重排，布局全靠 x-field-order 钉住：
        // 密钥对在前，代理字段钉在凭据字段之后。
        assert!(
            rendered.contains(
                r#""x-field-order":["access_key_id","access_key_secret","proxy_mode","proxy_url"]"#
            ),
            "应注入声明顺序: {rendered}"
        );
        // 描述写明支持的 scheme 与代理作用域。
        assert!(
            rendered.contains("socks5://") && rendered.contains("仅该提供商"),
            "proxy_url 描述应写明支持 scheme 与作用域: {rendered}"
        );
        // proxy_mode 是枚举下拉：schemars 对 Option<String> 不产 enum 数组，
        // 由注入函数手动补——缺了前端 SchemaForm 会退化为普通文本输入框。
        assert!(
            rendered.contains(r#""enum":["direct","inherit","custom"]"#),
            "proxy_mode 应注入枚举候选: {rendered}"
        );
        // proxy_url 与 proxy_mode=custom 联动显隐。
        assert!(
            rendered.contains(
                r#""x-visible-when":{"Equals":{"field":"proxy_mode","values":["custom"]}}"#
            ),
            "proxy_url 应仅在 custom 模式显隐: {rendered}"
        );
        for label in ["直连", "继承全局", "自定义"] {
            assert!(
                rendered.contains(label),
                "proxy_mode 应标注中文展示名 {label}: {rendered}"
            );
        }
        // aliyun 仅 access_key_id|access_key_secret 一行，proxy_mode 已在新行首列，
        // 无需前驱 x-end-row（自然换行无需补空列）。
        let value: serde_json::Value = serde_json::from_str(&rendered).unwrap();
        assert!(
            value["properties"]["access_key_secret"]["x-end-row"].is_null(),
            "access_key_secret 已在行尾、proxy_mode 自然换行，不应带 x-end-row: {rendered}"
        );
    }

    #[test]
    fn the_signature_follows_the_documented_recipe() {
        // 手工按同一套步骤算一遍，确认实现没有漏掉「再编码一次」那一步。
        let params = BTreeMap::from([
            ("Action".to_owned(), "AddDomainRecord".to_owned()),
            ("Format".to_owned(), "JSON".to_owned()),
        ]);

        let canonical = format!(
            "{}={}&{}={}",
            encode("Action"),
            encode("AddDomainRecord"),
            encode("Format"),
            encode("JSON")
        );
        let string_to_sign = format!("GET&%2F&{}", encode(&canonical));

        let mut mac = HmacSha1::new_from_slice(b"secret&").unwrap();
        mac.update(string_to_sign.as_bytes());
        let expected =
            base64::engine::general_purpose::STANDARD.encode(mac.finalize().into_bytes());

        assert_eq!(sign(&params, "secret").unwrap(), expected);
        // 关键：`/` 的编码结果没有被二次编码。
        assert!(!string_to_sign.contains("%252F"), "{string_to_sign}");
    }

    #[test]
    fn the_signature_changes_with_the_secret() {
        let params = BTreeMap::from([("Action".to_owned(), "X".to_owned())]);
        assert_ne!(
            sign(&params, "secret-a").unwrap(),
            sign(&params, "secret-b").unwrap()
        );
    }

    #[test]
    fn the_nonce_is_not_repeated() {
        assert_ne!(nonce(), nonce());
    }

    #[test]
    fn the_timestamp_is_iso8601_utc() {
        let stamp = timestamp();
        assert!(stamp.ends_with('Z'), "{stamp}");
        assert_eq!(stamp.len(), 20, "{stamp}");
    }
}
