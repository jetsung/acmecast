//! 内置的部署目标。

pub mod local;
pub mod ssh;

pub use local::{LocalInput, LocalTarget};
pub use ssh::{
    ResolvedSshConfig, RusshConnector, SshAuth, SshAuthSource, SshConnector, SshInput, SshTarget,
    SshTransport, probe_host,
};
