//! acmecast 核心层：错误类型、配置装载、凭据加密原语。
//!
//! 本 crate 不依赖任何其他 `acmecast-*` crate，是所有上层 crate 的共同基座。

pub mod config;
pub mod crypto;
pub mod error;
pub mod proxy;

pub use crypto::{CredentialCipher, REDACTED, redact_presence};
pub use error::{Error, Result};

/// 流水线网络操作的兜底超时（30 秒）。
///
/// 编译期默认值；运行时被 `[network] timeout_secs` / `ACMECAST_NETWORK_TIMEOUT_SECS`
/// 的配置值覆盖。供未注入配置的测试与构造路径直接引用。
pub const NETWORK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
pub use proxy::{
    ProxyMode, effective_proxy_url, inject_proxy_field_annotations, proxy_log_target,
    proxy_url_from_credentials, validate_proxy_field, validate_proxy_url,
};
