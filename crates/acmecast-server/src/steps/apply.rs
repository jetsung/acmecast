//! `cert.apply`：向 ACME CA 申请证书。
//!
//! 一次执行完成「建账号（或复用）→ 下单 → 完成挑战 → 提交 CSR → 拿到证书链」，
//! 产物 `cert_pem`／`key_pem`／`domains`／`fingerprint` 交给后续的入库与部署步骤。
//!
//! 账号凭据按标识从凭据系统取：**首次使用时注册**，签发的账号凭据写回凭据
//! 记录，之后的运行都复用同一账号——每次运行注册新账号既浪费又可能撞上
//! CA 的账号配额。

use std::path::PathBuf;
use std::sync::Arc;

use acmecast_access::AcmeAccountFields;
use acmecast_acme::{AccountMode, AcmeService, EstablishAccountInput};
use acmecast_dns::{
    AuthoritativeResolver, ChallengeKind as DnsChallengeKind, DnsProviderRegistry,
    PropagationPolicy, ResolverEntry, TxtRecord, challenge_record_name, challenge_write_name,
    ensure_kind_covers, load_extended_resolvers, wait_until_visible_phased,
};
use acmecast_pipeline::{PipelineStep, Result, StepContext, StepOutput};
use acmecast_store::FileStore;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::domain_error;

/// 申请密钥与证书的算法类型。
///
/// 显式 `rename` 而不靠 `rename_all`：`EcdsaP256` 这类带数字的变体在
/// snake_case 下的分词结果不可靠（见 `DnsChallengeKind` 的先例）。
/// CA 是否接受所选算法的 CSR 由 CA 决定，拒绝时按其错误如实上报。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum CertKeyAlgorithm {
    /// ECDSA on NIST P-256（缺省，与旧版行为一致）。
    #[default]
    #[serde(rename = "ecdsa_p256")]
    EcdsaP256,
    /// ECDSA on NIST P-384。
    #[serde(rename = "ecdsa_p384")]
    EcdsaP384,
    /// Ed25519。
    #[serde(rename = "ed25519")]
    Ed25519,
    /// RSA 2048。
    #[serde(rename = "rsa2048")]
    Rsa2048,
}

impl CertKeyAlgorithm {
    /// 对应的 rcgen 签名算法——决定密钥类型与 CSR 的签名算法，
    /// CA 据此签发同类型的证书。
    #[must_use]
    pub fn signature_algorithm(self) -> &'static rcgen::SignatureAlgorithm {
        match self {
            Self::EcdsaP256 => &rcgen::PKCS_ECDSA_P256_SHA256,
            Self::EcdsaP384 => &rcgen::PKCS_ECDSA_P384_SHA384,
            Self::Ed25519 => &rcgen::PKCS_ED25519,
            Self::Rsa2048 => &rcgen::PKCS_RSA_SHA256,
        }
    }

    /// 日志里的展示名（与证书详情的算法文案一致）。
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::EcdsaP256 => "ECDSA P-256",
            Self::EcdsaP384 => "ECDSA P-384",
            Self::Ed25519 => "Ed25519",
            Self::Rsa2048 => "RSA 2048",
        }
    }
}

/// `cert.apply` 的输入。
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct AcmeApplyInput {
    /// 要申请的域名集合；通配符（`*.example.com`）会被强制走 DNS-01。
    pub domains: Vec<String>,
    /// 申请密钥与证书的算法；缺省 ECDSA P-256。密钥类型在签发时即固定，
    /// 无法事后更改。
    #[serde(default)]
    #[schemars(default = "default_key_algorithm")]
    pub key_algorithm: CertKeyAlgorithm,
    /// 挑战类型。当前只支持 DNS-01——HTTP-01 的投放通道尚未提供。
    pub challenge: DnsChallengeKind,
    /// ACME 账号凭据标识（`acme.account` 类型）。
    pub account_credential_id: i64,
    /// DNS 提供商标识（如 `cloudflare`）；与 `dns_credential_id` 同时省略
    /// （或留空）时自动选择，只给其一时报错。DNS-01 时需要。
    #[serde(default)]
    pub dns_provider: Option<String>,
    /// DNS 提供商凭据标识；与 `dns_provider` 同时省略时自动选择，
    /// 只给其一时报错。DNS-01 时需要。
    #[serde(default)]
    pub dns_credential_id: Option<i64>,
    /// DNS zone；显式配置优先。`domains` 配置多个域名时必须显式配置
    /// （各域名需同属该 zone）。留空时仅当「去掉第一段后仍是合法 zone」
    /// 才自动推导（`a.example.com` → `example.com`）；注册域本身
    /// （`skiy.net`）与通配符注册域（`*.skiy.net`）无法可靠推导，
    /// 会在执行时报错要求显式配置。
    #[serde(default)]
    pub dns_zone: Option<String>,
    /// 是否等待 TXT 记录在公共解析器可见后再让 CA 验证。
    ///
    /// 使用内网 DNS（解析器查不到）时应关闭——否则会白等到超时。
    #[serde(default = "default_true")]
    pub wait_propagation: bool,
    /// 账号联系人；裸邮箱或 `mailto:` URI 均可（前缀由服务端注册账号时自动补全）。
    /// 仅首次注册账号时使用。
    #[serde(default)]
    pub contacts: Vec<String>,
    /// 跳过 CA 的 TLS 证书校验。仅用于自签测试 CA（如 pebble）；
    /// 生产 CA 保持默认 `false`，关闭校验等于放弃对 CA 身份的确认。
    #[serde(default)]
    pub insecure_skip_verify: bool,
    /// 到期前 N 天自动续期；缺省/空值时按 30 补齐，0 表示跳过过期校验，其余仅接受大于 0 的整数。
    #[serde(default = "default_renew_before_days")]
    #[schemars(default = "default_renew_before_days")]
    pub renew_before_days: Option<i32>,
}

fn default_renew_before_days() -> Option<i32> {
    Some(30)
}

fn default_true() -> bool {
    true
}

/// `key_algorithm` 的 schema 缺省值：表单无值时预填，保存的配置显式携带
/// `ecdsa_p256` 而不是靠运行时回退（schemars 不会为 enum 的 `#[default]`
/// 变体生成 schema default）。
fn default_key_algorithm() -> CertKeyAlgorithm {
    CertKeyAlgorithm::EcdsaP256
}

/// ACME 申请步骤。
#[derive(Debug)]
pub struct CertApplyStep {
    dns: Arc<DnsProviderRegistry>,
    /// DNS-01 传播等待策略；来自服务配置的 `[propagation]` 段，
    /// 装配期注入——它是系统级调优，不属于步骤输入。
    propagation: PropagationPolicy,
    /// `config.toml` 的 `[[resolvers]]` 条目；传播等待的阶段 2/3
    /// 用它扩展内置解析器集合。
    resolver_entries: Vec<ResolverEntry>,
    /// 证书库读取句柄：到期检测在任意触发来源的 `cert.apply` 步骤中
    /// 按库中已存证书的 `not_after` 与 `renew_before_days` 决定是否跳过。
    /// `renew_before_days=0` 视为强制更新，跳过到期检测。
    db: Option<sea_orm::DatabaseConnection>,
    /// 数据目录根路径：跳过分支需从库中证书记录的路径读取 PEM 以注入下游产物。
    data_dir: Option<PathBuf>,
}

impl CertApplyStep {
    /// 用 DNS 提供商注册表装配，传播等待取默认策略。
    #[must_use]
    pub fn new(dns: Arc<DnsProviderRegistry>) -> Self {
        Self::with_policy(dns, PropagationPolicy::default())
    }

    /// 用 DNS 提供商注册表与显式传播等待策略装配。
    #[must_use]
    pub fn with_policy(dns: Arc<DnsProviderRegistry>, propagation: PropagationPolicy) -> Self {
        Self {
            dns,
            propagation,
            resolver_entries: Vec::new(),
            db: None,
            data_dir: None,
        }
    }

    /// 以证书库句柄装配（到期检测在步骤内可查库）。
    #[must_use]
    pub fn with_db(dns: Arc<DnsProviderRegistry>, db: sea_orm::DatabaseConnection) -> Self {
        Self {
            dns,
            propagation: PropagationPolicy::default(),
            resolver_entries: Vec::new(),
            db: Some(db),
            data_dir: None,
        }
    }

    /// 以传播策略与证书库句柄装配。
    #[must_use]
    pub fn with_policy_and_db(
        dns: Arc<DnsProviderRegistry>,
        propagation: PropagationPolicy,
        db: sea_orm::DatabaseConnection,
    ) -> Self {
        Self {
            dns,
            propagation,
            resolver_entries: Vec::new(),
            db: Some(db),
            data_dir: None,
        }
    }

    /// 以传播策略、解析器扩展条目与证书库句柄装配。
    #[must_use]
    pub fn with_policy_resolvers_and_db(
        dns: Arc<DnsProviderRegistry>,
        propagation: PropagationPolicy,
        resolver_entries: Vec<ResolverEntry>,
        db: sea_orm::DatabaseConnection,
    ) -> Self {
        Self {
            dns,
            propagation,
            resolver_entries,
            db: Some(db),
            data_dir: None,
        }
    }

    /// 以传播策略、解析器扩展条目、证书库句柄与数据目录装配（跳过时可注入产物）。
    #[must_use]
    pub fn with_policy_resolvers_db_and_data_dir(
        dns: Arc<DnsProviderRegistry>,
        propagation: PropagationPolicy,
        resolver_entries: Vec<ResolverEntry>,
        db: sea_orm::DatabaseConnection,
        data_dir: PathBuf,
    ) -> Self {
        Self {
            dns,
            propagation,
            resolver_entries,
            db: Some(db),
            data_dir: Some(data_dir),
        }
    }
}

#[allow(dead_code)]
async fn resolve_cert_not_after_db(
    db: &sea_orm::DatabaseConnection,
    domains: &[String],
) -> Result<Option<chrono::DateTime<chrono::Utc>>> {
    let existing = acmecast_store::repository::CertRepository::new(db)
        .find_by_domains(domains)
        .await
        .map_err(|e| {
            acmecast_pipeline::Error::Core(acmecast_core::Error::Internal(format!(
                "查询证书库失败: {e}"
            )))
        })?;
    let Some(cert) = existing else {
        return Ok(None);
    };
    if cert.revoked_at.is_some() {
        return Ok(None);
    }
    Ok(Some(cert.not_after))
}

async fn resolve_cert_not_after(
    _ctx: &StepContext<'_>,
    _domains: &[String],
) -> Result<Option<chrono::DateTime<chrono::Utc>>> {
    Ok(None)
}

#[async_trait::async_trait]
impl PipelineStep for CertApplyStep {
    fn type_id(&self) -> &'static str {
        "cert.apply"
    }

    fn input_schema(&self) -> Option<schemars::schema::RootSchema> {
        let mut schema = schemars::schema_for!(AcmeApplyInput);
        // `domains` 配置多个域名时 `dns_zone` 必填：条件由前端 SchemaForm
        // 求值（与 ACME 账号凭据 `directory_url` 的 Equals 同通道）。`dns_zone`
        // 是 Option，schemars 出来是 anyOf 包裹的顶层节点——扩展挂在它上面，
        // 前端解包 anyOf 时会原样保留。结构变化时宁可少注入也不丢 schema。
        if let Some(schemars::schema::Schema::Object(zone)) = schema
            .schema
            .object
            .as_mut()
            .and_then(|object| object.properties.get_mut("dns_zone"))
        {
            zone.extensions.insert(
                "x-required-when".to_owned(),
                serde_json::json!({ "MinItems": { "field": "domains", "count": 2 } }),
            );
        }
        // `key_algorithm` 的枚举值是 snake_case 配置串，下拉里直接显示可读性
        // 差——展示名挂在字段节点上（schemars 会把带说明的字段包成
        // `allOf: [$ref]`，前端解包时外层属性原样保留，与 dns_zone 同通道）。
        if let Some(schemars::schema::Schema::Object(alg)) = schema
            .schema
            .object
            .as_mut()
            .and_then(|object| object.properties.get_mut("key_algorithm"))
        {
            alg.extensions.insert(
                "x-enum-labels".to_owned(),
                serde_json::json!({
                    "ecdsa_p256": "ECDSA P-256",
                    "ecdsa_p384": "ECDSA P-384",
                    "ed25519": "Ed25519",
                    "rsa2048": "RSA 2048",
                }),
            );
        }
        Some(schema)
    }

    fn validate_input(&self, input: &serde_json::Value) -> Result<()> {
        if let Some(schema) = self.input_schema() {
            let schema = serde_json::to_value(schema)
                .map_err(|e| acmecast_pipeline::Error::Core(acmecast_core::Error::Serialization(e.to_string())))?;
            acmecast_pipeline::input::validate(&schema, input)?;
        }
        if let Some(obj) = input.as_object() {
            if let Some(domains) = obj.get("domains").and_then(|v| v.as_array()) {
                for (idx, raw) in domains.iter().enumerate() {
                    if let Some(s) = raw.as_str() {
                        let s = s.trim();
                        if let Some(base) = s.strip_prefix("*.") {
                            // 一级域名的通配 *.xx 是非法的（xx 无点，顶级域无泛解析）
                            let base = base.trim();
                            if base.is_empty() || !base.contains('.') || base.starts_with('.') || base.ends_with('.') {
                                return Err(acmecast_pipeline::Error::InvalidInput {
                                    field: format!("domains[{idx}]"),
                                    reason: "一级域名的通配 *.xx 是非法的，需至少二级域如 *.example.com".to_owned(),
                                });
                            }
                        }
                    }
                }
            }
            if let Some(raw) = obj.get("renew_before_days") {
                if raw.is_null() {
                    return Err(acmecast_pipeline::Error::InvalidInput {
                        field: "renew_before_days".to_owned(),
                        reason: "必须为大于等于 0 的整数".to_owned(),
                    });
                }
                let days = raw.as_i64().ok_or_else(|| acmecast_pipeline::Error::InvalidInput {
                    field: "renew_before_days".to_owned(),
                    reason: "必须为大于等于 0 的整数".to_owned(),
                })?;
                if days < 0 {
                    return Err(acmecast_pipeline::Error::InvalidInput {
                        field: "renew_before_days".to_owned(),
                        reason: "必须为大于等于 0 的整数".to_owned(),
                    });
                }
                if days > 0 {
                    let domains_empty = obj
                        .get("domains")
                        .and_then(|value| value.as_array())
                        .is_none_or(|array| array.is_empty());
                    if domains_empty {
                        return Err(acmecast_pipeline::Error::InvalidInput {
                            field: "renew_before_days".to_owned(),
                            reason: "已设置到期前检测，需先配置 domains".to_owned(),
                        });
                    }
                }
            }
        }
        Ok(())
    }

    async fn execute(&self, ctx: &mut StepContext<'_>) -> Result<StepOutput> {
        let input: AcmeApplyInput = ctx
            .input_as()
            .map_err(|error| domain_error("cert.apply 输入不合法", error))?;
        if let Some(days) = input.renew_before_days {
            if days == 0 {
                ctx.log_info("到期检测已跳过（renew_before_days=0），直接申请");
            } else if let Some(db) = &self.db {
                let threshold_days = days;
                // 查询匹配记录以在跳过时注入产物供后续 cert.store/cert.deploy 使用
                let existing = acmecast_store::repository::CertRepository::new(db)
                    .find_by_domains(&input.domains)
                    .await
                    .map_err(|e| {
                        acmecast_pipeline::Error::Core(acmecast_core::Error::Internal(format!(
                            "查询证书库失败: {e}"
                        )))
                    })?;
                let candidate = existing.filter(|c| c.revoked_at.is_none());
                if let Some(cert) = candidate {
                    if cert.not_after > chrono::Utc::now() + chrono::Duration::days(i64::from(threshold_days)) {
                        let remaining = (cert.not_after - chrono::Utc::now()).num_days();
                        ctx.log_info(format!(
                            "跳过 cert.apply：证书距到期 {remaining} 天，阈值 {threshold_days} 天，不在续期窗口"
                        ));
                        let data_dir = self.data_dir.as_ref().ok_or_else(|| {
                            domain_error(
                                "跳过 cert.apply 后注入产物失败：数据目录未配置，无法读取库中证书",
                                "data_dir 未配置",
                            )
                        })?;
                        let store = FileStore::open(data_dir)
                            .await
                            .map_err(|e| domain_error("打开数据目录", e))?;
                        let cert_pem = store
                            .read(&cert.cert_pem_path)
                            .await
                            .map_err(|e| domain_error("读取库中证书文件", e))?;
                        let key_pem = store
                            .read(&cert.key_pem_path)
                            .await
                            .map_err(|e| domain_error("读取库中私钥文件", e))?;
                        let domains_value = {
                            let decoded = acmecast_store::entity::cert::decode_domains(&cert.domains);
                            if decoded.is_empty() {
                                input.domains.clone()
                            } else {
                                decoded
                            }
                        };
                        ctx.log_info(format!(
                            "已注入库中证书以继续部署（指纹 {}，证书 #{}）",
                            cert.fingerprint, cert.id
                        ));
                        return Ok(StepOutput::empty()
                            .with_log(acmecast_pipeline::StepLog::info(format!(
                                "跳过 cert.apply：到期检测判定不在续期窗口（剩余 {remaining} 天，阈值 {threshold_days} 天）"
                            )))
                            .with_artifact("cert_pem", serde_json::json!(cert_pem))
                            .with_artifact("key_pem", serde_json::json!(key_pem))
                            .with_artifact("domains", serde_json::json!(domains_value))
                            .with_artifact("fingerprint", serde_json::json!(cert.fingerprint.clone()))
                            .with_artifact("cert_id", serde_json::json!(cert.id)));
                    }
                }
            } else if let Ok(Some(not_after)) = resolve_cert_not_after(ctx, &input.domains).await {
                let threshold_days = days;
                let now = chrono::Utc::now();
                let threshold = chrono::Duration::days(i64::from(threshold_days));
                if not_after > now + threshold {
                    let remaining = (not_after - now).num_days();
                    ctx.log_info(format!(
                        "跳过 cert.apply：证书距到期 {remaining} 天，阈值 {threshold_days} 天，不在续期窗口"
                    ));
                    return Ok(StepOutput::empty().with_log(
                        acmecast_pipeline::StepLog::info(format!(
                            "跳过 cert.apply：到期检测判定不在续期窗口（剩余 {remaining} 天，阈值 {threshold_days} 天）"
                        )),
                    ));
                }
            }
        }
        ensure_kind_covers(&input.domains, input.challenge)
            .map_err(|e| domain_error("挑战类型", e))?;

        let service = self.account_service(ctx, &input).await?;

        ctx.log_info(format!("向 CA 下单：{}", input.domains.join(", ")));
        let mut order = service
            .new_order(&input.domains)
            .await
            .map_err(|e| domain_error("创建订单", e))?;

        for authorization in order
            .authorizations()
            .await
            .map_err(|e| domain_error("读取授权", e))?
        {
            if authorization.is_valid() {
                continue;
            }
            self.complete_challenge(ctx, &input, &mut order, &authorization)
                .await?;
        }

        // 密钥与 CSR 在本次运行中生成；私钥不落盘、不进日志，只随产物交给入库步骤。
        ctx.log_info(format!("生成本次申请的密钥：{}", input.key_algorithm.as_str()));
        let (key_pem, csr_der) = generate_key_and_csr(&input.domains, input.key_algorithm)?;
        wait_for_ready(&mut order).await?;
        ctx.log_info("提交 CSR 并等待签发");
        let cert_pem = order
            .finalize_and_download(&csr_der)
            .await
            .map_err(|e| domain_error("下载证书", e))?;

        let leaf =
            acmecast_cert::parse_pem_leaf(&cert_pem).map_err(|e| domain_error("解析证书", e))?;
        let fingerprint = leaf.fingerprint_sha256.clone();
        ctx.log_info(format!(
            "证书已签发：{}（有效期至 {}，SHA-256 指纹 {}）",
            leaf.domains.join(", "),
            leaf.not_after.format("%Y-%m-%d"),
            fingerprint
        ));

        Ok(StepOutput::empty()
            .with_artifact("cert_pem", serde_json::json!(cert_pem))
            .with_artifact("key_pem", serde_json::json!(key_pem))
            .with_artifact("domains", serde_json::json!(leaf.domains))
            .with_artifact("fingerprint", serde_json::json!(fingerprint)))
    }
}

impl CertApplyStep {
    /// 未指定 `dns_provider`/`dns_credential_id` 时自动选择对接提供商。
    ///
    /// 候选序：`tencent` → `tencent-eo` → `aliyun` → `cloudflare` → 其余已注册
    /// 且库中有凭据的提供商。逐个探测 zone 托管情况，命中即用——域名托管在
    /// 某家时对齐的那家自然排最前且探测通过，不在四家时按序回退。
    ///
    /// 通配授权的真实 zone 在选定提供商后由 `resolve_wildcard_zone` 逐级推导；
    /// 此处先用授权 identifier 的 base domain 做托管探测。
    /// 探测与日志见 [`provider_select`]。
    async fn auto_select_provider(
        &self,
        ctx: &mut StepContext<'_>,
        zone: &str,
    ) -> Result<super::provider_select::Selected> {
        let mut log = |message: String| ctx.log_info(message);
        let candidates =
            super::provider_select::build_candidates(&self.dns, ctx.credentials(), log).await?;
        super::provider_select::pick(candidates, zone, log).await
    }

    /// 建立或恢复 ACME 账号会话；首次注册时把签发的凭据写回凭据记录。
    async fn account_service(
        &self,
        ctx: &mut StepContext<'_>,
        input: &AcmeApplyInput,
    ) -> Result<acmecast_acme::AcmeService> {
        let resolved = ctx
            .credentials()
            .resolve(input.account_credential_id)
            .await
            .map_err(|e| domain_error("读取 ACME 账号凭据", e))?;
        let fields: AcmeAccountFields = resolved
            .as_fields()
            .map_err(|e| domain_error("ACME 账号凭据字段", e))?;
        let directory_url = fields
            .resolve_directory()
            .map_err(|e| domain_error("解析 CA 端点", e))?;

        let transport = transport_options(input);
        if let Some(stored) = fields
            .credentials()
            .map_err(|e| domain_error("账号凭据字段", e))?
        {
            ctx.log_info(format!("复用已有 ACME 账号（KID {:?}）", stored.kid()));
            return AcmeService::from_credentials(&stored, Some(&transport))
                .await
                .map_err(|e| domain_error("恢复 ACME 账号会话", e));
        }

        ctx.log_info(format!("首次使用，注册新 ACME 账号：{directory_url}"));
        let external_account = match (fields.eab_kid.as_deref().map(str::trim).filter(|s| !s.is_empty()), fields.eab_hmac_key.as_deref().map(str::trim).filter(|s| !s.is_empty())) {
            (Some(kid), Some(hmac)) => Some(acmecast_acme::ExternalAccountBinding { kid: kid.to_owned(), hmac_key: hmac.to_owned() }),
            (Some(_), None) | (None, Some(_)) => {
                return Err(domain_error("ACME EAB", "eab_kid 与 eab_hmac_key 需成对填写"));
            }
            _ => None,
        };
        if external_account.is_some() {
            ctx.log_info("携带 EAB 凭据注册 ACME 账号");
        }
        let established = EstablishAccountInput {
            directory_url: &directory_url,
            mode: AccountMode::Create {
                contacts: normalize_contacts(&input.contacts),
            },
            external_account: external_account.as_ref(),
        };
        let (service, credentials) = AcmeService::establish(&established, Some(&transport))
            .await
            .map_err(|e| domain_error("注册 ACME 账号", e))?;

        // 凭据写回是账号复用的前提：写回失败只警告不断流程——本次运行
        // 已拿到可用会话，但下次会再注册一个账号，日志里能看到。
        let mut persisted = fields.clone();
        persisted.credentials = Some(credentials.as_json().to_owned());
        if let Err(error) = ctx
            .credentials()
            .update_fields(
                input.account_credential_id,
                &serde_json::to_value(&persisted).expect("凭据字段应能序列化"),
            )
            .await
        {
            ctx.log_warn(format!(
                "ACME 账号凭据写回失败，下次运行将重新注册：{error}"
            ));
        }

        Ok(service)
    }

    /// 完成一个授权的挑战：写 TXT → 等传播 → 让 CA 验证 → 随记录清理。
    async fn complete_challenge(
        &self,
        ctx: &mut StepContext<'_>,
        input: &AcmeApplyInput,
        order: &mut acmecast_acme::PendingOrder,
        authorization: &acmecast_acme::AuthorizationInfo,
    ) -> Result<()> {
        if input.challenge != DnsChallengeKind::Dns01 {
            return Err(acmecast_pipeline::Error::Core(
                acmecast_core::Error::Internal(format!(
                    "挑战类型 {:?} 的交付通道尚未提供，请改用 dns-01",
                    input.challenge
                )),
            ));
        }
        let acme_kind = acmecast_acme::ChallengeKind::Dns01;

        // 提供商与凭据的三种形态：两者齐备走显式配置（行为不变）；
        // 两者均缺走自动选择；只给其一直接报错并指出缺哪一项——
        // 自动补另一项容易造成「提供商 A + 凭据 B」的错配。
        // 空串按「未填写」处理（与 dns_zone 的空白语义一致）：
        // 前端下拉的「自动选择」选项提交的就是空值。
        let dns_provider = input
            .dns_provider
            .as_deref()
            .map(str::trim)
            .filter(|provider| !provider.is_empty());
        let (provider, credentials) = match (dns_provider, input.dns_credential_id)
        {
            (Some(provider_id), Some(dns_credential_id)) => {
                let provider = self
                    .dns
                    .require(provider_id)
                    .map_err(|e| domain_error("DNS 提供商", e))?;
                let credentials = ctx
                    .credentials()
                    .resolve(dns_credential_id)
                    .await
                    .map_err(|e| domain_error("读取 DNS 凭据", e))?
                    .fields;
                (provider, credentials)
            }
            (Some(provider_id), None) => {
                return Err(acmecast_pipeline::Error::Core(
                    acmecast_core::Error::Internal(format!(
                        "已指定 dns_provider（{provider_id}）但缺少 dns_credential_id；请补齐凭据标识，或两者都留空以启用自动选择"
                    )),
                ));
            }
            (None, Some(dns_credential_id)) => {
                return Err(acmecast_pipeline::Error::Core(
                    acmecast_core::Error::Internal(format!(
                        "已指定 dns_credential_id（{dns_credential_id}）但缺少 dns_provider；请补齐提供商标识，或两者都留空以启用自动选择"
                    )),
                ));
            }
            (None, None) => {
                let selected = self
                    .auto_select_provider(ctx, &authorization.identifier.to_string())
                    .await?;
                let provider = self
                    .dns
                    .require(&selected.provider_id)
                    .map_err(|e| domain_error("DNS 提供商", e))?;
                (provider, selected.fields)
            }
        };

        let materials = order
            .challenge_materials(&authorization.identifier, acme_kind)
            .await
            .map_err(|e| domain_error("读取挑战材料", e))?;
        // 显式给了 zone 就用它；留空（未填或空串）则自动推导——
        // 通配授权（`*.gcli.cn`）的 identifier 为 base domain（`gcli.cn`）+ wildcard 标记，
        // 其 zone 即 base domain 本身；`*.x.idev.top` 则需在提供商中逐级探测父 zone。
        let zone = match input.dns_zone.as_deref().map(str::trim) {
            Some(zone) if !zone.is_empty() => zone.to_owned(),
            _ if authorization.wildcard => {
                resolve_wildcard_zone(provider, &credentials, &authorization.identifier).await?
            }
            _ => zone_of(&authorization.identifier).ok_or_else(|| {
                acmecast_pipeline::Error::Core(acmecast_core::Error::Internal(format!(
                    "无法从 {} 推导 DNS zone，请显式配置 dns_zone",
                    authorization.identifier
                )))
            })?,
        };
        let record = challenge_record(&zone, &authorization.identifier, &materials.dns_txt_value);
        if record.is_delegated() {
            // 跨 zone CNAME 委派：CA 查授权域名一侧，DNS 提供商写在 dns_zone 下。
            // 两名字不同正是该场景的预期，日志两个都打出来，便于对照 CA 侧现象。
            ctx.log_info(format!(
                "写入挑战记录 {}（CA 查询名 {}，经 CNAME 委派）",
                record.name, record.ca_name
            ));
        } else {
            ctx.log_info(format!("写入挑战记录 {}", record.name));
        }

        let ready = order.set_challenge_ready(&authorization.identifier, acme_kind);
        // 记录存在期间做「等传播 + 通知 CA」；无论成败，退出时记录都会被清理。
        // action 的错误类型随 with_txt_record 的约定是 DNS 错误，转成流水线错误放在外层。
        let action = async {
            if input.wait_propagation {
                let http = Arc::new(acmecast_dns::ReqwestTransport::new().map_err(|e| {
                    acmecast_dns::Error::provider(format!("构造解析器客户端: {e}"))
                })?);

                // 阶段 1：权威 NS 直查——最贴近 CA 视角，不受公共解析器
                // 否定缓存影响。首跳 zone 取 CA 查询名所属的委派源 zone：
                // CNAME 委派场景下 CNAME 记录本身在授权域名一侧，问 dns_zone
                // 的 NS 拿不到它，也就无从跟随。
                let auth = AuthoritativeResolver::new(record.ca_zone());
                let auth_refs: Vec<&dyn acmecast_dns::DnsResolver> = vec![&auth];

                // 阶段 2/3：全量 DNS + DoH，均由 load_extended_resolvers 构建
                // （内置 DoH 四家 + config.toml 扩展条目，按 endpoint 去重），
                // 再按 dns_provider 做优先级排序：
                //   tencent/tencen-eo → 腾讯优先
                //   aliyun → 阿里优先
                //   cloudflare → Cloudflare 优先
                //   其他 → Cloudflare → 阿里 → 其余
                let extended_raw = load_extended_resolvers(http, &self.resolver_entries);
                let extended =
                    acmecast_dns::prioritize_for_provider(extended_raw, Some(provider.type_id()));
                let ext_refs: Vec<&dyn acmecast_dns::DnsResolver> =
                    extended.iter().map(|r| r.as_ref()).collect();

                tracing::debug!(
                    provider = provider.type_id(),
                    authoritative = auth_refs.len(),
                    extended = ext_refs.len(),
                    "分阶段传播等待：权威 NS → 全量 DNS → DoH",
                );

                wait_until_visible_phased(
                    provider,
                    &credentials,
                    &record,
                    &auth_refs,
                    &ext_refs,
                    &ext_refs,
                    &self.propagation,
                )
                .await?;
                ctx.log_info(format!(
                    "TXT 记录已在权威/公共解析器可见：{}",
                    record.ca_name
                ));
            }
            ready
                .await
                .map_err(|e| acmecast_dns::Error::provider(e.to_string()))
        };
        acmecast_dns::with_txt_record(provider, &credentials, &record, action)
            .await
            .map_err(|e| domain_error("完成 DNS-01 挑战", e))?;

        Ok(())
    }
}

/// 轮询订单直到 Ready（可提交 CSR）。
///
/// CA 的挑战验证是**异步**的：`set_challenge_ready` 只是通知，状态从
/// pending 推进到 ready 有延迟（pebble 在 ALWAYS_VALID 下也有几十毫秒），
/// 立即 finalize 会撞上 `orderNotReady`。`Invalid` 直接失败——
/// 那意味着 CA 拒绝了这次验证，等下去没有意义。
async fn wait_for_ready(order: &mut acmecast_acme::PendingOrder) -> Result<()> {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(120);
    loop {
        match order
            .refresh()
            .await
            .map_err(|e| domain_error("刷新订单状态", e))?
        {
            acmecast_acme::OrderStatus::Ready => return Ok(()),
            acmecast_acme::OrderStatus::Invalid => {
                let reason = order.rejection_reason().await;
                return Err(acmecast_pipeline::Error::Core(
                    acmecast_core::Error::Internal(format!(
                        "CA 拒绝了本次验证（订单状态 invalid）：{reason}"
                    )),
                ));
            }
            acmecast_acme::OrderStatus::Valid => return Ok(()),
            acmecast_acme::OrderStatus::Pending | acmecast_acme::OrderStatus::Processing => {}
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(acmecast_pipeline::Error::Core(
                acmecast_core::Error::Internal("等待订单就绪超时（120 秒）".to_owned()),
            ));
        }
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }
}

/// 依据输入构造传输配置；当前只有 TLS 校验开关，代理留待后续接入。
fn transport_options(input: &AcmeApplyInput) -> acmecast_acme::ProxyConfig {
    acmecast_acme::ProxyConfig {
        http: None,
        https: None,
        socks5: None,
        accept_invalid_certs: input.insecure_skip_verify,
    }
}

/// 生成本次申请的密钥对与 CSR（SAN 覆盖全部域名，通配符原样保留）。
///
/// 密钥类型由 `algorithm` 决定，CSR 的签名算法随之确定，CA 据此签发
/// 同类型的证书。RSA 依赖 rcgen 的 aws-lc-rs 后端（ring 无 RSA keygen）。
fn generate_key_and_csr(
    domains: &[String],
    algorithm: CertKeyAlgorithm,
) -> Result<(String, Vec<u8>)> {
    let key = rcgen::KeyPair::generate_for(algorithm.signature_algorithm())
        .map_err(|e| domain_error("生成密钥", e))?;
    let mut params = rcgen::CertificateParams::default();
    // 必须清掉 rcgen 的默认 CN（「rcgen self signed cert」），且不放任何 CN：
    // LE 会把 CSR 的 CN 也并入标识符集合，与订单标识符做规范化后的完全相等
    // 比较（Boulder identifier.FromCSR）。CN 与 SAN 不一致——比如通配 SAN
    // 配上 base domain 的 CN——会多出一个订单没有的标识符，finalize 被以
    // 「CSR does not specify same identifiers as Order」拒绝。ACME 只以 SAN
    // 表达申请标识符，subject 留空（certbot 同款）最稳妥。
    params.distinguished_name = rcgen::DistinguishedName::new();
    params.subject_alt_names = domains
        .iter()
        .map(|domain| {
            domain
                .clone()
                .try_into()
                .map(rcgen::SanType::DnsName)
                .map_err(|e| domain_error("域名不合法", e))
        })
        .collect::<Result<_>>()?;
    let csr = params
        .serialize_request(&key)
        .map_err(|e| domain_error("生成 CSR", e))?;
    Ok((key.serialize_pem(), csr.der().to_vec()))
}

/// 按最终选定的 zone 构造挑战记录。
///
/// 两个名字各司其职：
///
/// - **CA 查询名**恒为 `_acme-challenge.<授权域名去通配前缀>`，与 zone 无关——
///   CA 按订单标识符查找，不知道记录存在哪个 zone。
/// - **写入名**按选定 zone 派生为 `_acme-challenge.<zone>`，保证它**必然属于**
///   该 zone。让「写入名不得越出 zone」成为构造上的恒等式，而不是需要事后
///   校验的约定：跨 zone CNAME 委派时（`*.skiy.net` 的挑战记录 CNAME 到
///   `_acme-challenge.666517.xyz`），写入名是 `_acme-challenge.666517.xyz`，
///   而 CA 查询名仍是 `_acme-challenge.skiy.net`。
///
/// 两者相同时退化为 `TxtRecord::new`，既有同 zone 场景行为不变。
fn challenge_record(zone: &str, identifier: &str, value: &str) -> TxtRecord {
    let ca_name = challenge_record_name(identifier);
    let write_name = challenge_write_name(zone);
    if write_name == ca_name {
        return TxtRecord::new(zone, write_name, value, 300);
    }
    TxtRecord::delegated(zone, write_name, ca_name, value, 300)
}

/// 从授权域名推导 DNS zone。
///
/// - 输入含 `*.` 前缀的通配符时（`*.gcli.cn`、`*.example.com`），zone 即剥前缀后的
///   剩余部分（`gcli.cn`、`example.com`），不经「去掉第一段」推导——这是通配符
///   场景的可靠语义：`*.gcli.cn` 的 zone 就是 `gcli.cn`。
/// - 非通配符时仍按「去掉第一段且剩余仍含点」推导（`a.example.com` → `example.com`）；
///   注册域本身（`skiy.net`、`example.com`）返回 `None`，由调用点要求显式配置。
/// - 输入为单段或无点时返回 `None`。
fn zone_of(domain: &str) -> Option<String> {
    let trimmed = domain.trim();
    if let Some(base) = trimmed.strip_prefix("*.") {
        let base = base.trim();
        // SEPC R1/R5：*.xxx.xx → xxx.xx，须为合法 zone（至少一含点、非首尾空段）
        if base.is_empty() || base.starts_with('.') || base.ends_with('.') || !base.contains('.') {
            return None;
        }
        return Some(base.to_owned());
    }
    // SEPC R2/R3/R5：非通配单域
    if !trimmed.contains('.') {
        return None;
    }
    let dot_count = trimmed.matches('.').count();
    if dot_count == 1 {
        // SEPC R3：本身二级域即 zone（xx → xxx.xx 不再剥离）
        if trimmed.starts_with('.') || trimmed.ends_with('.') || trimmed.split('.').any(|s| s.is_empty()) {
            return None;
        }
        return Some(trimmed.to_owned());
    }
    // R2/R5：三级及以上，去首标签
    let rest = trimmed.split_once('.')?.1;
    (rest.contains('.') && !rest.starts_with('.') && !rest.ends_with('.')).then(|| rest.to_owned())
}

/// 通配授权的 zone 解析：对 `x.idev.top` 逐级探测提供商侧是否存在，
/// 取最长匹配的已托管 zone；皆不存在时回退到原始 identifier 并让创建阶段
/// 报可诊断错误，不在此处吞掉。
async fn resolve_wildcard_zone(
    provider: &dyn acmecast_dns::DnsProvider,
    credentials: &serde_json::Value,
    identifier: &str,
) -> Result<String> {
    // SEPC R1：identifier 可能为 *.xxx.xx 或 xxx.xx，剥离 *. 前缀后按 base 逐级探测
    let base = identifier.trim().trim_start_matches("*.").trim();
    let mut candidates: Vec<String> = Vec::new();
    let mut cur = base;
    // 防御：identifier 可能含空白
    if cur.is_empty() {
        return Err(acmecast_pipeline::Error::Core(
            acmecast_core::Error::Internal(format!(
                "无法从 {} 推导 DNS zone，请显式配置 dns_zone",
                identifier
            )),
        ));
    }
    loop {
        candidates.push(cur.to_owned());
        // 剩余不再含点则停止（至少保留两段）
        let Some(dot) = cur.find('.') else { break };
        let next = &cur[dot + 1..];
        if !next.contains('.') {
            // 如 `idev.top` 已入候选，`top` 不应作为 zone
            break;
        }
        cur = next;
    }
    // 优先用 find_txt 探测：对阿里/腾讯/EO 均用 `zone` 作为 DomainName/Zone 参与查询，
    // 若 zone 不存在，find_txt 会返回 Provider 错误且含 NoExist/找不到 zone 的特征串。
    // 逐级尝试，首个不报“不存在”即命中。
    let mut last_not_found: Option<String> = None;
    for zone in &candidates {
        let probe = acmecast_dns::TxtRecord::new(zone, format!("_acme-challenge.{zone}"), "probe", 60);
        match provider.find_txt(credentials, &probe).await {
            Ok(_) => return Ok(zone.clone()),
            Err(e) => {
                let msg = e.to_string();
                // 视为“该 zone 未托管”的信号，继续回退父域
                let not_found = msg.contains("NoExist")
                    || msg.contains("找不到 zone")
                    || msg.contains("找不到域名")
                    || msg.contains("not exist")
                    || msg.contains("DomainName.NoExist")
                    || msg.contains("InvalidDomainName");
                if not_found {
                    last_not_found = Some(msg);
                    continue;
                }
                // 其他错误（鉴权、限流等）直接上抛，避免静默选错父域
                return Err(acmecast_pipeline::Error::Core(acmecast_core::Error::Internal(
                    format!("探测 DNS zone `{zone}` 失败: {msg}"),
                )));
            }
        }
    }
    // 皆未命中，回退到最具体的 identifier，让 create 阶段报原始错误
    if let Some(msg) = last_not_found {
        tracing::warn!(identifier, candidates = ?candidates, error = %msg, "通配 zone 逐级探测均未命中，回退到 identifier 本身");
    }
    Ok(identifier.to_owned())
}

/// 把联系人统一补全为 `mailto:` URI 形式。
///
/// CA 只认 URI 形式的联系方式；输入放宽为裸邮箱即可，前缀在这里统一补上。
/// 已带 `mailto:` 前缀的项原样保留（大小写不敏感，`MAILTO:` 也是合法写法），
/// 空白项视为没填，直接丢弃。
#[must_use]
fn normalize_contacts(contacts: &[String]) -> Vec<String> {
    contacts
        .iter()
        .map(|contact| contact.trim())
        .filter(|contact| !contact.is_empty())
        .map(|contact| {
            if contact.to_ascii_lowercase().starts_with("mailto:") {
                contact.to_owned()
            } else {
                format!("mailto:{contact}")
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{
        CertKeyAlgorithm, challenge_record, generate_key_and_csr, normalize_contacts, zone_of,
    };

    #[test]
    fn schema_marks_dns_zone_required_for_multiple_domains() {
        use std::sync::Arc;

        use super::CertApplyStep;
        use acmecast_dns::DnsProviderRegistry;
        use acmecast_pipeline::PipelineStep as _;

        let step = CertApplyStep::new(Arc::new(DnsProviderRegistry::new()));
        let schema = step.input_schema().expect("cert.apply 应声明输入结构");
        let zone = schema
            .schema
            .object
            .as_ref()
            .expect("顶层应是对象")
            .properties
            .get("dns_zone")
            .expect("dns_zone 应在 properties 里");
        let rendered = serde_json::to_value(zone).expect("dns_zone schema 应能序列化");
        assert_eq!(
            rendered["x-required-when"],
            serde_json::json!({ "MinItems": { "field": "domains", "count": 2 } }),
            "dns_zone 应携带多域名必填条件：{rendered}"
        );
        let description = rendered["description"].as_str().expect("dns_zone 应有说明");
        assert!(
            description.contains("多个域名"),
            "说明应包含多域名指引：{description}"
        );
    }

    #[test]
    fn schema_marks_key_algorithm_with_display_labels() {
        use std::sync::Arc;

        use super::CertApplyStep;
        use acmecast_dns::DnsProviderRegistry;
        use acmecast_pipeline::PipelineStep as _;

        let step = CertApplyStep::new(Arc::new(DnsProviderRegistry::new()));
        let schema = step.input_schema().expect("cert.apply 应声明输入结构");
        let algorithm = schema
            .schema
            .object
            .as_ref()
            .expect("顶层应是对象")
            .properties
            .get("key_algorithm")
            .expect("key_algorithm 应在 properties 里");
        let rendered = serde_json::to_value(algorithm).expect("key_algorithm schema 应能序列化");
        let labels = rendered["x-enum-labels"]
            .as_object()
            .expect("key_algorithm 应携带展示名映射");
        assert_eq!(
            labels["ecdsa_p256"], "ECDSA P-256",
            "四个选项都应有展示名：{rendered}"
        );
        assert_eq!(labels["rsa2048"], "RSA 2048");
        assert_eq!(
            rendered["default"], "ecdsa_p256",
            "schema 应携带缺省值供表单预填：{rendered}"
        );
    }

    #[test]
    fn each_algorithm_generates_matching_key_and_csr() {
        // RSA 位数由 rcgen（aws-lc-rs 后端）的 Rsa2048 档固定，不再重复断言。
        for (algorithm, expected) in [
            (CertKeyAlgorithm::EcdsaP256, acmecast_cert::KeyAlgorithm::EcdsaP256),
            (CertKeyAlgorithm::EcdsaP384, acmecast_cert::KeyAlgorithm::EcdsaP384),
            (CertKeyAlgorithm::Ed25519, acmecast_cert::KeyAlgorithm::Ed25519),
            (CertKeyAlgorithm::Rsa2048, acmecast_cert::KeyAlgorithm::Rsa),
        ] {
            let (key_pem, csr_der) = generate_key_and_csr(&["example.com".to_owned()], algorithm)
                .unwrap_or_else(|e| panic!("{algorithm:?} 生成应成功: {e}"));
            assert!(!csr_der.is_empty(), "CSR 应非空");
            assert_eq!(
                acmecast_cert::detect_algorithm(&key_pem).expect("识别私钥算法"),
                expected,
                "{algorithm:?} 生成的私钥类型应一致"
            );
        }
    }

    #[test]
    fn unknown_key_algorithm_is_rejected_and_default_is_p256() {
        use super::AcmeApplyInput;

        let input: AcmeApplyInput = serde_json::from_value(serde_json::json!({
            "domains": ["example.com"],
            "challenge": "dns-01",
            "account_credential_id": 1,
        }))
        .expect("缺省 key_algorithm 应可反序列化");
        assert_eq!(
            input.key_algorithm,
            CertKeyAlgorithm::EcdsaP256,
            "缺省应回退 ECDSA P-256"
        );

        let rejected = serde_json::from_value::<AcmeApplyInput>(serde_json::json!({
            "domains": ["example.com"],
            "challenge": "dns-01",
            "account_credential_id": 1,
            "key_algorithm": "dsa",
        }));
        assert!(rejected.is_err(), "未知算法值应在输入校验时被拒绝");
    }

    #[test]
    fn zone_of_only_trusts_multi_label_remainders() {
        // 三级以上域名剥第一段后剩余部分仍是合法 zone，推导可信。
        assert_eq!(zone_of("a.example.com").as_deref(), Some("example.com"));
        assert_eq!(
            zone_of("a.b.example.com").as_deref(),
            Some("b.example.com")
        );

        // 通配符的两级域名：zone 即 base domain（`*.gcli.cn` → `gcli.cn`、`*.skiy.net` → `skiy.net`）。
        assert_eq!(zone_of("*.gcli.cn").as_deref(), Some("gcli.cn"));
        assert_eq!(zone_of("*.skiy.net").as_deref(), Some("skiy.net"));
        assert_eq!(zone_of("*.example.com").as_deref(), Some("example.com"));

        // 二级域本身（非通配符）按 SEPC R3 视为自身即 zone，可直接推导。
        assert_eq!(zone_of("skiy.net").as_deref(), Some("skiy.net"));
        assert_eq!(zone_of("example.com").as_deref(), Some("example.com"));
        assert_eq!(zone_of("gcli.cn").as_deref(), Some("gcli.cn"));
    }

    #[test]
    fn a_cname_delegated_zone_splits_the_two_record_names() {
        // 流水线 #12 的真实配置：域名 *.skiy.net 的挑战记录 CNAME 委派到
        // 666517.xyz 托管。写入名落在 dns_zone 下，CA 查询名留在授权域名一侧。
        let record = challenge_record("666517.xyz", "skiy.net", "value");

        assert_eq!(
            record.name,
            "_acme-challenge.666517.xyz",
            "写入名应在 dns_zone 下"
        );
        assert_eq!(
            record.ca_name,
            "_acme-challenge.skiy.net",
            "CA 查询名应按授权域名"
        );
        assert!(record.is_delegated());
        assert_eq!(
            record.relative_name().expect("写入名属于 dns_zone"),
            "_acme-challenge"
        );
        // 权威首跳要走委派源一侧，否则拿不到那条 CNAME。
        assert_eq!(record.ca_zone(), "skiy.net");
    }

    #[test]
    fn a_same_zone_setup_keeps_both_names_identical() {
        let record = challenge_record("skiy.net", "skiy.net", "value");

        assert_eq!(record.name, "_acme-challenge.skiy.net");
        assert_eq!(record.ca_name, "_acme-challenge.skiy.net");
        assert!(!record.is_delegated());
        assert_eq!(record.ca_zone(), "skiy.net");
    }

    #[test]
    fn a_derived_parent_zone_is_not_a_cname_delegation() {
        // 未配 dns_zone 时 zone 由授权域名推导。`a.example.com` 的 zone 是
        // `example.com`，DNS 提供商写 `_acme-challenge.example.com`、CA 查
        // `_acme-challenge.a.example.com`——名字不同但同属一个 zone，属普通
        // 委派而非 CNAME 委派，权威首跳应落在 example.com。
        let zone = zone_of("a.example.com").expect("三级域名可推导 zone");
        let record = challenge_record(&zone, "a.example.com", "value");

        assert_eq!(record.name, "_acme-challenge.example.com");
        assert_eq!(record.ca_name, "_acme-challenge.a.example.com");
        assert!(
            !record.is_delegated(),
            "两个名字同属 example.com，不该判成 CNAME 委派"
        );
        assert_eq!(record.ca_zone(), "example.com");
    }

    #[test]
    fn wildcard_csr_has_no_cn_and_keeps_wildcard_san() {
        // LE 把 CSR 的 CN 也并入标识符集合，与订单标识符做完全相等比较：
        // CN 会把 base domain 带进集合，通配订单的 finalize 会被判
        // 「CSR does not specify same identifiers as Order」。
        // 这里直接检查 DER 字节：subject 不得含 CN OID（2.5.4.3），SAN 原样保留 `*.`。
        let (_key_pem, csr_der) = generate_key_and_csr(
            &["example.com".to_owned(), "*.example.com".to_owned()],
            CertKeyAlgorithm::default(),
        )
        .expect("生成 CSR 应成功");

        let cn_oid: &[u8] = &[0x06, 0x03, 0x55, 0x04, 0x03];
        assert!(
            !csr_der.windows(cn_oid.len()).any(|w| w == cn_oid),
            "CSR 的 subject 不应包含 CN"
        );
        assert!(
            csr_der
                .windows(b"*.example.com".len())
                .any(|w| w == b"*.example.com".as_slice()),
            "SAN 应原样保留通配符"
        );
    }

    #[test]
    fn bare_addresses_get_the_mailto_prefix() {
        assert_eq!(
            normalize_contacts(&["ops@example.com".to_owned()]),
            vec!["mailto:ops@example.com"]
        );
    }

    #[test]
    fn prefixed_addresses_are_kept_as_is() {
        assert_eq!(
            normalize_contacts(&["mailto:ops@example.com".to_owned()]),
            vec!["mailto:ops@example.com"]
        );
        // 前缀大小写不敏感，且不重复补全。
        assert_eq!(
            normalize_contacts(&["MAILTO:ops@example.com".to_owned()]),
            vec!["MAILTO:ops@example.com"]
        );
    }

    #[test]
    fn blank_entries_are_dropped() {
        assert!(normalize_contacts(&["".to_owned(), "  ".to_owned()]).is_empty());
    }

    #[test]
    fn mixed_entries_are_normalized_individually() {
        assert_eq!(
            normalize_contacts(&[
                "ops@example.com".to_owned(),
                "mailto:ca@example.com".to_owned(),
                "".to_owned(),
                " security@example.com ".to_owned(),
            ]),
            vec![
                "mailto:ops@example.com",
                "mailto:ca@example.com",
                "mailto:security@example.com",
            ]
        );
    }
}
