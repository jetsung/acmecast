//! DNS 挑战记录的生命周期。
//!
//! 挑战的麻烦之处不在「写记录」或「删记录」，而在**两者的配对**：一旦某条
//! 记录没被清掉，下一次对同一域名发起挑战时，CA 可能读到残留的旧值，
//! 表现为「校验值不匹配」——一个完全看不出真因的错误。所以清理必须发生在
//! 成功与失败**两条路径**上，而这件事值得有个专门的入口来保证。

use serde_json::Value;
use tracing::warn;

use crate::error::{Error, Result};
use crate::provider::{DnsProvider, TxtRecord};

/// ACME 的 DNS-01 挑战记录名前缀。
const CHALLENGE_PREFIX: &str = "_acme-challenge";

/// 本项目支持的挑战类型。
///
/// 与 [`acmecast_acme::ChallengeKind`] 不是同一个东西：后者是**协议里可能出现的
/// 全部类型**（含首版不使用的 TLS-ALPN-01），这里是**我们支持的子集**；而且它要
/// 作为流水线步骤的输入字段，因此带 `Serialize`/`Deserialize`/`JsonSchema`。
/// 两者之间用 `TryFrom` 转换，协议那边冒出新的类型时会明确报错。
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
pub enum ChallengeKind {
    /// DNS-01：在域名下写一条 TXT 记录。
    ///
    /// 显式写 `rename` 而不靠 `rename_all`：`Dns01` 这种带数字的变体在
    /// kebab-case 下会变成 `dns01`（数字前不插连字符），与协议的 `dns-01` 对不上。
    #[serde(rename = "dns-01")]
    Dns01,
    /// HTTP-01：在域名的 HTTP 端点提供令牌文件。
    #[serde(rename = "http-01")]
    Http01,
}

impl ChallengeKind {
    /// 协议里的取值。
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Dns01 => "dns-01",
            Self::Http01 => "http-01",
        }
    }
}

impl TryFrom<acmecast_acme::ChallengeKind> for ChallengeKind {
    type Error = Error;

    /// 协议那边出现本项目不支持的类型时明确报错，而不是硬塞进某个分支。
    fn try_from(kind: acmecast_acme::ChallengeKind) -> Result<Self> {
        match kind {
            acmecast_acme::ChallengeKind::Dns01 => Ok(Self::Dns01),
            acmecast_acme::ChallengeKind::Http01 => Ok(Self::Http01),
            other => Err(Error::UnsupportedChallenge(format!(
                "本项目暂不支持 {} 挑战",
                other.as_acme_name()
            ))),
        }
    }
}

/// 校验挑战类型能覆盖这些域名。
///
/// **必须在任何写操作之前调用**——spec 要求「通配符搭配 HTTP-01 时在验证开始前
/// 返回不支持的错误」，而不是等写了一半才发现走不通。
///
/// 通配符只能走 DNS-01：CA 不会为 `*.example.com` 下发 HTTP-01 挑战，
/// 因为对通配符的授权无法靠单个 HTTP 端点证明。
pub fn ensure_kind_covers(domains: &[String], kind: ChallengeKind) -> Result<()> {
    if kind == ChallengeKind::Dns01 {
        return Ok(());
    }

    let wildcards: Vec<&str> = domains
        .iter()
        .map(String::as_str)
        .filter(|domain| domain.starts_with("*."))
        .collect();

    if wildcards.is_empty() {
        return Ok(());
    }

    Err(Error::UnsupportedChallenge(format!(
        "{} 只能使用 DNS-01；HTTP-01 无法证明对通配符的授权",
        wildcards.join("、")
    )))
}

/// 按域名构造挑战记录名。
///
/// 通配符域名（`*.example.com`）要去掉 `*.`：挑战记录永远写在裸域名下，
/// CA 也是去那里找。
#[must_use]
pub fn challenge_record_name(domain: &str) -> String {
    let base = domain.strip_prefix("*.").unwrap_or(domain);
    format!("{CHALLENGE_PREFIX}.{base}")
}

/// 写入一条挑战记录，执行 `action`，然后**无论成败**都清理掉它。
///
/// 四种组合各有处置：
///
/// | action | 清理 | 返回 |
/// |---|---|---|
/// | 成功 | 成功 | `action` 的结果 |
/// | 成功 | 失败 | **清理错误**——成功却留了条记录，那是真问题 |
/// | 失败 | 成功 | 原始错误 |
/// | 失败 | 失败 | 原始错误（清理失败只记 warn，不能盖住真因） |
///
/// `action` 通常是「等待传播并让 CA 校验」，但它是个普通 future，
/// 因此本函数不关心它做什么。
pub async fn with_txt_record<F, T>(
    provider: &dyn DnsProvider,
    credentials: &Value,
    record: &TxtRecord,
    action: F,
) -> Result<T>
where
    F: Future<Output = Result<T>>,
{
    // 写入前先看这个名字下有没有既有记录。
    //
    // 同名不同值是真麻烦：CA 可能读到旧的那条，而报出来的只是「校验值不匹配」。
    // 同名同值则视为已达目的——幂等，不重复写。
    let existing = provider.find_txt(credentials, record).await?;
    if existing.iter().any(|value| value == &record.value) {
        warn!(
            name = %record.name,
            "挑战记录已存在且值相同，跳过写入"
        );
    } else if !existing.is_empty() {
        return Err(Error::provider(format!(
            "`{}` 下已有 TXT 记录（{} 条）且值与本次不同；\
             残留记录会让 CA 读到旧值，请先清理后再挑战",
            record.name,
            existing.len()
        )));
    } else {
        provider.create_txt(credentials, record).await?;
    }

    let outcome = action.await;
    let cleaned = provider.delete_txt(credentials, record).await;

    match (outcome, cleaned) {
        (Ok(value), Ok(())) => Ok(value),
        (Ok(_), Err(cleanup)) => Err(cleanup),
        (Err(original), Ok(())) => Err(original),
        (Err(original), Err(cleanup)) => {
            warn!(
                name = %record.name,
                error = %cleanup,
                "挑战记录清理失败；原始失败原因优先上报"
            );
            Err(original)
        }
    }
}

use std::future::Future;

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use async_trait::async_trait;
    use schemars::schema::RootSchema;
    use schemars::schema_for;
    use serde_json::json;

    use super::*;

    #[test]
    fn the_record_name_gets_the_challenge_prefix() {
        assert_eq!(
            challenge_record_name("example.com"),
            "_acme-challenge.example.com"
        );
    }

    #[test]
    fn a_wildcard_domain_drops_the_star() {
        // 挑战记录写在裸域名下：CA 找的是 `_acme-challenge.example.com`，
        // 不是 `_acme-challenge.*.example.com`。
        assert_eq!(
            challenge_record_name("*.example.com"),
            "_acme-challenge.example.com"
        );
    }

    /// 会记录调用的假提供商。
    #[derive(Debug, Default)]
    struct FakeProvider {
        /// 响应 `find_txt` 的既有值。
        existing: Vec<String>,
        calls: Mutex<Vec<String>>,
        /// 让删除失败。
        fail_delete: bool,
        /// 让写入失败。
        fail_create: bool,
    }

    impl FakeProvider {
        fn calls(&self) -> Vec<String> {
            self.calls.lock().expect("锁不应中毒").clone()
        }

        fn record(&self, call: &str) {
            self.calls.lock().expect("锁不应中毒").push(call.to_owned());
        }
    }

    #[async_trait]
    impl DnsProvider for FakeProvider {
        fn type_id(&self) -> &'static str {
            "fake"
        }

        fn display_name(&self) -> &'static str {
            "假 DNS"
        }

        fn credential_fields(&self) -> RootSchema {
            schema_for!(String)
        }

        async fn find_txt(&self, _credentials: &Value, _record: &TxtRecord) -> Result<Vec<String>> {
            self.record("find");
            Ok(self.existing.clone())
        }

        async fn create_txt(&self, _credentials: &Value, record: &TxtRecord) -> Result<()> {
            self.record(&format!("create {}", record.value));
            if self.fail_create {
                return Err(Error::provider("写入被拒绝"));
            }
            Ok(())
        }

        async fn delete_txt(&self, _credentials: &Value, record: &TxtRecord) -> Result<()> {
            self.record(&format!("delete {}", record.value));
            if self.fail_delete {
                return Err(Error::provider("删除被拒绝"));
            }
            Ok(())
        }
    }

    fn record() -> TxtRecord {
        TxtRecord::new("example.com", "_acme-challenge.example.com", "value-1", 60)
    }

    fn credentials() -> Value {
        json!({})
    }

    // ---- 挑战类型与域名的相容性 ----

    #[test]
    fn the_kind_spells_itself_the_way_the_protocol_does() {
        // `Dns01` 若靠 rename_all 会变成 `dns01`，与协议的 `dns-01` 不符。
        assert_eq!(ChallengeKind::Dns01.as_str(), "dns-01");
        assert_eq!(
            serde_json::to_string(&ChallengeKind::Dns01).unwrap(),
            "\"dns-01\""
        );
        assert_eq!(
            serde_json::from_str::<ChallengeKind>("\"http-01\"").unwrap(),
            ChallengeKind::Http01
        );
    }

    #[test]
    fn a_wildcard_with_dns01_is_fine() {
        let domains = vec!["*.example.com".to_owned()];
        ensure_kind_covers(&domains, ChallengeKind::Dns01).expect("通配符走 DNS-01 应放行");
    }

    #[test]
    fn a_wildcard_with_http01_is_refused() {
        let domains = vec!["*.example.com".to_owned()];
        let err = ensure_kind_covers(&domains, ChallengeKind::Http01).expect_err("应被拒绝");

        let text = err.to_string();
        assert!(text.contains("*.example.com"), "应指出是哪个域名: {text}");
        assert!(text.contains("DNS-01"), "应说明该用什么: {text}");
    }

    #[test]
    fn plain_domains_accept_either_kind() {
        let domains = vec!["example.com".to_owned(), "www.example.com".to_owned()];
        ensure_kind_covers(&domains, ChallengeKind::Http01).expect("非通配符可用 HTTP-01");
        ensure_kind_covers(&domains, ChallengeKind::Dns01).expect("当然也可用 DNS-01");
    }

    #[test]
    fn one_wildcard_in_the_list_is_enough_to_refuse() {
        // 混合域名里只要有一个通配符，整条流水线就不能走 HTTP-01。
        let domains = vec!["example.com".to_owned(), "*.example.com".to_owned()];
        let err = ensure_kind_covers(&domains, ChallengeKind::Http01).expect_err("应被拒绝");
        assert!(err.to_string().contains("*.example.com"), "{err}");
    }

    #[test]
    fn an_empty_domain_list_is_vacuously_fine() {
        ensure_kind_covers(&[], ChallengeKind::Http01).expect("没有域名就没什么可拒绝的");
    }

    /// 模拟挑战步骤的做法：**先**校验类型相容，通过了才动记录。
    async fn prepare(
        provider: &FakeProvider,
        domains: &[String],
        kind: ChallengeKind,
        credentials: &Value,
        record: &TxtRecord,
    ) -> Result<()> {
        ensure_kind_covers(domains, kind)?;
        with_txt_record(provider, credentials, record, async { Ok(()) }).await
    }

    #[tokio::test]
    async fn a_refusal_happens_before_anything_is_written() {
        // spec 要求「在验证开始前返回不支持的错误」——一个请求都不该发出去。
        let provider = FakeProvider::default();
        let domains = vec!["*.example.com".to_owned()];

        let result = prepare(
            &provider,
            &domains,
            ChallengeKind::Http01,
            &credentials(),
            &record(),
        )
        .await;

        assert!(result.is_err());
        assert!(
            provider.calls().is_empty(),
            "被拒绝时不该动任何记录: {:?}",
            provider.calls()
        );
    }

    #[tokio::test]
    async fn a_permitted_combination_proceeds_normally() {
        // 与上一条对照：通配符配 DNS-01 时流程照常走完并清理。
        let provider = FakeProvider::default();
        let domains = vec!["*.example.com".to_owned()];

        prepare(
            &provider,
            &domains,
            ChallengeKind::Dns01,
            &credentials(),
            &record(),
        )
        .await
        .expect("通配符走 DNS-01 应正常继续");

        assert_eq!(
            provider.calls(),
            vec!["find", "create value-1", "delete value-1"]
        );
    }

    #[tokio::test]
    async fn a_successful_action_cleans_up_afterwards() {
        let provider = FakeProvider::default();
        let result = with_txt_record(&provider, &credentials(), &record(), async {
            Ok("校验通过")
        })
        .await;

        assert_eq!(result.unwrap(), "校验通过");
        assert_eq!(
            provider.calls(),
            vec!["find", "create value-1", "delete value-1"],
            "写入、执行、清理"
        );
    }

    #[tokio::test]
    async fn a_failed_action_still_cleans_up() {
        // spec 场景：验证失败时仍要删除已创建的记录，并返回原始失败原因。
        let provider = FakeProvider::default();
        let result: Result<()> = with_txt_record(&provider, &credentials(), &record(), async {
            Err(Error::provider("CA 说校验值不匹配"))
        })
        .await;

        let err = result.expect_err("应返回原始错误");
        assert!(err.to_string().contains("校验值不匹配"), "{err}");
        assert_eq!(
            provider.calls(),
            vec!["find", "create value-1", "delete value-1"],
            "失败路径同样要清理"
        );
    }

    #[tokio::test]
    async fn a_failed_creation_skips_the_action_and_the_cleanup() {
        let provider = FakeProvider {
            fail_create: true,
            ..Default::default()
        };

        let ran = Arc::new(Mutex::new(false));
        let flag = Arc::clone(&ran);
        let result: Result<()> = with_txt_record(&provider, &credentials(), &record(), async {
            *flag.lock().unwrap() = true;
            Ok(())
        })
        .await;

        assert!(result.is_err());
        assert!(!*ran.lock().unwrap(), "没写进去就不该执行校验");
        assert_eq!(
            provider.calls(),
            vec!["find", "create value-1"],
            "没写进去就没什么可清理的"
        );
    }

    #[tokio::test]
    async fn cleanup_failure_after_success_is_reported() {
        // 成功了却留了条记录——这是真问题，不能当成功返回。
        let provider = FakeProvider {
            fail_delete: true,
            ..Default::default()
        };
        let result: Result<&str> = with_txt_record(&provider, &credentials(), &record(), async {
            Ok("校验通过")
        })
        .await;

        let err = result.expect_err("清理失败应上报");
        assert!(err.to_string().contains("删除被拒绝"), "{err}");
    }

    #[tokio::test]
    async fn the_original_error_wins_when_cleanup_also_fails() {
        // 两个都失败时，用户需要的是「为什么挑战失败」，
        // 清理失败只是附带信息，不该盖住真因。
        let provider = FakeProvider {
            fail_delete: true,
            ..Default::default()
        };
        let result: Result<()> = with_txt_record(&provider, &credentials(), &record(), async {
            Err(Error::provider("CA 说校验超时"))
        })
        .await;

        let err = result.expect_err("应报错");
        assert!(
            err.to_string().contains("校验超时"),
            "应保留原始原因: {err}"
        );
        assert!(
            !err.to_string().contains("删除被拒绝"),
            "清理失败不该盖住真因: {err}"
        );
    }

    // ---- 同名记录的处置 ----

    #[tokio::test]
    async fn an_identical_existing_record_makes_creation_idempotent() {
        let provider = FakeProvider {
            existing: vec!["value-1".to_owned()],
            ..Default::default()
        };

        with_txt_record(&provider, &credentials(), &record(), async { Ok(()) })
            .await
            .expect("同值已存在应视为已达目的");

        let calls = provider.calls();
        assert!(
            !calls.iter().any(|call| call.starts_with("create")),
            "不该重复写入: {calls:?}"
        );
        assert!(
            calls.iter().any(|call| call.starts_with("delete")),
            "仍然要清理: {calls:?}"
        );
    }

    #[tokio::test]
    async fn a_conflicting_existing_record_is_refused() {
        // 同名不同值：CA 可能读到旧的那条，而现象只是「校验值不匹配」。
        let provider = FakeProvider {
            existing: vec!["stale-value".to_owned()],
            ..Default::default()
        };

        let ran = Arc::new(Mutex::new(false));
        let flag = Arc::clone(&ran);
        let result: Result<()> = with_txt_record(&provider, &credentials(), &record(), async {
            *flag.lock().unwrap() = true;
            Ok(())
        })
        .await;

        let err = result.expect_err("同名不同值应被拒绝");
        assert!(err.to_string().contains("已有 TXT 记录"), "{err}");
        assert!(!*ran.lock().unwrap(), "不该继续往下走");

        let calls = provider.calls();
        assert!(
            !calls.iter().any(|call| call.starts_with("create")),
            "不该覆盖写入: {calls:?}"
        );
        assert!(
            !calls.iter().any(|call| call.starts_with("delete")),
            "既有记录不是我们写的，不该替别人删掉: {calls:?}"
        );
    }
}
