//! 内置 DNS 提供商。

pub mod acme_dns;
pub mod aliyun;
pub mod cloudflare;
pub mod tencent;
pub mod tencent_eo;

pub use acme_dns::{AcmeDnsCredentials, AcmeDnsProvider, acme_dns_credentials_schema};
pub use aliyun::AliyunProvider;
pub use cloudflare::CloudflareProvider;
pub use tencent::{TencentApiClient, TencentCredentials, TencentProvider, TencentSite};
pub use tencent_eo::TencentEoProvider;
