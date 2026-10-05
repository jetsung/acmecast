//! 权威名字服务器直查。
//!
//! 公共解析器（DoH）看的是**它缓存里的世界**：我们写入前查过一次「不存在」，
//! 这个否定答案会按 SOA 的 minimum（常见 600–1800 秒）被缓存住——创建→删除→
//! 再重试的循环里，公共解析器会稳定地「看不见」新记录，等待永远超时。
//! CA（Let's Encrypt）是从根区递归、最终落到权威 NS 上取答案的；要模仿它的
//! 视角，最直接的办法就是**跳过所有中间缓存，直接问权威 NS**。
//!
//! 分两步：先经可信公共 DNS 查 zone 的 NS 委派，再对每个 NS 的 IP
//! 单独发起 TXT+CNAME 查询。**全部应答 NS 一致**才算可见（商有 DNS 的
//! anycast POP 间有同步延迟，打到滞后的 POP 的 CA 会吃 NXDOMAIN）；
//! 单台无应答只跳过，全部失败才报错。NXDOMAIN/NODATA 是合法应答，
//! 意味着「还没有」。
//! CNAME 也必须直查权威：公共解析器缓存的过期 CNAME 会把跟随者引到
//! 错误的一侧（删除委派后的 TTL 窗口内尤其致命）。

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

    /// 直接向某个名字服务器一次取回 `name` 处的 TXT 值与 CNAME 目标。
    ///
    /// CNAME 必须和 TXT 一样走权威直查：公共解析器会按 TTL 缓存旧答案，
    /// 删除委派后的一段时间里仍「看得见」过期 CNAME，把检查器引去目标侧
    /// 取值、对真实写入的 TXT 报「不可见」。
    async fn records_at(&self, ns: IpAddr, name: &str) -> Result<(Vec<String>, Option<String>)>;
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
    pub fn with_authority(zone: impl Into<String>, authority: Arc<dyn Authority>) -> Self {
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
        let servers = self.authority.name_server_ips(&self.zone).await?;
        if servers.is_empty() {
            return Err(Error::provider(format!(
                "找不到 zone `{}` 的任何权威名字服务器",
                self.zone
            )));
        }

        // CNAME 判定同样只信权威直查：一旦有 CNAME，CA 就只跟随它取值
        // （RFC 8555），同名处的 TXT 对递归解析器不可见——阿里云会把两者
        // 一起返回，先查直连 TXT 就是「看见」CA 看不到的假记录。
        // 商有 DNS（阿里云 anycast）各 POP 之间有同步延迟：**任一台**看得到就
        // 放行，CA 的二级验证随机打到滞后的 POP 就会吃 NXDOMAIN，所以要求
        // 全部应答 NS 一致才算可见；单台查询失败只跳过，全部失败才算错。
        let mut responses: Vec<(Vec<String>, Option<String>)> = Vec::new();
        for server in &servers {
            match self.authority.records_at(*server, name).await {
                Ok(response) => responses.push(response),
                Err(error) => {
                    debug!(ns = %server, error = %error, "该权威 NS 查询失败，尝试下一台");
                }
            }
        }
        let Some(first) = responses.first() else {
            return Err(Error::provider(format!(
                "zone `{}` 的全部权威名字服务器查询失败",
                self.zone
            )));
        };

        if let Some(target) = &first.1 {
            // 全部应答 NS 必须报同一个 CNAME 才跟随；不一致说明委派正在
            // 增删同步中，回答「还没有」，等下一轮。
            if responses
                .iter()
                .all(|(_, cname)| cname.as_deref() == Some(target.as_str()))
            {
                return self.follow_cname(target).await;
            }
            return Ok(Vec::new());
        }
        if responses.iter().any(|(_, cname)| cname.is_some()) {
            return Ok(Vec::new());
        }

        let mut values: Vec<String> = Vec::new();
        for (txt, _) in &responses {
            if txt.is_empty() {
                return Ok(Vec::new()); // 有 NS 还没同步到，继续等
            }
            for value in txt {
                if !values.contains(value) {
                    values.push(value.clone());
                }
            }
        }
        Ok(values)
    }
}

impl AuthoritativeResolver {
    /// 跟随挑战委派（如 `_acme-challenge.a.example.com` → 独立托管的挑战 zone）：
    /// 只有目标 zone 的权威答案才算数；定位不到目标 zone 时回答「还没有」——
    /// 宁可等待超时，也不拿被 CNAME 遮蔽的假可见去骗 CA。
    async fn follow_cname(&self, raw_target: &str) -> Result<Vec<String>> {
        // CNAME 答案自带结尾的点；统一去掉，交给各 Authority 实现按需补全。
        let target = raw_target.trim_end_matches('.');
        for candidate in ancestor_zones(target) {
            let Ok(servers) = self.authority.name_server_ips(&candidate).await else {
                continue;
            };
            if servers.is_empty() {
                continue;
            }
            return self.query_servers(&servers, target).await;
        }
        Ok(Vec::new())
    }

    /// 对每台 NS 直查，要求**全部应答 NS** 都有记录才算可见——商有 DNS 的
    /// anycast POP 间存在同步延迟，任一台看得到就放行会让 CA 随机打到滞后的
    /// POP 时吃 NXDOMAIN。单台失败只跳过；全部失败时报错：
    /// 「查不了」≠「还没有」，混为一谈会把网络故障误判成传播未完成。
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
                    if found.is_empty() {
                        return Ok(Vec::new()); // 有 NS 还没同步到，继续等
                    }
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
    /// 用于查 NS 委派与 NS 地址解析的引导解析器（固定公共 DNS，不读系统 resolv.conf）。
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
            let servers = crate::builtin::BOOTSTRAP_SERVERS
                .iter()
                .filter_map(|ip| ip.parse::<IpAddr>().ok())
                .map(NameServerConfig::udp)
                .collect();
            let config = ResolverConfig::from_parts(None, vec![], servers);
            let resolver =
                TokioResolver::builder_with_config(config, TokioRuntimeProvider::default())
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
            .map_err(|error| Error::provider(format!("查询 zone `{zone}` 的 NS 失败: {error}")))?;

        let mut hosts: Vec<String> = Vec::new();
        for record in lookup.answers() {
            if let RData::NS(ns) = &record.data {
                hosts.push(ns.to_string());
            }
        }

        let mut ips: Vec<IpAddr> = Vec::new();
        // 逐型查 A/AAAA，不用 `lookup_ip`：它的默认策略是「AAAA 有值就不查 A」，
        // 遇到只有 IPv6 可达地址被优先返回、而本机没有 IPv6 出口时，
        // 全部 NS 查询都会「no connections available」，IPv4 地址明明存在却拿不到。
        for host in hosts {
            for record_type in [RecordType::A, RecordType::AAAA] {
                match resolver.lookup(fqdn(&host), record_type).await {
                    Ok(lookup) => {
                        for record in lookup.answers() {
                            match &record.data {
                                RData::A(v4) => ips.push(IpAddr::V4(v4.0)),
                                RData::AAAA(v6) => ips.push(IpAddr::V6(v6.0)),
                                _ => {}
                            }
                        }
                    }
                    Err(error) => {
                        debug!(ns = %host, error = %error, "名字服务器地址解析失败，跳过");
                    }
                }
            }
        }
        ips.sort();
        ips.dedup();
        Ok(ips)
    }

    async fn records_at(&self, ns: IpAddr, name: &str) -> Result<(Vec<String>, Option<String>)> {
        let resolver = ns_resolver(ns)?;
        let name = fqdn(name);
        let txt = txt_values(&resolver, &name).await;
        let cname = cname_value(&resolver, &name).await;
        Ok((txt?, cname?))
    }

    async fn txt_at(&self, ns: IpAddr, name: &str) -> Result<Vec<String>> {
        let resolver = ns_resolver(ns)?;
        txt_values(&resolver, &fqdn(name)).await
    }
}

/// 单独指向某一台权威 NS 的解析器。
///
/// 只配这一台、禁掉负缓存信任、单次尝试：问的就是**它**的现在，
/// 不接受任何中间人（含 hickory 自己的缓存语义）的答案。
fn ns_resolver(ns: IpAddr) -> Result<TokioResolver> {
    let mut server = NameServerConfig::udp(ns);
    server.trust_negative_responses = false;
    let config = ResolverConfig::from_parts(None, vec![], vec![server]);
    let mut options = ResolverOpts::default();
    options.attempts = 1;
    options.timeout = NS_TIMEOUT;
    options.recursion_desired = false;

    TokioResolver::builder_with_config(config, TokioRuntimeProvider::default())
        .with_options(options)
        .build()
        .map_err(|error| Error::provider(format!("NS {ns} 解析器构建失败: {error}")))
}

async fn txt_values(resolver: &TokioResolver, name: &str) -> Result<Vec<String>> {
    match resolver.lookup(name.to_owned(), RecordType::TXT).await {
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
        Err(error) if error.is_nx_domain() || error.is_no_records_found() => Ok(Vec::new()),
        Err(error) => Err(Error::provider(format!("NS 查询失败: {error}"))),
    }
}

async fn cname_value(resolver: &TokioResolver, name: &str) -> Result<Option<String>> {
    match resolver.lookup(name.to_owned(), RecordType::CNAME).await {
        Ok(lookup) => Ok(lookup
            .answers()
            .iter()
            .find_map(|record| match &record.data {
                RData::CNAME(cname) => Some(cname.to_string()),
                _ => None,
            })),
        // 没有 CNAME 是常态（否定应答），不算查询失败。
        Err(error) if error.is_nx_domain() || error.is_no_records_found() => Ok(None),
        Err(error) => Err(Error::provider(format!("NS 查询失败: {error}"))),
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
        per_ns_cnames: Mutex<std::collections::HashMap<(IpAddr, String), String>>,
    }

    impl FakeAuthority {
        fn new(servers: &[&str]) -> Self {
            Self {
                servers: servers.iter().map(|s| s.parse().unwrap()).collect(),
                answers: Mutex::new(std::collections::HashMap::new()),
                cnames: Mutex::new(std::collections::HashMap::new()),
                per_ns_cnames: Mutex::new(std::collections::HashMap::new()),
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

        fn cname_at(&self, ns: &str, name: &str, target: &str) {
            self.per_ns_cnames
                .lock()
                .unwrap()
                .insert((ns.parse().unwrap(), name.to_owned()), target.to_owned());
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

        async fn records_at(
            &self,
            ns: IpAddr,
            name: &str,
        ) -> Result<(Vec<String>, Option<String>)> {
            let txt = self.txt_at(ns, name).await?;
            let per_ns = self.per_ns_cnames.lock().unwrap();
            if let Some(cname) = per_ns.get(&(ns, name.to_owned())) {
                return Ok((txt, Some(cname.clone())));
            }
            drop(per_ns);
            let cname = self.cnames.lock().unwrap().get(name).cloned();
            Ok((txt, cname))
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
    async fn lagging_ns_holds_visibility_back_until_all_answered_ns_agree() {
        // run 33 的教训：第一台 NS 已同步到 TXT 就放行，CA 二级验证随机
        // 打到还没同步的 POP，拿回 NXDOMAIN。
        let authority = Arc::new(FakeAuthority::new(&["1.1.1.1", "9.9.9.9"]));
        authority.answer("1.1.1.1", "x.example.com", &["v-1"]);
        authority.answer("9.9.9.9", "x.example.com", &[]);

        let resolver =
            AuthoritativeResolver::with_authority("example.com", authority as Arc<dyn Authority>);
        assert!(
            resolver
                .lookup_txt("x.example.com")
                .await
                .unwrap()
                .is_empty(),
            "有应答 NS 还没同步到记录，不能报可见"
        );
    }

    #[tokio::test]
    async fn conflicting_cname_answers_are_not_followed() {
        // 委派增删正在同步、各 NS 答案不一致时，宁答「还没有」，不猜哪台对。
        let authority = Arc::new(FakeAuthority::new(&["1.1.1.1", "9.9.9.9"]));
        authority.cname_at("1.1.1.1", "x.example.com", "a.other.zone.");
        authority.cname_at("9.9.9.9", "x.example.com", "b.other.zone.");

        let resolver =
            AuthoritativeResolver::with_authority("example.com", authority as Arc<dyn Authority>);
        assert!(
            resolver
                .lookup_txt("x.example.com")
                .await
                .unwrap()
                .is_empty(),
            "各 NS 的 CNAME 不一致，说明还在同步，不能跟随"
        );
    }

    #[tokio::test]
    async fn empty_answer_is_a_valid_not_yet() {
        let authority = Arc::new(FakeAuthority::new(&["1.1.1.1"]));
        authority.answer("1.1.1.1", "x.example.com", &[]);

        let resolver =
            AuthoritativeResolver::with_authority("example.com", authority as Arc<dyn Authority>);
        assert!(
            resolver
                .lookup_txt("x.example.com")
                .await
                .unwrap()
                .is_empty()
        );
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
        assert!(
            err.to_string().contains("全部权威名字服务器查询失败"),
            "{err}"
        );
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

    #[tokio::test]
    async fn txt_at_a_cname_name_is_masked_and_does_not_count_as_visible() {
        // run 30 的真实事故：阿里云权威 NS 会把 CNAME 与同名 TXT 一起返回，
        // 但 CA 从递归视角只跟随 CNAME——目标没有 TXT 时，直连 TXT 是假可见。
        let authority = Arc::new(FakeAuthority::new(&["1.1.1.1"]));
        authority.answer("1.1.1.1", "_acme-challenge.x.example.com", &["v-stale"]);
        authority.answer("1.1.1.1", "_acme-challenge.other.zone", &[]);
        authority.cname(
            "_acme-challenge.x.example.com",
            "_acme-challenge.other.zone.",
        );

        let resolver =
            AuthoritativeResolver::with_authority("example.com", authority as Arc<dyn Authority>);
        assert!(
            resolver
                .lookup_txt("_acme-challenge.x.example.com")
                .await
                .unwrap()
                .is_empty(),
            "CNAME 遮蔽下的直连 TXT 不该被当作可见"
        );
    }

    #[tokio::test]
    async fn target_txt_wins_when_alias_also_has_masked_txt() {
        let authority = Arc::new(FakeAuthority::new(&["1.1.1.1"]));
        authority.answer("1.1.1.1", "_acme-challenge.x.example.com", &["v-stale"]);
        authority.answer("1.1.1.1", "_acme-challenge.other.zone", &["v-real"]);
        authority.cname(
            "_acme-challenge.x.example.com",
            "_acme-challenge.other.zone.",
        );

        let resolver =
            AuthoritativeResolver::with_authority("example.com", authority as Arc<dyn Authority>);
        assert_eq!(
            resolver
                .lookup_txt("_acme-challenge.x.example.com")
                .await
                .unwrap(),
            vec!["v-real".to_owned()],
            "采用目标侧的值，被遮蔽的直连值不参与"
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
