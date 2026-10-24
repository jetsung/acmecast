//! 证书处理：PEM / DER 编解码、X.509 语义解析、格式转换。
//!
//! 对应 `specs/certificate-management/spec.md`。本 crate 只处理**已在内存中的字节**，
//! 不负责证书的持久化（那是 `acmecast-store`）也不负责签发（那是 `acmecast-acme`）。

pub mod convert;
pub mod error;
pub mod expiry;
pub mod keys;
pub mod parse;
pub mod pem;

pub use convert::{
    JKS_MIN_PASSWORD_LEN, PfxEncryption, to_jks, to_p7b_der, to_p7b_pem, to_pfx, to_pfx_default,
    verify_jks, verify_pfx,
};
pub use error::{Error, Result};
pub use expiry::{CertStatus, ExpiryPolicy, remaining_days_for, status_for};
pub use keys::{
    KeyAlgorithm, detect_algorithm, matches_der, public_key_bytes, verify_matches_der,
    verify_matches_pem,
};
pub use parse::{CertificateInfo, organization_from_dn, parse_der, parse_pem, parse_pem_leaf};
pub use pem::{ca_pem_of, chain_to_pem, der_to_pem, first_der, pem_to_der_blocks, split_chain};

/// 主域名 → 文件名前缀，供部署目标与主存储共用同一派生规则。
///
/// `*` 显式映射为 `_wildcard`（`*.example.com` → `_wildcard.example.com`），
/// 字母数字与 `.`、`-`、`_` 原样保留，其余替换为 `_`；空域名兜底为 `certificate`。
#[must_use]
pub fn domain_file_prefix(domain: &str) -> String {
    let normalized = if let Some(stripped) = domain.strip_prefix("*.") {
        format!("_wildcard.{stripped}")
    } else if domain == "*" {
        "_wildcard".to_owned()
    } else {
        domain.to_owned()
    };
    let cleaned: String = normalized
        .chars()
        .map(|c| match c {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '.' | '-' | '_' => c,
            _ => '_',
        })
        .collect();
    if cleaned.is_empty() {
        "certificate".to_owned()
    } else {
        cleaned
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wildcard_domain_maps_to_wildcard_prefix() {
        assert_eq!(domain_file_prefix("*.example.com"), "_wildcard.example.com");
        assert_eq!(domain_file_prefix("*"), "_wildcard");
    }

    #[test]
    fn plain_domain_keeps_legal_chars_and_replaces_others() {
        assert_eq!(domain_file_prefix("sub.example.com"), "sub.example.com");
        assert_eq!(domain_file_prefix("a b.example.com"), "a_b.example.com");
        assert_eq!(
            domain_file_prefix("*.hello.example.com"),
            "_wildcard.hello.example.com"
        );
    }

    #[test]
    fn empty_domain_falls_back_to_certificate() {
        assert_eq!(domain_file_prefix(""), "certificate");
    }
}
