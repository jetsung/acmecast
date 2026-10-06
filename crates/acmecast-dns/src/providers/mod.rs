//! 内置 DNS 提供商。

pub mod aliyun;
pub mod cloudflare;
pub mod tencent;
pub mod tencent_eo;

pub use aliyun::AliyunProvider;
pub use cloudflare::CloudflareProvider;
pub use tencent::{TencentCredentials, TencentProvider, TencentSite};
pub use tencent_eo::TencentEoProvider;
