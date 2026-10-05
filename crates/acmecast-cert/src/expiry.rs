//! 证书到期状态判定。
//!
//! 关于实现上的一个坑：**状态判定必须比较时间点，不能比较天数**。
//! `chrono` 的 `num_days()` 向零截断，因此「已过期 1 小时」与「还有 1 小时到期」
//! 都会得到 `0` 天。若用 `remaining_days <= 0` 判过期，刚过期的证书会被误判为仍有效。

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

use crate::parse::CertificateInfo;

/// 证书相对当前时间的健康状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CertStatus {
    /// 距到期尚有余量。
    Healthy,
    /// 剩余有效期已进入告警窗口。
    ExpiringSoon,
    /// 已过期。
    Expired,
}

impl CertStatus {
    /// 是否已过期。
    #[must_use]
    pub fn is_expired(&self) -> bool {
        matches!(self, Self::Expired)
    }

    /// 是否需要续期（临近到期或已过期）。
    ///
    /// 到期扫描触发续期时用这个判断，而不是分别检查两个变体。
    #[must_use]
    pub fn needs_renewal(&self) -> bool {
        matches!(self, Self::Expired | Self::ExpiringSoon)
    }

    /// 与库中存储形态互转的字符串表示。
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Healthy => "healthy",
            Self::ExpiringSoon => "expiring_soon",
            Self::Expired => "expired",
        }
    }

    /// 解析字符串表示；未知取值返回 `None`。
    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "healthy" => Some(Self::Healthy),
            "expiring_soon" => Some(Self::ExpiringSoon),
            "expired" => Some(Self::Expired),
            _ => None,
        }
    }
}

/// 到期告警策略。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExpiryPolicy {
    /// 剩余有效期小于等于该天数时视为「临近到期」。
    ///
    /// 取「小于等于」而非「小于」：用户配置 30 天就是希望剩余 30 天时收到告警，
    /// 若用严格小于则恰好 30 天的那一刻不会告警，与直觉不符。
    pub warn_days: i64,
}

impl Default for ExpiryPolicy {
    fn default() -> Self {
        // 免费证书有效期 90 天，提前 30 天告警留出充足的重试窗口。
        Self { warn_days: 30 }
    }
}

impl ExpiryPolicy {
    /// 用指定告警天数构造。
    #[must_use]
    pub fn with_warn_days(warn_days: i64) -> Self {
        Self {
            warn_days: warn_days.max(0),
        }
    }

    /// 告警窗口时长。
    #[must_use]
    pub fn window(&self) -> Duration {
        Duration::days(self.warn_days)
    }
}

/// 按到期时间判定状态。
///
/// 这是状态推导的**唯一实现**——[`CertificateInfo::status_at`] 与
/// `acmecast_store::entity::cert::Model::status_at` 都委托到这里，
/// 使「解析出的证书」与「库中读出的证书记录」判定结果必然一致。
#[must_use]
pub fn status_for(
    not_after: DateTime<Utc>,
    now: DateTime<Utc>,
    policy: &ExpiryPolicy,
) -> CertStatus {
    // 用时间点比较，避免天数截断造成的误判。
    if now > not_after {
        return CertStatus::Expired;
    }
    if not_after <= now + policy.window() {
        return CertStatus::ExpiringSoon;
    }
    CertStatus::Healthy
}

/// 按到期时间计算剩余天数，向下取整；已过期时为负值。
#[must_use]
pub fn remaining_days_for(not_after: DateTime<Utc>, now: DateTime<Utc>) -> i64 {
    (not_after - now).num_days()
}

impl CertificateInfo {
    /// 距到期的时长；已过期时为负值。
    #[must_use]
    pub fn remaining(&self, now: DateTime<Utc>) -> Duration {
        self.not_after - now
    }

    /// 距到期的整数天数，向下取整；已过期时为负值。
    ///
    /// 仅用于展示。**不要用它做状态判定**——截断会让「刚过期」与
    /// 「即将到期」都得到 `0`，详见模块文档。
    #[must_use]
    pub fn remaining_days(&self, now: DateTime<Utc>) -> i64 {
        remaining_days_for(self.not_after, now)
    }

    /// 判定到期状态。
    #[must_use]
    pub fn status_at(&self, now: DateTime<Utc>, policy: &ExpiryPolicy) -> CertStatus {
        status_for(self.not_after, now, policy)
    }

    /// 用默认策略判定到期状态。
    #[must_use]
    pub fn status(&self, now: DateTime<Utc>) -> CertStatus {
        self.status_at(now, &ExpiryPolicy::default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 构造一张在 `not_after` 到期的证书，其余字段对本测试无意义。
    fn cert_expiring_at(not_after: DateTime<Utc>) -> CertificateInfo {
        CertificateInfo {
            domains: vec!["test.example.com".to_owned()],
            subject: "CN=test.example.com".to_owned(),
            issuer: "CN=Test CA".to_owned(),
            not_before: not_after - Duration::days(90),
            not_after,
            serial: "01".to_owned(),
            fingerprint_sha256: "00".repeat(32),
            is_ca: false,
        }
    }

    fn now() -> DateTime<Utc> {
        // 固定时间点，避免用例随真实时钟漂移。
        DateTime::from_timestamp(1_700_000_000, 0).unwrap()
    }

    // ---- 已过期判定 ----

    #[test]
    fn long_expired_is_expired() {
        let cert = cert_expiring_at(now() - Duration::days(10));
        assert_eq!(cert.status(now()), CertStatus::Expired);
    }

    #[test]
    fn barely_expired_is_still_expired() {
        // 关键边界：过期仅 1 小时时 `remaining_days()` 为 0，
        // 若用天数比较会被误判为「未过期」。
        let cert = cert_expiring_at(now() - Duration::hours(1));
        assert_eq!(
            cert.remaining_days(now()),
            0,
            "截断后剩余天数为 0——正因如此不能用天数判定"
        );
        assert_eq!(
            cert.status(now()),
            CertStatus::Expired,
            "刚过期 1 小时必须判为已过期"
        );
    }

    #[test]
    fn one_second_after_expiry_is_expired() {
        let cert = cert_expiring_at(now() - Duration::seconds(1));
        assert_eq!(cert.status(now()), CertStatus::Expired);
    }

    #[test]
    fn exactly_at_expiry_instant_is_not_yet_expired() {
        // narrow boundary：now == not_after 的瞬间还未过期。
        let cert = cert_expiring_at(now());
        assert_eq!(cert.status(now()), CertStatus::ExpiringSoon);
    }

    // ---- 临近到期判定 ----

    #[test]
    fn one_hour_before_expiry_is_expiring_soon() {
        let cert = cert_expiring_at(now() + Duration::hours(1));
        assert_eq!(cert.remaining_days(now()), 0);
        assert_eq!(cert.status(now()), CertStatus::ExpiringSoon);
    }

    #[test]
    fn exactly_at_threshold_is_expiring_soon() {
        // 策略说「剩余不足 30 天告警」，恰好 30 天时应告警。
        let policy = ExpiryPolicy::with_warn_days(30);
        let cert = cert_expiring_at(now() + Duration::days(30));
        assert_eq!(cert.status_at(now(), &policy), CertStatus::ExpiringSoon);
    }

    #[test]
    fn one_second_beyond_threshold_is_healthy() {
        let policy = ExpiryPolicy::with_warn_days(30);
        let cert = cert_expiring_at(now() + Duration::days(30) + Duration::seconds(1));
        assert_eq!(cert.status_at(now(), &policy), CertStatus::Healthy);
    }

    #[test]
    fn comfortably_far_out_is_healthy() {
        let cert = cert_expiring_at(now() + Duration::days(89));
        assert_eq!(cert.status(now()), CertStatus::Healthy);
    }

    #[test]
    fn threshold_boundaries_are_contiguous() {
        // 三个区间必须无缝衔接：不存在既非 Healthy 又非 ExpiringSoon 的缝隙。
        let policy = ExpiryPolicy::with_warn_days(7);
        for offset_hours in [0i64, 1, 23, 24, 25, 167, 168, 169, 200] {
            let cert = cert_expiring_at(now() + Duration::hours(offset_hours));
            let status = cert.status_at(now(), &policy);
            let expected = if Duration::hours(offset_hours) <= Duration::days(7) {
                CertStatus::ExpiringSoon
            } else {
                CertStatus::Healthy
            };
            assert_eq!(status, expected, "偏移 {offset_hours} 小时时判定错误");
        }
    }

    // ---- 策略配置 ----

    #[test]
    fn zero_warn_days_only_flags_already_expiring() {
        let policy = ExpiryPolicy::with_warn_days(0);
        // 还有 1 秒 → 仍算健康（窗口为 0）。
        let cert = cert_expiring_at(now() + Duration::seconds(1));
        assert_eq!(cert.status_at(now(), &policy), CertStatus::Healthy);
        // 已过期 → 过期。
        let cert = cert_expiring_at(now() - Duration::seconds(1));
        assert_eq!(cert.status_at(now(), &policy), CertStatus::Expired);
    }

    #[test]
    fn negative_warn_days_is_clamped_to_zero() {
        let policy = ExpiryPolicy::with_warn_days(-5);
        assert_eq!(policy.warn_days, 0);
        assert_eq!(policy.window(), Duration::zero());
    }

    #[test]
    fn default_policy_warns_thirty_days_ahead() {
        let policy = ExpiryPolicy::default();
        assert_eq!(policy.warn_days, 30);
        assert_eq!(policy.window(), Duration::days(30));
    }

    #[test]
    fn longer_warn_window_flags_more_certificates() {
        let cert = cert_expiring_at(now() + Duration::days(45));
        assert_eq!(
            cert.status_at(now(), &ExpiryPolicy::with_warn_days(30)),
            CertStatus::Healthy
        );
        assert_eq!(
            cert.status_at(now(), &ExpiryPolicy::with_warn_days(60)),
            CertStatus::ExpiringSoon,
            "放宽窗口后同一张证书应进入告警"
        );
    }

    // ---- 剩余时长与天数 ----

    #[test]
    fn remaining_is_negative_after_expiry() {
        let cert = cert_expiring_at(now() - Duration::days(3));
        assert!(cert.remaining(now()) < Duration::zero());
        assert_eq!(cert.remaining_days(now()), -3);
    }

    #[test]
    fn remaining_days_truncates_toward_zero() {
        let cert = cert_expiring_at(now() + Duration::days(29) + Duration::hours(23));
        assert_eq!(cert.remaining_days(now()), 29, "不足一天应向下取整");
    }

    // ---- 状态自身的语义 ----

    #[test]
    fn needs_renewal_covers_both_warning_and_expired() {
        assert!(CertStatus::ExpiringSoon.needs_renewal());
        assert!(CertStatus::Expired.needs_renewal());
        assert!(!CertStatus::Healthy.needs_renewal());
    }

    #[test]
    fn only_expired_reports_is_expired() {
        assert!(CertStatus::Expired.is_expired());
        assert!(!CertStatus::ExpiringSoon.is_expired());
        assert!(!CertStatus::Healthy.is_expired());
    }

    #[test]
    fn status_roundtrips_through_string() {
        for status in [
            CertStatus::Healthy,
            CertStatus::ExpiringSoon,
            CertStatus::Expired,
        ] {
            assert_eq!(CertStatus::parse(status.as_str()), Some(status));
        }
        assert_eq!(CertStatus::parse("unknown"), None);
    }
}
