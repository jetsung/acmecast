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
pub(crate) mod provider_select;
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
///
/// `proxy_url` 是全局出网代理（`[proxy] url` / `ACMECAST_PROXY_URL`）：
/// 注入各提供商，供 `proxy_mode=inherit` 的凭据在挑战调用里继承；
/// `direct` / `custom` 模式的凭据不受影响。留空即全部直连。
/// `network_timeout` 是网络操作超时（`[network] timeout_secs`）：
/// 建 reqwest 客户端时施加请求级超时。
#[must_use]
pub fn default_dns_registry(
    proxy_url: Option<&str>,
    network_timeout: std::time::Duration,
) -> Arc<DnsProviderRegistry> {
    let mut registry = DnsProviderRegistry::new();
    registry
        .register(
            acmecast_dns::CloudflareProvider::new()
                .with_global_proxy_and_timeout(proxy_url, network_timeout),
        )
        .expect("内置 DNS 提供商不应重复注册");
    registry
        .register(
            acmecast_dns::AliyunProvider::new()
                .with_global_proxy_and_timeout(proxy_url, network_timeout),
        )
        .expect("内置 DNS 提供商不应重复注册");
    registry
        .register(
            acmecast_dns::TencentProvider::new()
                .with_global_proxy_and_timeout(proxy_url, network_timeout),
        )
        .expect("内置 DNS 提供商不应重复注册");
    registry
        .register(
            acmecast_dns::TencentEoProvider::new()
                .with_global_proxy_and_timeout(proxy_url, network_timeout),
        )
        .expect("内置 DNS 提供商不应重复注册");
    Arc::new(registry)
}

/// 注册全部内置部署目标的注册表。
///
/// `proxy_url` 是全局出网代理（`[proxy] url` / `ACMECAST_PROXY_URL`）：注入各
/// 部署目标，供 `proxy_mode=inherit` 的凭据继承；`direct`/`custom` 不受影响。
/// `network_timeout` 是网络操作超时（`[network] timeout_secs`）：注入各
/// 部署目标，约束 SSH 连接与传输、S3/WebDAV 上传、腾讯云 API 调用。
#[must_use]
pub fn default_deploy_registry(
    proxy_url: Option<&str>,
    network_timeout: std::time::Duration,
) -> Arc<DeploymentRegistry> {
    let mut registry = DeploymentRegistry::new();
    registry
        .register(acmecast_deploy::LocalTarget)
        .expect("内置部署目标不应重复注册");
    registry
        .register(acmecast_deploy::SshTarget::live_with_proxy(proxy_url, network_timeout))
        .expect("内置部署目标不应重复注册");
    registry
        .register(acmecast_deploy::TencentSslTarget::default())
        .expect("内置部署目标不应重复注册");
    registry
        .register(acmecast_deploy::S3Target::default())
        .expect("内置 S3 部署目标不应重复注册");
    registry
        .register(acmecast_deploy::WebdavTarget::default())
        .expect("内置 WebDAV 部署目标不应重复注册");
    Arc::new(registry)
}

/// 装配内置流水线步骤；返回的注册表在服务启动时注入 [`RuntimeState`](crate::RuntimeState)。
///
/// `propagation` 是 DNS-01 传播等待策略，来自服务配置的 `[propagation]` 段。
/// `resolver_entries` 是 `config.toml` 的 `[[resolvers]]` 条目，供传播等待
/// 的阶段 2/3 扩展内置解析器集合。
/// `proxy_url` 是全局出网代理（`[proxy] url` / `ACMECAST_PROXY_URL`）：
/// 传给 `cert.apply`——DoH 解析器直连失败时经它回退；提供商侧
/// `proxy_mode=inherit` 的凭据则经调用方构建的 DNS 注册表继承同一份取值
/// （见 [`default_dns_registry`]）。
#[must_use]
pub fn default_steps(
    db: DatabaseConnection,
    data_dir: PathBuf,
    dns: Arc<DnsProviderRegistry>,
    deploy: Arc<DeploymentRegistry>,
    propagation: acmecast_dns::PropagationPolicy,
    resolver_entries: Vec<acmecast_dns::ResolverEntry>,
    proxy_url: Option<&str>,
) -> StepRegistry {
    let mut steps = StepRegistry::new();
    // 到期检测需读证书库：为 cert.apply 注入 db，0/负数=跳过检测，其余按 not_after 判定。
    // 跳过分支需注入库中证书产物，故一并注入 data_dir。
    steps
        .register(CertApplyStep::with_policy_resolvers_db_and_data_dir(
            dns,
            propagation,
            resolver_entries,
            db.clone(),
            data_dir.clone(),
        ).with_proxy_url(proxy_url))
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

    /// DNS 注册表携带全局代理：配了代理（含未配）四家照常注册——
    /// 装配不因代理配置变化，`proxy_mode=inherit` 的凭据经提供商继承它。
    #[test]
    fn the_dns_registry_carries_the_global_proxy() {
        for proxy in [None, Some("socks5://127.0.0.1:7890")] {
            let registry = default_dns_registry(proxy, acmecast_core::NETWORK_TIMEOUT);
            for type_id in [
                "cloud.cloudflare",
                "cloud.aliyun",
                "cloud.tencent",
                "dns.tencent-eo",
            ] {
                registry
                    .require(type_id)
                    .unwrap_or_else(|e| panic!("{type_id} 应已注册（proxy={proxy:?}）: {e}"));
            }
        }
    }

    /// 注册表把全局代理读进提供商的 `inherit` 分支：非法代理值在传输层
    /// 构建时即被拒（不触网、不调替身）；`direct` 忽略全局代理，走注入的
    /// 直连替身——构建链与 `default_dns_registry` 一致，仅传输层换成替身。
    #[tokio::test]
    async fn inherit_reads_the_global_proxy_while_direct_ignores_it() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};

        use acmecast_dns::{
            CloudflareProvider, DnsProvider, HttpRequest, HttpResponse, HttpTransport, TxtRecord,
        };

        /// 记录调用次数的直连替身：被调用即证明走了直连传输层。
        #[derive(Debug, Default)]
        struct Counting {
            calls: AtomicUsize,
        }

        #[async_trait::async_trait]
        impl HttpTransport for Counting {
            async fn send(&self, _request: HttpRequest) -> acmecast_dns::Result<HttpResponse> {
                self.calls.fetch_add(1, Ordering::SeqCst);
                Ok(HttpResponse::new(200, r#"{"result":[],"success":true}"#))
            }
        }

        let transport = Arc::new(Counting::default());
        let provider = CloudflareProvider::with_transport(Arc::clone(&transport)
            as Arc<dyn HttpTransport>)
        .with_global_proxy_and_timeout(Some("ftp://127.0.0.1:21"), acmecast_core::NETWORK_TIMEOUT);
        let record = TxtRecord::new("example.com", "_acme-challenge.example.com", "value", 60);

        // inherit：读全局代理 → 非法值在传输层构建时被拒，替身未被调用。
        let inherit = serde_json::json!({ "api_token": "t", "proxy_mode": "inherit" });
        let error = provider
            .find_txt(&inherit, &record)
            .await
            .expect_err("inherit 读到非法全局代理应被拒");
        assert!(
            error.to_string().contains("socks5"),
            "错误应提示支持的 scheme: {error}"
        );
        assert_eq!(
            transport.calls.load(Ordering::SeqCst),
            0,
            "传输层构建失败不应发出请求"
        );

        // direct：忽略全局代理 → 走注入的直连替身。
        let direct = serde_json::json!({ "api_token": "t", "proxy_mode": "direct" });
        let _ = provider.find_txt(&direct, &record).await;
        assert_eq!(
            transport.calls.load(Ordering::SeqCst),
            1,
            "direct 应走注入的直连替身，不受全局代理影响"
        );
    }
}
