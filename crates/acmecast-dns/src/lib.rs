//! DNS 挑战：提供商抽象与 ACME 挑战记录管理。
//!
//! 对应 `specs/dns-challenge/spec.md`。本 crate 定义「怎么在某家 DNS 服务上
//! 增删 TXT 记录」，以及挑战流程本身（写入、等待传播、清理）。

pub mod challenge;
pub mod error;
pub mod http;
pub mod http01;
pub mod propagation;
pub mod provider;
pub mod providers;
pub mod registry;

pub use challenge::{ChallengeKind, challenge_record_name, ensure_kind_covers, with_txt_record};
pub use error::{Error, Result};
pub use http::{HttpRequest, HttpResponse, HttpTransport, ReqwestTransport};
pub use http01::Http01Answer;
pub use propagation::{
    DnsResolver, DohResolver, PropagationPolicy, default_resolvers, wait_until_visible,
};
pub use provider::{DnsProvider, TxtRecord, parse_credentials};
pub use providers::{AliyunProvider, CloudflareProvider};
pub use registry::DnsProviderRegistry;
