//! ACME Directory URL 解析。
//!
//! 支持两种来源：内置 CA 别名（如 `letsencrypt`）与用户提供的自定义 URL。
//! 自定义 URL 让内网 CA、企业私有 CA 与其他公共 ACME 服务同样可用。

use acmecast_core::Error as CoreError;

use crate::error::{AcmeError, Result};

/// 内置 CA 别名。
///
/// 别名与 certd 的 `sslProvider` 取值对齐，便于既有配置迁移。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum CaKind {
    /// Let's Encrypt 生产环境。
    LetsEncrypt,
    /// Let's Encrypt 测试环境（签发假证书，用于集成测试）。
    LetsEncryptStaging,
    /// ZeroSSL。
    ZeroSsl,
    /// Google Trust Services。
    Google,
    /// SSL.com。
    SslCom,
    /// 用户自定义的 ACME 服务，必须自行提供 Directory URL。
    Custom,
}

impl CaKind {
    /// 全部已知别名的字符串表示，用于错误提示与表单选项。
    pub const ALL: &'static [&'static str] = &[
        "letsencrypt",
        "letsencrypt-staging",
        "zerossl",
        "google",
        "sslcom",
        "custom",
    ];

    /// 从字符串解析别名；未知取值返回 `None`。
    #[must_use]
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "letsencrypt" | "le" => Some(Self::LetsEncrypt),
            "letsencrypt-staging" | "le-staging" => Some(Self::LetsEncryptStaging),
            "zerossl" => Some(Self::ZeroSsl),
            "google" | "gts" => Some(Self::Google),
            "sslcom" | "ssl.com" => Some(Self::SslCom),
            "custom" => Some(Self::Custom),
            _ => None,
        }
    }

    /// 别名对应的 Directory URL。自定义 CA 没有内置 URL，返回 `None`。
    #[must_use]
    pub fn builtin_url(&self) -> Option<&'static str> {
        match self {
            Self::LetsEncrypt => Some("https://acme-v02.api.letsencrypt.org/directory"),
            Self::LetsEncryptStaging => {
                Some("https://acme-staging-v02.api.letsencrypt.org/directory")
            }
            Self::ZeroSsl => Some("https://acme.zerossl.com/v2/DV90"),
            Self::Google => Some("https://dv.acme-v02.api.pki.goog/directory"),
            Self::SslCom => Some("https://acme.ssl.com/ssl/v2/DV"),
            // 自定义 CA 必须由用户提供 URL，这里刻意不提供兜底值：
            // 给一个默认值会让用户误以为已配置成功。
            Self::Custom => None,
        }
    }
}

/// 解析出最终使用的 Directory URL。
///
/// - `custom_url` 非空时直接采用（自定义 CA 走这条路，也允许覆盖内置 CA 的端点）
/// - 否则按 CA 别名取内置 URL
/// - 自定义 CA 且未提供 URL 时返回 [`AcmeError::UnknownCa`] 之外的明确校验错误
///
/// 对应 spec 场景「自定义 CA 缺少 Directory URL」。
pub fn resolve_directory_url(ca: CaKind, custom_url: Option<&str>) -> Result<String> {
    if let Some(url) = custom_url.map(str::trim).filter(|u| !u.is_empty()) {
        return validate_directory_url(url);
    }

    match ca.builtin_url() {
        Some(url) => Ok(url.to_owned()),
        None => Err(AcmeError::Directory(
            "自定义 ACME 必须填写 Directory URL（形如 \
             https://ca.example.com/acme/directory）；\
             内置 CA 请勿选择 custom"
                .to_owned(),
        )),
    }
}

/// 校验 Directory URL 的基本形态。
///
/// 只校验 scheme 与主机名是否具备，不发起网络请求——
/// 是否真的可达由首次请求决定。
fn validate_directory_url(url: &str) -> Result<String> {
    if !url.starts_with("http://") && !url.starts_with("https://") {
        return Err(AcmeError::Directory(format!(
            "Directory URL 必须以 http:// 或 https:// 开头，实际为 `{url}`"
        )));
    }

    let rest = url
        .trim_start_matches("https://")
        .trim_start_matches("http://");
    let host = rest.split('/').next().unwrap_or_default();
    if host.is_empty() {
        return Err(AcmeError::Directory(format!(
            "Directory URL 缺少主机名: `{url}`"
        )));
    }

    Ok(url.to_owned())
}

impl From<AcmeError> for CoreError {
    fn from(err: AcmeError) -> Self {
        CoreError::External(err.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_ca_resolves_to_its_directory() {
        let url = resolve_directory_url(CaKind::LetsEncrypt, None).unwrap();
        assert!(url.starts_with("https://"), "{url}");
        assert!(url.contains("letsencrypt"), "{url}");
    }

    #[test]
    fn staging_uses_staging_endpoint() {
        let url = resolve_directory_url(CaKind::LetsEncryptStaging, None).unwrap();
        assert!(url.contains("staging"), "{url}");
    }

    #[test]
    fn custom_ca_with_url_is_accepted() {
        let url = resolve_directory_url(CaKind::Custom, Some("https://ca.internal/acme/directory"))
            .unwrap();
        assert_eq!(url, "https://ca.internal/acme/directory");
    }

    #[test]
    fn custom_ca_without_url_is_rejected_with_clear_message() {
        let err = resolve_directory_url(CaKind::Custom, None).unwrap_err();
        let text = err.to_string();
        assert!(text.contains("Directory URL"), "错误应指出缺什么: {text}");
        assert!(
            text.contains("自定义 ACME"),
            "错误应点明是自定义 CA: {text}"
        );
    }

    #[test]
    fn custom_ca_with_blank_url_is_rejected() {
        // 只有空白字符等同于未提供。
        let err = resolve_directory_url(CaKind::Custom, Some("   ")).unwrap_err();
        assert!(matches!(err, AcmeError::Directory(_)));
    }

    #[test]
    fn custom_url_overrides_builtin_ca() {
        // 允许用户给内置 CA 换端点（例如走自建反代）。
        let url =
            resolve_directory_url(CaKind::LetsEncrypt, Some("https://proxy.local/dir")).unwrap();
        assert_eq!(url, "https://proxy.local/dir");
    }

    #[test]
    fn unknown_scheme_is_rejected() {
        let err = resolve_directory_url(CaKind::Custom, Some("ftp://ca.local/dir")).unwrap_err();
        assert!(err.to_string().contains("https://"), "{err}");
    }

    #[test]
    fn url_without_host_is_rejected() {
        let err = resolve_directory_url(CaKind::Custom, Some("https:///dir")).unwrap_err();
        assert!(err.to_string().contains("主机名"), "{err}");
    }

    #[test]
    fn every_alias_roundtrips() {
        for alias in CaKind::ALL {
            assert!(CaKind::parse(alias).is_some(), "别名 `{alias}` 应可解析");
        }
    }

    #[test]
    fn unknown_alias_is_none() {
        assert!(CaKind::parse("not-a-ca").is_none());
    }

    #[test]
    fn alias_parsing_is_case_insensitive() {
        assert_eq!(CaKind::parse("LetsEncrypt"), Some(CaKind::LetsEncrypt));
        assert_eq!(CaKind::parse("  zerossl  "), Some(CaKind::ZeroSsl));
    }

    #[test]
    fn custom_alias_has_no_builtin_url() {
        assert!(
            CaKind::Custom.builtin_url().is_none(),
            "自定义 CA 不应有内置 URL，否则会掩盖配置缺失"
        );
    }

    #[test]
    fn every_builtin_ca_has_an_https_url() {
        for kind in [
            CaKind::LetsEncrypt,
            CaKind::LetsEncryptStaging,
            CaKind::ZeroSsl,
            CaKind::Google,
            CaKind::SslCom,
        ] {
            let url = kind.builtin_url().expect("内置 CA 应有 URL");
            assert!(url.starts_with("https://"), "{kind:?} -> {url}");
        }
    }
}
