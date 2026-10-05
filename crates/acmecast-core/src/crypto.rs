//! 凭据静态加密原语。
//!
//! 采用 AES-256-GCM，密文自带随机 nonce（12 字节），输出布局为
//! `nonce || ciphertext || tag`。密钥**必须**来自配置；
//! 缺少密钥时服务应快速失败，绝不能退化为明文存储。

use aes_gcm::{
    Aes256Gcm, Key, KeyInit, Nonce,
    aead::rand_core::RngCore,
    aead::{Aead, OsRng},
};
use base64::Engine;

use crate::error::{Error, Result};

/// AES-256-GCM 的 nonce 长度（字节）。
const NONCE_LEN: usize = 12;
/// 密钥长度（字节）：AES-256。
const KEY_LEN: usize = 32;

/// 脱敏占位符。
///
/// Debug 输出里出现它，就表示此处原本是秘密。集中定义是为了让全项目的
/// 脱敏写法一致——分散的字面量迟早会拼错一个字母，而拼错的表现是「看起来脱敏了」。
pub const REDACTED: &str = "<redacted>";

/// 有值即脱敏，无值仍显示 `None`——好让「没配」与「配了但不该看」区分得开。
#[must_use]
pub fn redact_presence(value: &Option<String>) -> &'static str {
    match value {
        Some(_) => REDACTED,
        None => "None",
    }
}

/// 凭据加解密器，持有主密钥并提供加解密操作。
pub struct CredentialCipher {
    cipher: Aes256Gcm,
}

impl std::fmt::Debug for CredentialCipher {
    /// 密钥永不参与格式化输出，避免误入日志。
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CredentialCipher")
            .field("key", &REDACTED)
            .finish()
    }
}

impl CredentialCipher {
    /// 从原始 32 字节密钥构造。
    pub fn from_bytes(key: &[u8]) -> Result<Self> {
        if key.len() != KEY_LEN {
            return Err(Error::Config(format!(
                "凭据加密密钥长度必须为 {KEY_LEN} 字节，实际为 {} 字节",
                key.len()
            )));
        }
        let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(key));
        Ok(Self { cipher })
    }

    /// 从 base64 编码的密钥构造，这是配置文件中使用的形式。
    pub fn from_base64(key_b64: &str) -> Result<Self> {
        let raw = base64::engine::general_purpose::STANDARD
            .decode(key_b64.trim())
            .map_err(|e| Error::Config(format!("凭据加密密钥不是合法的 base64: {e}")))?;
        Self::from_bytes(&raw)
    }

    /// 生成一个可直接配置使用的随机密钥（base64 形式），供安装时使用。
    pub fn generate_key_base64() -> String {
        let mut raw = [0u8; KEY_LEN];
        OsRng.fill_bytes(&mut raw);
        base64::engine::general_purpose::STANDARD.encode(raw)
    }

    /// 加密：输出 `nonce || ciphertext || tag`.
    pub fn encrypt(&self, plaintext: &[u8]) -> Result<Vec<u8>> {
        let mut nonce_raw = [0u8; NONCE_LEN];
        OsRng.fill_bytes(&mut nonce_raw);
        let nonce = Nonce::from_slice(&nonce_raw);

        let ciphertext = self
            .cipher
            .encrypt(nonce, plaintext)
            .map_err(|e| Error::Crypto(e.to_string()))?;

        let mut out = Vec::with_capacity(NONCE_LEN + ciphertext.len());
        out.extend_from_slice(&nonce_raw);
        out.extend_from_slice(&ciphertext);
        Ok(out)
    }

    /// 解密 [`encrypt`](Self::encrypt) 的输出。密文被篡改时返回
    /// [`Error::Decryption`]，而非 Panic。
    pub fn decrypt(&self, blob: &[u8]) -> Result<Vec<u8>> {
        if blob.len() <= NONCE_LEN {
            return Err(Error::Decryption("密文长度不足，缺少有效载荷".into()));
        }
        let (nonce_raw, ciphertext) = blob.split_at(NONCE_LEN);
        self.cipher
            .decrypt(Nonce::from_slice(nonce_raw), ciphertext)
            .map_err(|_| Error::Decryption("密文校验失败，可能已被篡改或使用了不同的密钥".into()))
    }

    /// 加密字符串并返回可直接落库的 base64 文本。
    pub fn encrypt_string(&self, plaintext: &str) -> Result<String> {
        let blob = self.encrypt(plaintext.as_bytes())?;
        Ok(base64::engine::general_purpose::STANDARD.encode(blob))
    }

    /// 还原 [`encrypt_string`](Self::encrypt_string) 的输出。
    pub fn decrypt_string(&self, encoded: &str) -> Result<String> {
        let blob = base64::engine::general_purpose::STANDARD
            .decode(encoded.trim())
            .map_err(|e| Error::Decryption(format!("密文不是合法 base64: {e}")))?;
        let plaintext = self.decrypt(&blob)?;
        String::from_utf8(plaintext)
            .map_err(|e| Error::Decryption(format!("解密结果不是合法 UTF-8: {e}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key() -> String {
        CredentialCipher::generate_key_base64()
    }

    #[test]
    fn roundtrip_string() {
        let cipher = CredentialCipher::from_base64(&key()).unwrap();
        let secret = "sk-averysecrettoken-1234567890";
        let encoded = cipher.encrypt_string(secret).unwrap();

        assert_ne!(encoded, secret, "密文不得等于明文");
        assert!(
            !encoded.contains("averysecrettoken"),
            "密文不得包含明文片段"
        );
        assert_eq!(cipher.decrypt_string(&encoded).unwrap(), secret);
    }

    #[test]
    fn same_plaintext_yields_different_ciphertext() {
        let cipher = CredentialCipher::from_base64(&key()).unwrap();
        let a = cipher.encrypt(b"same-input").unwrap();
        let b = cipher.encrypt(b"same-input").unwrap();
        assert_ne!(a, b, "nonce 随机，相同明文应产生不同密文");
    }

    #[test]
    fn wrong_key_is_rejected_not_panicking() {
        let a = CredentialCipher::from_base64(&key()).unwrap();
        let b = CredentialCipher::from_base64(&key()).unwrap();
        let encoded = a.encrypt_string("secret").unwrap();
        assert!(matches!(
            b.decrypt_string(&encoded),
            Err(Error::Decryption(_))
        ));
    }

    #[test]
    fn tampered_ciphertext_is_rejected() {
        let cipher = CredentialCipher::from_base64(&key()).unwrap();
        let mut blob = cipher.encrypt(b"secret").unwrap();
        let last = blob.len() - 1;
        blob[last] ^= 0xff;
        assert!(matches!(cipher.decrypt(&blob), Err(Error::Decryption(_))));
    }

    #[test]
    fn debug_output_never_leaks_key() {
        let key_text = key();
        let cipher = CredentialCipher::from_base64(&key_text).unwrap();
        let rendered = format!("{cipher:?}");

        assert!(!rendered.contains(&key_text), "Debug 输出不得包含密钥原文");
        assert!(rendered.contains("redacted"), "应以脱敏占位符替代密钥");
    }

    #[test]
    fn wrong_key_length_is_config_error() {
        assert!(matches!(
            CredentialCipher::from_bytes(&[0u8; 16]),
            Err(Error::Config(_))
        ));
    }

    #[test]
    fn generated_key_is_usable() {
        let k = key();
        let cipher = CredentialCipher::from_base64(&k).unwrap();
        assert!(cipher.encrypt_string("x").is_ok());
    }

    #[test]
    fn invalid_base64_key_reports_config_error() {
        assert!(matches!(
            CredentialCipher::from_base64("!!!not-base64!!!"),
            Err(Error::Config(_))
        ));
    }

    #[test]
    fn truncated_blob_reports_decryption_error() {
        let cipher = CredentialCipher::from_base64(&key()).unwrap();
        assert!(matches!(
            cipher.decrypt(&[0u8; 4]),
            Err(Error::Decryption(_))
        ));
    }
}
