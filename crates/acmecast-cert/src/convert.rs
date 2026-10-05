//! 格式转换：PFX（PKCS#12）、P7B（PKCS#7 certs-only）与 JKS（Java KeyStore）。
//!
//! 三条路径都用**纯 Rust** 实现，不依赖 OpenSSL：
//! - PFX 用 `p12-keystore`
//! - P7B 用 RustCrypto 的 `cms`
//! - JKS 用 `jks`
//!
//! 转换是**纯函数**：输入 PEM 文本，输出字节。调用方拿到结果自行决定写文件还是回响应，
//! 本模块不碰磁盘，因此不可能修改仓库中已持久化的原始 PEM。

use std::time::SystemTime;

use cms::builder::SignedDataBuilder;
use cms::cert::CertificateChoices;
use cms::content_info::ContentInfo;
use cms::signed_data::EncapsulatedContentInfo;
use der::{Decode, Encode};
use p12_keystore::{
    Certificate, EncryptionAlgorithm, KeyStore, KeyStoreEntry, MacAlgorithm, PrivateKey,
    PrivateKeyChain,
};

use crate::error::{Error, Result};
use crate::pem;

/// PFX 的 PEM 头标记。
pub const P7B_PEM_HEADER: &str = "-----BEGIN PKCS7-----";
/// PFX 的 PEM 尾标记。
pub const P7B_PEM_FOOTER: &str = "-----END PKCS7-----";

/// PFX 的加密算法选择。
///
/// 兼容性差异很大：现代栈（Windows 10+、OpenSSL 3、JDK 9+）都支持 PBES2+AES，
/// 但老旧的客户端可能只认传统 PBE。因此把选择权交给调用方。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PfxEncryption {
    /// PBES2 + HMAC-SHA256 + AES-256-CBC。**推荐默认**，现代栈通用。
    #[default]
    Aes256,
    /// PKCS#12 传统的 SHA-1 + 3DES。为兼容老旧客户端保留。
    TripleDes,
    /// PKCS#12 传统的 SHA-1 + 40 位 RC2。仅用于极老的客户端（如 Windows XP 时代）。
    Rc2,
}

impl From<PfxEncryption> for EncryptionAlgorithm {
    fn from(value: PfxEncryption) -> Self {
        match value {
            PfxEncryption::Aes256 => EncryptionAlgorithm::PbeWithHmacSha256AndAes256,
            PfxEncryption::TripleDes => EncryptionAlgorithm::PbeWithShaAnd3KeyTripleDesCbc,
            PfxEncryption::Rc2 => EncryptionAlgorithm::PbeWithShaAnd40BitRc4Cbc,
        }
    }
}

/// 把证书链与私钥打包为 PFX（PKCS#12）。
///
/// - `cert_chain_pem`：PEM 格式的证书链，**叶子证书必须在最前**，根证书在最后
/// - `key_pem`：PKCS#8 格式的私钥 PEM
/// - `password`：保护 PFX 的口令；空字符串表示不加密（不建议）
/// - `alias`：条目的别名，导出到 Java KeyStore 等场景会用到
pub fn to_pfx(
    cert_chain_pem: &str,
    key_pem: &str,
    password: &str,
    alias: &str,
    encryption: PfxEncryption,
) -> Result<Vec<u8>> {
    let certs = load_certificates(cert_chain_pem)?;
    let key = load_private_key(key_pem)?;

    // localKeyId 用叶子证书的 DER 摘要派生：同一对密钥/证书每次都得到相同值，
    // 便于比对与复现，且不会泄漏私钥信息。
    let local_key_id = leaf_key_id(&certs)?;

    let mut keystore = KeyStore::new();
    keystore.add_entry(
        alias,
        KeyStoreEntry::PrivateKeyChain(PrivateKeyChain::new(local_key_id, key, certs)),
    );

    keystore
        .writer(password)
        .encryption_algorithm(encryption.into())
        .mac_algorithm(MacAlgorithm::HmacSha256)
        .write()
        .map_err(|e| Error::Conversion(format!("PKCS#12 编码失败: {e}")))
}

/// 用默认加密算法（PBES2 + AES-256）打包 PFX。
pub fn to_pfx_default(
    cert_chain_pem: &str,
    key_pem: &str,
    password: &str,
    alias: &str,
) -> Result<Vec<u8>> {
    to_pfx(
        cert_chain_pem,
        key_pem,
        password,
        alias,
        PfxEncryption::default(),
    )
}

/// 校验 PFX 能否被解析回来，并取出私钥条目。
///
/// 用于自检——生成后立刻验证产物可用，而不是等用户导入时才发现问题。
pub fn verify_pfx(pfx_der: &[u8], password: &str) -> Result<()> {
    let keystore =
        KeyStore::from_pkcs12(pfx_der, password, p12_keystore::Pkcs12ImportPolicy::Strict)
            .map_err(|e| Error::Conversion(format!("生成的 PFX 无法被解析回来: {e}")))?;

    if keystore.private_key_chain().is_none() {
        return Err(Error::Conversion("生成的 PFX 中不含私钥条目".to_owned()));
    }
    Ok(())
}

/// 把证书链转换为 P7B（PKCS#7 certs-only SignedData）的 DER 编码。
///
/// certs-only 是 P7B 的标准形态：`eContentType` 为 `id-data` 且内容为 detached，
/// 不含签名者与摘要算法，仅承载证书集合。
pub fn to_p7b_der(cert_chain_pem: &str) -> Result<Vec<u8>> {
    let ders = pem::pem_to_der_blocks(cert_chain_pem)?;
    if ders.is_empty() {
        return Err(Error::NoCertificate);
    }

    let encapsulated = EncapsulatedContentInfo {
        econtent_type: const_oid::db::rfc5911::ID_DATA,
        // certs-only：不带内嵌内容。
        econtent: None,
    };

    let mut builder = SignedDataBuilder::new(&encapsulated);

    for der_bytes in &ders {
        let cert = x509_cert::Certificate::from_der(der_bytes)
            .map_err(|e| Error::X509(format!("链中存在无法解析的证书: {e}")))?;
        builder
            .add_certificate(CertificateChoices::Certificate(cert))
            .map_err(|e| Error::Conversion(format!("添加证书到 P7B 失败: {e}")))?;
    }

    let content_info: ContentInfo = builder
        .build()
        .map_err(|e| Error::Conversion(format!("P7B 构造失败: {e}")))?;

    content_info
        .to_der()
        .map_err(|e| Error::Conversion(format!("P7B DER 编码失败: {e}")))
}

/// 把证书链转换为 P7B 的 PEM 文本。
pub fn to_p7b_pem(cert_chain_pem: &str) -> Result<String> {
    let der_bytes = to_p7b_der(cert_chain_pem)?;
    Ok(encode_pkcs7_pem(&der_bytes))
}

/// 把 DER 编码的 P7B 包装为 PEM。
#[must_use]
pub fn encode_pkcs7_pem(der_bytes: &[u8]) -> String {
    use base64::Engine;

    let body = base64::engine::general_purpose::STANDARD.encode(der_bytes);
    let mut out = String::with_capacity(body.len() + 64);
    out.push_str(P7B_PEM_HEADER);
    out.push('\n');
    for chunk in body.as_bytes().chunks(64) {
        out.push_str(&String::from_utf8_lossy(chunk));
        out.push('\n');
    }
    out.push_str(P7B_PEM_FOOTER);
    out.push('\n');
    out
}

// ---- JKS（Java KeyStore）----

/// JKS 口令的最小长度。
///
/// 这是 **Java 自身的约束**：`keytool` 与 `KeyStore` 都拒绝短于 6 个字符的口令，
/// `jks` crate 的默认 `min_password_len` 同为 6。提前在入口校验，
/// 是为了给出比「格式转换失败」更明确的错误。
pub const JKS_MIN_PASSWORD_LEN: usize = 6;

/// 把证书链与私钥打包为 JKS（Java KeyStore）。
///
/// - `cert_chain_pem`：PEM 格式的证书链，**叶子证书必须在最前**，根证书在最后
/// - `key_pem`：PKCS#8 格式的私钥 PEM
/// - `password`：**同时**用作 store 口令与私钥口令，长度至少 [`JKS_MIN_PASSWORD_LEN`]
/// - `alias`：条目别名。JKS 的别名**不区分大小写**，写入时统一转小写（与 Java 行为一致）
///
/// 与 PFX 不同，JKS 没有加密算法选项：该格式只定义了一种私钥保护算法
/// （Oracle 私有的 XOR + SHA-1 KeyProtector），无法协商更强的加密。
/// 这是格式的固有限制，不是本实现的取舍。需要现代加密时应当用 PFX。
///
/// 产物**非确定性**：条目携带创建时间戳，私钥保护器带随机 salt。
pub fn to_jks(cert_chain_pem: &str, key_pem: &str, password: &str, alias: &str) -> Result<Vec<u8>> {
    if password.len() < JKS_MIN_PASSWORD_LEN {
        return Err(Error::Conversion(format!(
            "JKS 口令至少需要 {JKS_MIN_PASSWORD_LEN} 个字符，当前为 {}",
            password.len()
        )));
    }

    let ders = pem::pem_to_der_blocks(cert_chain_pem)?;
    if ders.is_empty() {
        return Err(Error::NoCertificate);
    }

    let certificate_chain = ders
        .iter()
        .map(|der_bytes| jks::Certificate {
            cert_type: "X509".to_owned(),
            content: der_bytes.clone(),
        })
        .collect();

    let entry = jks::PrivateKeyEntry {
        creation_time: SystemTime::now(),
        private_key: crate::keys::pkcs8_der(key_pem)?,
        certificate_chain,
    };

    let mut keystore = jks::KeyStore::new();
    keystore
        .set_private_key_entry(alias, entry, password.as_bytes())
        .map_err(|e| Error::Conversion(format!("添加 JKS 私钥条目失败: {e}")))?;

    let mut out = Vec::new();
    keystore
        .store(&mut out, password.as_bytes())
        .map_err(|e| Error::Conversion(format!("JKS 编码失败: {e}")))?;
    Ok(out)
}

/// 校验 JKS 能否被解析回来，并取出私钥条目。
///
/// 比 [`verify_pfx`] 多走一步：不仅校验 store 的完整性摘要，还要求私钥条目
/// **能被口令解密**。理由是格式转换里最容易写坏的就是私钥保护器本身，
/// 只验摘要会漏掉这类损坏。
pub fn verify_jks(jks_der: &[u8], password: &str) -> Result<()> {
    let mut keystore = jks::KeyStore::new();
    keystore
        .load(jks_der, password.as_bytes())
        .map_err(|e| Error::Conversion(format!("生成的 JKS 无法被解析回来: {e}")))?;

    let mut found_key_entry = false;
    for alias in keystore.aliases() {
        if !keystore.is_private_key_entry(&alias) {
            continue;
        }
        let entry = keystore
            .get_private_key_entry(&alias, password.as_bytes())
            .map_err(|e| Error::Conversion(format!("JKS 中的私钥无法解密: {e}")))?;

        if entry.private_key.is_empty() {
            return Err(Error::Conversion("JKS 中的私钥条目为空".to_owned()));
        }
        if entry.certificate_chain.is_empty() {
            return Err(Error::Conversion("JKS 中的私钥条目没有证书链".to_owned()));
        }
        found_key_entry = true;
    }

    if !found_key_entry {
        return Err(Error::Conversion("生成的 JKS 中不含私钥条目".to_owned()));
    }
    Ok(())
}

/// 解析 PEM 证书链为 p12-keystore 的证书表示。
fn load_certificates(cert_chain_pem: &str) -> Result<Vec<Certificate>> {
    let ders = pem::pem_to_der_blocks(cert_chain_pem)?;
    if ders.is_empty() {
        return Err(Error::NoCertificate);
    }

    ders.iter()
        .map(|der_bytes| {
            Certificate::from_der(der_bytes)
                .map_err(|e| Error::X509(format!("链中存在无法解析的证书: {e}")))
        })
        .collect()
}

/// 解析 PKCS#8 私钥 PEM。
fn load_private_key(key_pem: &str) -> Result<PrivateKey> {
    let der_bytes = crate::keys::pkcs8_der(key_pem)?;
    PrivateKey::from_der(&der_bytes)
        .map_err(|e| Error::KeyMismatch(format!("私钥无法用于 PKCS#12: {e}")))
}

/// 由叶子证书派生 localKeyId。
fn leaf_key_id(certs: &[Certificate]) -> Result<Vec<u8>> {
    let leaf = certs
        .first()
        .ok_or_else(|| Error::Conversion("证书链为空".to_owned()))?;

    use sha2::{Digest, Sha256};
    Ok(Sha256::digest(leaf.as_der()).to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_encryption_is_aes256() {
        assert_eq!(PfxEncryption::default(), PfxEncryption::Aes256);
    }

    #[test]
    fn encryption_mapping_is_total() {
        for value in [
            PfxEncryption::Aes256,
            PfxEncryption::TripleDes,
            PfxEncryption::Rc2,
        ] {
            let _: EncryptionAlgorithm = value.into();
        }
    }

    #[test]
    fn empty_chain_is_rejected() {
        match to_p7b_der("") {
            Ok(_) => panic!("空链应报错"),
            Err(err) => assert!(matches!(err, Error::NoCertificate), "{err:?}"),
        }
    }

    #[test]
    fn pkcs7_pem_has_correct_markers() {
        let pem_text = encode_pkcs7_pem(&[0x30, 0x00]);
        assert!(pem_text.starts_with(P7B_PEM_HEADER));
        assert!(pem_text.trim_end().ends_with(P7B_PEM_FOOTER));
        assert!(pem_text.ends_with('\n'));
    }
}
