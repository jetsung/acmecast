//! 腾讯云 SSL 证书库部署（含国际站）。
//!
//! 把证书链与私钥上传到腾讯云 SSL 证书服务（API 3.0，服务 `ssl`），供
//! CLB / CDN / WAF 等云资源引用。部署前先查云端是否已有**同域名**证书，
//! 已有则更新已有证书的证书内容，原证书 ID 不变，引用该 ID 的云资源无需改绑。
//!
//! 国内站与国际站是两套互不相通的账户体系（见 [`acmecast_dns::TencentSite`]），
//! 但 SSL 证书 API 的 Action 与参数结构相同，凭据的 `account_site` 决定端点。
//!
//! # API 文档
//!
//! - 上传证书（国内版）：<https://cloud.tencent.com/document/api/400/41665>
//! - 上传证书（国际版）：<https://www.tencentcloud.com/zh/document/product/1007/36579>
//! - 查询证书列表：<https://cloud.tencent.com/document/api/400/41671>
//! - 删除证书：<https://cloud.tencent.com/document/api/400/107800>
//! - 更新证书并同步云资源（`UploadUpdateCertificateInstance`）：
//!   <https://cloud.tencent.com/document/api/400/119791>

use std::sync::Arc;

use acmecast_access::CredentialStore;
use acmecast_cert::parse_pem_leaf;
use acmecast_dns::TencentApiClient;
use acmecast_dns::http::{HttpTransport, ReqwestTransport};
use acmecast_dns::providers::tencent::TencentCredentials;
use async_trait::async_trait;
use schemars::schema::RootSchema;
use schemars::schema_for;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::error::{Error, Result};
use crate::target::{CertMaterials, DeployMode, DeployOutcome, DeploymentTarget, parse_input};

/// SSL 证书 API 3.0 的服务名与版本（国内站与国际站同构）。
const SSL_SERVICE: &str = "ssl";
const SSL_VERSION: &str = "2019-12-05";

/// `DescribeCertificates` 的单页数量上限（文档允许的最大值，顶格防漏判）。
const PAGE_LIMIT: u64 = 1000;

/// `credential_id` 可引用的凭据类型：仅腾讯云账号密钥（DNS 与 SSL 通用）。
/// `tencent-eo` 不含 SSL 部署能力（EdgeOne 仅服务 DNS），不接受。
const ACCEPTED_CREDENTIAL_TYPES: [&str; 1] = ["tencent"];

/// 腾讯云 SSL 证书库部署。
#[derive(Debug)]
pub struct TencentSslTarget {
    client: TencentApiClient,
}

/// 腾讯云 SSL 部署的输入。
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct TencentSslInput {
    /// 引用一张 `tencent` 类型凭据（腾讯云账号密钥，DNS 与 SSL 通用）。
    /// `tencent-eo` 不含 SSL 部署能力，不接受。
    pub credential_id: i64,
    /// 上传到云端的证书备注名；不填则不设置。
    #[serde(default)]
    pub alias: Option<String>,
    /// 上传请求的 `Repeatable`：`false`（默认）时相同指纹的证书已存在返回
    /// 云端已有证书 ID、不产生重复；`true` 时允许重复上传。
    #[serde(default)]
    pub repeatable: bool,
    /// 云端已有同域名证书时是否替换。
    ///
    /// 替换走 `UploadUpdateCertificateInstance`：原证书 ID 不变、只更新证书
    /// 内容，可选同步刷新绑定的云资源（`resource_types` 指定资源类型）。
    #[serde(default = "default_replace_existing")]
    pub replace_existing: bool,
    /// 替换时需同步刷新的云资源类型列表（对应 `ResourceTypes`）。
    ///
    /// 空时仅更新云端证书本身，已绑定资源不自动刷新。支持：`clb`、`cdn`、
    /// `waf`、`live`、`ddos`、`teo`、`apigateway`、`vod`、`tke`、`tcb`、`tse`、
    /// `cos`、`mqtt`、`scf`、`tdmq`、`gaap`。
    #[serde(default)]
    pub resource_types: Vec<String>,
    /// `resource_types` 指定的云资源的地域列表（构造 `ResourceTypesRegions`）。
    /// 未指定地域或 `resource_types` 为空时不额外携带或由资源类型决定。
    #[serde(default)]
    pub resource_regions: Vec<String>,
}

fn default_replace_existing() -> bool {
    true
}

impl TencentSslInput {
    /// 备注名的提交形态：空白的按未填写处理。
    fn alias_value(&self) -> Option<&str> {
        self.alias
            .as_deref()
            .map(str::trim)
            .filter(|alias| !alias.is_empty())
    }
}

impl TencentSslTarget {
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
        }
    }

    /// 查出云端与新证书主域名相同的全部服务器证书 ID。
    ///
    /// `SearchKey` 只是把候选集缩小到「域名沾边的证书」的加速手段，服务端的
    /// 模糊匹配语义并不可控，因此正确性落在本地：全部分页拉完后按主域名
    /// **精确相等**过滤（含泛域名形式），绝不多删一张。
    async fn find_existing_certificates(
        &self,
        credentials: &TencentCredentials,
        domain: &str,
    ) -> Result<Vec<String>> {
        // 通配符会干扰服务端匹配：`*.example.com` 用剥掉 `*.` 的部分去搜。
        let search_key = domain.trim_start_matches("*.");
        let mut matched = Vec::new();
        let mut offset = 0;
        loop {
            let body = self
                .call(
                    credentials,
                    "DescribeCertificates",
                    json!({
                        "Offset": offset,
                        "Limit": PAGE_LIMIT,
                        "SearchKey": search_key,
                        "CertificateType": "SVR",
                    }),
                )
                .await?;

            let page = body["Certificates"].as_array().cloned().unwrap_or_default();
            for cert in &page {
                let is_same_domain = cert["Domain"].as_str() == Some(domain);
                let is_server_cert = cert["CertificateType"].as_str() == Some("SVR");
                if is_same_domain && is_server_cert
                    && let Some(id) = cert["CertificateId"].as_str()
                {
                    matched.push(id.to_owned());
                }
            }

            offset += PAGE_LIMIT;
            let total = body["TotalCount"].as_u64().unwrap_or(0);
            // 翻页由服务端声称的总数驱动；总数缺失（防御）时第一页已处理，就此打住。
            if offset >= total {
                break;
            }
        }
        Ok(matched)
    }

    /// 上传证书，返回云端证书 ID。
    ///
    /// `Repeatable=false`：相同指纹的证书已存在时服务端直接返回那张的 ID，
    /// 不在证书库产生重复——上传动作因此天然幂等。`Repeatable=true` 时允许
    /// 重复上传（用于需要多张同指纹证书的场景）。
    async fn upload_certificate(
        &self,
        credentials: &TencentCredentials,
        input: &TencentSslInput,
        materials: &CertMaterials,
    ) -> Result<String> {
        let mut payload = json!({
            "CertificatePublicKey": materials.chain_pem,
            "CertificatePrivateKey": materials.key_pem,
            "CertificateType": "SVR",
            "Repeatable": input.repeatable,
        });
        if let Some(alias) = input.alias_value() {
            payload["Alias"] = json!(alias);
        }

        let body = self.call(credentials, "UploadCertificate", payload).await?;
        body["CertificateId"]
            .as_str()
            .map(str::to_owned)
            .ok_or_else(|| {
                Error::cloud(
                    "tencent-ssl",
                    format!("UploadCertificate 响应缺少 CertificateId: {body}"),
                )
            })
    }

    /// 更新云端已有证书的内容（证书链与私钥），保持原证书 ID 不变。
    ///
    /// 走 `UploadUpdateCertificateInstance`（<https://cloud.tencent.com/document/api/400/119791>）：
    /// 相比「上传新 + 删除旧」，原证书 ID 不变，引用该 ID 的云资源无需改绑。
    /// `resource_types` 中选择的资源类型携带 `ResourceTypes` 与
    /// `ResourceTypesRegions`，使绑定的云资源同步刷新为新证书内容。
    async fn update_certificate_instance(
        &self,
        credentials: &TencentCredentials,
        input: &TencentSslInput,
        materials: &CertMaterials,
        certificate_id: &str,
    ) -> Result<()> {
        let mut payload = json!({
            "CertificateId": certificate_id,
            "CertificatePublicKey": materials.chain_pem,
            "CertificatePrivateKey": materials.key_pem,
            "Repeatable": input.repeatable,
        });
        if !input.resource_types.is_empty() {
            payload["ResourceTypes"] = json!(input.resource_types);
            if !input.resource_regions.is_empty() {
                let regions = input.resource_regions.clone();
                payload["ResourceTypesRegions"] = json!(input
                    .resource_types
                    .iter()
                    .flat_map(|resource_type| {
                        regions.iter().map(move |region| {
                            json!({ "Region": region, "Type": resource_type })
                        })
                    })
                    .collect::<Vec<_>>());
            }
        }

        self.call(credentials, "UploadUpdateCertificateInstance", payload)
            .await?;
        Ok(())
    }

    /// 发一次 SSL 证书 API 调用，把 DNS crate 的错误换算成部署侧语义。
    async fn call(
        &self,
        credentials: &TencentCredentials,
        action: &str,
        payload: Value,
    ) -> Result<Value> {
        self.client
            .call(credentials, SSL_SERVICE, SSL_VERSION, action, payload)
            .await
            .map_err(|error| {
                // 剥掉 DNS crate 的错误外壳（「调用 DNS 服务失败」），保留
                // 厂商原始错误码——那才是排查要看的东西。
                let reason = match &error {
                    acmecast_dns::Error::Provider(reason) => reason.clone(),
                    other => other.to_string(),
                };
                Error::cloud("tencent-ssl", format!("{action}: {reason}"))
            })
    }
}

impl Default for TencentSslTarget {
    fn default() -> Self {
        Self::new()
    }
}

/// 取出输入引用的腾讯云凭据，并校验类型白名单。
async fn resolve_tencent_credentials(
    id: i64,
    credentials: &CredentialStore<'_>,
) -> Result<TencentCredentials> {
    let resolved = credentials
        .resolve(id)
        .await
        .map_err(|e| Error::Remote(format!("取凭据 `{id}` 失败: {e}")))?;

    if !ACCEPTED_CREDENTIAL_TYPES.contains(&resolved.type_id.as_str()) {
        return Err(Error::invalid_input(
            "credential_id",
            format!(
                "凭据 `{id}`（{}）不是可用于 SSL 部署的腾讯云凭据，需要 tencent 类型（tencent-eo 仅服务 DNS，无 SSL 部署能力）",
                resolved.type_id
            ),
        ));
    }

    serde_json::from_value(resolved.fields).map_err(|e| {
        Error::invalid_input("credential_id", format!("凭据 `{id}` 的字段不合法: {e}"))
    })
}

/// 从证书链解析主域名（叶子证书的 SAN 首项）。
fn primary_domain_of(materials: &CertMaterials) -> Result<String> {
    let leaf = parse_pem_leaf(&materials.chain_pem).map_err(|e| {
        Error::invalid_input("materials", format!("证书链解析失败: {e}"))
    })?;
    leaf.primary_domain()
        .map(str::to_owned)
        .ok_or_else(|| Error::invalid_input("materials", "证书没有主域名（SAN 与 CN 均为空）"))
}

#[async_trait]
impl DeploymentTarget for TencentSslTarget {
    fn type_id(&self) -> &'static str {
        "tencent-ssl"
    }

    fn display_name(&self) -> &'static str {
        "腾讯云 SSL 证书"
    }

    fn supports_skip_write(&self) -> bool {
        true
    }

    fn input_schema(&self) -> RootSchema {
        use schemars::schema::Schema;

        let mut schema = schema_for!(TencentSslInput);
        // serde_json 的 Map 会把 schemars 产物的 properties 按字母序重排——
        // 用 x-field-order 钉住渲染顺序（凭据引用、备注、重复开关、替换开关、资源类型与地域）。
        schema.schema.extensions.insert(
            "x-field-order".to_owned(),
            json!([
                "credential_id",
                "alias",
                "repeatable",
                "replace_existing",
                "resource_types",
                "resource_regions"
            ]),
        );
        // credential_id 仅接受 tencent 凭据：前端据此过滤下拉，避免选到
        // tencent-eo（EdgeOne 无 SSL 部署能力）或 SSH 等无关凭据。
        if let Some(Schema::Object(credential_id)) = schema
            .schema
            .object
            .as_mut()
            .and_then(|object| object.properties.get_mut("credential_id"))
        {
            credential_id
                .extensions
                .insert("x-credential-types".to_owned(), json!(["tencent"]));
        }
        schema
    }

    fn example_input(&self) -> Value {
        json!({
            "credential_id": 1,
            "alias": "acmecast example.com",
            "repeatable": false,
            "replace_existing": true,
            "resource_types": [],
            "resource_regions": [],
        })
    }

    async fn deploy(
        &self,
        input: &Value,
        materials: &CertMaterials,
        credentials: &CredentialStore<'_>,
        mode: DeployMode,
    ) -> Result<DeployOutcome> {
        let input: TencentSslInput = parse_input(input)?;

        // 指纹与上次一致且未强制：云端状态被假定未变，什么都不发。若云端
        // 证书被人手动删了，force 重跑一次即可恢复——与 local 目标跳过
        // 重写文件的语义一致。
        if mode == DeployMode::SkipWrite {
            return Ok(DeployOutcome::skipped(Vec::new()));
        }

        let tencent_credentials = resolve_tencent_credentials(input.credential_id, credentials).await?;
        let domain = primary_domain_of(materials)?;

        // 已有的同域名证书：只在开启替换时才查——关闭替换时查询结果用不上，
        // 白白多一次调用。
        let existing = if input.replace_existing {
            self.find_existing_certificates(&tencent_credentials, &domain)
                .await?
        } else {
            Vec::new()
        };

        // 有存量：逐张调用 UploadUpdateCertificateInstance 原地更新，原证书
        // ID 不变，引用旧 ID 的 CLB 等云资源无需改绑。任一更新失败记告警、
        // 不阻塞其他证书与整体部署——与原「删除失败不阻塞」语义对齐。
        // 无存量（或关闭替换）：按上传流程处理，Record（或关闭时）的语义
        // 保留不变。
        let paths = if existing.is_empty() {
            let certificate_id = self
                .upload_certificate(&tencent_credentials, &input, materials)
                .await?;
            vec![certificate_id]
        } else {
            let mut failures = Vec::new();
            for certificate_id in &existing {
                match self
                    .update_certificate_instance(
                        &tencent_credentials,
                        &input,
                        materials,
                        certificate_id,
                    )
                    .await
                {
                    Ok(()) => {
                        tracing::info!(
                            "腾讯云 SSL：已更新同域名证书 {certificate_id}（{domain}）"
                        );
                    }
                    Err(error) => {
                        failures.push(format!("{certificate_id}: {error}"));
                        tracing::warn!(
                            "腾讯云 SSL：同域名证书 {certificate_id} 更新失败（{domain}）：{error}"
                        );
                    }
                }
            }
            if !failures.is_empty() {
                tracing::warn!(
                    "腾讯云 SSL：部分同域名证书更新失败，请人工回溯（{domain}）：{}",
                    failures.join("；")
                );
            }
            existing
        };

        // paths 记云端证书 ID：本目标的「部署产物」就是证书库里那张证书，
        // 部署历史凭它回溯（local 目标的 paths 是文件路径，语义随目标而异）。
        Ok(DeployOutcome::written(paths))
    }
}
