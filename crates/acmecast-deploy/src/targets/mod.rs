//! 内置的部署目标。

pub mod local;
pub mod ssh;
pub mod s3;
pub mod tencent;
pub mod webdav;

pub use local::{LocalInput, LocalTarget};
pub use ssh::{
    ResolvedSshConfig, RusshConnector, SshAuth, SshAuthSource, SshConnector, SshInput, SshTarget,
    SshTransport, probe_host,
};
pub use s3::{S3Input, S3Target};
pub use tencent::{TencentSslInput, TencentSslTarget};
pub use webdav::{WebdavInput, WebdavTarget};
