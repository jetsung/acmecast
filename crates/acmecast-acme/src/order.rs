//! 订单与授权：把 CA 侧的订单/授权/挑战映射为项目自己的类型。
//!
//! 底层库的类型不出现在本模块的公开签名里。

use std::time::Duration;

use instant_acme::{
    AuthorizationStatus, Challenge, ChallengeStatus, ChallengeType, Order, Problem, RetryPolicy,
};

use crate::error::{AcmeError, Result};

/// 挑战类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChallengeKind {
    /// HTTP-01：需要在 `/.well-known/acme-challenge/{token}` 投放授权串。
    Http01,
    /// DNS-01：需要写入 `_acme-challenge.{domain}` 的 TXT 记录。
    Dns01,
    /// TLS-ALPN-01：本项目首版不使用。
    TlsAlpn01,
}

impl ChallengeKind {
    /// 与 RFC 8555 §9 中挑战名一致的字符串表示。
    #[must_use]
    pub fn as_acme_name(&self) -> &'static str {
        match self {
            Self::Http01 => "http-01",
            Self::Dns01 => "dns-01",
            Self::TlsAlpn01 => "tls-alpn-01",
        }
    }
}

/// 授权状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthorizationStateKind {
    /// 待验证。
    Pending,
    /// 已验证通过，可跳过挑战直接复用。
    Valid,
    /// 验证失败。
    Invalid,
    /// 已吊销。
    Revoked,
    /// 已过期。
    Expired,
    /// 已停用。
    Deactivated,
}

/// 一个挑战的基本信息。
///
/// 只包含**无需账号密钥**即可获得的内容。键值授权串与 DNS TXT 值需要账号密钥
/// 参与计算，须通过 [`PendingOrder::challenge_materials`] 获取。
#[derive(Debug, Clone)]
pub struct ChallengeInfo {
    /// 挑战类型。
    pub kind: ChallengeKind,
    /// ACME 令牌。
    pub token: String,
    /// HTTP-01 需要投放的路径。
    pub http_path: String,
}

/// 挑战的应答材料：调用方拿去投放的实际内容。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChallengeMaterials {
    /// 键值授权串（`{token}.{账号密钥指纹}`）。
    ///
    /// HTTP-01 直接把它作为 `/.well-known/acme-challenge/{token}` 的响应体。
    pub key_authorization: String,
    /// DNS-01 需要写入 `_acme-challenge.{域名}` 的 TXT 记录值，
    /// 即键值授权串的 SHA-256 摘要的 base64url（无填充）编码。
    pub dns_txt_value: String,
}

impl ChallengeMaterials {
    /// HTTP-01 的投放路径。
    #[must_use]
    pub fn http_path(token: &str) -> String {
        ChallengeInfo::well_known_path(token)
    }
}

impl ChallengeInfo {
    /// HTTP-01 的资源路径。
    #[must_use]
    pub fn well_known_path(token: &str) -> String {
        format!("/.well-known/acme-challenge/{token}")
    }

    /// DNS-01 的记录名（相对域名部分）。
    #[must_use]
    pub fn dns_record_name(domain_without_wildcard: &str) -> String {
        format!("_acme-challenge.{domain_without_wildcard}")
    }
}

/// 一个域名标识符对应的授权。
#[derive(Debug, Clone)]
pub struct AuthorizationInfo {
    /// 标识符显示名；通配符形如 `*.example.com`。
    pub identifier: String,
    /// 是否为通配符。
    pub wildcard: bool,
    /// 当前状态。
    pub state: AuthorizationStateKind,
    /// 可用挑战。
    pub challenges: Vec<ChallengeInfo>,
}

impl AuthorizationInfo {
    /// 该授权是否已无需再验证。
    #[must_use]
    pub fn is_valid(&self) -> bool {
        matches!(self.state, AuthorizationStateKind::Valid)
    }

    /// 取得指定类型的挑战。
    #[must_use]
    pub fn challenge(&self, kind: ChallengeKind) -> Option<&ChallengeInfo> {
        self.challenges.iter().find(|c| c.kind == kind)
    }
}

/// 进行中的订单。
pub struct PendingOrder {
    inner: Order,
    /// 等待 CA 确认时的重试策略。
    retry: RetryPolicy,
}

impl std::fmt::Debug for PendingOrder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PendingOrder")
            .field("url", &self.url())
            .finish_non_exhaustive()
    }
}

impl PendingOrder {
    /// 用底层订单与重试策略构造。
    pub(crate) fn new(inner: Order, retry: RetryPolicy) -> Self {
        Self { inner, retry }
    }

    /// 拉取全部授权信息。
    ///
    /// 已处于 valid 的授权会原样返回，调用方据 [`AuthorizationInfo::is_valid`]
    /// 决定是否跳过挑战。
    pub async fn authorizations(&mut self) -> Result<Vec<AuthorizationInfo>> {
        let mut stream = self.inner.authorizations();
        let mut out = Vec::new();

        while let Some(mut handle) = stream
            .next()
            .await
            .transpose()
            .map_err(|e| AcmeError::Order(format!("读取授权失败: {e}")))?
        {
            // `AuthorizationState` 不可 Clone，且 `refresh()` 返回的是借用，
            // 因此必须在借用期内把需要的字段全部提取成自有数据。
            let auth_state = handle
                .refresh()
                .await
                .map_err(|e| AcmeError::Order(format!("读取授权状态失败: {e}")))?;
            let identifier = auth_state.identifier().to_string();
            let wildcard = auth_state.wildcard;
            let status = map_authorization_status(auth_state.status);
            let challenges: Vec<ChallengeInfo> = auth_state
                .challenges
                .iter()
                .filter_map(map_challenge)
                .collect();

            out.push(AuthorizationInfo {
                identifier,
                wildcard,
                state: status,
                challenges,
            });
        }

        Ok(out)
    }

    /// 通知 CA 某域名的指定挑战已就绪。
    ///
    /// 内部会重新遍历授权以取得可变的挑战句柄——底层库的授权是流式借用，
    /// 无法跨 await 长期持有。
    pub async fn set_challenge_ready(
        &mut self,
        identifier: &str,
        kind: ChallengeKind,
    ) -> Result<()> {
        let mut stream = self.inner.authorizations();
        while let Some(mut handle) = stream
            .next()
            .await
            .transpose()
            .map_err(|e| AcmeError::Challenge(format!("读取授权失败: {e}")))?
        {
            let current = handle
                .refresh()
                .await
                .map_err(|e| AcmeError::Challenge(format!("读取授权状态失败: {e}")))?
                .identifier()
                .to_string();
            if current != identifier {
                continue;
            }
            let Some(mut challenge) = handle.challenge(to_challenge_type(kind)) else {
                return Err(AcmeError::Challenge(format!(
                    "域名 `{identifier}` 不支持 {} 挑战",
                    kind.as_acme_name()
                )));
            };
            challenge.set_ready().await.map_err(|e| {
                AcmeError::Challenge(format!("通知 CA 校验 `{identifier}` 失败: {e}"))
            })?;
            return Ok(());
        }

        Err(AcmeError::Challenge(format!(
            "订单中找不到域名 `{identifier}` 对应的授权"
        )))
    }

    /// 取得挑战的应答材料。
    ///
    /// 键值授权串 = `{token}.{账号密钥指纹}`，其 SHA-256 摘要的 base64url 编码
    /// 即 DNS-01 需要写入的 TXT 值。两者都需要账号密钥参与，因此只能在
    /// 持有账号的 [`PendingOrder`] 上计算。
    pub async fn challenge_materials(
        &mut self,
        identifier: &str,
        kind: ChallengeKind,
    ) -> Result<ChallengeMaterials> {
        let mut stream = self.inner.authorizations();
        while let Some(mut handle) = stream
            .next()
            .await
            .transpose()
            .map_err(|e| AcmeError::Challenge(format!("读取授权失败: {e}")))?
        {
            let current = handle
                .refresh()
                .await
                .map_err(|e| AcmeError::Challenge(format!("读取授权状态失败: {e}")))?
                .identifier()
                .to_string();
            if current != identifier {
                continue;
            }

            let Some(challenge) = handle.challenge(to_challenge_type(kind)) else {
                return Err(AcmeError::Challenge(format!(
                    "域名 `{identifier}` 不支持 {} 挑战",
                    kind.as_acme_name()
                )));
            };

            let key_authorization = challenge.key_authorization();
            return Ok(ChallengeMaterials {
                key_authorization: key_authorization.as_str().to_owned(),
                dns_txt_value: key_authorization.dns_value(),
            });
        }

        Err(AcmeError::Challenge(format!(
            "订单中找不到域名 `{identifier}` 对应的授权"
        )))
    }

    /// 提交 CSR 并等待订单就绪，然后下载证书链。
    ///
    /// `processing` 状态下会持续轮询，不重复提交 CSR。
    pub async fn finalize_and_download(&mut self, csr_der: &[u8]) -> Result<String> {
        self.inner
            .finalize_csr(csr_der)
            .await
            .map_err(|e| AcmeError::Order(format!("提交 CSR 失败: {e}")))?;

        self.inner
            .poll_certificate(&self.retry)
            .await
            .map_err(|e| match AcmeError::from(e) {
                AcmeError::Timeout(detail) => {
                    AcmeError::Timeout(format!("等待证书签发超时: {detail}"))
                }
                other => AcmeError::Order(format!("下载证书失败: {other}")),
            })
    }

    /// 订单当前状态。
    pub async fn refresh(&mut self) -> Result<OrderStatus> {
        let state = self
            .inner
            .refresh()
            .await
            .map_err(|e| AcmeError::Order(format!("刷新订单状态失败: {e}")))?;
        Ok(map_order_status(state.status))
    }

    /// CA 拒绝本次验证时给出的原因。
    ///
    /// 订单变 `invalid` 只说明「失败了」；真正的原因（如 DNS 跟随 CNAME 后
    /// 取不到 TXT、限速、账户问题）藏在订单的 problem 文档和各挑战的 error
    /// 里，不捞出来运维就只能对着「订单状态 invalid」猜。
    /// 尽力而为：任何一步取不到都跳过，全空时给占位文案，不掩盖原始错误。
    pub async fn rejection_reason(&mut self) -> String {
        let mut reasons: Vec<String> = Vec::new();

        if let Ok(state) = self.inner.refresh().await
            && let Some(problem) = &state.error
        {
            reasons.push(problem_text(problem));
        }

        let mut stream = self.inner.authorizations();
        while let Some(Ok(mut handle)) = stream.next().await {
            let Ok(state) = handle.refresh().await else {
                continue;
            };
            let identifier = state.identifier().to_string();
            for challenge in &state.challenges {
                if let Some(problem) = &challenge.error {
                    reasons.push(format!(
                        "`{identifier}` 的 {} 挑战: {}",
                        challenge_type_name(&challenge.r#type),
                        problem_text(problem)
                    ));
                }
            }
        }

        if reasons.is_empty() {
            "CA 未给出具体原因".to_owned()
        } else {
            reasons.join("；")
        }
    }

    /// 订单 URL，用于排障与续期关联。
    #[must_use]
    pub fn url(&self) -> &str {
        self.inner.url()
    }
}

/// 订单状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrderStatus {
    /// 等待域名验证。
    Pending,
    /// 验证通过、等待签发。
    Ready,
    /// CA 处理中。
    Processing,
    /// 已签发。
    Valid,
    /// 失败。
    Invalid,
}

/// 构造等待 CA 确认的重试策略。
///
/// 指数退避，总时长由 `timeout` 封顶，避免把单个域名的卡顿拖垮整条流水线。
///
/// 返回底层类型，仅限 crate 内部使用——对外由 [`crate::service::RetryConfig`] 表达。
#[must_use]
pub(crate) fn retry_policy(
    initial_delay: Duration,
    backoff: f32,
    timeout: Duration,
) -> RetryPolicy {
    RetryPolicy::new()
        .initial_delay(initial_delay)
        .backoff(backoff)
        .timeout(timeout)
}

/// 把 CA 的 problem 文档压成一行可读文本（`Display` 的前缀是「API error」，
/// 对日志没有信息量，这里只留 detail、类型与子问题）。
fn problem_text(problem: &Problem) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(detail) = &problem.detail {
        parts.push(detail.clone());
    }
    if let Some(kind) = &problem.r#type {
        parts.push(format!("({kind})"));
    }
    for sub in &problem.subproblems {
        parts.push(sub.to_string());
    }
    if parts.is_empty() {
        "CA 未给出原因".to_owned()
    } else {
        parts.join(" ")
    }
}

fn challenge_type_name(kind: &ChallengeType) -> String {
    match kind {
        ChallengeType::Http01 => "http-01".to_owned(),
        ChallengeType::Dns01 => "dns-01".to_owned(),
        ChallengeType::TlsAlpn01 => "tls-alpn-01".to_owned(),
        ChallengeType::DeviceAttest01 => "device-attest-01".to_owned(),
        ChallengeType::Unknown(name) => name.clone(),
        _ => "未知挑战".to_owned(),
    }
}

/// 把底层挑战映射为项目类型；不认识的挑战类型被跳过而非报错。
fn map_challenge(challenge: &Challenge) -> Option<ChallengeInfo> {
    let kind = match challenge.r#type {
        ChallengeType::Http01 => ChallengeKind::Http01,
        ChallengeType::Dns01 => ChallengeKind::Dns01,
        ChallengeType::TlsAlpn01 => ChallengeKind::TlsAlpn01,
        _ => return None,
    };

    Some(ChallengeInfo {
        kind,
        token: challenge.token.clone(),
        http_path: ChallengeInfo::well_known_path(&challenge.token),
    })
}

/// 把项目挑战类型转回底层类型。
fn to_challenge_type(kind: ChallengeKind) -> ChallengeType {
    match kind {
        ChallengeKind::Http01 => ChallengeType::Http01,
        ChallengeKind::Dns01 => ChallengeType::Dns01,
        ChallengeKind::TlsAlpn01 => ChallengeType::TlsAlpn01,
    }
}

fn map_authorization_status(status: AuthorizationStatus) -> AuthorizationStateKind {
    match status {
        AuthorizationStatus::Pending => AuthorizationStateKind::Pending,
        AuthorizationStatus::Valid => AuthorizationStateKind::Valid,
        AuthorizationStatus::Invalid => AuthorizationStateKind::Invalid,
        AuthorizationStatus::Revoked => AuthorizationStateKind::Revoked,
        AuthorizationStatus::Expired => AuthorizationStateKind::Expired,
        AuthorizationStatus::Deactivated => AuthorizationStateKind::Deactivated,
    }
}

fn map_order_status(status: instant_acme::OrderStatus) -> OrderStatus {
    match status {
        instant_acme::OrderStatus::Pending => OrderStatus::Pending,
        instant_acme::OrderStatus::Ready => OrderStatus::Ready,
        instant_acme::OrderStatus::Processing => OrderStatus::Processing,
        instant_acme::OrderStatus::Valid => OrderStatus::Valid,
        instant_acme::OrderStatus::Invalid => OrderStatus::Invalid,
    }
}

/// 把底层挑战状态映射为项目类型，供上层判断。
#[must_use]
pub fn map_challenge_status(status: ChallengeStatus) -> &'static str {
    match status {
        ChallengeStatus::Pending => "pending",
        ChallengeStatus::Processing => "processing",
        ChallengeStatus::Valid => "valid",
        ChallengeStatus::Invalid => "invalid",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn challenge_kind_names_match_rfc() {
        assert_eq!(ChallengeKind::Http01.as_acme_name(), "http-01");
        assert_eq!(ChallengeKind::Dns01.as_acme_name(), "dns-01");
        assert_eq!(ChallengeKind::TlsAlpn01.as_acme_name(), "tls-alpn-01");
    }

    #[test]
    fn well_known_path_is_standard() {
        assert_eq!(
            ChallengeInfo::well_known_path("abc123"),
            "/.well-known/acme-challenge/abc123"
        );
    }

    #[test]
    fn dns_record_name_has_acme_challenge_prefix() {
        assert_eq!(
            ChallengeInfo::dns_record_name("example.com"),
            "_acme-challenge.example.com"
        );
    }

    #[test]
    fn valid_authorization_is_skippable() {
        let info = AuthorizationInfo {
            identifier: "example.com".to_owned(),
            wildcard: false,
            state: AuthorizationStateKind::Valid,
            challenges: vec![],
        };
        assert!(info.is_valid());
    }

    #[test]
    fn pending_authorization_needs_challenge() {
        let info = AuthorizationInfo {
            identifier: "example.com".to_owned(),
            wildcard: false,
            state: AuthorizationStateKind::Pending,
            challenges: vec![],
        };
        assert!(!info.is_valid());
    }

    #[test]
    fn challenge_lookup_by_kind() {
        let dns = ChallengeInfo {
            kind: ChallengeKind::Dns01,
            token: "t".to_owned(),
            http_path: String::new(),
        };
        let info = AuthorizationInfo {
            identifier: "example.com".to_owned(),
            wildcard: false,
            state: AuthorizationStateKind::Pending,
            challenges: vec![dns],
        };
        assert!(info.challenge(ChallengeKind::Dns01).is_some());
        assert!(info.challenge(ChallengeKind::Http01).is_none());
    }

    #[test]
    fn unknown_challenge_types_are_skipped_not_fatal() {
        // 构造一个未知类型（`Unknown` 变体），应被过滤掉。
        let challenge = Challenge {
            r#type: ChallengeType::Unknown("weird-01".to_owned()),
            url: "https://ca/chall/1".to_owned(),
            token: "tok".to_owned(),
            status: ChallengeStatus::Pending,
            error: None,
        };
        assert!(
            map_challenge(&challenge).is_none(),
            "未知挑战类型应被跳过而非中止整个订单"
        );
    }

    #[test]
    fn known_challenge_types_are_mapped() {
        for (raw, want) in [
            (ChallengeType::Http01, ChallengeKind::Http01),
            (ChallengeType::Dns01, ChallengeKind::Dns01),
            (ChallengeType::TlsAlpn01, ChallengeKind::TlsAlpn01),
        ] {
            let challenge = Challenge {
                r#type: raw,
                url: "https://ca/chall/1".to_owned(),
                token: "tok".to_owned(),
                status: ChallengeStatus::Pending,
                error: None,
            };
            assert_eq!(map_challenge(&challenge).unwrap().kind, want);
        }
    }

    #[test]
    fn challenge_kind_roundtrips_to_acme_type() {
        assert!(matches!(
            to_challenge_type(ChallengeKind::Dns01),
            ChallengeType::Dns01
        ));
        assert!(matches!(
            to_challenge_type(ChallengeKind::Http01),
            ChallengeType::Http01
        ));
    }

    #[test]
    fn retry_policy_is_configurable() {
        // 超时很短的用例在测试里能快速失败。
        let policy = retry_policy(Duration::from_millis(10), 1.5, Duration::from_millis(50));
        let _ = policy;
    }

    #[test]
    fn problem_text_prefers_detail_and_keeps_type() {
        let problem = Problem {
            r#type: Some("urn:ietf:params:acme:error:dns".to_owned()),
            detail: Some(
                "DNS problem: NXDOMAIN looking up TXT for _acme-challenge.example.com".to_owned(),
            ),
            status: Some(400),
            subproblems: vec![],
        };
        let text = problem_text(&problem);
        assert!(text.contains("NXDOMAIN"), "{text}");
        assert!(text.contains("urn:ietf:params:acme:error:dns"), "{text}");
        assert!(!text.contains("API error"), "{text}");
    }

    #[test]
    fn empty_problem_still_gives_a_placeholder() {
        let problem = Problem {
            r#type: None,
            detail: None,
            status: None,
            subproblems: vec![],
        };
        assert_eq!(problem_text(&problem), "CA 未给出原因");
    }

    #[test]
    fn challenge_type_names_are_stable() {
        assert_eq!(challenge_type_name(&ChallengeType::Dns01), "dns-01");
        assert_eq!(
            challenge_type_name(&ChallengeType::Unknown("weird-01".to_owned())),
            "weird-01"
        );
    }

    #[test]
    fn authorization_status_mapping_is_total() {
        for (raw, want) in [
            (
                AuthorizationStatus::Pending,
                AuthorizationStateKind::Pending,
            ),
            (AuthorizationStatus::Valid, AuthorizationStateKind::Valid),
            (
                AuthorizationStatus::Invalid,
                AuthorizationStateKind::Invalid,
            ),
            (
                AuthorizationStatus::Expired,
                AuthorizationStateKind::Expired,
            ),
        ] {
            assert_eq!(map_authorization_status(raw), want);
        }
    }
}
