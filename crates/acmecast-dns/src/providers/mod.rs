//! 内置 DNS 提供商。

pub mod aliyun;
pub mod cloudflare;

pub use aliyun::AliyunProvider;
pub use cloudflare::CloudflareProvider;
