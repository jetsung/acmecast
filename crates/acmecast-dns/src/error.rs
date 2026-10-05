//! DNS 挑战错误类型。

/// `acmecast-dns` 的错误类型。
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// 引用了一个未注册的 DNS 提供商类型。
    ///
    /// 附带已注册的清单：这个错误几乎只在配置写错时出现，
    /// 而「到底有哪些可用」正是此时第一个要问的问题。
    #[error("未知的 DNS 提供商类型: {type_id}（已注册: {}）", known.join(", "))]
    UnknownProviderType {
        /// 请求的类型标识。
        type_id: String,
        /// 当前已注册的类型标识。
        known: Vec<String>,
    },

    /// 同一个类型标识被注册了两次。
    #[error("DNS 提供商类型重复注册: {0}")]
    DuplicateProviderType(String),

    /// 凭据内容不满足提供商的要求。
    #[error("DNS 凭据不合法：字段 `{field}` {reason}")]
    InvalidCredentials {
        /// 出错的字段名。
        field: String,
        /// 不合法的原因。
        reason: String,
    },

    /// 挑战类型与域名不相容。
    ///
    /// 最典型的是「通配符 + HTTP-01」：与其等 CA 返回一个含糊的错误，
    /// 不如在动任何东西之前就说清楚。
    #[error("挑战类型不适用：{0}")]
    UnsupportedChallenge(String),

    /// 与 DNS 服务交互失败。
    ///
    /// 说明必须**已脱敏**：厂商的报错常把请求里的密钥原样回显。
    #[error("调用 DNS 服务失败: {0}")]
    Provider(String),

    /// 上游 core 层的错误。
    #[error(transparent)]
    Core(#[from] acmecast_core::Error),
}

impl Error {
    /// 构造一条「凭据字段不合法」。
    pub fn invalid_credentials(field: impl Into<String>, reason: impl Into<String>) -> Self {
        Self::InvalidCredentials {
            field: field.into(),
            reason: reason.into(),
        }
    }

    /// 构造一条与厂商交互有关的错误。
    pub fn provider(reason: impl Into<String>) -> Self {
        Self::Provider(reason.into())
    }
}

/// 本 crate 统一的返回类型别名。
pub type Result<T> = std::result::Result<T, Error>;
