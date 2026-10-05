//! acmecast 核心层：错误类型、配置装载、凭据加密原语。
//!
//! 本 crate 不依赖任何其他 `acmecast-*` crate，是所有上层 crate 的共同基座。

pub mod config;
pub mod crypto;
pub mod error;

pub use crypto::{CredentialCipher, REDACTED, redact_presence};
pub use error::{Error, Result};
