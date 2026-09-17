//! 私钥解析，以及与证书公钥的一致性校验。
//!
//! 校验方式是比较**公钥字节**：从私钥算出公钥，与证书 SPKI 里的
//! `subject_public_key` 逐字节比对。三种算法的编码恰好一致，因此无需
//! 引入 p256/rsa 等算法库——`ring` 已在依赖树中（经 `instant-acme`）。
//!
//! | 算法 | 私钥侧（ring） | 证书侧（SPKI subjectPublicKey） |
//! | --- | --- | --- |
//! | RSA | DER 编码的 RSAPublicKey | 同左 |
//! | ECDSA | 未压缩点 `04 || X || Y` | 同左 |
//! | Ed25519 | 32 字节原始公钥 | 同左 |

use ring::signature::{
    ECDSA_P256_SHA256_FIXED_SIGNING, ECDSA_P384_SHA384_FIXED_SIGNING, EcdsaKeyPair, Ed25519KeyPair,
    KeyPair, RsaKeyPair,
};
use x509_parser::prelude::{FromDer, X509Certificate};

use crate::error::{Error, Result};

/// 私钥的算法类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyAlgorithm {
    /// RSA。
    Rsa,
    /// ECDSA on NIST P-256。
    EcdsaP256,
    /// ECDSA on NIST P-384。
    EcdsaP384,
    /// Ed25519。
    Ed25519,
}

impl KeyAlgorithm {
    /// 展示用的名称。
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Rsa => "RSA",
            Self::EcdsaP256 => "ECDSA P-256",
            Self::EcdsaP384 => "ECDSA P-384",
            Self::Ed25519 => "Ed25519",
        }
    }
}

/// PKCS#8 私钥的 PEM 头标记。
const PKCS8_PEM_HEADER: &str = "-----BEGIN PRIVATE KEY-----";
/// PKCS#8 私钥的 PEM 尾标记。
const PKCS8_PEM_FOOTER: &str = "-----END PRIVATE KEY-----";

/// 从私钥 PEM 中识别算法。
pub fn detect_algorithm(private_key_pem: &str) -> Result<KeyAlgorithm> {
    Ok(load_public_key(private_key_pem)?.0)
}

/// 从私钥 PEM 算出公钥字节。
///
/// 返回的字节与证书 SPKI 的 `subject_public_key` 直接可比。
pub fn public_key_bytes(private_key_pem: &str) -> Result<Vec<u8>> {
    Ok(load_public_key(private_key_pem)?.1)
}

/// 加载私钥并算出公钥，同时确定算法。
///
/// 实现方式是**依次尝试**用 `ring` 加载：加载成功即确定了算法。
/// 之所以不解析 PKCS#8 里的算法 OID，是因为「能否被该算法的实现接受」
/// 本身就是最权威的判定，而自行比对 OID 字符串多一处可能出错的地方。
/// 四种尝试的代价可忽略——这不是热路径。
fn load_public_key(private_key_pem: &str) -> Result<(KeyAlgorithm, Vec<u8>)> {
    let der = decode_pkcs8_pem(private_key_pem)?;
    let rng = ring::rand::SystemRandom::new();

    if let Ok(pair) = RsaKeyPair::from_pkcs8(&der) {
        return Ok((KeyAlgorithm::Rsa, pair.public_key().as_ref().to_vec()));
    }
    if let Ok(pair) = EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, &der, &rng) {
        return Ok((KeyAlgorithm::EcdsaP256, pair.public_key().as_ref().to_vec()));
    }
    if let Ok(pair) = EcdsaKeyPair::from_pkcs8(&ECDSA_P384_SHA384_FIXED_SIGNING, &der, &rng) {
        return Ok((KeyAlgorithm::EcdsaP384, pair.public_key().as_ref().to_vec()));
    }
    if let Ok(pair) = Ed25519KeyPair::from_pkcs8(&der) {
        return Ok((KeyAlgorithm::Ed25519, pair.public_key().as_ref().to_vec()));
    }

    Err(Error::KeyMismatch(
        "无法识别私钥算法：仅支持 RSA / ECDSA P-256 / ECDSA P-384 / Ed25519 的 PKCS#8 私钥"
            .to_owned(),
    ))
}

/// 判断私钥与 DER 编码的证书是否匹配。
///
/// 返回 `true` 表示证书的公钥确实由该私钥生成。
///
/// 之所以接受 DER 而非 [`CertificateInfo`]：后者只保留解析后的**事实摘要**
/// （域名、有效期等），不含 SPKI 原始字节，因此无法据此校验。
pub fn matches_der(private_key_pem: &str, cert_der: &[u8]) -> Result<bool> {
    let (_remaining, cert) = X509Certificate::from_der(cert_der)
        .map_err(|e| Error::X509(format!("不是合法的 DER 证书: {e}")))?;

    let cert_public_key = cert.tbs_certificate.subject_pki.subject_public_key.data;
    let key_public_key = public_key_bytes(private_key_pem)?;

    Ok(cert_public_key == key_public_key.as_slice())
}

/// 校验私钥与 PEM 编码的证书匹配。
///
/// 这是「入库前校验」的主入口：证书与私钥通常都以 PEM 文本给出。
pub fn verify_matches_pem(private_key_pem: &str, cert_pem: &str) -> Result<()> {
    let cert_der = crate::pem::first_der(cert_pem)?;
    verify_matches_der(private_key_pem, &cert_der)
}

/// 校验私钥与 DER 编码的证书匹配，不匹配时报错。
///
/// 供「入库前校验」使用——调用方拿到 `Ok(())` 即可放心入库。
pub fn verify_matches_der(private_key_pem: &str, cert_der: &[u8]) -> Result<()> {
    if matches_der(private_key_pem, cert_der)? {
        return Ok(());
    }

    let algorithm = detect_algorithm(private_key_pem)
        .map(|a| a.as_str().to_owned())
        .unwrap_or_else(|_| "未知".to_owned());

    Err(Error::KeyMismatch(format!(
        "私钥与证书不匹配：私钥算法为 {algorithm}，其公钥与证书中的公钥不一致"
    )))
}

/// 从私钥 PEM 中解出 PKCS#8 的 DER 字节。
///
/// 供格式转换（PKCS#12 打包）复用——那里需要 DER 而非 PEM。
pub(crate) fn pkcs8_der(pem: &str) -> Result<Vec<u8>> {
    decode_pkcs8_pem(pem)
}

/// 从 PEM 中解出 PKCS#8 的 DER 字节。
fn decode_pkcs8_pem(pem: &str) -> Result<Vec<u8>> {
    use base64::Engine;

    let text = pem.trim();
    let Some(start) = text.find(PKCS8_PEM_HEADER) else {
        return Err(Error::KeyMismatch(format!(
            "私钥缺少 `{PKCS8_PEM_HEADER}` 标记；\
             仅支持 PKCS#8 格式（`openssl pkcs8 -topk8`），\
             PKCS#1 的 `BEGIN RSA PRIVATE KEY` 请先转换"
        )));
    };
    let after_header = &text[start + PKCS8_PEM_HEADER.len()..];
    let Some(end) = after_header.find(PKCS8_PEM_FOOTER) else {
        return Err(Error::KeyMismatch(format!(
            "私钥缺少 `{PKCS8_PEM_FOOTER}` 结束标记"
        )));
    };

    let body: String = after_header[..end]
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect();

    base64::engine::general_purpose::STANDARD
        .decode(&body)
        .map_err(|e| Error::KeyMismatch(format!("私钥 base64 解码失败: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_private_key_marker_is_rejected() {
        match decode_pkcs8_pem("not a key") {
            Ok(_) => panic!("非 PEM 应报错"),
            Err(err) => {
                let text = err.to_string();
                assert!(text.contains("PKCS#8"), "应提示格式要求: {text}");
                assert!(
                    text.contains("BEGIN RSA PRIVATE KEY"),
                    "应指出 PKCS#1 需转换: {text}"
                );
            }
        }
    }

    #[test]
    fn missing_footer_is_rejected() {
        let broken = format!("{PKCS8_PEM_HEADER}\nAAAA\n");
        assert!(decode_pkcs8_pem(&broken).is_err());
    }

    #[test]
    fn invalid_base64_is_rejected() {
        let broken = format!("{PKCS8_PEM_HEADER}\n!!!\n{PKCS8_PEM_FOOTER}\n");
        assert!(decode_pkcs8_pem(&broken).is_err());
    }

    #[test]
    fn algorithm_names_are_distinct() {
        let names = [
            KeyAlgorithm::Rsa.as_str(),
            KeyAlgorithm::EcdsaP256.as_str(),
            KeyAlgorithm::EcdsaP384.as_str(),
            KeyAlgorithm::Ed25519.as_str(),
        ];
        let unique: std::collections::BTreeSet<_> = names.iter().collect();
        assert_eq!(unique.len(), names.len());
    }
}
