//! 腾讯云 API 3.0 通用调用端与 DNS 提供商（云解析 DNSPod）。
//!
//! 与 EdgeOne 提供商（[`super::tencent_eo`]）共用一套腾讯云 API 3.0 基础：
//! POST + JSON + TC3-HMAC-SHA256 签名，签名、凭据与端点定义在本文件，EdgeOne
//! 只换服务名、版本与 Action。不引入官方 SDK——签名百行内能写清，且自带的
//! HTTP 栈会绕开可注入的传输层，与阿里云手写 RPC 签名是同一个先例。
//!
//! [`TencentApiClient`] 是与服务无关的通用调用端（服务名、版本与 Action 都由
//! 调用方传入），证书部署目标（`acmecast-deploy` 的腾讯云 SSL）同样复用它。
//!
//! # API 文档
//!
//! - 腾讯云解析 DNS（国内版）：
//!   <https://cloud.tencent.com/document/api/1427/56180>
//! - 腾讯云解析 DNS（国际版）：
//!   <https://www.tencentcloud.com/zh/document/api/1157/49041>

use std::sync::Arc;

use async_trait::async_trait;
use hmac::{Hmac, Mac};
use schemars::schema::RootSchema;
use schemars::schema_for;
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::error::{Error, Result};
use crate::http::{
    HttpRequest, HttpResponse, HttpTransport, ReqwestTransport, inject_proxy_field_annotations,
    transport_for_credentials,
};
use crate::provider::{DnsProvider, TxtRecord, parse_credentials};

type HmacSha256 = Hmac<Sha256>;

/// 云解析（DNSPod）API 3.0 的服务名与版本。
pub(crate) const DNSPOD_SERVICE: &str = "dnspod";
pub(crate) const DNSPOD_VERSION: &str = "2021-03-23";

/// SSL 证书 API 3.0 的服务名与版本（凭据部署用途的探测接口）。
const SSL_SERVICE: &str = "ssl";
const SSL_VERSION: &str = "2019-12-05";

/// DNSPod 的 `CreateRecord`/`DeleteRecord` 接受的 TTL 区间（秒）。
///
/// 下限按套餐最严处取：API 文档写 60 起，但免费/标准版套餐实际
/// 不支持低于 600 的 TTL（真机报 `LimitExceeded.RecordTtlLimit`，
/// 见 histories/28）。挑战记录活几分钟即删，钳到 600 没有代价。
pub(crate) const TTL_MIN: u32 = 600;
pub(crate) const TTL_MAX: u32 = 86400;

/// 腾讯云账号站点。
///
/// 国内站与国际站是两套互不相通的账户体系：同一对密钥只能登录其中一边，
/// API 端点也不同，选错站点的表现是执行期的 `AuthFailure.*`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum TencentSite {
    /// 国内站（`*.tencentcloudapi.com`）。
    #[default]
    Cn,
    /// 国际站（`*.intl.tencentcloudapi.com`）。
    Intl,
}

/// 腾讯云体系的凭据字段（云解析与 EdgeOne 共用一份定义）。
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct TencentCredentials {
    /// API 密钥 ID（SecretId），与 SecretKey 在「访问管理-密钥管理」成对创建。
    pub secret_id: String,
    /// API 密钥 Key（SecretKey），TC3 签名的派生密钥来源。
    pub secret_key: String,
    /// 账号站点：凭据属于哪一套账户体系，决定请求发往哪个端点。
    #[serde(default)]
    pub account_site: TencentSite,
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

/// 按服务名与站点给出 API 主机名。
///
/// 国际站即域名中段插入 `intl.`：`dnspod.tencentcloudapi.com` 变成
/// `dnspod.intl.tencentcloudapi.com`。
#[must_use]
pub fn api_host(service: &str, site: TencentSite) -> String {
    match site {
        TencentSite::Cn => format!("{service}.tencentcloudapi.com"),
        TencentSite::Intl => format!("{service}.intl.tencentcloudapi.com"),
    }
}

/// HMAC-SHA256 一步。
fn hmac_bytes(key: &[u8], message: &[u8]) -> Vec<u8> {
    let mut mac = HmacSha256::new_from_slice(key).expect("HMAC 接受任意长度密钥");
    mac.update(message);
    mac.finalize().into_bytes().to_vec()
}

/// 字节的十六进制小写编码。
fn hex(data: &[u8]) -> String {
    data.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// SHA-256 的十六进制小写摘要。
fn sha256_hex(data: &[u8]) -> String {
    hex(&Sha256::digest(data))
}

/// 按 TC3-HMAC-SHA256 拼出 `Authorization` 头。
///
/// 步骤（见腾讯云 API 3.0「签名方法 v3」文档）：
/// 1. 规范请求 = `POST` / `/` / 空查询串 / 规范头（content-type、host、
///    x-tc-action 小写排序，每行以换行结尾）/ 签名头清单 / payload 摘要；
/// 2. 待签名串 = `TC3-HMAC-SHA256` / 时间戳 / `日期/服务/tc3_request` / 上一步摘要；
/// 3. 派生密钥链 `kDate → kService → kSigning`，起步密钥是 `TC3 + SecretKey`；
/// 4. 对待签名串的 HMAC 结果做十六进制编码。
///
/// canonical 里的 `x-tc-action` 一律小写（文档要求），实际请求头仍用原大小写。
#[must_use]
pub fn tc3_authorization(
    credentials: &TencentCredentials,
    timestamp: i64,
    service: &str,
    host: &str,
    action: &str,
    payload: &str,
) -> String {
    let payload_hash = sha256_hex(payload.as_bytes());
    let canonical_headers = format!(
        "content-type:application/json; charset=utf-8\nhost:{host}\nx-tc-action:{}\n",
        action.to_lowercase()
    );
    let canonical_request =
        format!("POST\n/\n\n{canonical_headers}\ncontent-type;host;x-tc-action\n{payload_hash}");

    let date = chrono::DateTime::from_timestamp(timestamp, 0)
        .expect("时间戳应在 DateTime 表示范围内")
        .format("%Y-%m-%d")
        .to_string();

    let string_to_sign = format!(
        "TC3-HMAC-SHA256\n{timestamp}\n{date}/{service}/tc3_request\n{}",
        sha256_hex(canonical_request.as_bytes())
    );

    let k_date = hmac_bytes(
        format!("TC3{}", credentials.secret_key).as_bytes(),
        date.as_bytes(),
    );
    let k_service = hmac_bytes(&k_date, service.as_bytes());
    let k_signing = hmac_bytes(&k_service, b"tc3_request");
    let signature = hex(&hmac_bytes(&k_signing, string_to_sign.as_bytes()));

    format!(
        "TC3-HMAC-SHA256 Credential={}/{date}/{service}/tc3_request, \
         SignedHeaders=content-type;host;x-tc-action, Signature={signature}",
        credentials.secret_id
    )
}

/// 腾讯云 API 3.0 的调用端：签名、组装与发送。
#[derive(Debug)]
pub struct TencentApiClient {
    pub(crate) http: Arc<dyn HttpTransport>,
}

impl TencentApiClient {
    /// 注入传输层：部署目标与测试各自组装。
    #[must_use]
    pub fn with_transport(http: Arc<dyn HttpTransport>) -> Self {
        Self { http }
    }

    /// 发一次 API 调用，成功时返回 `Response` 节点。
    pub async fn call(
        &self,
        credentials: &TencentCredentials,
        service: &str,
        version: &str,
        action: &str,
        payload: Value,
    ) -> Result<Value> {
        self.call_with(&self.http, credentials, service, version, action, payload)
            .await
    }

    /// 同 [`Self::call`]，但传输层由调用方按凭据选定（带代理凭据走代理）。
    pub async fn call_with(
        &self,
        http: &Arc<dyn HttpTransport>,
        credentials: &TencentCredentials,
        service: &str,
        version: &str,
        action: &str,
        payload: Value,
    ) -> Result<Value> {
        let host = api_host(service, credentials.account_site);
        let timestamp = chrono::Utc::now().timestamp();
        let body = payload.to_string();
        let authorization =
            tc3_authorization(credentials, timestamp, service, &host, action, &body);

        // X-TC-Timestamp 必须与签名用的时间戳同源：差一秒签名就作废。
        let request = HttpRequest::new("POST", format!("https://{host}/"))
            .with_header("Content-Type", "application/json; charset=utf-8")
            .with_header("X-TC-Action", action)
            .with_header("X-TC-Version", version)
            .with_header("X-TC-Timestamp", timestamp.to_string())
            .with_header("Authorization", authorization)
            .with_body(payload);

        let response = http.send(request).await?;
        ensure_success(response, action)
    }
}

/// 腾讯云常见业务错误码的定位提示。
///
/// 探测失败的提示要让人「一眼知道去哪修」：凭据页的原因文本按错误码
/// 给出一句可操作的中文，而不是透传服务端的英文长句。未收录的错误码
/// 走通用格式透传原始 message。
fn error_hint(code: &str) -> Option<&'static str> {
    match code {
        "AuthFailure.UnauthorizedOperation" => {
            Some("密钥未被授权该接口，请在腾讯云 CAM 为此密钥补授权")
        }
        "AuthFailure.SecretIdNotFound" => Some("SecretId 不存在，请核对密钥与账号站点是否匹配"),
        "AuthFailure.SignatureFailure" => Some("签名不匹配，请核对 SecretKey 与账号站点"),
        "AuthFailure.TokenFailure" => Some("临时密钥已失效，请改用长期密钥"),
        _ if code.starts_with("UnauthorizedOperation") => {
            Some("密钥未被授权该接口，请在腾讯云 CAM 为此密钥补授权")
        }
        _ if code.starts_with("OperationDenied") => {
            Some("操作被拒绝（多为权限不足），请核对该密钥的 CAM 授权")
        }
        _ => None,
    }
}

/// 校验响应成功，并取回 `Response` 节点。
///
/// 腾讯云 API 3.0 的业务错误也回 HTTP 200——错误藏在 `Response.Error` 里，
/// 所以判断顺序是先看业务错误、再看传输层状态；网关层错误（502 等）通常
/// 连 JSON 都没有，原文保留进错误信息才有排查线索。
pub fn ensure_success(response: HttpResponse, action: &str) -> Result<Value> {
    let body = response
        .json()
        .unwrap_or_else(|_| json!({ "raw": &response.body }));

    if let Some(code) = body["Response"]["Error"]["Code"].as_str() {
        let message = body["Response"]["Error"]["Message"].as_str().unwrap_or("");
        let request_id = body["Response"]["RequestId"].as_str().unwrap_or("");
        // 常见错误码给一句可操作的中文提示；未收录的透传原始 message。
        // request id 统一放尾部，供提工单定位。
        return Err(Error::provider(match error_hint(code) {
            Some(hint) => format!("腾讯云 {action} 被拒绝（{code}）：{hint} [request id: {request_id}]"),
            None => format!("腾讯云 {action} 失败：{code}: {message} [request id: {request_id}]"),
        }));
    }

    if response.is_success() {
        return Ok(body["Response"].clone());
    }

    Err(Error::provider(format!(
        "腾讯云 {action} 失败（HTTP {}）：{body}",
        response.status
    )))
}

/// 腾讯云云解析（DNSPod）。
///
/// DNSPod 的记录模型是「域名 + 主机记录」：zone 就是注册域，
/// 记录名要先剥成相对主机记录（`relative_name`）再提交。
#[derive(Debug)]
pub struct TencentProvider {
    client: TencentApiClient,
    /// 全局出网代理（`[proxy] url` / `ACMECAST_PROXY_URL`）；仅
    /// `proxy_mode=inherit` 的凭据读它。`None` 即直连。
    global_proxy_url: Option<String>,
    /// 网络操作超时（`[network] timeout_secs`）：`proxy_mode=custom` 建
    /// reqwest 客户端时施加请求级超时。
    network_timeout: std::time::Duration,
}

impl TencentProvider {
    /// 走真实网络。
    #[must_use]
    pub fn new() -> Self {
        Self::with_transport(Arc::new(ReqwestTransport::default()))
    }

    /// 注入传输层，供测试用。
    #[must_use]
    pub fn with_transport(http: Arc<dyn HttpTransport>) -> Self {
        Self {
            client: TencentApiClient::with_transport(http),
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
            &self.client.http,
            global_proxy_url,
            self.network_timeout,
        )
    }

    /// 探测凭据的 DNS 用途是否可用：调 DNSPod 的 `DescribeDomainList`。
    ///
    /// 这是凭据页「测试」按钮在凭据声明了 DNS 能力时的探测动作——直接
    /// 探测 DNS 业务所需的最轻只读接口（域名列表单页，`Limit=1`）：密钥
    /// 有效且已授权 DNSPod 只读权限即返回可用。端点随凭据的
    /// `account_site` 切换（国内/国际站）。
    pub async fn verify_credentials(&self, credentials: &Value) -> Result<()> {
        let http = self.http_for(credentials, self.global_proxy_url())?;
        let credentials: TencentCredentials = parse_credentials(credentials)?;
        self.client
            .call_with(
                &http,
                &credentials,
                DNSPOD_SERVICE,
                DNSPOD_VERSION,
                "DescribeDomainList",
                json!({ "Limit": 1 }),
            )
            .await
            .map(|_| ())
    }

    /// 探测凭据的部署用途是否可用：调 SSL 的 `DescribeCertificates`。
    ///
    /// 这是凭据页「测试」按钮在凭据仅声明部署能力时的探测动作——探测
    /// SSL 部署业务所需的最轻只读接口（证书列表单页，`Limit=1`），与
    /// 部署路径共用同一服务与签名。端点随凭据的 `account_site` 切换。
    pub async fn verify_credentials_for_deploy(&self, credentials: &Value) -> Result<()> {
        let http = self.http_for(credentials, self.global_proxy_url())?;
        let credentials: TencentCredentials = parse_credentials(credentials)?;
        self.client
            .call_with(
                &http,
                &credentials,
                SSL_SERVICE,
                SSL_VERSION,
                "DescribeCertificates",
                json!({ "Limit": 1 }),
            )
            .await
            .map(|_| ())
    }

    /// 列出某个主机记录下的全部 TXT 记录（RecordId 与值）。
    async fn list_records(
        &self,
        http: &Arc<dyn HttpTransport>,
        credentials: &TencentCredentials,
        record: &TxtRecord,
    ) -> Result<Vec<(i64, String)>> {
        let body = self
            .client
            .call_with(
                http,
                credentials,
                DNSPOD_SERVICE,
                DNSPOD_VERSION,
                "DescribeRecordFilterList",
                json!({
                    "Domain": record.zone,
                    "SubDomain": record.relative_name()?,
                    "RecordType": ["TXT"],
                }),
            )
            .await?;

        Ok(body["RecordList"]
            .as_array()
            .map(|records| {
                records
                    .iter()
                    .filter_map(|record| {
                        Some((
                            record["RecordId"].as_i64()?,
                            record["Value"].as_str()?.to_owned(),
                        ))
                    })
                    .collect()
            })
            .unwrap_or_default())
    }
}

impl Default for TencentProvider {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl DnsProvider for TencentProvider {
    fn type_id(&self) -> &'static str {
        "cloud.tencent"
    }

    fn display_name(&self) -> &'static str {
        "腾讯云"
    }

    fn credential_fields(&self) -> RootSchema {
        schema_for!(TencentCredentials)
    }

    async fn create_txt(&self, credentials: &Value, record: &TxtRecord) -> Result<()> {
        let http = self.http_for(credentials, self.global_proxy_url())?;
        let credentials: TencentCredentials = parse_credentials(credentials)?;

        self.client
            .call_with(
                &http,
                &credentials,
                DNSPOD_SERVICE,
                DNSPOD_VERSION,
                "CreateRecord",
                json!({
                    "Domain": record.zone,
                    "SubDomain": record.relative_name()?,
                    "RecordType": "TXT",
                    "RecordLine": "默认",
                    "Value": record.value,
                    "TTL": record.ttl.clamp(TTL_MIN, TTL_MAX),
                }),
            )
            .await?;
        Ok(())
    }

    async fn probe_zone(&self, credentials: &Value, zone: &str) -> Result<()> {
        let http = self.http_for(credentials, self.global_proxy_url())?;
        let credentials: TencentCredentials = parse_credentials(credentials)?;
        // DescribeDomain 对不存在的域名报错，存在即托管在 DNSPod。
        self.client
            .call_with(
                &http,
                &credentials,
                DNSPOD_SERVICE,
                DNSPOD_VERSION,
                "DescribeDomain",
                json!({ "Domain": zone }),
            )
            .await?;
        Ok(())
    }

    async fn find_txt(&self, credentials: &Value, record: &TxtRecord) -> Result<Vec<String>> {
        let http = self.http_for(credentials, self.global_proxy_url())?;
        let credentials: TencentCredentials = parse_credentials(credentials)?;

        Ok(self
            .list_records(&http, &credentials, record)
            .await?
            .into_iter()
            .map(|(_, value)| value)
            .collect())
    }

    async fn delete_txt(&self, credentials: &Value, record: &TxtRecord) -> Result<()> {
        let http = self.http_for(credentials, self.global_proxy_url())?;
        let credentials: TencentCredentials = parse_credentials(credentials)?;

        // 删除要 RecordId，而 trait 的入参只有记录本身；先查出来，
        // 并且**只删值与我们这条相同的**——同名但不是我们写的记录不该被替人清掉。
        let ids: Vec<i64> = self
            .list_records(&http, &credentials, record)
            .await?
            .into_iter()
            .filter(|(_, value)| value == &record.value)
            .map(|(id, _)| id)
            .collect();

        // 一条都没找到也算成功：删除是幂等的，清理路径上不该为「它本来就不在」报错。
        for id in ids {
            self.client
                .call_with(
                    &http,
                    &credentials,
                    DNSPOD_SERVICE,
                    DNSPOD_VERSION,
                    "DeleteRecord",
                    json!({ "Domain": record.zone, "RecordId": id }),
                )
                .await?;
        }
        Ok(())
    }
}

/// 腾讯云体系凭据字段的 schema（云解析与 EdgeOne 共用）。
///
/// serde_json 的 Map 会把 schemars 产物的 `properties` 按字母序重排——
/// 这里用 `x-field-order` 钉住渲染顺序；`account_site` 再挂 `x-end-row`
/// （半宽框渲染后补空列占位换行）：站点选择独占第一行但框保持一列宽，
/// 两半密钥成对排在第二行。
#[must_use]
pub fn tencent_credentials_schema() -> RootSchema {
    let mut schema = schema_for!(TencentCredentials);
    schema.schema.extensions.insert(
        "x-field-order".to_owned(),
        serde_json::json!([
            "account_site",
            "secret_id",
            "secret_key",
            "proxy_mode",
            "proxy_url"
        ]),
    );
    // account_site 独占第一行但框保持一列宽（x-end-row 补空列占位换行）：
    // 必须在 proxy 注入前标注，否则 inject 的栅格模拟会误判 secret_key 需补空列，
    // 导致 secret_id|secret_key 已占满一行后仍多出空行、proxy 行被挤错。
    if let Some(validation) = schema.schema.object.as_mut()
        && let Some(schemars::schema::Schema::Object(site)) =
            validation.properties.get_mut("account_site")
    {
        site.extensions
            .insert("x-end-row".to_owned(), serde_json::Value::Bool(true));
    }
    inject_proxy_field_annotations(&mut schema);
    schema
}

#[cfg(test)]
mod tests {
    use super::*;

    fn credentials(site: TencentSite) -> TencentCredentials {
        TencentCredentials {
            secret_id: "AKIDexample".to_owned(),
            secret_key: "secret-key".to_owned(),
            account_site: site,
            proxy_mode: None,
            proxy_url: None,
        }
    }

    #[test]
    fn the_host_follows_the_service_and_the_site() {
        assert_eq!(
            api_host("dnspod", TencentSite::Cn),
            "dnspod.tencentcloudapi.com"
        );
        assert_eq!(
            api_host("dnspod", TencentSite::Intl),
            "dnspod.intl.tencentcloudapi.com"
        );
        assert_eq!(
            api_host("teo", TencentSite::Intl),
            "teo.intl.tencentcloudapi.com"
        );
    }

    #[test]
    fn the_authorization_follows_the_documented_recipe() {
        // 官方「签名方法 v3」示例向量（DescribeInstances，cam.org 那条）的
        // 同构手工复算：固定时间戳与 payload，用 hmac 原语按文档四步独立
        // 算一遍，再与 tc3_authorization 的输出比对。
        let credentials = credentials(TencentSite::Cn);
        let timestamp = 1_667_117_385_i64;
        let service = "dnspod";
        let host = api_host(service, TencentSite::Cn);
        let action = "CreateRecord";
        let payload = "{}";

        let date = chrono::DateTime::from_timestamp(timestamp, 0)
            .unwrap()
            .format("%Y-%m-%d")
            .to_string();

        let canonical_request = format!(
            "POST\n/\n\ncontent-type:application/json; charset=utf-8\nhost:{host}\nx-tc-action:createrecord\n\ncontent-type;host;x-tc-action\n{}",
            sha256_hex(payload.as_bytes())
        );
        let string_to_sign = format!(
            "TC3-HMAC-SHA256\n{timestamp}\n{date}/{service}/tc3_request\n{}",
            sha256_hex(canonical_request.as_bytes())
        );
        let k_date = hmac_bytes(b"TC3secret-key", date.as_bytes());
        let k_service = hmac_bytes(&k_date, service.as_bytes());
        let k_signing = hmac_bytes(&k_service, b"tc3_request");
        let expected_signature = hex(&hmac_bytes(&k_signing, string_to_sign.as_bytes()));

        let authorization =
            tc3_authorization(&credentials, timestamp, service, &host, action, payload);
        assert_eq!(
            authorization,
            format!(
                "TC3-HMAC-SHA256 Credential=AKIDexample/{date}/{service}/tc3_request, \
                 SignedHeaders=content-type;host;x-tc-action, Signature={expected_signature}"
            )
        );
    }

    #[test]
    fn the_signature_changes_with_the_secret() {
        let first = tc3_authorization(
            &credentials(TencentSite::Cn),
            123,
            "dnspod",
            "dnspod.tencentcloudapi.com",
            "CreateRecord",
            "{}",
        );
        let other = TencentCredentials {
            secret_key: "another-key".to_owned(),
            ..credentials(TencentSite::Cn)
        };
        let second = tc3_authorization(
            &other,
            123,
            "dnspod",
            "dnspod.tencentcloudapi.com",
            "CreateRecord",
            "{}",
        );
        assert_ne!(first, second);
    }

    #[test]
    fn the_site_defaults_to_cn_and_rejects_unknowns() {
        let parsed: TencentCredentials =
            serde_json::from_value(json!({ "secret_id": "id", "secret_key": "key" }))
                .expect("缺 account_site 应默认国内站");
        assert_eq!(parsed.account_site, TencentSite::Cn);

        let parsed: TencentCredentials = serde_json::from_value(json!({
            "secret_id": "id", "secret_key": "key", "account_site": "intl"
        }))
        .expect("intl 应能解析");
        assert_eq!(parsed.account_site, TencentSite::Intl);

        serde_json::from_value::<TencentCredentials>(json!({
            "secret_id": "id", "secret_key": "key", "account_site": "eu"
        }))
        .expect_err("未知站点应被拒绝");
    }

    #[test]
    fn a_business_error_surfaces_even_over_http_200() {
        // 腾讯云 API 3.0 的业务错误也回 HTTP 200：错误在 Response.Error 里。
        let response = HttpResponse::new(
            200,
            r#"{"Response":{"Error":{"Code":"AuthFailure.SignatureFailure","Message":"签名不匹配"},"RequestId":"req-1"}}"#,
        );
        let err = ensure_success(response, "CreateRecord").expect_err("业务错误应报错");
        let text = err.to_string();
        assert!(text.contains("AuthFailure.SignatureFailure"), "{text}");
        assert!(text.contains("签名不匹配"), "{text}");
    }

    #[test]
    fn a_gateway_error_keeps_the_raw_body() {
        let response = HttpResponse::new(502, "<html>Bad Gateway</html>");
        let err = ensure_success(response, "CreateRecord").expect_err("网关错误应报错");
        assert!(err.to_string().contains("Bad Gateway"), "{err}");
    }

    #[test]
    fn a_success_response_unwraps_the_response_node() {
        let response = HttpResponse::new(200, r#"{"Response":{"RecordId":162}}"#);
        let body = ensure_success(response, "CreateRecord").expect("成功应返回 Response 节点");
        assert_eq!(body["RecordId"], 162);
    }

    #[test]
    fn the_schema_keeps_the_declared_field_order() {
        let rendered = serde_json::to_string(&tencent_credentials_schema()).unwrap();
        // Map 会把 properties 按字母序重排，布局全靠 x-field-order 钉住：
        // 站点选择打头（x-end-row 独占一行半宽），两半密钥成对随后；
        // 代理行独立（secret_key 已在行尾，proxy 自然新行起，不补 x-end-row）。
        assert!(
            rendered.contains(
                r#""x-field-order":["account_site","secret_id","secret_key","proxy_mode","proxy_url"]"#
            ),
            "应注入声明顺序: {rendered}"
        );
        let value: serde_json::Value = serde_json::from_str(&rendered).unwrap();
        assert_eq!(
            value["properties"]["account_site"]["x-end-row"],
            serde_json::json!(true),
            "account_site 应标注 x-end-row 独占一行半宽: {rendered}"
        );
        assert!(
            !rendered.contains("x-full-width"),
            "不做整行标注，框保持一列宽: {rendered}"
        );
        // secret_id|secret_key 已占满一行，proxy 行自然新行起，无需 secret_key 补 x-end-row。
        assert!(
            value["properties"]["secret_key"]["x-end-row"].is_null(),
            "secret_key 已在行尾、proxy 自然换行，不应带 x-end-row: {rendered}"
        );
        // proxy_mode 是枚举下拉：三候选 + 中文展示名，schemars 对
        // Option<String> 不产 enum 数组，由注入函数手动补。
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
    }
}
