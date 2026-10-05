//! 证书实体。
//!
//! 承载 ACME 签发的证书与其私钥的**存放位置**（实际 PEM 内容写在数据目录文件中，
//! 库中只存相对路径），并直接持有签发所用的 ACME 账号凭据标识。
//!
//! 关于 `acme_account_access_id`：certd 在吊销证书时需要反向解析流水线配置才能
//! 找到 ACME 账号（见 `certd/PLAN-custom-acme.md` 第 4.5 节）。本实现从建表起
//! 就把它落在证书行上，吊销时直接读表，**不提供**反查流水线的回退路径——
//! 新库不存在历史数据，引入回退等于把已知缺陷换个形式写回来。

use acmecast_cert::{CertStatus, ExpiryPolicy, remaining_days_for, status_for};
use chrono::{DateTime, Duration, Utc};
use sea_orm::entity::prelude::*;

/// 证书表。
#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq)]
#[sea_orm(table_name = "acmecast_cert")]
pub struct Model {
    /// 自增主键。
    #[sea_orm(primary_key)]
    pub id: i64,
    /// 规范化后的域名集合，用于去重：同一集合重复签发时更新而非新增。
    ///
    /// 这是**去重键**（[`encode_domains`] 的产物：小写、去重、字典序排序、逗号连接），
    /// 不是展示用的域名顺序。证书里 SAN 的原始顺序才是权威的申请顺序，
    /// 需要展示时应当从证书本身解析。
    pub domains: String,
    /// 证书 PEM 文件在数据目录下的相对路径。
    pub cert_pem_path: String,
    /// 私钥 PEM 文件在数据目录下的相对路径。
    pub key_pem_path: String,
    /// 证书 SHA-256 指纹，全局唯一。
    #[sea_orm(unique)]
    pub fingerprint: String,
    /// 签发者主题名（CA），供展示使用。
    pub issuer: Option<String>,
    /// 生效时间。
    pub not_before: chrono::DateTime<chrono::Utc>,
    /// 到期时间。
    pub not_after: chrono::DateTime<chrono::Utc>,
    /// 签发本次证书所用的 ACME 账号凭据标识。
    ///
    /// 为空表示来源不是 ACME 签发（例如用户手动上传），这类记录无法吊销。
    pub acme_account_access_id: Option<i64>,
    /// 吊销时间，非空表示已吊销。
    pub revoked_at: Option<chrono::DateTime<chrono::Utc>>,
    /// 记录创建时间。
    pub created_at: chrono::DateTime<chrono::Utc>,
    /// 记录更新时间。
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

/// 域名集合去重键的分隔符。域名本身不含逗号，因此可安全用作分隔符。
const DOMAIN_SEPARATOR: &str = ",";

/// 规范化域名集合：去首尾空白、转小写、去空项、去重、字典序排序。
///
/// 这一步是「集合」语义的落地：`["B.com", "a.com"]` 与 `["a.com", "b.com", "a.com"]`
/// 指向同一份证书需求，必须落到同一条记录上。排序保证了顺序无关，
/// 小写化则对应 DNS 的大小写不敏感。
#[must_use]
pub fn normalize_domains<S: AsRef<str>>(domains: &[S]) -> Vec<String> {
    let mut normalized: Vec<String> = domains
        .iter()
        .map(|domain| domain.as_ref().trim().to_lowercase())
        .filter(|domain| !domain.is_empty())
        .collect();

    normalized.sort_unstable();
    normalized.dedup();
    normalized
}

/// 把域名集合编码为去重键。空集合得到空字符串。
#[must_use]
pub fn encode_domains<S: AsRef<str>>(domains: &[S]) -> String {
    normalize_domains(domains).join(DOMAIN_SEPARATOR)
}

/// 从去重键解析回域名列表。空键得到空列表。
#[must_use]
pub fn decode_domains(encoded: &str) -> Vec<String> {
    encoded
        .split(DOMAIN_SEPARATOR)
        .filter(|domain| !domain.is_empty())
        .map(str::to_owned)
        .collect()
}

impl Model {
    /// 该记录覆盖的域名集合（解析自去重键，已排序去重）。
    #[must_use]
    pub fn domain_set(&self) -> Vec<String> {
        decode_domains(&self.domains)
    }

    /// 判定到期状态。
    ///
    /// 委托给 `acmecast_cert::status_for`——它是状态推导的唯一实现，
    /// 因此从库里读出的记录与刚从 PEM 解析出的证书判定结果必然一致。
    #[must_use]
    pub fn status_at(&self, now: DateTime<Utc>, policy: &ExpiryPolicy) -> CertStatus {
        status_for(self.not_after, now, policy)
    }

    /// 用默认策略（提前 30 天告警）判定到期状态。
    #[must_use]
    pub fn status(&self, now: DateTime<Utc>) -> CertStatus {
        status_for(self.not_after, now, &ExpiryPolicy::default())
    }

    /// 距到期的时长；已过期时为负值。
    #[must_use]
    pub fn remaining(&self, now: DateTime<Utc>) -> Duration {
        self.not_after - now
    }

    /// 距到期的整数天数，向下取整；已过期时为负值。
    ///
    /// 仅用于展示。状态判定请用 [`Model::status_at`]——它比较时间点，
    /// 不受天数截断影响。
    #[must_use]
    pub fn remaining_days(&self, now: DateTime<Utc>) -> i64 {
        remaining_days_for(self.not_after, now)
    }

    /// 是否已被吊销。
    #[must_use]
    pub fn is_revoked(&self) -> bool {
        self.revoked_at.is_some()
    }

    /// 是否可用于吊销——即记录了签发它的 ACME 账号。
    ///
    /// 手动上传的证书没有账号，无法吊销。
    #[must_use]
    pub fn is_revocable(&self) -> bool {
        self.acme_account_access_id.is_some() && !self.is_revoked()
    }

    /// 是否需要续期。
    ///
    /// 已吊销的证书不再需要续期，因此单独排除。
    #[must_use]
    pub fn needs_renewal(&self, now: DateTime<Utc>, policy: &ExpiryPolicy) -> bool {
        !self.is_revoked() && self.status_at(now, policy).needs_renewal()
    }
}

/// 证书与其他实体的关系。
#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    /// 可选地归属于签发它的 ACME 账号凭据。
    ///
    /// 之所以用 `def()` 而非 `has_one`：这是语义上的弱引用，
    /// 删除凭据时不应级联删除已签发的证书。
    #[sea_orm(
        belongs_to = "super::credential::Entity",
        from = "Column::AcmeAccountAccessId",
        to = "super::credential::Column::Id",
        on_update = "NoAction",
        on_delete = "NoAction"
    )]
    Credential,
}

impl Related<super::credential::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Credential.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}

#[cfg(test)]
mod tests {
    use super::*;

    /// 固定时间点，避免用例随真实时钟漂移。
    fn now() -> DateTime<Utc> {
        DateTime::from_timestamp(1_700_000_000, 0).unwrap()
    }

    /// 构造一条在 `not_after` 到期的证书记录。
    fn record_expiring_at(not_after: DateTime<Utc>) -> Model {
        Model {
            id: 1,
            domains: "example.com".to_owned(),
            cert_pem_path: "certs/1/cert.pem".to_owned(),
            key_pem_path: "certs/1/key.pem".to_owned(),
            fingerprint: "ab".repeat(32),
            issuer: Some("CN=Test CA".to_owned()),
            not_before: not_after - Duration::days(90),
            not_after,
            acme_account_access_id: Some(7),
            revoked_at: None,
            created_at: not_after - Duration::days(90),
            updated_at: not_after - Duration::days(90),
        }
    }

    #[test]
    fn status_is_identical_to_the_cert_crate() {
        // store 的推导只是委托，因此「库中读出的记录」与「刚从 PEM 解析出的证书」
        // 对同一到期时间必须给出相同结论。
        let policy = ExpiryPolicy::with_warn_days(30);
        for offset_days in [-10i64, -1, 0, 1, 15, 29, 30, 31, 89] {
            let not_after = now() + Duration::days(offset_days);
            let record = record_expiring_at(not_after);

            assert_eq!(
                record.status_at(now(), &policy),
                status_for(not_after, now(), &policy),
                "偏移 {offset_days} 天时与 cert crate 判定不一致"
            );
        }
    }

    #[test]
    fn barely_expired_record_is_expired() {
        // 与 cert crate 相同的陷阱：剩余天数为 0 不代表未过期。
        let record = record_expiring_at(now() - Duration::hours(1));
        assert_eq!(record.remaining_days(now()), 0);
        assert_eq!(record.status(now()), CertStatus::Expired);
    }

    #[test]
    fn healthy_record_does_not_need_renewal() {
        let policy = ExpiryPolicy::with_warn_days(30);
        let record = record_expiring_at(now() + Duration::days(89));
        assert_eq!(record.status(now()), CertStatus::Healthy);
        assert!(!record.needs_renewal(now(), &policy));
    }

    #[test]
    fn expiring_record_needs_renewal() {
        let policy = ExpiryPolicy::with_warn_days(30);
        let record = record_expiring_at(now() + Duration::days(10));
        assert!(record.needs_renewal(now(), &policy));
    }

    #[test]
    fn expired_record_needs_renewal() {
        let policy = ExpiryPolicy::with_warn_days(30);
        let record = record_expiring_at(now() - Duration::days(2));
        assert!(record.needs_renewal(now(), &policy));
    }

    #[test]
    fn revoked_record_never_needs_renewal() {
        // 已吊销的证书即使临近到期也不该再触发续期，否则会无限重签。
        let policy = ExpiryPolicy::with_warn_days(30);
        let mut record = record_expiring_at(now() + Duration::days(1));
        record.revoked_at = Some(now() - Duration::days(1));

        assert!(record.is_revoked());
        assert!(!record.needs_renewal(now(), &policy));
    }

    #[test]
    fn record_with_account_is_revocable() {
        let record = record_expiring_at(now() + Duration::days(89));
        assert!(record.is_revocable());
    }

    #[test]
    fn uploaded_record_without_account_is_not_revocable() {
        // 手动上传的证书没有签发账号，吊销时无法签名。
        let mut record = record_expiring_at(now() + Duration::days(89));
        record.acme_account_access_id = None;

        assert!(!record.is_revocable(), "没有账号的记录不应可吊销");
        assert!(!record.is_revoked());
    }

    #[test]
    fn already_revoked_record_is_not_revocable() {
        let mut record = record_expiring_at(now() + Duration::days(89));
        record.revoked_at = Some(now());
        assert!(!record.is_revocable(), "不应重复吊销");
    }

    #[test]
    fn remaining_reflects_sign_for_expired_records() {
        let record = record_expiring_at(now() - Duration::days(3));
        assert!(record.remaining(now()) < Duration::zero());
        assert_eq!(record.remaining_days(now()), -3);
    }

    #[test]
    fn default_policy_is_thirty_days() {
        let record = record_expiring_at(now() + Duration::days(29));
        // `status` 用默认策略，29 天应落在 30 天窗口内。
        assert_eq!(record.status(now()), CertStatus::ExpiringSoon);
    }

    // ---- 域名集合规范化 ----

    #[test]
    fn normalization_is_order_case_and_duplicate_insensitive() {
        // 三种写法描述的是同一个证书需求，必须得到同一个去重键。
        let a = encode_domains(&["b.example.com", "a.example.com"]);
        let b = encode_domains(&["a.example.com", "b.example.com"]);
        let c = encode_domains(&["  A.Example.COM ", "b.example.com", "b.example.com"]);

        assert_eq!(a, b, "顺序不应影响去重键");
        assert_eq!(a, c, "大小写、空白与重复项都应被规范化掉");
        assert_eq!(a, "a.example.com,b.example.com");
    }

    #[test]
    fn normalization_keeps_wildcard_and_base_domain_apart() {
        // 通配符与裸域名是两个不同的 SAN，不能被当成同一个。
        let both = encode_domains(&["example.com", "*.example.com"]);
        assert_eq!(both, "*.example.com,example.com");
        assert_ne!(both, encode_domains(&["example.com"]));
    }

    #[test]
    fn empty_domain_entries_are_dropped() {
        assert_eq!(encode_domains(&["", "  ", "example.com"]), "example.com");
        assert_eq!(encode_domains::<&str>(&[]), "");
    }

    #[test]
    fn encode_and_decode_round_trip() {
        let domains = vec!["*.example.com".to_owned(), "example.com".to_owned()];
        let decoded = decode_domains(&encode_domains(&domains));
        assert_eq!(decoded, normalize_domains(&domains));

        // 空键解析为空列表，而不是含一个空串的列表。
        assert!(decode_domains("").is_empty());
    }

    #[test]
    fn model_exposes_its_domain_set() {
        let mut record = record_expiring_at(now());
        record.domains = encode_domains(&["b.example.com", "a.example.com"]);
        assert_eq!(record.domain_set(), vec!["a.example.com", "b.example.com"]);
    }
}
