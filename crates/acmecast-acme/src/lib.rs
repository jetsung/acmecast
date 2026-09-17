//! ACME 客户端（RFC 8555）。
//!
//! 本 crate 是 [`instant_acme`] 的**防腐层**：对外只暴露 `acmecast` 自己的
//! 类型，底层库的类型一律不越过 crate 边界。这样替换底层实现时，
//! 上层的流水线与证书模块无需改动。
//!
//! 能力范围见 `specs/acme-client/spec.md`。

pub mod account;
pub mod directory;
pub mod error;
pub mod order;
pub mod service;
/// 进程内的 mock ACME 服务器，开放给其它 crate 的测试注入使用。
///
/// 由 `testing` feature 控制：生产构建不启用，测试代码不会进二进制。
#[cfg(any(test, feature = "testing"))]
pub mod testing;

pub use account::{AccountCredentials, AccountMode, EstablishAccountInput, ExternalAccountBinding};
pub use directory::{CaKind, resolve_directory_url};
pub use error::{AcmeError, Result};
pub use order::{
    AuthorizationInfo, AuthorizationStateKind, ChallengeInfo, ChallengeKind, ChallengeMaterials,
    OrderStatus, PendingOrder,
};
pub use service::{AcmeService, ProxyConfig, Transport};
