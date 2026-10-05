//! 凭据体系：类型化的凭据定义、注册表与按标识注入。
//!
//! 对应 `specs/access-credential/spec.md`。本 crate 负责「凭据长什么样、怎么校验」；
//! 存储归 `acmecast-store`，加解密原语归 [`acmecast_core::CredentialCipher`]。

pub mod credential;
pub mod credential_store;
pub mod error;
pub mod registry;

pub use credential::acme_account::{
    AcmeAccountFields, AcmeAccountInput, AcmeAccountType, TYPE_ID as ACME_ACCOUNT_TYPE_ID,
};
pub use credential::{ConnectivityOutcome, CredentialType};
pub use credential_store::{
    CREDENTIAL_REFERENCE_FIELD, CredentialStore, PipelineReference, ResolvedCredential,
};
pub use error::{Error, Result};
pub use registry::CredentialRegistry;
