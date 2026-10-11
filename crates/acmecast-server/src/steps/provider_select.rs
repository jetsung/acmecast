//! DNS-01 挑战的 DNS 提供商自动选择。
//!
//! `cert.apply` 未显式指定 `dns_provider` 时，按「域名托管商对齐优先」的
//! 固定候选序逐个探测 zone 托管情况，选定第一个可用的提供商：
//!
//! 1. `tencent`（腾讯云云解析）→ `tencent-eo`（EdgeOne）→ `aliyun` → `cloudflare`；
//! 2. 其余提供商按注册表稳定序（`type_ids()` 排序）追加在最后。
//!
//! 域名托管在某一家时，对齐的那家排在最前且探测通过，自然命中；
//! 域名不在这四家时按序回退——两个需求由同一条候选序统一实现。

use acmecast_access::CredentialStore;
use acmecast_dns::{DnsProvider, DnsProviderRegistry};
use acmecast_pipeline::{Error, Result};
use serde_json::Value;

/// 托管商对齐的候选优先级：腾讯云体系最前，`tencent` 优先于 `tencent-eo`。
const PRIORITY: [&str; 4] = ["tencent", "tencent-eo", "aliyun", "cloudflare"];

/// 按候选序排列「有凭据且已注册」的提供商类型。
///
/// `with_credentials` 是确认有可用凭据的类型集合，`registry_types` 是
/// DNS 注册表的类型清单（稳定序）。返回值只含两者交集，顺序为
/// [`PRIORITY`] 在前、其余按 `registry_types` 序在后。
pub(crate) fn candidate_order<'a>(
    with_credentials: &[&'a str],
    registry_types: &[&'a str],
) -> Vec<&'a str> {
    let mut out = Vec::new();
    for priority in PRIORITY {
        if with_credentials.contains(&priority) {
            out.push(priority);
        }
    }
    for type_id in registry_types {
        if !out.contains(type_id) && with_credentials.contains(type_id) {
            out.push(type_id);
        }
    }
    out
}

/// 一个候选提供商及其解密后的可用凭据。
pub(crate) struct Candidate<'a> {
    pub type_id: String,
    pub provider: &'a dyn DnsProvider,
    /// `(凭据标识, 解密后的字段值)`，按 id 升序。
    pub credentials: Vec<(i64, Value)>,
}

/// 自动选择的结果。
#[derive(Debug)]
pub(crate) struct Selected {
    /// 选定的提供商类型标识。
    pub provider_id: String,
    /// 所用凭据标识。
    pub credential_id: i64,
    /// 解密后的凭据字段值，交给后续 TXT 管理。
    pub fields: Value,
}

/// 逐个候选探测 zone 托管情况，选定第一个可用的（凭据，提供商）组合。
///
/// 每个失败候选的原因经 `log` 输出，保证回退过程可从步骤日志追溯；
/// 全部失败时报错汇总各候选原因，而不是只留最后一个。
pub(crate) async fn pick(
    candidates: Vec<Candidate<'_>>,
    zone: &str,
    mut log: impl FnMut(String),
) -> Result<Selected> {
    let mut failures: Vec<String> = Vec::new();
    for candidate in candidates {
        for (credential_id, fields) in candidate.credentials {
            match candidate.provider.probe_zone(&fields, zone).await {
                Ok(()) => {
                    log(format!(
                        "自动选择 DNS 提供商 `{}`（凭据 #{credential_id}）：zone `{zone}` 托管探测通过",
                        candidate.type_id
                    ));
                    return Ok(Selected {
                        provider_id: candidate.type_id.to_owned(),
                        credential_id,
                        fields,
                    });
                }
                Err(error) => {
                    log(format!(
                        "候选 DNS 提供商 `{}`（凭据 #{credential_id}）不可用：{error}",
                        candidate.type_id
                    ));
                    failures.push(format!(
                        "{}（凭据 #{credential_id}）：{error}",
                        candidate.type_id
                    ));
                }
            }
        }
    }

    Err(Error::Core(acmecast_core::Error::Internal(format!(
        "没有可用的 DNS 提供商能管理 zone `{zone}`，已按候选序尝试：\n- {}",
        failures.join("\n- ")
    ))))
}

/// 从凭据存储与 DNS 注册表构造候选清单。
///
/// 只列「已注册提供商 ∩ 库中有凭据」的类型；凭据在此处解密
/// （探测需要真实字段值），解密失败的凭据按候选失败处理而非整体失败。
pub(crate) async fn build_candidates<'a>(
    dns: &'a DnsProviderRegistry,
    store: &CredentialStore<'_>,
    mut log: impl FnMut(String),
) -> Result<Vec<Candidate<'a>>> {
    let registry_types = dns.type_ids();
    let registry_refs: Vec<&str> = registry_types.iter().map(String::as_str).collect();
    let mut with_credentials = Vec::new();
    for type_id in &registry_types {
        let listed = store.list_by_type(type_id).await?;
        if !listed.is_empty() {
            with_credentials.push(type_id.as_str());
        }
    }

    let mut candidates = Vec::new();
    for type_id in candidate_order(&with_credentials, &registry_refs) {
        let Some(provider) = dns.get(type_id) else {
            continue;
        };
        let mut credentials = Vec::new();
        for summary in store.list_by_type(type_id).await? {
            match store.resolve(summary.id).await {
                Ok(resolved) => credentials.push((resolved.id, resolved.fields)),
                Err(error) => {
                    log(format!(
                        "候选 DNS 提供商 `{type_id}` 的凭据 #{} 读取失败：{error}",
                        summary.id
                    ));
                }
            }
        }
        candidates.push(Candidate {
            type_id: type_id.to_owned(),
            provider,
            credentials,
        });
    }
    Ok(candidates)
}

#[cfg(test)]
mod tests {
    use super::*;
    use acmecast_dns::TxtRecord;
    use serde_json::json;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    /// 可编程探测结果的测试提供商。
    #[derive(Debug)]
    struct ProbeProvider {
        id: &'static str,
        ok: bool,
        probes: Arc<AtomicUsize>,
    }

    impl ProbeProvider {
        fn ok(id: &'static str) -> (Self, Arc<AtomicUsize>) {
            let probes = Arc::new(AtomicUsize::new(0));
            (
                Self {
                    id,
                    ok: true,
                    probes: Arc::clone(&probes),
                },
                probes,
            )
        }
        fn failing(id: &'static str) -> (Self, Arc<AtomicUsize>) {
            let probes = Arc::new(AtomicUsize::new(0));
            (
                Self {
                    id,
                    ok: false,
                    probes: Arc::clone(&probes),
                },
                probes,
            )
        }
    }

    #[async_trait::async_trait]
    impl DnsProvider for ProbeProvider {
        fn type_id(&self) -> &'static str {
            self.id
        }
        fn display_name(&self) -> &'static str {
            "测试"
        }
        fn credential_fields(&self) -> schemars::schema::RootSchema {
            schemars::schema_for!(Value)
        }
        async fn find_txt(&self, _: &Value, _: &TxtRecord) -> acmecast_dns::Result<Vec<String>> {
            Ok(vec![])
        }
        async fn create_txt(&self, _: &Value, _: &TxtRecord) -> acmecast_dns::Result<()> {
            Ok(())
        }
        async fn delete_txt(&self, _: &Value, _: &TxtRecord) -> acmecast_dns::Result<()> {
            Ok(())
        }
        async fn probe_zone(&self, _: &Value, _: &str) -> acmecast_dns::Result<()> {
            self.probes.fetch_add(1, Ordering::SeqCst);
            if self.ok {
                Ok(())
            } else {
                Err(acmecast_dns::Error::provider("zone 未托管".to_owned()))
            }
        }
    }

    fn candidate(p: &ProbeProvider) -> Candidate<'_> {
        Candidate {
            type_id: p.type_id().to_owned(),
            provider: p,
            credentials: vec![(7, json!({}))],
        }
    }

    #[test]
    fn priority_providers_lead_and_the_rest_follow_registry_order() {
        let order = candidate_order(
            &["cloudflare", "aliyun", "route53", "tencent"],
            &["aliyun", "cloudflare", "route53", "tencent"],
        );
        assert_eq!(order, vec!["tencent", "aliyun", "cloudflare", "route53"]);
    }

    #[test]
    fn types_without_credentials_are_dropped() {
        let order = candidate_order(&["aliyun"], &["tencent", "aliyun", "cloudflare"]);
        assert_eq!(order, vec!["aliyun"]);
    }

    #[test]
    fn tencent_eo_comes_after_tencent() {
        let order = candidate_order(
            &["tencent-eo", "tencent", "aliyun", "cloudflare"],
            &["tencent", "tencent-eo", "aliyun", "cloudflare"],
        );
        assert_eq!(order, vec!["tencent", "tencent-eo", "aliyun", "cloudflare"]);
    }

    #[tokio::test]
    async fn the_first_candidate_that_probes_clean_wins() {
        let (first, first_probes) = ProbeProvider::failing("tencent");
        let (second, second_probes) = ProbeProvider::ok("aliyun");
        let (third, _third_probes) = ProbeProvider::ok("cloudflare");

        let mut logs = Vec::new();
        let picked = pick(
            vec![candidate(&first), candidate(&second), candidate(&third)],
            "example.com",
            &mut |message| logs.push(message),
        )
        .await
        .expect("第二个候选应命中");

        assert_eq!(picked.provider_id, "aliyun");
        assert_eq!(picked.credential_id, 7);
        assert_eq!(first_probes.load(Ordering::SeqCst), 1, "首选被探测一次");
        assert_eq!(second_probes.load(Ordering::SeqCst), 1);
        assert!(
            logs.iter().any(|m| m.contains("tencent") && m.contains("不可用")),
            "跳过原因应写日志：{logs:?}"
        );
        assert!(
            logs.iter()
                .any(|m| m.contains("自动选择") && m.contains("aliyun")),
            "选定结果应写日志：{logs:?}"
        );
    }

    #[tokio::test]
    async fn the_leading_aligned_provider_wins_directly() {
        let (first, _) = ProbeProvider::ok("tencent");
        let (second, second_probes) = ProbeProvider::ok("aliyun");

        let mut logs = Vec::new();
        let picked = pick(
            vec![candidate(&first), candidate(&second)],
            "example.com",
            &mut |message| logs.push(message),
        )
        .await
        .expect("首选应命中");

        assert_eq!(picked.provider_id, "tencent");
        assert_eq!(second_probes.load(Ordering::SeqCst), 0, "命中后不再探测后续候选");
    }

    #[tokio::test]
    async fn exhausted_candidates_fail_with_every_reason() {
        let (first, _) = ProbeProvider::failing("tencent");
        let (second, _) = ProbeProvider::failing("aliyun");

        let err = pick(
            vec![candidate(&first), candidate(&second)],
            "example.com",
            &mut |_| {},
        )
        .await
        .expect_err("全部失败应报错");

        let text = err.to_string();
        assert!(text.contains("tencent"), "{text}");
        assert!(text.contains("aliyun"), "{text}");
        assert!(text.contains("zone 未托管"), "{text}");
    }
}
