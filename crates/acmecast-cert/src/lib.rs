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
pub use parse::{CertificateInfo, parse_der, parse_pem, parse_pem_leaf};
pub use pem::{chain_to_pem, der_to_pem, first_der, pem_to_der_blocks};
