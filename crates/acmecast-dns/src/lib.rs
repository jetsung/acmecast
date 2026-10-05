#![allow(missing_docs)]
pub mod authoritative;
pub mod builtin;
pub mod challenge;
pub mod error;
pub mod http;
pub mod http01;
pub mod propagation;
pub mod provider;
pub mod providers;
pub mod registry;
pub mod resolvers;

pub use authoritative::{AuthoritativeResolver, Authority, HickoryAuthority};
pub use challenge::{ChallengeKind, challenge_record_name, ensure_kind_covers, with_txt_record};
pub use error::{Error, Result};
pub use http::{HttpRequest, HttpResponse, HttpTransport, ReqwestTransport};
pub use http01::Http01Answer;
pub use propagation::{
    DnsResolver, DohResolver, PropagationPolicy, default_resolvers, resolvers_for_zone,
    wait_until_visible,
};
pub use provider::{DnsProvider, TxtRecord, parse_credentials};
pub use providers::{AliyunProvider, CloudflareProvider};
pub use registry::DnsProviderRegistry;
pub use resolvers::{ResolverEntry, load_extended_resolvers};
