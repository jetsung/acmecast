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
pub use challenge::{
    ChallengeKind, challenge_record_name, challenge_write_name, ensure_kind_covers, with_txt_record,
};
pub use error::{Error, Result};
pub use http::{
    HttpRequest, HttpResponse, HttpTransport, ProxyMode, ReqwestTransport, proxy_log_target,
    proxy_url_from_credentials, validate_proxy_url,
};
pub use http01::Http01Answer;
pub use propagation::{
    DnsResolver, DohResolver, PropagationPolicy, default_resolvers, doh_resolvers_for_provider,
    prioritize_for_provider, resolvers_for_provider, resolvers_for_zone, wait_until_visible,
    wait_until_visible_phased,
};
pub use provider::{DnsProvider, TxtRecord, parse_credentials};
pub use providers::{
    AliyunProvider, CloudflareProvider, TencentApiClient, TencentEoProvider, TencentProvider,
};
pub use registry::DnsProviderRegistry;
pub use resolvers::{
    PROXY_URL_ENV, ResolverEntry, doh_transport, doh_transport_from, load_extended_resolvers,
};
