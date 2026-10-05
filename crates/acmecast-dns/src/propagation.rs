//! 记录的传播等待。
//!
//! 写完 TXT 记录并不等于它能被查到：厂商的 API 立刻承认，但 DNS 世界还要
//! 等各级缓存走完。所以记录写入后、通知 CA 之前，我们必须按 CA 的视角
//! 确认它真的查得到。
//!
//! 分两段：先向提供商侧确认记录确实收下了（它都不认，下游无从谈起），
//! 再逐个询问解析器——**只要有一家**看到期望值就算就绪。要求「全部看到」
//! 会把等待绑死在最慢（或根本不可达）的那家解析器上。
//!
//! 默认用 [`crate::authoritative::AuthoritativeResolver`] **直查权威 NS**：
//! 公共解析器的否定缓存（NXDOMAIN 按 SOA minimum 缓存，常见 600–1800 秒）
//! 会在「写入→删除→重试」的循环里稳定地查不到新记录，等待永远超时。
//! `ACMECAST_DOH_RESOLVERS` 可显式改回公共解析器集合（或 `none` 跳过等待）。

use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use serde_json::Value;
use tokio::time::sleep;
use tracing::{debug, info, warn};

use crate::error::{Error, Result};
use crate::http::{HttpRequest, HttpTransport, encode};
use crate::provider::{DnsProvider, TxtRecord};

/// TXT 记录的 DNS 类型号。
const TXT_TYPE: u64 = 16;

/// 一个「解析器视角」：从某个位置（公共递归或权威 NS）回答「这条 TXT 现在查得到吗」。
#[async_trait]
pub trait DnsResolver: Send + Sync + std::fmt::Debug {
    /// 解析器名称，用于错误信息——「Google DNS 上还看不到」比「某个解析器上」有用。
    fn name(&self) -> &str;

    /// 查询某个名字下的 TXT 记录值。
    async fn lookup_txt(&self, name: &str) -> Result<Vec<String>>;
}

/// 走 DNS-over-HTTPS 的解析器。
///
/// 选 DoH 而非原生 DNS 查询：它复用本 crate 已有的 [`HttpTransport`]，
/// 于是「多个解析器」不过是多个 URL，测试也能用同一套传输层替身。
#[derive(Debug)]
pub struct DohResolver {
    http: Arc<dyn HttpTransport>,
    name: String,
    endpoint: String,
}

impl DohResolver {
    /// Google Public DNS。
    #[must_use]
    pub fn google(http: Arc<dyn HttpTransport>) -> Self {
        Self {
            http,
            name: "Google DNS".to_owned(),
            endpoint: "https://dns.google/resolve".to_owned(),
        }
    }

    /// Cloudflare 1.1.1.1。
    #[must_use]
    pub fn cloudflare(http: Arc<dyn HttpTransport>) -> Self {
        Self {
            http,
            name: "Cloudflare DNS".to_owned(),
            endpoint: "https://cloudflare-dns.com/dns-query".to_owned(),
        }
    }

    /// 腾讯 DNSPod 公共解析。
    #[must_use]
    pub fn tencent(http: Arc<dyn HttpTransport>) -> Self {
        Self {
            http,
            name: "腾讯 DNSPod".to_owned(),
            endpoint: "https://1.12.12.12/resolve".to_owned(),
        }
    }

    /// 阿里公共解析。
    #[must_use]
    pub fn aliyun(http: Arc<dyn HttpTransport>) -> Self {
        Self {
            http,
            name: "阿里 DNS".to_owned(),
            endpoint: "https://120.53.53.53/resolve".to_owned(),
        }
    }

    /// 任意 DNS-over-JSON 端点（遵循 RFC 8484 之外的 Google JSON 约定，
    /// 腾讯/阿里/Cloudflare/Google 均兼容 `?name=&type=` 与 `Accept: application/dns-json`）。
    #[must_use]
    pub fn custom(
        http: Arc<dyn HttpTransport>,
        name: impl Into<String>,
        endpoint: impl Into<String>,
    ) -> Self {
        Self {
            http,
            name: name.into(),
            endpoint: endpoint.into(),
        }
    }
}

#[async_trait]
impl DnsResolver for DohResolver {
    fn name(&self) -> &str {
        &self.name
    }

    async fn lookup_txt(&self, name: &str) -> Result<Vec<String>> {
        let url = format!("{}?name={}&type=TXT", self.endpoint, encode(name));
        let request = HttpRequest::new("GET", url).with_header("Accept", "application/dns-json");

        let response = self.http.send(request).await?;
        if !response.is_success() {
            return Err(Error::provider(format!(
                "{} 查询 `{name}` 失败（HTTP {}）",
                self.name, response.status
            )));
        }

        let body = response.json()?;

        // TXT 记录的 `data` 是带引号的字符串，取出来要脱掉引号。
        Ok(body["Answer"]
            .as_array()
            .map(|answers| {
                answers
                    .iter()
                    .filter(|answer| answer["type"].as_u64() == Some(TXT_TYPE))
                    .filter_map(|answer| answer["data"].as_str())
                    .map(|data| data.trim_matches('"').to_owned())
                    .collect()
            })
            .unwrap_or_default())
    }
}

/// 默认的解析器集合。
///
/// 内置四家：腾讯 DNSPod、阿里、Google、Cloudflare。默认四家全用——
/// 单个解析器可能因为自身缓存、线路或**网络可达性**问题给出滞后甚至查不到的答案
/// （Google/Cloudflare 的 DoH 在部分网络环境下不可达），多问几家既能降低
/// 「本地已生效、CA 那边还没有」的概率，也能避免一家被墙就整体卡死：
/// 查询失败的解析器按「未传播」处理，只要有一家看到即可继续。
///
/// 可用环境变量 `ACMECAST_DOH_RESOLVERS` 覆盖默认集合：逗号分隔的
/// `名称=端点`（或仅端点，名称取主机名），如
/// `ACMECAST_DOH_RESOLVERS="dns.google=https://dns.google/resolve,https://1.12.12.12/resolve"`。
/// 设为 `none` 可跳过传播等待（不推荐，CA 侧可能因记录未传播而校验失败）。
///
/// 返回 `Vec<Box<dyn DnsResolver>>` 而非引用切片，是因为解析器要有地方存；
/// 调用方按下面这样把它交给 [`wait_until_visible`]：
///
/// ```ignore
/// let owned = default_resolvers(http);
/// let resolvers: Vec<&dyn DnsResolver> = owned.iter().map(AsRef::as_ref).collect();
/// wait_until_visible(provider, credentials, record, &resolvers, &policy).await?;
/// ```
#[must_use]
pub fn default_resolvers(http: Arc<dyn HttpTransport>) -> Vec<Box<dyn DnsResolver>> {
    let Ok(spec) = std::env::var(RESOLVERS_ENV_KEY) else {
        return builtin_resolvers(http);
    };
    let spec = spec.trim().to_owned();
    if spec.is_empty() {
        return builtin_resolvers(http);
    }
    if spec.eq_ignore_ascii_case("none") {
        return Vec::new();
    }

    let resolvers = spec
        .split(',')
        .filter_map(|entry| {
            let entry = entry.trim();
            if entry.is_empty() {
                return None;
            }
            let (name, endpoint) = match entry.split_once('=') {
                Some((name, endpoint)) => (name.trim().to_owned(), endpoint.trim().to_owned()),
                None => (host_of(entry).to_owned(), entry.to_owned()),
            };
            if endpoint.is_empty() {
                return None;
            }
            let resolver: Box<dyn DnsResolver> =
                Box::new(DohResolver::custom(Arc::clone(&http), name, endpoint));
            Some(resolver)
        })
        .collect::<Vec<_>>();

    if resolvers.is_empty() {
        tracing::warn!(env = RESOLVERS_ENV_KEY, value = %spec, "配置未解析出任何解析器，回退到内置集合");
        return builtin_resolvers(http);
    }
    tracing::info!(count = resolvers.len(), "使用环境变量配置的 DoH 解析器");
    resolvers
}

/// 传播等待解析器配置的环境变量名。
pub(crate) const RESOLVERS_ENV_KEY: &str = "ACMECAST_DOH_RESOLVERS";

/// 某个 zone 应使用哪组解析器来确认「CA 能查到这条记录」。
///
/// 默认走**权威 NS 直查**（见 [`crate::authoritative`]）：公共解析器的否定缓存
/// （NXDOMAIN 按 SOA minimum 缓存，常见 600–1800 秒）会在「写入→删除→重试」
/// 的循环里稳定地查不到新记录，让等待永远超时——而 CA 是从根区递归到权威
/// NS 取答案的，根本不看这些缓存。
///
/// 若显式设置了 `ACMECAST_DOH_RESOLVERS`（含 `none`=跳过等待），按配置走，
/// 保留排障与定制能力。
#[must_use]
pub fn resolvers_for_zone(http: Arc<dyn HttpTransport>, zone: &str) -> Vec<Box<dyn DnsResolver>> {
    if std::env::var(RESOLVERS_ENV_KEY).is_ok_and(|value| !value.trim().is_empty()) {
        return default_resolvers(http);
    }
    let _ = http;
    vec![Box::new(crate::authoritative::AuthoritativeResolver::new(
        zone,
    ))]
}

/// 内置解析器集合：腾讯/阿里（国内可达）+ Google/Cloudflare（国际可达）。
fn builtin_resolvers(http: Arc<dyn HttpTransport>) -> Vec<Box<dyn DnsResolver>> {
    vec![
        Box::new(DohResolver::tencent(Arc::clone(&http))),
        Box::new(DohResolver::aliyun(Arc::clone(&http))),
        Box::new(DohResolver::google(Arc::clone(&http))),
        Box::new(DohResolver::cloudflare(http)),
    ]
}

/// 从端点 URL 里取主机名，作为未显式命名时的解析器名称。
pub(crate) fn host_of(endpoint: &str) -> &str {
    endpoint
        .trim_start_matches("https://")
        .trim_start_matches("http://")
        .split('/')
        .next()
        .unwrap_or(endpoint)
}

/// 传播等待的策略。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PropagationPolicy {
    /// 每轮查询之间的间隔。
    pub interval: Duration,
    /// 总超时。
    pub timeout: Duration,
}

impl Default for PropagationPolicy {
    fn default() -> Self {
        // ACME 的 DNS-01 常见传播是几十秒；两分钟足够覆盖慢解析器，
        // 又不至于让一次失败的挑战挂太久。
        Self {
            interval: Duration::from_secs(5),
            timeout: Duration::from_secs(120),
        }
    }
}

/// 单次解析器查询的超时。
///
/// 不可达的端点（被墙的 DoH）要**快速**失败：查询本身没有超时的话，
/// 一次 TCP 连接挂起到内核超时要一分多钟——120 秒的总预算里只跑得动
/// 两轮，「排除不可达」之类的机制根本没机会生效。
const LOOKUP_TIMEOUT: Duration = Duration::from_secs(5);

/// 等这条记录在**任意一家**解析器上可见。
///
/// 未可见时按 `policy.interval` 重试，直到 `policy.timeout`——
/// 超时返回的错误会指明卡在哪一段：是提供商侧就没认，还是解析器还没看到。
/// 两者要查的方向完全不同，混成一句「超时了」等于没说。
///
/// `resolvers` 为空（`ACMECAST_DOH_RESOLVERS=none`）时跳过解析器阶段，
/// 提供商侧确认后立即返回。
pub async fn wait_until_visible(
    provider: &dyn DnsProvider,
    credentials: &Value,
    record: &TxtRecord,
    resolvers: &[&dyn DnsResolver],
    policy: &PropagationPolicy,
) -> Result<()> {
    let deadline = Instant::now() + policy.timeout;
    let mut published = false;
    let mut round = 0_u32;
    // 跨轮保留，供超时错误指明「是谁没跟上」「是谁查不了」。
    let mut missing: Vec<String> = Vec::new();
    let mut unreachable: Vec<String> = Vec::new();

    loop {
        round += 1;

        // 第一段：提供商侧确认。它都不认，下游无从谈起。
        if !published {
            let found = provider.find_txt(credentials, record).await?;
            published = found.iter().any(|value| value == &record.value);
        }

        if published && resolvers.is_empty() {
            info!(name = %record.name, round, "传播等待已禁用，提供商侧确认即就绪");
            return Ok(());
        }

        if published {
            missing.clear();
            unreachable.clear();
            for resolver in resolvers {
                // 查询**失败**（含超时）与「查到了但值不对」是两回事：前者多半是
                // 这个解析器本身出了问题（端点不可达、被墙），按「不可达」归类，
                // 不参与判定——任一家看到即可放行，所以失败只会让自己出局，
                // 不会像旧的「全部可见」语义那样把整个等待拖死。
                let outcome =
                    match tokio::time::timeout(LOOKUP_TIMEOUT, resolver.lookup_txt(&record.name))
                        .await
                    {
                        Ok(result) => result,
                        Err(_elapsed) => Err(Error::provider(format!(
                            "{} 查询 `{}` 超时（{} 秒）",
                            resolver.name(),
                            record.name,
                            LOOKUP_TIMEOUT.as_secs()
                        ))),
                    };
                match outcome {
                    Ok(values) => {
                        if values.iter().any(|value| value == &record.value) {
                            info!(
                                name = %record.name,
                                round,
                                resolver = resolver.name(),
                                "记录已在解析器上可见"
                            );
                            return Ok(());
                        }
                        missing.push(resolver.name().to_owned());
                    }
                    Err(err) => {
                        warn!(resolver = resolver.name(), error = %err, "解析器查询失败，按不可达处理");
                        unreachable.push(resolver.name().to_owned());
                    }
                }
            }

            debug!(name = %record.name, round, missing = ?missing, unreachable = ?unreachable, "尚无解析器可见该记录");
        }

        if Instant::now() >= deadline {
            return Err(timeout_error(
                published,
                record,
                &missing,
                &unreachable,
                policy,
            ));
        }
        sleep(policy.interval).await;
    }
}

/// 按「卡在哪一段」组装超时错误。
///
/// 只列出**没跟上**的解析器：已经就绪的那些是噪音，而排查时真正要看的
/// 是「谁还差着」。
fn timeout_error(
    published: bool,
    record: &TxtRecord,
    missing: &[String],
    unreachable: &[String],
    policy: &PropagationPolicy,
) -> Error {
    let waited = humanize(policy.timeout);

    if !published {
        return Error::provider(format!(
            "等待 {waited} 后，提供商侧仍未确认记录 `{}` 已写入",
            record.name
        ));
    }

    // 「没看到的」与「查不了的」分开列：前者是传播问题，后者是网络问题，
    // 排查方向完全不同。
    let mut parts: Vec<String> = Vec::new();
    if !missing.is_empty() {
        parts.push(format!("{} 上不可见", missing.join("、")));
    }
    if !unreachable.is_empty() {
        parts.push(format!(
            "{} 查询失败（网络不可达？）",
            unreachable.join("、")
        ));
    }
    if parts.is_empty() {
        parts.push("全部解析器查询均失败".to_owned());
    }

    Error::provider(format!(
        "记录 `{}` 已写入提供商侧，但等待 {waited} 后：{}",
        record.name,
        parts.join("；")
    ))
}

/// 把时长写成人看得懂的样子。
///
/// 直接 `as_secs()` 会把「80 毫秒」显示成「0 秒」——排查时会让人以为
/// 根本没等。
fn humanize(duration: Duration) -> String {
    if duration.as_secs() >= 1 {
        format!("{} 秒", duration.as_secs())
    } else {
        format!("{} 毫秒", duration.as_millis())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    /// 环境变量会跨测试共享进程状态，解析配置的测试统一串行。
    /// `serde_json` 无关；这里用 `Mutex` 占住一把全局锁来串行化。
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    /// 设置环境变量并返回恢复句柄（drop 时还原）。
    ///
    /// `set_var`/`remove_var` 在 edition 2024 下是 unsafe（多线程读环境变量
    /// 理论上有竞争）；测试进程内用 `ENV_LOCK` 串行化，安全性由测试约定保证。
    #[allow(unsafe_code)]
    struct EnvGuard(&'static str);
    #[allow(unsafe_code)]
    impl EnvGuard {
        fn set(key: &'static str, value: &str) -> Self {
            // SAFETY: 所有读环境变量的测试都持有 ENV_LOCK，无并发访问。
            unsafe { std::env::set_var(key, value) };
            Self(key)
        }
    }
    #[allow(unsafe_code)]
    impl Drop for EnvGuard {
        fn drop(&mut self) {
            // SAFETY: 同上。
            unsafe { std::env::remove_var(self.0) };
        }
    }

    #[test]
    fn env_var_overrides_builtin_resolvers() {
        let _guard = ENV_LOCK.lock().unwrap();
        let _env = EnvGuard::set(
            "ACMECAST_DOH_RESOLVERS",
            "my-dns=https://dns.example.com/resolve,https://1.12.12.12/resolve",
        );
        let http: Arc<dyn HttpTransport> = Arc::new(crate::http::ReqwestTransport::default());
        let resolvers = default_resolvers(http);
        assert_eq!(resolvers.len(), 2);
        assert_eq!(resolvers[0].name(), "my-dns");
        // 未命名时取主机名。
        assert_eq!(resolvers[1].name(), "1.12.12.12");
    }

    #[test]
    fn env_var_none_disables_propagation_wait() {
        let _guard = ENV_LOCK.lock().unwrap();
        let _env = EnvGuard::set("ACMECAST_DOH_RESOLVERS", "none");
        let http: Arc<dyn HttpTransport> = Arc::new(crate::http::ReqwestTransport::default());
        assert!(default_resolvers(http).is_empty());
    }

    #[test]
    fn empty_or_invalid_env_falls_back_to_builtin() {
        let _guard = ENV_LOCK.lock().unwrap();
        let http: Arc<dyn HttpTransport> = Arc::new(crate::http::ReqwestTransport::default());

        let _env = EnvGuard::set("ACMECAST_DOH_RESOLVERS", "  ");
        assert_eq!(default_resolvers(Arc::clone(&http)).len(), 4);

        // 全是无法解析的碎片：回退内置。
        let _env = EnvGuard::set("ACMECAST_DOH_RESOLVERS", " ,, =");
        assert_eq!(default_resolvers(http).len(), 4);
    }

    #[test]
    fn builtin_set_contains_china_and_global_resolvers() {
        let _guard = ENV_LOCK.lock().unwrap();
        let http: Arc<dyn HttpTransport> = Arc::new(crate::http::ReqwestTransport::default());
        let names = default_resolvers(http)
            .iter()
            .map(|r| r.name().to_owned())
            .collect::<Vec<_>>();
        assert!(names.contains(&"腾讯 DNSPod".to_owned()));
        assert!(names.contains(&"阿里 DNS".to_owned()));
        assert!(names.contains(&"Google DNS".to_owned()));
        assert!(names.contains(&"Cloudflare DNS".to_owned()));
    }

    /// 按脚本应答的解析器：每调一次前进一格，用完后一直返回最后一个。
    #[derive(Debug)]
    struct ScriptedResolver {
        name: String,
        /// 每次查询依次返回的值列表。
        script: Mutex<Vec<Vec<String>>>,
        calls: Mutex<usize>,
    }

    impl ScriptedResolver {
        fn new(name: &str, script: Vec<Vec<String>>) -> Self {
            Self {
                name: name.to_owned(),
                script: Mutex::new(script),
                calls: Mutex::new(0),
            }
        }

        fn calls(&self) -> usize {
            *self.calls.lock().expect("锁不应中毒")
        }
    }

    #[async_trait]
    impl DnsResolver for ScriptedResolver {
        fn name(&self) -> &str {
            &self.name
        }

        async fn lookup_txt(&self, _name: &str) -> Result<Vec<String>> {
            let mut calls = self.calls.lock().expect("锁不应中毒");
            let index = *calls;
            *calls += 1;
            drop(calls);

            let script = self.script.lock().expect("锁不应中毒");
            Ok(script
                .get(index)
                .or_else(|| script.last())
                .cloned()
                .unwrap_or_default())
        }
    }

    /// 只实现传播等待用到的那部分能力的假提供商。
    #[derive(Debug)]
    struct Publishes {
        values: Mutex<Vec<String>>,
        deleted: Mutex<Vec<String>>,
    }

    impl Publishes {
        fn always(value: &str) -> Self {
            Self {
                values: Mutex::new(vec![value.to_owned()]),
                deleted: Mutex::new(Vec::new()),
            }
        }

        fn never() -> Self {
            Self {
                values: Mutex::new(Vec::new()),
                deleted: Mutex::new(Vec::new()),
            }
        }

        fn deleted(&self) -> Vec<String> {
            self.deleted.lock().expect("锁不应中毒").clone()
        }
    }

    #[async_trait]
    impl DnsProvider for Publishes {
        fn type_id(&self) -> &'static str {
            "fake"
        }

        fn display_name(&self) -> &'static str {
            "假 DNS"
        }

        fn credential_fields(&self) -> schemars::schema::RootSchema {
            schemars::schema_for!(String)
        }

        async fn find_txt(&self, _credentials: &Value, _record: &TxtRecord) -> Result<Vec<String>> {
            Ok(self.values.lock().expect("锁不应中毒").clone())
        }

        async fn create_txt(&self, _credentials: &Value, _record: &TxtRecord) -> Result<()> {
            Ok(())
        }

        async fn delete_txt(&self, _credentials: &Value, record: &TxtRecord) -> Result<()> {
            self.deleted
                .lock()
                .expect("锁不应中毒")
                .push(record.name.clone());
            Ok(())
        }
    }

    #[tokio::test]
    async fn a_timeout_still_cleans_up_the_record() {
        // spec：超时后返回明确错误**并清理已写入的记录**。清理由
        // `with_txt_record` 负责——这正是把写入与清理配成一对的好处。
        let provider = Publishes::always("value-1");
        let resolver = ScriptedResolver::new("解析器", vec![vec![]]);

        let result = crate::challenge::with_txt_record(
            &provider,
            &serde_json::json!({}),
            &record(),
            wait_until_visible(
                &provider,
                &serde_json::json!({}),
                &record(),
                &[&resolver],
                &fast_policy(),
            ),
        )
        .await;

        let err = result.expect_err("应超时");
        assert!(err.to_string().contains("不可见"), "{err}");

        assert_eq!(
            provider.deleted(),
            vec!["_acme-challenge.example.com".to_owned()],
            "超时后记录必须被清掉"
        );
    }

    fn record() -> TxtRecord {
        TxtRecord::new("example.com", "_acme-challenge.example.com", "value-1", 60)
    }

    fn fast_policy() -> PropagationPolicy {
        PropagationPolicy {
            interval: Duration::from_millis(5),
            timeout: Duration::from_millis(80),
        }
    }

    #[tokio::test]
    async fn a_visible_record_returns_immediately() {
        let provider = Publishes::always("value-1");
        let resolver = ScriptedResolver::new("解析器", vec![vec!["value-1".to_owned()]]);

        wait_until_visible(
            &provider,
            &serde_json::json!({}),
            &record(),
            &[&resolver],
            &fast_policy(),
        )
        .await
        .expect("已可见应立刻返回");
        assert_eq!(resolver.calls(), 1, "一轮即可");
    }

    #[tokio::test]
    async fn an_unpropagated_record_is_retried_until_visible() {
        // spec 场景：提供商侧已写入但解析器还查不到 → 继续等待并重试。
        let provider = Publishes::always("value-1");
        let resolver = ScriptedResolver::new(
            "解析器",
            vec![
                vec![],
                vec!["别的值".to_owned()],
                vec!["value-1".to_owned()],
            ],
        );

        wait_until_visible(
            &provider,
            &serde_json::json!({}),
            &record(),
            &[&resolver],
            &fast_policy(),
        )
        .await
        .expect("第三轮可见应成功");
        assert_eq!(resolver.calls(), 3);
    }

    #[tokio::test]
    async fn any_single_visible_resolver_is_enough() {
        // 任一家看到即放行：另一家滞后（负缓存、线路）不再阻塞整个等待。
        let provider = Publishes::always("value-1");
        let lagging = ScriptedResolver::new("慢的", vec![vec![]]);
        let ready = ScriptedResolver::new("先就绪的", vec![vec!["value-1".to_owned()]]);

        wait_until_visible(
            &provider,
            &serde_json::json!({}),
            &record(),
            &[&lagging, &ready],
            &fast_policy(),
        )
        .await
        .expect("一家可见即返回");
        assert_eq!(lagging.calls(), 1, "判定不陪慢的那家等到底");
    }

    #[tokio::test]
    async fn no_resolvers_means_provider_confirmation_only() {
        // `ACMECAST_DOH_RESOLVERS=none`：跳过解析器阶段，提供商侧认了就算就绪。
        let provider = Publishes::always("value-1");

        wait_until_visible(
            &provider,
            &serde_json::json!({}),
            &record(),
            &[],
            &fast_policy(),
        )
        .await
        .expect("无解析器时提供商侧确认即就绪");
    }

    #[test]
    fn the_default_set_covers_public_resolvers() {
        use crate::http::HttpResponse;

        #[derive(Debug)]
        struct Unused;

        #[async_trait]
        impl HttpTransport for Unused {
            async fn send(&self, _request: HttpRequest) -> Result<HttpResponse> {
                unreachable!("本用例只构造解析器，不发请求")
            }
        }

        // default_resolvers 会读环境变量，必须与其它 env 测试串行。
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let resolvers = default_resolvers(Arc::new(Unused));
        let names: Vec<&str> = resolvers.iter().map(|resolver| resolver.name()).collect();

        assert_eq!(names.len(), 4, "国内+国际各两家，避免单点不可达");
        assert!(names.contains(&"腾讯 DNSPod"), "{names:?}");
        assert!(names.contains(&"阿里 DNS"), "{names:?}");
        assert!(names.contains(&"Google DNS"), "{names:?}");
        assert!(names.contains(&"Cloudflare DNS"), "{names:?}");
    }

    #[tokio::test]
    async fn a_timeout_says_it_was_the_provider_side() {
        // 卡在提供商侧与卡在解析器侧，要查的方向完全不同。
        let provider = Publishes::never();
        let resolver = ScriptedResolver::new("解析器", vec![vec![]]);

        let err = wait_until_visible(
            &provider,
            &serde_json::json!({}),
            &record(),
            &[&resolver],
            &fast_policy(),
        )
        .await
        .expect_err("提供商侧一直不认应超时");

        let text = err.to_string();
        assert!(text.contains("提供商侧"), "{text}");
        assert!(text.contains("_acme-challenge.example.com"), "{text}");
        assert_eq!(resolver.calls(), 0, "提供商侧没认就不必去问解析器");
    }

    #[tokio::test]
    async fn a_timeout_names_the_resolvers_that_never_saw_it() {
        let provider = Publishes::always("value-1");
        let stale = ScriptedResolver::new("卡住的", vec![vec!["旧值".to_owned()]]);
        let stuck = ScriptedResolver::new("查不到的", vec![vec![]]);

        let err = wait_until_visible(
            &provider,
            &serde_json::json!({}),
            &record(),
            &[&stale, &stuck],
            &fast_policy(),
        )
        .await
        .expect_err("没有任何解析器看到应超时");

        let text = err.to_string();
        assert!(text.contains("已写入提供商侧"), "{text}");
        assert!(text.contains("卡住的"), "应指出没跟上的解析器: {text}");
        assert!(text.contains("查不到的"), "{text}");
        assert!(text.contains("不可见"), "{text}");
    }

    #[tokio::test(start_paused = true)]
    async fn a_hanging_resolver_query_is_cut_off_by_the_lookup_timeout() {
        // 被阻断的 DoH 端点会让 TCP 连接挂起一分多钟。没有单次查询超时的话，
        // 一轮就吃掉大半个总超时——「快速排除不可达」根本轮不到发生。
        use crate::http::HttpResponse;

        #[derive(Debug)]
        struct Hangs;

        #[async_trait]
        impl HttpTransport for Hangs {
            async fn send(&self, _request: HttpRequest) -> Result<HttpResponse> {
                tokio::time::sleep(Duration::from_secs(3600)).await;
                Ok(HttpResponse::new(200, "{}"))
            }
        }

        let provider = Publishes::always("value-1");
        let resolver = DohResolver::google(Arc::new(Hangs));
        let err = wait_until_visible(
            &provider,
            &serde_json::json!({}),
            &record(),
            &[&resolver],
            &fast_policy(),
        )
        .await
        .expect_err("挂起的查询应被超时切断并判为失败");

        let text = err.to_string();
        assert!(text.contains("Google DNS"), "{text}");
        assert!(
            text.contains("查询失败"),
            "挂起应归类为查不了而不是没看到: {text}"
        );
    }

    #[tokio::test]
    async fn a_query_error_counts_as_not_visible() {
        // 解析器查询失败（NXDOMAIN、网络抖动）都当作「还没传播」，
        // 继续重试到超时——那正是等待要处理的情形。
        #[derive(Debug)]
        struct AlwaysFails;

        #[async_trait]
        impl DnsResolver for AlwaysFails {
            fn name(&self) -> &str {
                "总是失败"
            }

            async fn lookup_txt(&self, _name: &str) -> Result<Vec<String>> {
                Err(Error::provider("网络不通"))
            }
        }

        let provider = Publishes::always("value-1");
        let err = wait_until_visible(
            &provider,
            &serde_json::json!({}),
            &record(),
            &[&AlwaysFails],
            &fast_policy(),
        )
        .await
        .expect_err("一直失败应超时");

        assert!(err.to_string().contains("总是失败"), "{err}");
    }

    #[tokio::test]
    async fn an_always_unreachable_resolver_does_not_block_the_visible_ones() {
        // 一个解析器（如被墙的 DoH 端点）永远查询失败，另一个正常返回记录值：
        // 不可达的那家被排除出判定，等待应成功返回而不是拖到超时。
        #[derive(Debug)]
        struct AlwaysFails;

        #[async_trait]
        impl DnsResolver for AlwaysFails {
            fn name(&self) -> &str {
                "被墙的"
            }

            async fn lookup_txt(&self, _name: &str) -> Result<Vec<String>> {
                Err(Error::provider("网络不通"))
            }
        }

        let provider = Publishes::always("value-1");
        let failing = AlwaysFails;
        let seeing = ScriptedResolver::new("正常的", vec![vec!["value-1".to_owned()]]);

        wait_until_visible(
            &provider,
            &serde_json::json!({}),
            &record(),
            &[&failing, &seeing],
            &fast_policy(),
        )
        .await
        .expect("可达解析器已看到记录，不可达的不应阻塞");
    }

    // ---- DoH 解析器 ----

    #[tokio::test]
    async fn the_doh_resolver_parses_txt_answers() {
        use crate::http::{HttpResponse, HttpTransport};

        #[derive(Debug)]
        struct OneResponse(Mutex<Option<HttpResponse>>);

        #[async_trait]
        impl HttpTransport for OneResponse {
            async fn send(&self, request: HttpRequest) -> Result<HttpResponse> {
                // DoH 的 JSON 应答要显式请求，缺了这个头会拿到二进制格式。
                assert_eq!(request.headers["Accept"], "application/dns-json");
                assert!(request.url.contains("type=TXT"), "{}", request.url);
                Ok(self.0.lock().unwrap().take().expect("只应答一次"))
            }
        }

        let response = HttpResponse::new(
            200,
            r#"{"Status":0,"Answer":[
                {"name":"_acme-challenge.example.com.","type":16,"data":"\"value-1\""},
                {"name":"_acme-challenge.example.com.","type":5,"data":"ignored"}
            ]}"#,
        );
        let transport = Arc::new(OneResponse(Mutex::new(Some(response))));

        let resolver = DohResolver::google(transport);
        let values = resolver
            .lookup_txt("_acme-challenge.example.com")
            .await
            .expect("应能解析");

        // 引号被脱掉，非 TXT 的记录被忽略。
        assert_eq!(values, vec!["value-1".to_owned()]);
        assert_eq!(resolver.name(), "Google DNS");
    }

    #[test]
    fn builtin_constants_are_bounded_and_descriptive() {
        // 规模受控，不直接消费 dnsdata 全量高密度数据
        assert!(
            crate::builtin::DNS_SERVERS.len() <= 12,
            "DNS 内置过多请抽样"
        );
        assert!(crate::builtin::DOT_SERVERS.len() <= 8, "DoT 内置过多");
        assert!(crate::builtin::DOH_SERVERS.len() <= 10, "DoH 内置过多");
        assert!(crate::builtin::BOOTSTRAP_SERVERS.len() <= 10);
        // 至少覆盖国内外代表
        assert!(crate::builtin::DNS_SERVERS.contains(&"223.5.5.5"));
        assert!(crate::builtin::DOH_SERVERS.contains(&"https://dns.alidns.com/dns-query"));
    }

    #[test]
    fn resolvers_for_zone_stays_authoritative_by_default() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let http: Arc<dyn HttpTransport> = Arc::new(crate::http::ReqwestTransport::default());
        // 未设 env 时走权威直查，不自动混入扩展
        let v = resolvers_for_zone(Arc::clone(&http), "example.com");
        assert_eq!(v.len(), 1);
        assert!(v[0].name().contains("权威"), "{}", v[0].name());
    }

    #[test]
    fn extended_resolvers_empty_entries_fallback_to_builtin() {
        let _guard = ENV_LOCK.lock().unwrap();
        let http: Arc<dyn HttpTransport> = Arc::new(crate::http::ReqwestTransport::default());
        // 无条目时仅返回内置集合（default_resolvers 在未设 env 时为内置四家）。
        let v = crate::resolvers::load_extended_resolvers(Arc::clone(&http), &[]);
        assert_eq!(v.len(), 4, "无条目时仅内置");
        // 再次确认空切片与缺省等价
        let v2 = crate::resolvers::load_extended_resolvers(http, &[]);
        assert_eq!(v2.len(), 4, "空条目时仅内置");
    }

    #[test]
    fn extended_resolvers_skips_invalid_and_dedupes() {
        let _guard = ENV_LOCK.lock().unwrap();
        let http: Arc<dyn HttpTransport> = Arc::new(crate::http::ReqwestTransport::default());
        use crate::resolvers::ResolverEntry;
        let entries = [
            // 非 https，跳过
            ResolverEntry {
                kind: "doh".into(),
                endpoint: "http://bad.example/dns-query".into(),
                name: None,
            },
            // dot 未实现，跳过
            ResolverEntry {
                kind: "dot".into(),
                endpoint: "dns.alidns.com".into(),
                name: None,
            },
            // 非 IP，跳过
            ResolverEntry {
                kind: "dns".into(),
                endpoint: "not-an-ip".into(),
                name: None,
            },
            // 与内置 alidns 重复，去重
            ResolverEntry {
                kind: "doh".into(),
                endpoint: "https://dns.alidns.com/dns-query".into(),
                name: None,
            },
            // 新增
            ResolverEntry {
                kind: "doh".into(),
                endpoint: "https://example.com/dns-query".into(),
                name: Some("custom".into()),
            },
        ];
        let v = crate::resolvers::load_extended_resolvers(http, &entries);
        // dot 与 dns 非法/未实现被跳过；与内置重复的 alidns 去重后仅新增 example.com 一条
        assert!(v.len() >= 5, "至少内置4 + 新增1 =5, got {}", v.len());
        assert!(v.iter().any(|r| r.name() == "custom"));
    }

    #[test]
    fn extended_resolvers_none_short_circuits() {
        let _guard = ENV_LOCK.lock().unwrap();
        let http: Arc<dyn HttpTransport> = Arc::new(crate::http::ReqwestTransport::default());
        use crate::resolvers::ResolverEntry;
        let entries = [ResolverEntry {
            kind: "doh".into(),
            endpoint: "https://example.com/dns-query".into(),
            name: None,
        }];
        let _e = EnvGuard::set("ACMECAST_DOH_RESOLVERS", "none");
        let v = crate::resolvers::load_extended_resolvers(http, &entries);
        assert!(v.is_empty(), "none 时跳过传播等待");
    }

    #[tokio::test]
    async fn the_doh_resolver_reports_a_http_failure() {
        use crate::http::{HttpResponse, HttpTransport};

        #[derive(Debug)]
        struct Failing;

        #[async_trait]
        impl HttpTransport for Failing {
            async fn send(&self, _request: HttpRequest) -> Result<HttpResponse> {
                Ok(HttpResponse::new(503, "service unavailable"))
            }
        }

        let err = DohResolver::cloudflare(Arc::new(Failing))
            .lookup_txt("_acme-challenge.example.com")
            .await
            .expect_err("非 2xx 应报错");
        assert!(err.to_string().contains("Cloudflare DNS"), "{err}");
        assert!(err.to_string().contains("503"), "{err}");
    }
}
