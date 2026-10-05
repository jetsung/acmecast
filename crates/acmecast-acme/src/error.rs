//! ACME 错误类型。
//!
//! 把 [`instant_acme::Error`] 翻译成项目自己的错误，使上层不必引入底层库。

/// ACME 相关错误。
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum AcmeError {
    /// 无法把 CA 标识解析为 Directory URL，例如自定义 CA 未提供 URL。
    #[error("无法解析 ACME Directory URL: {0}")]
    Directory(String),

    /// 未知的 CA 别名。
    #[error("未知的 CA 标识 `{0}`，可用值: {available}", available = .1.join(", "))]
    UnknownCa(String, Vec<&'static str>),

    /// CA 返回的 RFC 7807 problem 文档。
    ///
    /// 保留 URN 类型串，使上层能据 `urn:ietf:params:acme:error:badNonce`
    /// 等标识做针对性处理。
    #[error("ACME 服务返回错误 [{kind}]: {detail}")]
    Problem {
        /// RFC 8555 §6.7 的错误类型 URN；CA 未给出时为 `unknown`。
        kind: String,
        /// 人类可读的说明。
        detail: String,
        /// 是否由 badNonce 引起（此类错误可安全重试）。
        is_bad_nonce: bool,
        /// 是否表示 CA 要求 EAB 凭据。
        requires_external_account: bool,
    },

    /// 账号操作失败：注册、加载、恢复。
    #[error("账号操作失败: {0}")]
    Account(String),

    /// 订单操作失败。
    #[error("订单操作失败: {0}")]
    Order(String),

    /// 挑战校验失败或超时。
    #[error("挑战校验失败: {0}")]
    Challenge(String),

    /// 等待 CA 确认订单就绪超时。
    #[error("等待订单就绪超时: {0}")]
    Timeout(String),

    /// 吊销失败。
    #[error("吊销失败: {0}")]
    Revocation(String),

    /// 密码学操作失败（生成密钥、构建 CSR 等）。
    #[error("密码学操作失败: {0}")]
    Crypto(String),

    /// 网络层失败，含代理配置错误。
    #[error("网络请求失败: {0}")]
    Transport(String),

    /// 底层库返回的其他错误。
    #[error("ACME 底层错误: {0}")]
    Other(String),
}

/// 本 crate 统一的返回类型别名。
pub type Result<T> = std::result::Result<T, AcmeError>;

/// RFC 8555 §6.7 定义的错误 URN 前缀。
const ACME_ERROR_PREFIX: &str = "urn:ietf:params:acme:error:";

impl AcmeError {
    /// 从 CA 返回的 problem 文档构造错误。
    ///
    /// `type` 字段形如 `urn:ietf:params:acme:error:badNonce`；
    /// 此函数据此判断该错误是否可安全重试、是否表示缺少 EAB。
    #[must_use]
    pub fn from_problem(r#type: Option<String>, detail: Option<String>) -> Self {
        let urn = r#type.as_deref().unwrap_or_default();

        let is_bad_nonce = urn
            .strip_prefix(ACME_ERROR_PREFIX)
            .is_some_and(|kind| kind == "badNonce");

        let requires_external_account = urn
            .strip_prefix(ACME_ERROR_PREFIX)
            .is_some_and(|kind| kind == "externalAccountRequired");

        Self::Problem {
            kind: r#type.unwrap_or_else(|| "unknown".to_owned()),
            detail: detail.unwrap_or_else(|| "CA 未给出具体原因".to_owned()),
            is_bad_nonce,
            requires_external_account,
        }
    }

    /// 该错误是否可安全重试（目前仅 badNonce）。
    #[must_use]
    pub fn is_retryable(&self) -> bool {
        matches!(self, Self::Problem { is_bad_nonce, .. } if *is_bad_nonce)
    }

    /// 该错误是否表示 CA 要求提供 EAB 凭据。
    #[must_use]
    pub fn requires_external_account(&self) -> bool {
        matches!(self, Self::Problem { requires_external_account, .. } if *requires_external_account)
    }
}

impl From<instant_acme::Error> for AcmeError {
    fn from(err: instant_acme::Error) -> Self {
        match err {
            // Problem 的 `type` / `detail` 直接决定上层能否针对性处理。
            instant_acme::Error::Api(problem) => Self::from_problem(problem.r#type, problem.detail),
            instant_acme::Error::Crypto => Self::Crypto("底层库密码学操作失败".to_owned()),
            instant_acme::Error::KeyRejected => Self::Crypto("私钥字节不被接受".to_owned()),
            instant_acme::Error::Timeout(next_poll) => Self::Timeout(match next_poll {
                Some(_) => "CA 建议稍后再次轮询".to_owned(),
                None => "已超出最大等待时间".to_owned(),
            }),
            instant_acme::Error::Unsupported(feature) => {
                Self::Other(format!("CA 不支持该特性: {feature}"))
            }
            other => Self::Other(other.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bad_nonce_is_detected_and_retryable() {
        let err = AcmeError::from_problem(
            Some("urn:ietf:params:acme:error:badNonce".to_owned()),
            Some("JWS has an invalid anti-replay nonce".to_owned()),
        );
        assert!(err.is_retryable());
        assert!(!err.requires_external_account());
        assert!(err.to_string().contains("nonce"));
    }

    #[test]
    fn external_account_required_is_detected() {
        let err = AcmeError::from_problem(
            Some("urn:ietf:params:acme:error:externalAccountRequired".to_owned()),
            Some("no external account binding".to_owned()),
        );
        assert!(err.requires_external_account());
        assert!(!err.is_retryable());
    }

    #[test]
    fn other_problems_are_not_retryable() {
        let err = AcmeError::from_problem(
            Some("urn:ietf:params:acme:error:unauthorized".to_owned()),
            Some("invalid response".to_owned()),
        );
        assert!(!err.is_retryable());
        assert!(!err.requires_external_account());
    }

    #[test]
    fn missing_detail_falls_back_to_generic_text() {
        let err = AcmeError::from_problem(None, None);
        assert!(err.to_string().contains("未给出具体原因"));
    }

    #[test]
    fn non_problem_errors_are_not_retryable() {
        assert!(!AcmeError::Timeout("x".into()).is_retryable());
        assert!(!AcmeError::Order("x".into()).is_retryable());
    }

    #[test]
    fn error_urn_prefix_is_stripped_for_comparison() {
        // 前缀必须完整匹配，避免 `myerror:badNonce` 被误判。
        let err = AcmeError::from_problem(Some("myerror:badNonce".to_owned()), None);
        assert!(!err.is_retryable());
    }
}
