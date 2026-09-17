//! X.509 证书语义解析。
//!
//! 从 DER 中提取业务关心的事实：SAN 域名、签发者、有效期、指纹、是否为 CA。
//! 不理解证书的用途，也不做信任链校验——那是 ACME 客户端与 TLS 栈的职责。

use chrono::{DateTime, Utc};
use sha2::{Digest, Sha256};
use x509_parser::prelude::{FromDer, X509Certificate};

use crate::error::{Error, Result};
use crate::pem;

/// 从证书中解析出的关键事实。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CertificateInfo {
    /// SAN 中的 DNS 名称，保持证书里的原始顺序，含通配符（如 `*.example.com`）。
    ///
    /// SAN 缺失时回退到 Subject 的 CN——这是历史遗留写法，
    /// 现代 CA 都要求 SAN，但旧证书仍可能只有 CN。
    pub domains: Vec<String>,
    /// Subject 的字符串表示。
    pub subject: String,
    /// Issuer 的字符串表示（即签发它的 CA）。
    pub issuer: String,
    /// 生效时间（UTC）。
    pub not_before: DateTime<Utc>,
    /// 到期时间（UTC）。
    pub not_after: DateTime<Utc>,
    /// 序列号，十六进制字符串。
    pub serial: String,
    /// 整个 DER 的 SHA-256 指纹，小写十六进制。
    pub fingerprint_sha256: String,
    /// 是否为 CA 证书（Basic Constraints 的 `CA:TRUE`）。
    pub is_ca: bool,
}

impl CertificateInfo {
    /// 证书主域名（SAN 的第一项）。
    #[must_use]
    pub fn primary_domain(&self) -> Option<&str> {
        self.domains.first().map(String::as_str)
    }

    /// 是否包含通配符域名。
    #[must_use]
    pub fn is_wildcard(&self) -> bool {
        self.domains.iter().any(|d| d.starts_with("*."))
    }

    /// 给定时间点是否在有效期内。
    #[must_use]
    pub fn is_valid_at(&self, moment: DateTime<Utc>) -> bool {
        moment >= self.not_before && moment <= self.not_after
    }
}

/// 解析单张 DER 证书。
pub fn parse_der(der: &[u8]) -> Result<CertificateInfo> {
    let (remaining, cert) = X509Certificate::from_der(der)
        .map_err(|e| Error::X509(format!("不是合法的 DER 证书: {e}")))?;

    if !remaining.is_empty() {
        return Err(Error::X509(format!(
            "DER 尾部有 {} 字节未消费，可能不是单张证书",
            remaining.len()
        )));
    }

    let validity = cert.validity();
    let mut domains = extract_san_domains(&cert);

    // SAN 为空时回退到 CN。RFC 6125 已不推荐，但旧证书仍可能存在。
    if domains.is_empty()
        && let Some(cn) = extract_common_name(&cert)
    {
        domains.push(cn);
    }

    Ok(CertificateInfo {
        domains,
        subject: cert.subject().to_string(),
        issuer: cert.issuer().to_string(),
        not_before: asn1_time_to_chrono(&validity.not_before),
        not_after: asn1_time_to_chrono(&validity.not_after),
        serial: cert.raw_serial_as_string(),
        fingerprint_sha256: sha256_hex(der),
        is_ca: cert
            .basic_constraints()
            .ok()
            .flatten()
            .is_some_and(|ext| ext.value.ca),
    })
}

/// 解析 PEM 中的证书链，返回每张证书的信息。
pub fn parse_pem(pem_text: &str) -> Result<Vec<CertificateInfo>> {
    let blocks = pem::pem_to_der_blocks(pem_text)?;
    if blocks.is_empty() {
        return Err(Error::NoCertificate);
    }
    blocks.iter().map(|der| parse_der(der)).collect()
}

/// 解析 PEM 中第一张证书（叶子证书）。
///
/// 证书链的惯例是叶子在前，因此这等价于「取签发给你的那张」。
pub fn parse_pem_leaf(pem_text: &str) -> Result<CertificateInfo> {
    let der = pem::first_der(pem_text)?;
    parse_der(&der)
}

/// 提取 SAN 扩展中的 DNS 名称。
fn extract_san_domains(cert: &X509Certificate<'_>) -> Vec<String> {
    use x509_parser::extensions::GeneralName;

    let Ok(Some(extension)) = cert.subject_alternative_name() else {
        return Vec::new();
    };

    extension
        .value
        .general_names
        .iter()
        .filter_map(|name| match name {
            GeneralName::DNSName(dns) => Some((*dns).to_owned()),
            // IP、邮箱、URI 等 SAN 类型不参与证书的域名集合，
            // 它们与 ACME 的标识符语义不同。
            _ => None,
        })
        .collect()
}

/// 从 Subject 中取 CN。
fn extract_common_name(cert: &X509Certificate<'_>) -> Option<String> {
    cert.subject()
        .iter_common_name()
        .next()
        .and_then(|attr| attr.as_str().ok())
        .map(ToOwned::to_owned)
}

/// `ASN1Time` 转 `chrono::DateTime<Utc>`。
///
/// x509-parser 内部用 `time` crate；这里换算成 Unix 时间戳再构造 chrono 值，
/// 避免把 `time` 加成本 crate 的直接依赖。
fn asn1_time_to_chrono(time: &x509_parser::time::ASN1Time) -> DateTime<Utc> {
    let seconds = time.to_datetime().unix_timestamp();
    DateTime::from_timestamp(seconds, 0).unwrap_or(DateTime::UNIX_EPOCH)
}

/// 计算 SHA-256 指纹的小写十六进制表示。
fn sha256_hex(data: &[u8]) -> String {
    let digest = Sha256::digest(data);
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_hex_is_lowercase_and_64_chars() {
        let hex = sha256_hex(b"abc");
        assert_eq!(hex.len(), 64);
        assert!(
            hex.chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
        );
        // 已知向量：sha256("abc")
        assert_eq!(
            hex,
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn invalid_der_is_rejected() {
        match parse_der(&[0x00, 0x01, 0x02]) {
            Ok(_) => panic!("非法 DER 应报错"),
            Err(err) => assert!(matches!(err, Error::X509(_)), "{err:?}"),
        }
    }

    #[test]
    fn empty_pem_reports_no_certificate() {
        match parse_pem("") {
            Ok(_) => panic!("空 PEM 应报错"),
            Err(err) => assert!(matches!(err, Error::NoCertificate), "{err:?}"),
        }
    }
}
