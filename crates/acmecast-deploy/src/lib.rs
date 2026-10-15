//! 证书部署：部署目标抽象与结果记录。
//!
//! 对应 `specs/cert-deployment/spec.md`。本 crate 定义「怎么把一份证书放到
//! 目标上去」，以及部署结果的记录；具体目标（本地文件、SSH 远程）各自实现。

pub mod deployer;
pub mod error;
pub mod registry;
pub mod ssh_host;
pub mod state;
pub mod target;
pub mod targets;

pub use deployer::Deployer;
pub use error::{Error, Result};
pub use registry::DeploymentRegistry;
pub use ssh_host::{SshHostFields, SSH_HOST_TYPE_ID, ssh_host_fields_schema};
pub use state::{
    DatabaseDeploymentState, DeploymentEntry, DeploymentPage, DeploymentQuery,
    DeploymentStateStore, InMemoryDeploymentState, target_key_for, target_ref_of,
};
pub use target::{CertMaterials, DeployMode, DeployOutcome, DeploymentTarget, parse_input};
pub use targets::{
    LocalInput, LocalTarget, ResolvedSshConfig, RusshConnector, SshAuth, SshAuthSource,
    SshConnector, SshInput, SshTarget, SshTransport, probe_host,
};
