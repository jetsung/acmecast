//! 全局错误类型。
//!
//! 各上层 crate 定义自己的错误枚举并用 `#[from]` 收敛到 [`Error`]，
//! 使得 `acmecast-server` 只需处理一种错误即可生成统一的 HTTP 响应。

/// 所有 acmecast 组件共享的错误类型。
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// 配置项缺失或非法。
    #[error("配置错误: {0}")]
    Config(String),

    /// 凭据解密失败：密文被篡改、密钥不对或格式不符。
    #[error("凭据解密失败: {0}")]
    Decryption(String),

    /// 加解密以外的密码学操作失败，例如生成密钥材料。
    #[error("密码学操作失败: {0}")]
    Crypto(String),

    /// 输入输出与文件系统错误。
    #[error("IO 错误: {0}")]
    Io(#[from] std::io::Error),

    /// YAML/JSON 等结构化数据解析失败。
    #[error("序列化错误: {0}")]
    Serialization(String),

    /// 参数校验未通过，携带出错字段名以便调用方精确定位。
    #[error("校验失败: {field}: {reason}")]
    Validation {
        /// 未通过校验的字段名。
        field: String,
        /// 失败原因，可直接呈现给用户的描述。
        reason: String,
    },

    /// 请求的业务对象不存在。
    #[error("{entity} 不存在: {id}")]
    NotFound {
        /// 业务对象类型，如「流水线」「证书」。
        entity: String,
        /// 对象标识。
        id: String,
    },

    /// 资源冲突，例如同一指纹重复申请。
    #[error("资源冲突: {0}")]
    Conflict(String),

    /// 外部服务（CA、DNS 厂商、SSH 主机等）返回了错误。
    #[error("外部服务错误: {0}")]
    External(String),

    /// 操作超时。
    #[error("操作超时: {0}")]
    Timeout(String),

    /// 鉴权失败或令牌无效。
    #[error("鉴权失败: {0}")]
    Unauthorized(String),

    /// 内部不应发生的情形，用于替代 panic。
    #[error("内部错误: {0}")]
    Internal(String),
}

/// 本 crate 统一的返回类型别名。
pub type Result<T> = std::result::Result<T, Error>;

impl Error {
    /// 构造 [`Error::Validation`]。
    pub fn validation(field: impl Into<String>, reason: impl Into<String>) -> Self {
        Self::Validation {
            field: field.into(),
            reason: reason.into(),
        }
    }

    /// 构造 [`Error::NotFound`]。
    pub fn not_found(entity: impl Into<String>, id: impl Into<String>) -> Self {
        Self::NotFound {
            entity: entity.into(),
            id: id.into(),
        }
    }

    /// 校验通过的断言：条件为真时返回 unit，否则返回带字段名的校验错误。
    pub fn require(condition: bool, field: &str, reason: &str) -> Result<()> {
        if condition {
            Ok(())
        } else {
            Err(Self::validation(field, reason))
        }
    }
}

impl From<serde_yaml::Error> for Error {
    fn from(err: serde_yaml::Error) -> Self {
        Self::Serialization(err.to_string())
    }
}

impl From<serde_json::Error> for Error {
    fn from(err: serde_json::Error) -> Self {
        Self::Serialization(err.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validation_error_carries_field_name() {
        let err = Error::validation("domains", "至少需要一个域名");
        assert!(err.to_string().contains("domains"));
        assert!(err.to_string().contains("至少需要一个域名"));

        match &err {
            Error::Validation { field, reason } => {
                assert_eq!(field, "domains");
                assert_eq!(reason, "至少需要一个域名");
            }
            other => panic!("期望 Validation，实际 {other:?}"),
        }
    }

    #[test]
    fn require_returns_ok_when_condition_holds() {
        assert!(Error::require(true, "f", "reason").is_ok());
        assert!(Error::require(false, "f", "reason").is_err());
    }

    #[test]
    fn not_found_includes_entity_and_id() {
        let err = Error::not_found("流水线", "42");
        let text = err.to_string();
        assert!(text.contains("流水线"));
        assert!(text.contains("42"));
    }
}
