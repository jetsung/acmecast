//! 内置流水线步骤：把 ACME 申请、证书入库与部署串成可执行的流水线。
//!
//! 领域 crate（`acmecast-acme`、`acmecast-dns`、`acmecast-deploy`）各自提供
//! 能力，这里把它们编排成 [`acmecast_pipeline::PipelineStep`]——server 启动
//! 时经 [`default_steps`] 注册进步骤注册表，流水线配置按 `type_id` 引用。
//!
//! 步骤是**无状态单例**：需要的服务（DNS 注册表、部署注册表、数据库、
//! 数据目录）在装配时注入，执行期只从 [`StepContext`](acmecast_pipeline::StepContext)
//! 拿输入、凭据与产物。

pub mod apply;
pub mod deploy;
pub mod store;

use std::path::PathBuf;
use std::sync::Arc;

use acmecast_deploy::registry::DeploymentRegistry;
use acmecast_deploy::state::DatabaseDeploymentState;
use acmecast_dns::registry::DnsProviderRegistry;
use acmecast_pipeline::StepRegistry;
use sea_orm::DatabaseConnection;

pub use apply::CertApplyStep;
pub use deploy::CertDeployStep;
pub use store::CertStoreStep;

/// 把领域 crate 的错误包进流水线错误。
///
/// 各领域错误类型彼此独立，流水线不依赖它们；执行日志里保留前缀
/// 即可定位到来源。
pub(crate) fn domain_error(
    context: &str,
    error: impl std::fmt::Display,
) -> acmecast_pipeline::Error {
    acmecast_pipeline::Error::Core(acmecast_core::Error::Internal(format!(
        "{context}: {error}"
    )))
}

/// 注册全部内置 DNS 提供商的注册表。
#[must_use]
pub fn default_dns_registry() -> Arc<DnsProviderRegistry> {
    let mut registry = DnsProviderRegistry::new();
    registry
        .register(acmecast_dns::CloudflareProvider::new())
        .expect("内置 DNS 提供商不应重复注册");
    registry
        .register(acmecast_dns::AliyunProvider::new())
        .expect("内置 DNS 提供商不应重复注册");
    registry
        .register(acmecast_dns::TencentProvider::new())
        .expect("内置 DNS 提供商不应重复注册");
    registry
        .register(acmecast_dns::TencentEoProvider::new())
        .expect("内置 DNS 提供商不应重复注册");
    Arc::new(registry)
}

/// 注册全部内置部署目标的注册表。
#[must_use]
pub fn default_deploy_registry() -> Arc<DeploymentRegistry> {
    let mut registry = DeploymentRegistry::new();
    registry
        .register(acmecast_deploy::LocalTarget)
        .expect("内置部署目标不应重复注册");
    registry
        .register(acmecast_deploy::SshTarget::live())
        .expect("内置部署目标不应重复注册");
    Arc::new(registry)
}

/// 装配内置流水线步骤；返回的注册表在服务启动时注入 [`RuntimeState`](crate::RuntimeState)。
///
/// `propagation` 是 DNS-01 传播等待策略，来自服务配置的 `[propagation]` 段。
#[must_use]
pub fn default_steps(
    db: DatabaseConnection,
    data_dir: PathBuf,
    dns: Arc<DnsProviderRegistry>,
    deploy: Arc<DeploymentRegistry>,
    propagation: acmecast_dns::PropagationPolicy,
) -> StepRegistry {
    let mut steps = StepRegistry::new();
    steps
        .register(CertApplyStep::with_policy(dns, propagation))
        .expect("内置步骤不应重复注册");
    steps
        .register(CertStoreStep::new(db.clone(), data_dir))
        .expect("内置步骤不应重复注册");
    let state = Arc::new(DatabaseDeploymentState::new(db));
    steps
        .register(CertDeployStep::new(deploy, state))
        .expect("内置步骤不应重复注册");
    steps
}

#[cfg(test)]
mod tests {
    use super::*;
    use acmecast_pipeline::PipelineStep;

    /// 通知事件常量必须与步骤 `type_id` 同源：事件订阅过滤靠字符串匹配，
    /// 常量漂移会导致「配置了订阅却永远不触发」的静默失效。
    #[test]
    fn notification_event_constants_match_step_type_ids() {
        let dns = Arc::new(DnsProviderRegistry::new());
        let apply = CertApplyStep::new(dns);
        assert_eq!(apply.type_id(), acmecast_notify::EVENT_CERT_APPLY);

        let deploy = CertDeployStep::new(
            Arc::new(DeploymentRegistry::new()),
            Arc::new(DatabaseDeploymentState::new(
                sea_orm::DatabaseConnection::Disconnected,
            )),
        );
        assert_eq!(deploy.type_id(), acmecast_notify::EVENT_CERT_DEPLOY);
    }
}
