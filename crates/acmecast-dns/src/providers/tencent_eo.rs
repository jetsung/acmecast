//! 腾讯云 EdgeOne 的 DNS 提供商。
//!
//! 复用云解析提供商的 API 3.0 基础（签名、凭据、端点），只换服务名、
//! 版本与 Action。与 DNSPod 的「域名 + 主机记录」不同，EdgeOne 的记录
//! 挂在 zone（站点）下：先按 zone 名换 `ZoneId`，再操作完整记录名。
//!
//! # API 文档
//!
//! - 腾讯云 EdgeOne（国内）：
//!   <https://cloud.tencent.com/document/product/1552/80720>
//! - 腾讯云 EdgeOne（国际）：
//!   <https://www.tencentcloud.com/zh/document/product/1145/50488>

use std::sync::Arc;

use async_trait::async_trait;
use schemars::schema::RootSchema;
use schemars::schema_for;
use serde_json::{Value, json};

use super::tencent::{
    STS_SERVICE, STS_VERSION, TTL_MAX, TTL_MIN, TencentApiClient, TencentCredentials,
};
use crate::error::{Error, Result};
use crate::http::{HttpTransport, ReqwestTransport};
use crate::provider::{DnsProvider, TxtRecord, parse_credentials};

/// EdgeOne（TEO）API 3.0 的服务名与版本。
const TEO_SERVICE: &str = "teo";
const TEO_VERSION: &str = "2022-09-01";

/// 腾讯云 EdgeOne 的 DNS 解析。
///
/// 挑战记录挂在 EdgeOne 托管的 zone 下，`dns_zone` 必须是已接入
/// EdgeOne 的站点域名；只托管在 DNSPod 的域名在这里查不到 zone。
#[derive(Debug)]
pub struct TencentEoProvider {
    client: TencentApiClient,
}

impl TencentEoProvider {
    /// 走真实网络。
    #[must_use]
    pub fn new() -> Self {
        Self::with_transport(Arc::new(ReqwestTransport::default()))
    }

    /// 注入传输层，供测试用。
    #[must_use]
    pub fn with_transport(http: Arc<dyn HttpTransport>) -> Self {
        Self {
            client: TencentApiClient { http },
        }
    }

    /// 探测凭据是否可用：调 STS 的 `GetCallerIdentity`（与云解析提供商同款）。
    ///
    /// EdgeOne 凭据与腾讯云 DNS 凭据是同一套密钥体系，探测也走同一个
    /// 身份接口；端点随凭据的 `account_site` 切换。
    pub async fn verify_credentials(&self, credentials: &Value) -> Result<()> {
        let credentials: TencentCredentials = parse_credentials(credentials)?;
        self.client
            .call(
                &credentials,
                STS_SERVICE,
                STS_VERSION,
                "GetCallerIdentity",
                json!({}),
            )
            .await
            .map(|_| ())
    }

    /// 按 zone 名换 `ZoneId`。
    ///
    /// 每次现查、不缓存：一次挑战只有寥寥几次请求，缓存的失效复杂度
    /// 换不来可感知的收益，与 Cloudflare 提供商每次换 zone_id 是同一取舍。
    async fn zone_id(&self, credentials: &TencentCredentials, zone: &str) -> Result<String> {
        let body = self
            .client
            .call(
                credentials,
                TEO_SERVICE,
                TEO_VERSION,
                "DescribeZones",
                json!({
                    "Filters": [{ "Name": "zone-name", "Values": [zone] }],
                }),
            )
            .await?;

        body["Zones"]
            .as_array()
            .and_then(|zones| zones.first())
            .and_then(|zone| zone["ZoneId"].as_str())
            .map(str::to_owned)
            .ok_or_else(|| {
                Error::provider(format!(
                    "EdgeOne 上找不到 zone `{zone}`——请确认域名已接入 EdgeOne（仅托管在云解析 DNSPod 的域名不适用本提供商）"
                ))
            })
    }

    /// 列出某个记录名下的全部 TXT 记录（RecordId 与内容）。
    ///
    /// RecordId 用原样 JSON 值承载：国内站是数字，国际站是
    /// `record-xxxx` 字符串，删除时要原样传回，不能假定整数。
    async fn list_records(
        &self,
        credentials: &TencentCredentials,
        zone_id: &str,
        record: &TxtRecord,
    ) -> Result<Vec<(Value, String)>> {
        let body = self
            .client
            .call(
                credentials,
                TEO_SERVICE,
                TEO_VERSION,
                "DescribeDnsRecords",
                json!({
                    "ZoneId": zone_id,
                    "Filters": [
                        { "Name": "name", "Values": [record.name] },
                        { "Name": "type", "Values": ["TXT"] },
                    ],
                    // teo 的 DescribeDnsRecords 默认每页 20 条，顶格到上限
                    // 1000：同名 TXT 多于 20 条时被截断会让清理漏删残留。
                    "Limit": 1000,
                }),
            )
            .await?;

        Ok(body["DnsRecords"]
            .as_array()
            .map(|records| {
                records
                    .iter()
                    .filter_map(|record| {
                        Some((
                            record["RecordId"].clone(),
                            record["Content"].as_str()?.to_owned(),
                        ))
                    })
                    .collect()
            })
            .unwrap_or_default())
    }
}

impl Default for TencentEoProvider {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl DnsProvider for TencentEoProvider {
    fn type_id(&self) -> &'static str {
        "tencent-eo"
    }

    fn display_name(&self) -> &'static str {
        "腾讯云 EdgeOne"
    }

    fn credential_fields(&self) -> RootSchema {
        schema_for!(TencentCredentials)
    }

    async fn create_txt(&self, credentials: &Value, record: &TxtRecord) -> Result<()> {
        let credentials: TencentCredentials = parse_credentials(credentials)?;
        let zone_id = self.zone_id(&credentials, &record.zone).await?;

        self.client
            .call(
                &credentials,
                TEO_SERVICE,
                TEO_VERSION,
                "CreateDnsRecord",
                json!({
                    "ZoneId": zone_id,
                    "Name": record.name,
                    "Type": "TXT",
                    "Content": record.value,
                    "TTL": record.ttl.clamp(TTL_MIN, TTL_MAX),
                }),
            )
            .await?;
        Ok(())
    }

    async fn find_txt(&self, credentials: &Value, record: &TxtRecord) -> Result<Vec<String>> {
        let credentials: TencentCredentials = parse_credentials(credentials)?;
        let zone_id = self.zone_id(&credentials, &record.zone).await?;

        Ok(self
            .list_records(&credentials, &zone_id, record)
            .await?
            .into_iter()
            .map(|(_, content)| content)
            .collect())
    }

    async fn delete_txt(&self, credentials: &Value, record: &TxtRecord) -> Result<()> {
        let credentials: TencentCredentials = parse_credentials(credentials)?;
        let zone_id = self.zone_id(&credentials, &record.zone).await?;

        // 先查后删，并且**只删内容与我们这条相同的**——同名但不是
        // 我们写的记录不该被替人清掉；一条都没有就视为删除成功。
        let ids: Vec<Value> = self
            .list_records(&credentials, &zone_id, record)
            .await?
            .into_iter()
            .filter(|(_, content)| content == &record.value)
            .map(|(id, _)| id)
            .collect();

        if ids.is_empty() {
            return Ok(());
        }
        self.client
            .call(
                &credentials,
                TEO_SERVICE,
                TEO_VERSION,
                "DeleteDnsRecords",
                json!({ "ZoneId": zone_id, "RecordIds": ids }),
            )
            .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_display_name_identifies_the_product() {
        assert_eq!(TencentEoProvider::new().type_id(), "tencent-eo");
        assert_eq!(TencentEoProvider::new().display_name(), "腾讯云 EdgeOne");
    }
}
