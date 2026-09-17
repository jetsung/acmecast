//! 内置的部署目标。

pub mod local;
pub mod ssh;

pub use local::{LocalInput, LocalTarget};
pub use ssh::{RusshConnector, SshAuth, SshConnector, SshInput, SshTarget, SshTransport};
