//! 证书错误类型。

/// `acmecast-cert` 的错误类型。
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// PEM 结构不合法：缺少头尾标记或 base64 无法解码。
    #[error("PEM 解析失败: {0}")]
    Pem(String),

    /// DER 不是合法的 X.509 证书。
    #[error("X.509 解析失败: {0}")]
    X509(String),

    /// 输入的 PEM 中没有找到任何证书块。
    #[error("PEM 中没有任何证书块")]
    NoCertificate,

    /// 私钥与证书不匹配。
    #[error("私钥与证书不匹配: {0}")]
    KeyMismatch(String),

    /// 格式转换失败（PFX / P7B / JKS 等）。
    #[error("格式转换失败: {0}")]
    Conversion(String),

    /// 上游 core 层的错误。
    #[error(transparent)]
    Core(#[from] acmecast_core::Error),
}

/// 本 crate 统一的返回类型别名。
pub type Result<T> = std::result::Result<T, Error>;
