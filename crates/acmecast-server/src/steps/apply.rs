//! `cert.apply`：向 ACME CA 申请证书。
//!
//! 一次执行完成「建账号（或复用）→ 下单 → 完成挑战 → 提交 CSR → 拿到证书链」，
//! 产物 `cert_pem`／`key_pem`／`domains`／`fingerprint` 交给后续的入库与部署步骤。
//!
//! 账号凭据按标识从凭据系统取：**首次使用时注册**，签发的账号凭据写回凭据
//! 记录，之后的运行都复用同一账号——每次运行注册新账号既浪费又可能撞上
//! CA 的账号配额。

use std::sync::Arc;

use acmecast_access::AcmeAccountFields;
use acmecast_acme::{AccountMode, AcmeService, EstablishAccountInput};
use acmecast_dns::{
    ChallengeKind as DnsChallengeKind, DnsProviderRegistry, PropagationPolicy, TxtRecord,
    challenge_record_name, ensure_kind_covers, wait_until_visible,
};
use acmecast_pipeline::{PipelineStep, Result, StepContext, StepOutput};
use schemars::JsonSchema;
use serde::Deserialize;

use super::domain_error;

/// `cert.apply` 的输入。
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct AcmeApplyInput {
    /// 要申请的域名集合；通配符（`*.example.com`）会被强制走 DNS-01。
    pub domains: Vec<String>,
    /// 挑战类型。当前只支持 DNS-01——HTTP-01 的投放通道尚未提供。
    pub challenge: DnsChallengeKind,
    /// ACME 账号凭据标识（`acme.account` 类型）。
    pub account_credential_id: i64,
    /// DNS 提供商标识（如 `cloudflare`）；DNS-01 时必填。
    #[serde(default)]
    pub dns_provider: Option<String>,
    /// DNS 提供商凭据标识；DNS-01 时必填。
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
}

fn default_true() -> bool {
    true
}

/// ACME 申请步骤。
#[derive(Debug)]
pub struct CertApplyStep {
    dns: Arc<DnsProviderRegistry>,
    /// DNS-01 传播等待策略；来自服务配置的 `[propagation]` 段，
    /// 装配期注入——它是系统级调优，不属于步骤输入。
    propagation: PropagationPolicy,
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
        Self { dns, propagation }
    }
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
        Some(schema)
    }

    async fn execute(&self, ctx: &mut StepContext<'_>) -> Result<StepOutput> {
        let input: AcmeApplyInput = ctx
            .input_as()
            .map_err(|error| domain_error("cert.apply 输入不合法", error))?;
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
        let (key_pem, csr_der) = generate_key_and_csr(&input.domains)?;
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
        let established = EstablishAccountInput {
            directory_url: &directory_url,
            mode: AccountMode::Create {
                contacts: normalize_contacts(&input.contacts),
            },
            external_account: None,
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

        let provider_id = input.dns_provider.as_deref().ok_or_else(|| {
            acmecast_pipeline::Error::Core(acmecast_core::Error::Internal(
                "dns-01 挑战需要配置 dns_provider".to_owned(),
            ))
        })?;
        let dns_credential_id = input.dns_credential_id.ok_or_else(|| {
            acmecast_pipeline::Error::Core(acmecast_core::Error::Internal(
                "dns-01 挑战需要配置 dns_credential_id".to_owned(),
            ))
        })?;
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

        let materials = order
            .challenge_materials(&authorization.identifier, acme_kind)
            .await
            .map_err(|e| domain_error("读取挑战材料", e))?;
        // 显式给了 zone 就用它；留空（未填或空串）则从主域名推导——
        // 表单里“没填”很容易存成空串，不该让用户撞上「找不到域名 ``」这种错。
        let zone = match input.dns_zone.as_deref().map(str::trim) {
            Some(zone) if !zone.is_empty() => zone.to_owned(),
            _ => zone_of(&authorization.identifier).ok_or_else(|| {
                acmecast_pipeline::Error::Core(acmecast_core::Error::Internal(format!(
                    "无法从 {} 推导 DNS zone，请显式配置 dns_zone",
                    authorization.identifier
                )))
            })?,
        };
        let record = TxtRecord::new(
            &zone,
            challenge_record_name(&authorization.identifier),
            &materials.dns_txt_value,
            300,
        );
        ctx.log_info(format!("写入挑战记录 {}", record.name));

        let ready = order.set_challenge_ready(&authorization.identifier, acme_kind);
        // 记录存在期间做「等传播 + 通知 CA」；无论成败，退出时记录都会被清理。
        // action 的错误类型随 with_txt_record 的约定是 DNS 错误，转成流水线错误放在外层。
        let action = async {
            if input.wait_propagation {
                let http = Arc::new(acmecast_dns::ReqwestTransport::new().map_err(|e| {
                    acmecast_dns::Error::provider(format!("构造解析器客户端: {e}"))
                })?);
                let resolvers = acmecast_dns::resolvers_for_zone(http, &zone);
                let refs: Vec<&dyn acmecast_dns::DnsResolver> =
                    resolvers.iter().map(|resolver| resolver.as_ref()).collect();
                wait_until_visible(
                    provider,
                    &credentials,
                    &record,
                    &refs,
                    &self.propagation,
                )
                .await?;
                ctx.log_info(format!("TXT 记录已在权威/公共解析器可见：{}", record.name));
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
fn generate_key_and_csr(domains: &[String]) -> Result<(String, Vec<u8>)> {
    let key = rcgen::KeyPair::generate().map_err(|e| domain_error("生成密钥", e))?;
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

/// 从授权域名推导 DNS zone：剥掉通配符前缀并去掉第一段（`a.example.com` →
/// `example.com`）。
///
/// 剩余部分不再含点（只剩 TLD）时推导不可信：注册域本身（`skiy.net`、
/// `example.com`）与通配符注册域（`*.skiy.net`）剥前缀去段后都只剩 TLD，
/// 拿它当 zone 会一路走到 DNS 提供商才报「找不到域名 `net`」。这种域名
/// 返回 `None`，由调用点要求显式配置 `dns_zone`。
fn zone_of(domain: &str) -> Option<String> {
    let rest = domain.trim_start_matches("*.").split_once('.')?.1;
    rest.contains('.').then(|| rest.to_owned())
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
    use super::{generate_key_and_csr, normalize_contacts, zone_of};

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
    fn zone_of_only_trusts_multi_label_remainders() {
        // 三级以上域名剥第一段后剩余部分仍是合法 zone，推导可信。
        assert_eq!(zone_of("a.example.com").as_deref(), Some("example.com"));
        assert_eq!(
            zone_of("a.b.example.com").as_deref(),
            Some("b.example.com")
        );

        // 注册域本身与通配符注册域剥前缀去段后只剩 TLD：`*.skiy.net` 一度
        // 被推导成 `net` 拿去查 Cloudflare（histories/26），必须交给显式配置。
        assert_eq!(zone_of("skiy.net"), None);
        assert_eq!(zone_of("*.skiy.net"), None);
        assert_eq!(zone_of("example.com"), None);
    }

    #[test]
    fn wildcard_csr_has_no_cn_and_keeps_wildcard_san() {
        // LE 把 CSR 的 CN 也并入标识符集合，与订单标识符做完全相等比较：
        // CN 会把 base domain 带进集合，通配订单的 finalize 会被判
        // 「CSR does not specify same identifiers as Order」。
        // 这里直接检查 DER 字节：subject 不得含 CN OID（2.5.4.3），SAN 原样保留 `*.`。
        let (_key_pem, csr_der) =
            generate_key_and_csr(&["example.com".to_owned(), "*.example.com".to_owned()])
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
