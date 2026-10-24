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
/// `dns.tencent-eo` 不含 SSL 部署能力（EdgeOne 仅服务 DNS），不接受。
const ACCEPTED_CREDENTIAL_TYPES: [&str; 1] = ["cloud.tencent"];

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
    /// 云端已存在的证书 ID（可选）。
    ///
    /// 非空时跳过「同域名存在性判断 / 自动匹配」，直接以该证书为
    /// `OldCertificateId` 发起替换（绑定的云资源切换到本次上传的新证书）；
    /// 空白值按未提供处理。
    #[serde(default)]
    pub certificate_id: Option<String>,
    /// 上传请求的 `Repeatable`：`false`（默认）时相同指纹的证书已存在返回
    /// 云端已有证书 ID、不产生重复；`true` 时允许重复上传。上传与替换
    /// （`UpdateCertificateInstance`）都按界面输入透传该参数。
    #[serde(default)]
    pub repeatable: bool,
}

/// 替换时可同步切换的云资源类型清单（`ResourceTypes`，多选）。
///
/// `input_schema()` 的 `x-options` 与 `deploy()` 的取值校验共用本清单：
/// 前端选项由后端 schema 注入（固定枚举、非云端拉取），清单外值在后端
/// 被拒绝且不发云端调用。清单即 `UpdateCertificateInstance` 接口文档
/// 支持的 16 种资源类型（不含它不支持的 lighthouse/server）。
pub const RESOURCE_TYPES: [(&str, &str); 16] = [
    ("cdn", "内容分发网络CDN"),
    ("clb", "负载均衡CLB"),
    ("live", "云直播LIVE"),
    ("waf", "Web应用防火墙WAF"),
    ("ddos", "DDoS防护"),
    ("apigateway", "API网关"),
    ("teo", "边缘安全加速平台EO"),
    ("tke", "容器服务TKE"),
    ("cos", "对象存储COS"),
    ("vod", "云点播VOD"),
    ("tcb", "云开发TCB"),
    ("tse", "云原生API网关TSE"),
    ("tdmq", "消息队列RabbitMQ版"),
    ("mqtt", "消息队列MQTT版"),
    ("scf", "云函数SCF"),
    ("gaap", "全球加速GA2.0"),
];

/// `DescribeDeployedResources` 支持按类型查询的清单：接口的 `ResourceType`
/// 必填且一次只能查一种（SDK 实证），遍历此清单逐个查询。`antiddos` 是
/// 该接口的类型名，对应 `UpdateCertificateInstance` 清单里的 `ddos`。
const DEPLOYED_RESOURCE_QUERY_TYPES: [&str; 7] =
    ["clb", "cdn", "live", "vod", "waf", "antiddos", "teo"];

impl TencentSslInput {
    /// 备注名的提交形态：空白的按未填写处理。
    fn alias_value(&self) -> Option<&str> {
        self.alias
            .as_deref()
            .map(str::trim)
            .filter(|alias| !alias.is_empty())
    }

    /// 选中证书 ID 的提交形态：空白的按未提供处理（清空下拉后残留的空串
    /// 不应把部署打进「更新不存在的证书」分支）。
    fn certificate_id_value(&self) -> Option<&str> {
        self.certificate_id
            .as_deref()
            .map(str::trim)
            .filter(|id| !id.is_empty())
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

    /// 拉取云端的服务器证书（SVR）列表，遍历 `DescribeCertificates` 的全部
    /// 分页，返回每张证书的原始 JSON 项（`CertificateId`/`Domain`/`Alias`）。
    ///
    /// `search_key` 仅在 `Some` 时传入：同域名匹配用它缩小候选集（服务端的
    /// 模糊匹配语义并不可控，正确性落在调用方的本地过滤）；同步证书列表
    /// 传 `None` 拿全量。
    pub async fn describe_certificates(
        &self,
        credentials: &TencentCredentials,
        search_key: Option<&str>,
    ) -> Result<Vec<Value>> {
        let mut items = Vec::new();
        let mut offset = 0;
        loop {
            let mut payload = json!({
                "Offset": offset,
                "Limit": PAGE_LIMIT,
                "CertificateType": "SVR",
            });
            if let Some(search_key) = search_key {
                payload["SearchKey"] = json!(search_key);
            }
            let body = self
                .call(credentials, "DescribeCertificates", payload)
                .await?;

            items.extend(body["Certificates"].as_array().cloned().unwrap_or_default());

            offset += PAGE_LIMIT;
            let total = body["TotalCount"].as_u64().unwrap_or(0);
            // 翻页由服务端声称的总数驱动；总数缺失（防御）时第一页已处理，就此打住。
            if offset >= total {
                break;
            }
        }
        Ok(items)
    }

    /// 查出云端与新证书主域名相同的全部服务器证书 ID。
    ///
    /// 全部分页拉完后按主域名**精确相等**过滤（含泛域名形式），绝不多删一张。
    async fn find_existing_certificates(
        &self,
        credentials: &TencentCredentials,
        domain: &str,
    ) -> Result<Vec<String>> {
        // 通配符会干扰服务端匹配：`*.example.com` 用剥掉 `*.` 的部分去搜。
        let search_key = domain.trim_start_matches("*.");
        let certificates = self
            .describe_certificates(credentials, Some(search_key))
            .await?;
        Ok(certificates
            .iter()
            .filter(|cert| cert["Domain"].as_str() == Some(domain))
            .filter_map(|cert| cert["CertificateId"].as_str().map(str::to_owned))
            .collect())
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

    /// 更新云端已有证书绑定的云资源：把 `OldCertificateId` 绑定的资源切换
    /// 到本次上传的新证书（走 `UpdateCertificateInstance`，
    /// <https://cloud.tencent.com/document/product/400/91649>）。
    ///
    /// 「选中证书 ID 直写」与「同域名自动匹配替换」两个分支调用本方法，
    /// 仅 `certificate_id` 的来源不同（选中值 / 匹配值）。
    ///
    /// 说明：早期实现走 `UploadUpdateCertificateInstance`（直接改写证书内
    /// 容、ID 不变），但该接口是白名单功能（`CertificateWhiteFuncError`），
    /// 且参数表没有 `Repeatable`、资源类型仅支持 `clb`。官方工单建议改用
    /// `UpdateCertificateInstance`：无需白名单、支持 16 种资源类型；代价是
    /// 证书 ID 变化——新证书先入库，旧证书绑定的云资源切换到新 ID，旧证书
    /// 随后可下线。
    ///
    /// 抓取一张云端证书绑定的云资源类型集合（`DescribeDeployedResources`）。
    ///
    /// 用于「表单未填 `resource_types`」时按旧证书的实际绑定填充：绑定了
    /// 什么类型的资源，更新调用就切换什么类型。接口的 `ResourceType`
    /// 必填且一次只能查一种，遍历 [`DEPLOYED_RESOURCE_QUERY_TYPES`] 逐个
    /// 查询；`Count>0` 视为有绑定。类型名映射：本接口用 `antiddos`，而
    /// `UpdateCertificateInstance` 的 `ResourceTypes` 用 `ddos`，其余
    /// 直通；不在 [`RESOURCE_TYPES`] 清单内的类型丢弃并记告警（避免把
    /// 清单外值送进更新调用被拒）。
    async fn describe_deployed_resource_types(
        &self,
        credentials: &TencentCredentials,
        certificate_id: &str,
    ) -> Result<Vec<String>> {
        let mut types: Vec<String> = Vec::new();
        for query_type in DEPLOYED_RESOURCE_QUERY_TYPES {
            let body = self
                .call(
                    credentials,
                    "DescribeDeployedResources",
                    json!({
                        "CertificateIds": [certificate_id],
                        "ResourceType": query_type,
                    }),
                )
                .await?;

            let bound = body["DeployedResources"]
                .as_array()
                .map(|items| {
                    items
                        .iter()
                        .any(|item| item["Count"].as_u64().unwrap_or(0) > 0)
                })
                .unwrap_or(false);
            if !bound {
                continue;
            }
            let normalized = if query_type == "antiddos" {
                "ddos"
            } else {
                query_type
            };
            if !RESOURCE_TYPES.iter().any(|(value, _)| *value == normalized) {
                tracing::warn!(
                    "腾讯云 SSL：证书 {certificate_id} 绑定了清单外资源类型 {query_type}，跳过"
                );
                continue;
            }
            if !types.iter().any(|known| known == normalized) {
                types.push(normalized.to_owned());
            }
        }
        Ok(types)
    }

    /// 解析替换调用的最终 `ResourceTypes`：固定取旧证书的云端绑定
    /// （`DescribeDeployedResources` 逐类型查询，见
    /// [`Self::describe_deployed_resource_types`]）。
    ///
    /// 抓取为空——旧证书未绑定任何云资源——时以可读错误失败：
    /// `UpdateCertificateInstance` 要求旧证书先有绑定资源（否则报
    /// `CertificateDeployInstanceEmpty`），需先在控制台把它手动部署到
    /// 目标资源再重试。
    async fn resolve_resource_types(
        &self,
        credentials: &TencentCredentials,
        old_certificate_id: &str,
    ) -> Result<Vec<String>> {
        let deployed = self
            .describe_deployed_resource_types(credentials, old_certificate_id)
            .await?;
        if deployed.is_empty() {
            return Err(Error::invalid_input(
                "certificate_id",
                format!(
                    "证书 `{old_certificate_id}` 未绑定任何云资源，无法替换；请先在腾讯云控制台把它手动部署到目标资源（https://console.cloud.tencent.com/ssl/dsc/deploy?id={old_certificate_id}）后再重试",
                ),
            ));
        }
        Ok(deployed)
    }

    /// 把 `OldCertificateId` 绑定的云资源切换到本次上传的新证书
    /// （`UpdateCertificateInstance`，异步接口，返回 `DeployRecordId`）。
    ///
    /// `resource_types` 来自旧证书的云端绑定（`DescribeDeployedResources`
    /// 抓取，见 [`Self::resolve_resource_types`]）；`repeatable` 按界面输入
    /// 透传（该接口支持此参数；存在相同重复证书时更新任务会失败）。接口
    /// 没有 `Alias` 参数——备注名是 `UploadCertificate` 的，无法随更新
    /// 传递。云端证书 ID 会变为新上传的证书 ID，旧证书内容保持不变、
    /// 随后可人工下线。
    async fn update_certificate_instance(
        &self,
        credentials: &TencentCredentials,
        materials: &CertMaterials,
        certificate_id: &str,
        resource_types: &[String],
        repeatable: bool,
    ) -> Result<()> {
        let payload = json!({
            "OldCertificateId": certificate_id,
            "CertificatePublicKey": materials.chain_pem,
            "CertificatePrivateKey": materials.key_pem,
            "ResourceTypes": resource_types,
            "Repeatable": repeatable,
        });

        self.call(credentials, "UpdateCertificateInstance", payload)
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

/// 证书下拉项的显示文本：无别名时 `证书ID (域名)`，有别名时
/// `证书ID (域名 - 别名)`（如 `a9ZwitUs (idev.top - idev)`）。
///
/// 部署 crate 与同步端点共用：label 是部署表单「证书 ID」下拉的显示口径，
/// 与 spec 示例保持同一分隔符（` - `，非全角破折号）。空白别名按无别名处理。
pub fn format_cert_label(id: &str, domain: &str, alias: Option<&str>) -> String {
    match alias.map(str::trim).filter(|alias| !alias.is_empty()) {
        Some(alias) => format!("{id} ({domain} - {alias})"),
        None => format!("{id} ({domain})"),
    }
}

/// 取出输入引用的腾讯云凭据，并校验类型白名单。
///
/// 同步证书列表端点与部署共用同一套校验（类型白名单 + deploy 能力），
/// 保证「非腾讯云凭据在任何云端调用之前以可读输入错误失败」的口径一致。
pub async fn resolve_tencent_credentials(
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
                "凭据 `{id}`（{}）不是可用于 SSL 部署的腾讯云凭据，需要 cloud.tencent 类型（dns.tencent-eo 仅服务 DNS，无 SSL 部署能力）",
                resolved.type_id
            ),
        ));
    }

    // 能力校验在解密字段之前、任何云端调用之前：凭据仅声明 dns 能力时
    // 部署直接以可读错误失败，避免把权限错误拖到 API 调用时才暴露。
    if !resolved
        .capabilities
        .iter()
        .any(|item| item == acmecast_store::capability::DEPLOY)
    {
        return Err(Error::invalid_input(
            "credential_id",
            format!(
                "凭据 `{id}`（{}）不支持部署用途：需要具备 deploy 能力（可在凭据编辑中勾选）",
                resolved.name
            ),
        ));
    }

    serde_json::from_value(resolved.fields).map_err(|e| {
        Error::invalid_input("credential_id", format!("凭据 `{id}` 的字段不合法: {e}"))
    })
}

/// 从证书链解析主域名（叶子证书的 SAN 首项）。
fn primary_domain_of(materials: &CertMaterials) -> Result<String> {
    let leaf = parse_pem_leaf(&materials.chain_pem)
        .map_err(|e| Error::invalid_input("materials", format!("证书链解析失败: {e}")))?;
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
        // 与文件类目标（local/ssh/s3/webdav 的 fullchain.cer/ca.cer/key 3 文件）区分：
        // 腾讯云是云 API 上传，仅需 fullchain+key 两个材料，不产生/不要求 ca.cer。
        "腾讯云 SSL 证书(仅需 fullchain+key)"
    }

    fn input_schema(&self) -> RootSchema {
        use schemars::schema::Schema;

        let mut schema = schema_for!(TencentSslInput);
        // serde_json 的 Map 会把 schemars 产物的 properties 按字母序重排——
        // 用 x-field-order 钉住渲染顺序（凭据引用、备注、选中证书 ID、重复
        // 开关）。certificate_id 紧跟 alias，便于前端把「同步」按钮放在
        // 就近位置。
        schema.schema.extensions.insert(
            "x-field-order".to_owned(),
            json!(["credential_id", "alias", "certificate_id", "repeatable"]),
        );
        // credential_id 仅接受 cloud.tencent 凭据：前端据此过滤下拉，避免选到
        // dns.tencent-eo（EdgeOne 无 SSL 部署能力）或 deploy.ssh 等无关凭据。
        if let Some(object) = schema.schema.object.as_mut() {
            if let Some(Schema::Object(credential_id)) = object.properties.get_mut("credential_id")
            {
                credential_id
                    .extensions
                    .insert("x-credential-types".to_owned(), json!(["cloud.tencent"]));
            }
        }
        schema
    }

    fn example_input(&self) -> Value {
        json!({
            "credential_id": 1,
            "alias": "acmecast example.com",
            "repeatable": false,
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

        let tencent_credentials =
            resolve_tencent_credentials(input.credential_id, credentials).await?;
        let domain = primary_domain_of(materials)?;

        // 选中证书 ID：替换模式——ResourceTypes 从该证书的云端绑定抓取
        // （DescribeDeployedResources）；未绑定任何资源时以可读错误失败
        // （提示先手动部署）。Repeatable 按界面输入透传。
        if let Some(certificate_id) = input.certificate_id_value() {
            let resource_types = self
                .resolve_resource_types(&tencent_credentials, certificate_id)
                .await?;
            self.update_certificate_instance(
                &tencent_credentials,
                materials,
                certificate_id,
                &resource_types,
                input.repeatable,
            )
            .await?;
            tracing::info!("腾讯云 SSL：已替换选中证书 {certificate_id}（{domain}）");
            return Ok(DeployOutcome::written(vec![certificate_id.to_owned()]));
        }

        // 未选证书 ID：查同域名证书——命中即替换（不再有「仅上传」开关），
        // 未命中走上传。
        let existing = self
            .find_existing_certificates(&tencent_credentials, &domain)
            .await?;

        // 有存量：逐张替换——resource_types 每张分别解析（用户填了统一用；
        // 未填按各自绑定抓取），任一张失败记告警、不阻塞其他张与整体部署。
        // 无存量：按上传流程处理。
        let paths = if existing.is_empty() {
            let certificate_id = self
                .upload_certificate(&tencent_credentials, &input, materials)
                .await?;
            vec![certificate_id]
        } else {
            let mut failures = Vec::new();
            for certificate_id in &existing {
                // 抓取失败/为空属确定性输入问题（旧证书未绑定资源，需先在
                // 控制台手动部署），整体失败并提示——静默跳过会让「部署
                // 成功」变成什么都没发生的空转；仅更新调用失败按「告警不
                // 阻塞」容错。
                let resource_types = self
                    .resolve_resource_types(&tencent_credentials, certificate_id)
                    .await?;
                match self
                    .update_certificate_instance(
                        &tencent_credentials,
                        materials,
                        certificate_id,
                        &resource_types,
                        input.repeatable,
                    )
                    .await
                {
                    Ok(()) => {
                        tracing::info!("腾讯云 SSL：已替换同域名证书 {certificate_id}（{domain}）");
                    }
                    Err(error) => {
                        failures.push(format!("{certificate_id}: {error}"));
                        tracing::warn!(
                            "腾讯云 SSL：同域名证书 {certificate_id} 替换失败（{domain}）：{error}"
                        );
                    }
                }
            }
            if !failures.is_empty() {
                tracing::warn!(
                    "腾讯云 SSL：部分同域名证书替换失败，请人工回溯（{domain}）：{}",
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
