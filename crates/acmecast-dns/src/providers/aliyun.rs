//! 阿里云 DNS 提供商。
//!
//! 用的是阿里云的 RPC 风格 API（`Action` 参数分派、HMAC-SHA1 签名），
//! 而非它的新版 OpenAPI——前者不需要额外 SDK，签名逻辑三十行就能写清。

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
use crate::http::{HttpRequest, HttpResponse, HttpTransport, ReqwestTransport, encode};
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
}

/// 阿里云 DNS。
#[derive(Debug)]
pub struct AliyunProvider {
    http: Arc<dyn HttpTransport>,
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
        Self { http }
    }

    /// 发一次 RPC 调用。
    async fn call(
        &self,
        credentials: &AliyunCredentials,
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
            ("Version".to_owned(), API_VERSION.to_owned()),
        ]);
        params.extend(extra);

        let signature = sign(&params, &credentials.access_key_secret)?;
        params.insert("Signature".to_owned(), signature);

        let query = params
            .iter()
            .map(|(key, value)| format!("{}={}", encode(key), encode(value)))
            .collect::<Vec<_>>()
            .join("&");

        let request = HttpRequest::new("GET", format!("{API_ENDPOINT}?{query}"));
        let response = self.http.send(request).await?;

        ensure_success(response, action)
    }

    /// 列出某个主机记录下的全部 TXT 记录（RecordId 与值）。
    async fn list_records(
        &self,
        credentials: &AliyunCredentials,
        record: &TxtRecord,
    ) -> Result<Vec<(String, String)>> {
        let query = BTreeMap::from([
            ("DomainName".to_owned(), record.zone.clone()),
            ("RRKeyWord".to_owned(), record.relative_name()?),
            ("TypeKeyWord".to_owned(), "TXT".to_owned()),
        ]);
        let body = self
            .call(credentials, "DescribeDomainRecords", query)
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
        "aliyun"
    }

    fn display_name(&self) -> &'static str {
        "阿里云 DNS"
    }

    fn credential_fields(&self) -> RootSchema {
        schema_for!(AliyunCredentials)
    }

    async fn create_txt(&self, credentials: &Value, record: &TxtRecord) -> Result<()> {
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

        self.call(&credentials, "AddDomainRecord", extra).await?;
        Ok(())
    }

    async fn find_txt(&self, credentials: &Value, record: &TxtRecord) -> Result<Vec<String>> {
        let credentials: AliyunCredentials = parse_credentials(credentials)?;

        Ok(self
            .list_records(&credentials, record)
            .await?
            .into_iter()
            .map(|(_, value)| value)
            .collect())
    }

    async fn delete_txt(&self, credentials: &Value, record: &TxtRecord) -> Result<()> {
        let credentials: AliyunCredentials = parse_credentials(credentials)?;

        // 删除要 RecordId，而 trait 的入参只有记录本身；先查出来，
        // 并且**只删值与我们这条相同的**——同名但不是我们写的记录不该被替人清掉。
        let ids: Vec<String> = self
            .list_records(&credentials, record)
            .await?
            .into_iter()
            .filter(|(_, value)| value == &record.value)
            .map(|(id, _)| id)
            .collect();

        // 一条都没找到也算成功：删除是幂等的，清理路径上不该为「它本来就不在」报错。
        for id in ids {
            let extra = BTreeMap::from([("RecordId".to_owned(), id)]);
            self.call(&credentials, "DeleteDomainRecord", extra).await?;
        }
        Ok(())
    }
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
