//! 权威名字服务器直查。
//!
//! 公共解析器（DoH）看的是**它缓存里的世界**：我们写入前查过一次「不存在」，
//! 这个否定答案会按 SOA 的 minimum（常见 600–1800 秒）被缓存住——创建→删除→
//! 再重试的循环里，公共解析器会稳定地「看不见」新记录，等待永远超时。
//! CA（Let's Encrypt）是从根区递归、最终落到权威 NS 上取答案的；要模仿它的
//! 视角，最直接的办法就是**跳过所有中间缓存，直接问权威 NS**。
//!
//! 分两步：先用系统配置的解析器查 zone 的 NS 委派，再对每个 NS 的 IP
//! 单独发起 TXT 查询。任一 NS 有应答即采用其答案（权威服务器之间由
//! 传送机制保持一致）；NXDOMAIN/NODATA 是合法应答，意味着「还没有」。

use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use hickory_resolver::TokioResolver;
use hickory_resolver::config::{NameServerConfig, ResolverConfig, ResolverOpts};
use hickory_resolver::net::runtime::TokioRuntimeProvider;
use hickory_resolver::proto::rr::{RData, RecordType};
use tracing::debug;

use crate::error::{Error, Result};
use crate::propagation::DnsResolver;

/// 向权威 NS 取答案的两步查询，抽成 trait 以便测试替换。
#[async_trait]
pub trait Authority: Send + Sync + std::fmt::Debug {
    /// zone 的权威名字服务器 IP（先查 NS 委派，再把 NS 域名翻译成地址）。
    async fn name_server_ips(&self, zone: &str) -> Result<Vec<IpAddr>>;

    /// 直接向某个名字服务器查询 `name` 的 TXT 值。
    async fn txt_at(&self, ns: IpAddr, name: &str) -> Result<Vec<String>>;

    /// `name` 若有 CNAME 指向的目标就返回它（RFC 8555 要求 CA 跟随挑战 CNAME）。
    async fn cname_target(&self, name: &str) -> Result<Option<String>>;
}

/// 用 [`TokioResolver`] 实现的权威查询。
#[derive(Debug)]
pub struct AuthoritativeResolver {
    zone: String,
    name: String,
    authority: Arc<dyn Authority>,
}

impl AuthoritativeResolver {
    /// 针对某个 zone 装配真实权威查询器。
    #[must_use]
    pub fn new(zone: impl Into<String>) -> Self {
        let zone = zone.into();
        let name = format!("权威 NS（{zone}）");
        Self {
            zone,
            name,
            authority: Arc::new(HickoryAuthority::default()),
        }
    }

    /// 注入自定义的权威查询实现（测试用）。
    #[must_use]
    pub fn with_authority(
        zone: impl Into<String>,
        authority: Arc<dyn Authority>,
    ) -> Self {
        let zone = zone.into();
        let name = format!("权威 NS（{zone}）");
        Self {
            zone,
            name,
            authority,
        }
    }
}

#[async_trait]
impl DnsResolver for AuthoritativeResolver {
    fn name(&self) -> &str {
        &self.name
    }

    async fn lookup_txt(&self, name: &str) -> Result<Vec<String>> {
        let direct = self.query_zone(&self.zone, name).await?;
        if !direct.is_empty() {
            return Ok(direct);
        }

        // 权威侧没有记录：若这个名字被 CNAME 到别处（RFC 8555 的挑战委派，
        // 如 `_acme-challenge.a.example.com` → 独立托管的挑战 zone），CA 会
        // 跟随 CNAME 到目标 zone 取值，我们也必须问到目标 zone 的权威。
        let Ok(Some(target)) = self.authority.cname_target(name).await else {
            return Ok(direct);
        };
        // CNAME 答案自带结尾的点；统一去掉，交给各 Authority 实现按需补全。
        let target = target.trim_end_matches('.');
        for candidate in ancestor_zones(target) {
            let servers = match self.authority.name_server_ips(&candidate).await {
                Ok(servers) if !servers.is_empty() => servers,
                _ => continue,
            };
            return self.query_servers(&servers, target).await;
        }
        Ok(direct)
    }
}

impl AuthoritativeResolver {
    /// 查 zone 的 NS 并取值。
    async fn query_zone(&self, zone: &str, name: &str) -> Result<Vec<String>> {
        let servers = self.authority.name_server_ips(zone).await?;
        self.query_servers(&servers, name).await
    }

    /// 对每台 NS 直查；任一台有应答即采用其答案（权威服务器之间靠区域传送
    /// 保持一致），单台失败只跳过——一台挂掉的 NS 不该毁掉整次判定。
    /// 全部失败时报错：「查不了」≠「还没有」，混为一谈会把网络故障误判成传播未完成。
    async fn query_servers(&self, servers: &[IpAddr], name: &str) -> Result<Vec<String>> {
        if servers.is_empty() {
            return Err(Error::provider(format!(
                "找不到 zone `{}` 的任何权威名字服务器",
                self.zone
            )));
        }

        let mut values: Vec<String> = Vec::new();
        let mut answered = false;
        for server in servers {
            match self.authority.txt_at(*server, name).await {
                Ok(found) => {
                    answered = true;
                    for value in found {
                        if !values.contains(&value) {
                            values.push(value);
                        }
                    }
                }
                Err(error) => {
                    debug!(ns = %server, error = %error, "该权威 NS 查询失败，尝试下一台");
                }
            }
        }

        if answered {
            Ok(values)
        } else {
            Err(Error::provider(format!(
                "zone `{}` 的全部权威名字服务器查询失败",
                self.zone
            )))
        }
    }
}

/// 基于 hickory-resolver 的 [`Authority`] 实现。
#[derive(Default)]
pub struct HickoryAuthority {
    /// 用于查 NS 委派与 NS 域名解析的引导解析器（走系统 /etc/resolv.conf）。
    bootstrap: std::sync::OnceLock<TokioResolver>,
}

impl std::fmt::Debug for HickoryAuthority {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HickoryAuthority").finish_non_exhaustive()
    }
}

impl HickoryAuthority {
    fn bootstrap(&self) -> Result<&TokioResolver> {
        if self.bootstrap.get().is_none() {
            let resolver = TokioResolver::builder_tokio()
                .map_err(|error| Error::provider(format!("引导解析器构建失败: {error}")))?
                .build()
                .map_err(|error| Error::provider(format!("引导解析器构建失败: {error}")))?;
            let _ = self.bootstrap.set(resolver);
        }
        self.bootstrap
            .get()
            .ok_or_else(|| Error::provider("引导解析器不可用"))
    }
}

/// 每台权威 NS 的查询超时与重试次数。
const NS_TIMEOUT: Duration = Duration::from_secs(3);

#[async_trait]
impl Authority for HickoryAuthority {
    async fn name_server_ips(&self, zone: &str) -> Result<Vec<IpAddr>> {
        let resolver = self.bootstrap()?;
        let zone = fqdn(zone);

        let lookup = resolver
            .lookup(zone.clone(), RecordType::NS)
            .await
            .map_err(|error| {
                Error::provider(format!("查询 zone `{zone}` 的 NS 失败: {error}"))
            })?;

        let mut hosts: Vec<String> = Vec::new();
        for record in lookup.answers() {
            if let RData::NS(ns) = &record.data {
                hosts.push(ns.to_string());
            }
        }

        let mut ips: Vec<IpAddr> = Vec::new();
        for host in hosts {
            match resolver.lookup_ip(host.as_str()).await {
                Ok(lookup) => ips.extend(lookup.iter()),
                Err(error) => {
                    debug!(ns = %host, error = %error, "名字服务器域名解析失败，跳过");
                }
            }
        }
        ips.sort();
        ips.dedup();
        Ok(ips)
    }

    async fn cname_target(&self, name: &str) -> Result<Option<String>> {
        let resolver = self.bootstrap()?;
        match resolver.lookup(fqdn(name), RecordType::CNAME).await {
            Ok(lookup) => Ok(lookup.answers().iter().find_map(|record| match &record.data {
                RData::CNAME(cname) => Some(cname.to_string()),
                _ => None,
            })),
            // 没有 CNAME 是常态（否定应答），不算查询失败。
            Err(error) if error.is_nx_domain() || error.is_no_records_found() => Ok(None),
            Err(error) => Err(Error::provider(format!("查询 `{name}` 的 CNAME 失败: {error}"))),
        }
    }

    async fn txt_at(&self, ns: IpAddr, name: &str) -> Result<Vec<String>> {
        // 只配这一台 NS、禁掉负缓存信任、单次尝试：问的就是**它**的现在，
        // 不接受任何中间人（含 hickory 自己的缓存语义）的答案。
        let mut server = NameServerConfig::udp(ns);
        server.trust_negative_responses = false;
        let config = ResolverConfig::from_parts(None, vec![], vec![server]);
        let mut options = ResolverOpts::default();
        options.attempts = 1;
        options.timeout = NS_TIMEOUT;
        options.recursion_desired = false;

        let resolver = TokioResolver::builder_with_config(config, TokioRuntimeProvider::default())
            .with_options(options)
            .build()
            .map_err(|error| Error::provider(format!("NS {ns} 解析器构建失败: {error}")))?;

        match resolver.lookup(fqdn(name), RecordType::TXT).await {
            Ok(lookup) => Ok(lookup
                .answers()
                .iter()
                .filter_map(|record| match &record.data {
                    RData::TXT(txt) => Some(txt.to_string().trim_matches('"').to_owned()),
                    _ => None,
                })
                .collect()),
            // 「域名不存在」与「没有 TXT」都是合法的权威否定应答：
            // 意思是「还没有」，不是查询失败。
            Err(error) if error.is_nx_domain() || error.is_no_records_found() => {
                Ok(Vec::new())
            }
            Err(error) => Err(Error::provider(format!("NS {ns} 查询失败: {error}"))),
        }
    }
}

/// 从目标名向上逐层找候选 zone：`a.b.example.com` → `b.example.com`、`example.com`。
///
/// CNAME 目标的真正 zone 注册在哪一层没有通用规则，逐层试 NS 委派、
/// 问到 NS 的那层就是 zone。
fn ancestor_zones(target: &str) -> Vec<String> {
    let trimmed = target.trim_end_matches('.');
    let labels: Vec<&str> = trimmed.split('.').collect();
    // 至少留两级（`example.com`），从最靠近目标的层开始试。
    (1..labels.len().saturating_sub(1))
        .map(|skip| labels[skip..].join("."))
        .collect()
}

/// 补全 FQDN 结尾的点，避免被当作相对名拼上搜索域。
fn fqdn(name: &str) -> String {
    if name.ends_with('.') {
        name.to_owned()
    } else {
        format!("{name}.")
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::redundant_clone)]

    use std::sync::Mutex;

    use super::*;

    /// 单台 NS 的脚本化结果。
    type NsAnswer = std::result::Result<Vec<String>, ()>;

    /// 可编程的假权威：NS 列表固定，按 (NS, 查询名) 返回脚本化结果。
    #[derive(Debug)]
    struct FakeAuthority {
        servers: Vec<IpAddr>,
        answers: Mutex<std::collections::HashMap<(IpAddr, String), NsAnswer>>,
        cnames: Mutex<std::collections::HashMap<String, String>>,
    }

    impl FakeAuthority {
        fn new(servers: &[&str]) -> Self {
            Self {
                servers: servers.iter().map(|s| s.parse().unwrap()).collect(),
                answers: Mutex::new(std::collections::HashMap::new()),
                cnames: Mutex::new(std::collections::HashMap::new()),
            }
        }

        fn answer(&self, ns: &str, name: &str, values: &[&str]) {
            self.answers.lock().unwrap().insert(
                (ns.parse().unwrap(), name.to_owned()),
                Ok(values.iter().map(|v| (*v).to_owned()).collect()),
            );
        }

        fn fail(&self, ns: &str, name: &str) {
            self.answers
                .lock()
                .unwrap()
                .insert((ns.parse().unwrap(), name.to_owned()), Err(()));
        }

        fn cname(&self, name: &str, target: &str) {
            self.cnames
                .lock()
                .unwrap()
                .insert(name.to_owned(), target.to_owned());
        }
    }

    #[async_trait]
    impl Authority for FakeAuthority {
        async fn name_server_ips(&self, _zone: &str) -> Result<Vec<IpAddr>> {
            Ok(self.servers.clone())
        }

        async fn txt_at(&self, ns: IpAddr, name: &str) -> Result<Vec<String>> {
            let answers = self.answers.lock().unwrap();
            answers
                .get(&(ns, name.to_owned()))
                .cloned()
                .unwrap_or_else(|| Ok(Vec::new()))
                .map_err(|()| Error::provider(format!("假失败 {ns}")))
        }

        async fn cname_target(&self, name: &str) -> Result<Option<String>> {
            Ok(self.cnames.lock().unwrap().get(name).cloned())
        }
    }

    #[tokio::test]
    async fn one_answering_ns_is_enough_even_if_another_is_down() {
        let authority = Arc::new(FakeAuthority::new(&["1.1.1.1", "9.9.9.9"]));
        authority.answer("1.1.1.1", "_acme-challenge.example.com", &["v-1"]);
        authority.fail("9.9.9.9", "_acme-challenge.example.com");

        let resolver = AuthoritativeResolver::with_authority(
            "example.com",
            Arc::clone(&authority) as Arc<dyn Authority>,
        );
        assert_eq!(
            resolver
                .lookup_txt("_acme-challenge.example.com")
                .await
                .unwrap(),
            vec!["v-1".to_owned()]
        );
    }

    #[tokio::test]
    async fn empty_answer_is_a_valid_not_yet() {
        let authority = Arc::new(FakeAuthority::new(&["1.1.1.1"]));
        authority.answer("1.1.1.1", "x.example.com", &[]);

        let resolver =
            AuthoritativeResolver::with_authority("example.com", authority as Arc<dyn Authority>);
        assert!(resolver.lookup_txt("x.example.com").await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn all_ns_failing_is_an_error_not_an_empty_answer() {
        // 全部查询失败 ≠ 「还没有记录」：前者要报「不可达」，否则会误判传播完成。
        let authority = Arc::new(FakeAuthority::new(&["1.1.1.1", "9.9.9.9"]));
        authority.fail("1.1.1.1", "x.example.com");
        authority.fail("9.9.9.9", "x.example.com");

        let resolver =
            AuthoritativeResolver::with_authority("example.com", authority as Arc<dyn Authority>);
        let err = resolver.lookup_txt("x.example.com").await.unwrap_err();
        assert!(err.to_string().contains("全部权威名字服务器查询失败"), "{err}");
    }

    #[tokio::test]
    async fn zone_without_ns_is_an_error() {
        let authority = Arc::new(FakeAuthority::new(&[]));
        let resolver =
            AuthoritativeResolver::with_authority("example.com", authority as Arc<dyn Authority>);
        let err = resolver.lookup_txt("x.example.com").await.unwrap_err();
        assert!(err.to_string().contains("找不到"), "{err}");
    }

    #[tokio::test]
    async fn cname_challenge_is_followed_to_the_target() {
        // spec：RFC 8555 允许 `_acme-challenge` 用 CNAME 委派到别处；
        // CA 会跟随目标取值，我们的可见性判定也必须跟随。
        let authority = Arc::new(FakeAuthority::new(&["1.1.1.1"]));
        authority.answer("1.1.1.1", "_acme-challenge.x.example.com", &[]);
        authority.answer("1.1.1.1", "_acme-challenge.iothive.cn", &["v-9"]);
        authority.cname(
            "_acme-challenge.x.example.com",
            "_acme-challenge.iothive.cn.",
        );

        let resolver =
            AuthoritativeResolver::with_authority("example.com", authority as Arc<dyn Authority>);
        assert_eq!(
            resolver
                .lookup_txt("_acme-challenge.x.example.com")
                .await
                .unwrap(),
            vec!["v-9".to_owned()]
        );
    }

    #[test]
    fn ancestor_zones_walks_up_but_keeps_two_labels() {
        assert_eq!(
            ancestor_zones("_acme-challenge.iothive.cn."),
            vec!["iothive.cn".to_owned()]
        );
        assert_eq!(
            ancestor_zones("a.b.example.com"),
            vec!["b.example.com".to_owned(), "example.com".to_owned()]
        );
        // 两级名没有更上的可试层级（不能退化成只剩 `com`）。
        assert!(ancestor_zones("example.com").is_empty());
    }

    #[test]
    fn display_name_mentions_the_zone() {
        let resolver = AuthoritativeResolver::with_authority(
            "example.com",
            Arc::new(FakeAuthority::new(&[])) as Arc<dyn Authority>,
        );
        assert_eq!(resolver.name(), "权威 NS（example.com）");
    }

    #[test]
    fn fqdn_appends_only_a_missing_dot() {
        assert_eq!(fqdn("example.com"), "example.com.");
        assert_eq!(fqdn("example.com."), "example.com.");
    }
}
