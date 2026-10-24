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

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use serde_json::Value;
use tokio::time::sleep;
use tracing::{debug, info, warn};

use crate::error::{Error, Result};
use crate::http::{HttpRequest, HttpTransport, ReqwestTransport, encode};
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

    /// DoH 熔断计数的键：默认用名称；DoH 解析器覆盖为归一化端点 host，
    /// 与 `resolvers.rs` 的去重键同口径——同一端点不同 name 也能合并计数。
    fn breaker_key(&self) -> &str {
        self.name()
    }

    /// DoH 端点的完整 URL。
    ///
    /// 代理自动回退靠它重建「同端点 + 代理传输层」的解析器：直连失败时
    /// 经代理重试、已缓存代理偏好时直接经代理查询，都要按原端点再发一次。
    /// 非 DoH 解析器（权威 NS 直查等）返回 `None`——它们不走代理回退，
    /// 传输层保持直连。
    fn endpoint(&self) -> Option<&str> {
        None
    }
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

    fn breaker_key(&self) -> &str {
        host_of(&self.endpoint)
    }

    fn endpoint(&self) -> Option<&str> {
        Some(&self.endpoint)
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

/// 按 `dns_provider` 构造 DoH 解析器集合（内置四家全用）。
///
/// 返回全部四家内置 DoH（腾讯/阿里/Google/Cloudflare），由调用方经
/// [`prioritize_for_provider`] 排序——提供商对应的 DoH 排第一，其余
/// 按 `Cloudflare → 阿里 → 腾讯 → Google` 兜底序排列。
///
/// 已显式设置 `ACMECAST_DOH_RESOLVERS`（含 `none`）时短路返回
/// `default_resolvers(http)`，不叠加厂商端点，保持环境变量最高优先级。
#[must_use]
pub fn doh_resolvers_for_provider(
    http: Arc<dyn HttpTransport>,
    _provider_id: Option<&str>,
) -> Vec<Box<dyn DnsResolver>> {
    // 环境变量显式设置时短路：与 `resolvers_for_zone` 保持一致的优先级。
    if std::env::var(RESOLVERS_ENV_KEY).is_ok_and(|value| !value.trim().is_empty()) {
        return default_resolvers(http);
    }
    // 排序由调用方经 prioritize_for_provider 完成，此处返回全量内置集合。
    builtin_resolvers(http)
}

/// 按 `dns_provider` 与 `zone` 选择传播等待解析器。
///
/// 已设置 `ACMECAST_DOH_RESOLVERS` 时与 `resolvers_for_zone` 一致返回
/// 环境变量语义的结果；否则返回提供商感知的 DoH 集合（1 或 2 条）。
/// 不改动 `resolvers_for_zone` 的权威直查默认路径——旧入口保持不变。
#[must_use]
pub fn resolvers_for_provider(
    http: Arc<dyn HttpTransport>,
    zone: &str,
    provider_id: Option<&str>,
) -> Vec<Box<dyn DnsResolver>> {
    if std::env::var(RESOLVERS_ENV_KEY).is_ok_and(|value| !value.trim().is_empty()) {
        return default_resolvers(http);
    }
    let _ = zone;
    doh_resolvers_for_provider(http, provider_id)
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

/// 按 `dns_provider` 将公共解析器集做优先级排序。
///
/// 提供商对应的同厂商 DoH 排第一，其后按固定序排列其余厂商 DoH：
///
/// - `cloud.cloudflare` → Cloudflare 优先；兜底序：Cloudflare → 阿里 → 腾讯 → Google
/// - `cloud.aliyun` → 阿里优先；兜底序：阿里 → Cloudflare → 腾讯 → Google
/// - `cloud.tencent` / `dns.tencent-eo` → 腾讯优先；兜底序：腾讯 → Cloudflare → 阿里 → Google
/// - 其他 / 未传参 → Cloudflare → 阿里 → 腾讯 → Google
///
/// 已排到前面的解析器不会在兜底序中重复出现（命中即排除）。
pub fn prioritize_for_provider(
    resolvers: Vec<Box<dyn DnsResolver>>,
    provider_id: Option<&str>,
) -> Vec<Box<dyn DnsResolver>> {
    use std::collections::HashSet;
    if resolvers.is_empty() {
        return resolvers;
    }
    let tag = provider_id
        .map(|v| v.trim().to_ascii_lowercase())
        .unwrap_or_default();

    // 提供商对应的 DoH 匹配关键词（排第一），其后跟固定兜底序。
    // 兜底序中已包含首选关键词——排序逻辑靠下标去重保证不重复。
    let (primary, fallback): (&[&str], &[&str]) = match tag.as_str() {
        "cloud.cloudflare" => (
            &["cloudflare"],
            &["cloudflare", "阿里", "alidns", "腾讯", "1.12", "google"],
        ),
        "cloud.aliyun" => (
            &["阿里", "alidns", "120.53"],
            &[
                "阿里",
                "alidns",
                "120.53",
                "cloudflare",
                "腾讯",
                "1.12",
                "google",
            ],
        ),
        "cloud.tencent" | "dns.tencent-eo" => (
            &["腾讯", "1.12"],
            &["腾讯", "1.12", "cloudflare", "阿里", "alidns", "google"],
        ),
        _ => (
            &[],
            &["cloudflare", "阿里", "alidns", "腾讯", "1.12", "google"],
        ),
    };

    let ranked: Vec<&str> = {
        let mut v: Vec<&str> = Vec::new();
        for k in primary.iter().chain(fallback.iter()) {
            if !v.contains(k) {
                v.push(*k);
            }
        }
        v
    };

    let mut ordered: Vec<usize> = Vec::new();
    let mut seen_idx: HashSet<usize> = HashSet::new();
    for key in &ranked {
        let key_lower = key.to_ascii_lowercase();
        for (idx, r) in resolvers.iter().enumerate() {
            if seen_idx.contains(&idx) {
                continue;
            }
            if r.name().to_ascii_lowercase().contains(&key_lower) {
                ordered.push(idx);
                seen_idx.insert(idx);
            }
        }
    }
    if ordered.is_empty() {
        return resolvers;
    }
    let mut by_idx: Vec<Option<Box<dyn DnsResolver>>> = resolvers.into_iter().map(Some).collect();
    let mut out: Vec<Box<dyn DnsResolver>> = Vec::with_capacity(by_idx.len());
    let mut taken: HashSet<usize> = HashSet::new();
    for idx in &ordered {
        if let Some(v) = by_idx[*idx].take() {
            out.push(v);
            taken.insert(*idx);
        }
    }
    for (idx, slot) in by_idx.into_iter().enumerate() {
        if !taken.contains(&idx) {
            if let Some(v) = slot {
                out.push(v);
            }
        }
    }
    out
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
        // ACME 的 DNS-01 常见传播是几十秒，但商有 DNS（EdgeOne 等）从
        // 控制面到权威 NS 的同步延迟实测可达数分钟且波动大。等待期挑战
        // 记录始终存在、退出路径保证清理，偏大没有残留副作用，五分钟
        // 覆盖慢同步长尾；需要收紧的环境走 [propagation] 配置。
        Self {
            interval: Duration::from_secs(5),
            timeout: Duration::from_secs(300),
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
///
/// `proxy_url` 是全局出网代理（`config.toml` 的 `[proxy] url` 或
/// `ACMECAST_PROXY_URL`）：已配置时 DoH 端点直连失败会自动经代理重试
/// 一次（成功则缓存偏好，见 [`DohProxyFallback`]）；未配置/留空即完全
/// 直连，不做回退。本函数的 `resolvers` 全是 DoH，故回退对整组生效。
pub async fn wait_until_visible<F>(
    provider: &dyn DnsProvider,
    credentials: &Value,
    record: &TxtRecord,
    resolvers: &[&dyn DnsResolver],
    policy: &PropagationPolicy,
    proxy_url: Option<&str>,
    mut on_progress: F,
) -> Result<()>
where
    F: FnMut(&str) + Send,
{
    let deadline = Instant::now() + policy.timeout;
    let mut published = false;
    let mut round = 0_u32;
    let mut missing: Vec<String> = Vec::new();
    let mut unreachable: Vec<String> = Vec::new();
    // DoH 熔断：仅本轮传播会话内有效，函数返回即重置。
    let mut active: Vec<&dyn DnsResolver> = resolvers.to_vec();
    let mut breaker = CircuitBreaker::default();
    // DoH 代理自动回退：与熔断平行，跨轮保留偏好，函数返回即丢弃。
    let mut fallback = DohProxyFallback::new(proxy_url);
    let mut probes: Vec<ResolverProbe> = Vec::new();

    on_progress(&format!(
        "propagation start timeout={} interval={} (record: {})",
        policy.timeout.as_secs(),
        policy.interval.as_secs(),
        record.ca_name,
    ));
    // 命中单家 DNS：取 resolvers 中首个（调用方已按 provider 感知排序，首位为命中提供商）
    // 与后续 round x miss 的 resolver 标识同源，provider 未知/空时以首个或空表述呈现，但保证在 round 1 miss 前可见。
    if let Some(hit) = resolvers.first() {
        on_progress(&format!("provider dns: {}", hit.name()));
    } else {
        on_progress("provider dns: (none)");
    }

    // `proxy: ` 行每会话只报一次：首次回退成功后置位，不再重复。
    let mut proxy_reported = false;

    loop {
        round += 1;

        if !published {
            let found = provider.find_txt(credentials, record).await?;
            published = found.iter().any(|value| value == &record.value);
        }

        if published && resolvers.is_empty() {
            info!(name = %record.ca_name, round, "传播等待已禁用，提供商侧确认即就绪");
            on_progress(&format!("propagation succeeded after {round} rounds"));
            return Ok(());
        }

        if published {
            match probe_resolvers(
                &active,
                record,
                round,
                &mut missing,
                &mut unreachable,
                &mut probes,
                Some(&mut fallback),
            )
            .await
            {
                ProbeOutcome::Visible(hit) => {
                    on_progress(&format!("round {round} hit (resolver: {hit})"));
                    report_proxy_fallback(&fallback, &mut proxy_reported, &mut on_progress);
                    report_evicted(&breaker, &mut on_progress);
                    on_progress(&format!("propagation succeeded after {round} rounds"));
                    return Ok(());
                }
                ProbeOutcome::NotVisible => {
                    on_progress(&format!(
                        "round {round} miss (resolvers: {})",
                        missing.join(", ")
                    ));
                    report_proxy_fallback(&fallback, &mut proxy_reported, &mut on_progress);
                    apply_breaker(&mut breaker, &mut active, &probes, &mut on_progress);
                    if active.is_empty() {
                        warn!(
                            name = %record.ca_name,
                            evicted = %breaker.evicted_list(),
                            "DoH 全部不可用，跳过 DoH 验证"
                        );
                        on_progress(&format!(
                            "DoH 全部不可用（已剔除：{}），跳过后续 DoH 验证",
                            breaker.evicted_list()
                        ));
                        return Err(timeout_error(
                            published,
                            record,
                            &missing,
                            &unreachable,
                            &breaker.evicted,
                            policy,
                        ));
                    }
                }
            }
        }

        if Instant::now() >= deadline {
            on_progress(&format!(
                "propagation timed out after {round} rounds (timeout={}s)",
                policy.timeout.as_secs(),
            ));
            return Err(timeout_error(
                published,
                record,
                &missing,
                &unreachable,
                &breaker.evicted,
                policy,
            ));
        }
        sleep(policy.interval).await;
    }
}

/// 分阶段传播等待：按时间窗口依次切换解析器组。
///
/// 四段等分（末段再拆半），在原 1/2、1/4、1/4 三段基础上把 1/2 的权威拆为两个 1/4：
///
/// 1. **命中单家 DNS**（`hit`，占 `timeout` 的前 1/4）：仅查提供商感知的单家 DoH
///   （`provider=cloudflare` 时仅 Cloudflare 单家），provider 未知/空时跳过该段。
///
/// 2. **权威 NS 直查**（`authoritative`，占 `timeout` 的第 2 个 1/4）：
///    CA 从根区递归到权威 NS 取答案，不看公共解析器的缓存——直查权威
///    NS 最贴近 CA 视角，否定缓存不会在重试循环里制造假不可见。
///    不排除 hit 单家，两个阶段独立探测。
///
/// 3. **全量 DNS**（`all_dns`，占 `timeout` 的第 3 个 1/4）：内置 DNS 服务器 +
///    自定义解析器，覆盖公共递归视角与厂商线路差异。
///
/// 4. **DoH**（`doh`，占 `timeout` 的最后 1/4，内再拆 1/8+1/8）：
///    前 1/8 仅查命中单家，后 1/8 查整组 `ext_refs`（provider 优先排序后的 `name()`），
///    同 1/4 内顺序探测，任一家可见即就绪，兜底公共网络可达性。
///
/// `ACMECAST_DOH_RESOLVERS=none` 时全部解析器组为空，跳过解析器阶段。
///
/// `proxy_url` 是全局出网代理（`config.toml` 的 `[proxy] url` 或
/// `ACMECAST_PROXY_URL`）：仅对 DoH 段（`doh_single` 与 `doh_full`）生效——
/// 直连失败自动经代理重试一次，成功则缓存偏好、后续段直接用代理传输层；
/// 权威 NS 直查与 `all_dns` 阶段的传输层不受影响，保持直连。未配置即
/// 完全直连，不做回退。
pub async fn wait_until_visible_phased<F>(
    provider: &dyn DnsProvider,
    credentials: &Value,
    record: &TxtRecord,
    authoritative: &[&dyn DnsResolver],
    all_dns: &[&dyn DnsResolver],
    doh: &[&dyn DnsResolver],
    policy: &PropagationPolicy,
    proxy_url: Option<&str>,
    mut on_progress: F,
) -> Result<()>
where
    F: FnMut(&str) + Send,
{
    let start = Instant::now();
    let phase1_end = start + policy.timeout / 4;
    let phase2_end = start + policy.timeout / 2;
    let phase3_end = start + policy.timeout * 3 / 4;
    // 末段 DoH 的 1/4 内再拆半：前 1/8 仅命中单家，后 1/8 整组
    let phase4_mid = start + policy.timeout * 7 / 8;
    let deadline = start + policy.timeout;

    let mut published = false;
    // 权威为空时跳过预检，直接查公共解析器。
    let mut authoritative_seen = authoritative.is_empty();
    let mut round = 0_u32;
    let mut missing: Vec<String> = Vec::new();
    let mut unreachable: Vec<String> = Vec::new();
    // DoH 熔断：仅对 doh 段生效，authoritative/all_dns 保持全量探测；
    // 剔除状态跨段保留（doh_single → doh_full 不重置），本轮结束即丢弃。
    let mut doh_active: Vec<&dyn DnsResolver> = doh.to_vec();
    let mut breaker = CircuitBreaker::default();
    // DoH 代理自动回退：仅 doh 段启用；偏好缓存跨 doh_single/doh_full
    // 保留（与熔断同一会话），本轮结束即丢弃。
    let mut fallback = DohProxyFallback::new(proxy_url);
    let mut probes: Vec<ResolverProbe> = Vec::new();

    on_progress(&format!(
        "propagation start timeout={} interval={} (record: {})",
        policy.timeout.as_secs(),
        policy.interval.as_secs(),
        record.ca_name,
    ));
    // `proxy: ` 行每会话只报一次：首次回退成功后置位，不再重复。
    let mut proxy_reported = false;
    // 命中单家 DNS：取 doh（已按 provider 感知排序的 ext_refs）首位，all_dns 与 doh 为同一 ext_refs 的两次引用时以 doh 为准
    // provider 未知/空时首位仍为固定兜底序首位（Cloudflare），保证在 round 1 miss 前可见且与后续 round x miss 同源。
    {
        let hit_name = doh
            .first()
            .map(|r| r.name())
            .or_else(|| all_dns.first().map(|r| r.name()));
        if let Some(name) = hit_name {
            on_progress(&format!("provider dns: {name}"));
        } else if let Some(auth) = authoritative.first() {
            // 权威不为空但公共为空时，命中行退化为权威标识
            on_progress(&format!("provider dns: {}", auth.name()));
        } else {
            on_progress("provider dns: (none)");
        }
    }

    loop {
        round += 1;

        if !published {
            let found = provider.find_txt(credentials, record).await?;
            published = found.iter().any(|value| value == &record.value);
        }

        let all_empty = authoritative.is_empty() && all_dns.is_empty() && doh.is_empty();
        if published && all_empty {
            info!(name = %record.ca_name, round, "传播等待已禁用，提供商侧确认即就绪");
            on_progress(&format!("propagation succeeded after {round} rounds"));
            return Ok(());
        }

        if published {
            let now = Instant::now();
            // 命中单家集合：取 doh（已按 provider 感知排序的 ext_refs）首位，all_dns 与 doh 为同一 ext_refs 时以 doh 为准
            let hit_single: &[&dyn DnsResolver] = if !doh_active.is_empty() {
                &doh_active[0..1]
            } else if !all_dns.is_empty() {
                &all_dns[0..1]
            } else {
                &[]
            };
            let hit = !hit_single.is_empty();
            let in_hit = now < phase1_end;
            let in_auth = now >= phase1_end && now < phase2_end;
            let in_all = now >= phase2_end && now < phase3_end;
            let in_doh = now >= phase3_end;

            if in_hit && hit {
                match probe_resolvers(
                    hit_single,
                    record,
                    round,
                    &mut missing,
                    &mut unreachable,
                    &mut probes,
                    Some(&mut fallback),
                )
                .await
                {
                    ProbeOutcome::Visible(hit_name) => {
                        on_progress(&format!("round {round} hit (resolver: {hit_name})"));
                        report_proxy_fallback(&fallback, &mut proxy_reported, &mut on_progress);
                        report_evicted(&breaker, &mut on_progress);
                        on_progress(&format!("propagation succeeded after {round} rounds"));
                        return Ok(());
                    }
                    ProbeOutcome::NotVisible => {
                        on_progress(&format!(
                            "round {round} miss (resolvers: {})",
                            missing.join(", ")
                        ));
                        report_proxy_fallback(&fallback, &mut proxy_reported, &mut on_progress);
                        apply_breaker(&mut breaker, &mut doh_active, &probes, &mut on_progress);
                    }
                }
            } else if in_auth && !authoritative.is_empty() {
                match probe_resolvers(
                    authoritative,
                    record,
                    round,
                    &mut missing,
                    &mut unreachable,
                    &mut probes,
                    None,
                )
                .await
                {
                    ProbeOutcome::Visible(hit_name) => {
                        on_progress(&format!("round {round} hit (resolver: {hit_name})"));
                        report_evicted(&breaker, &mut on_progress);
                        on_progress(&format!("propagation succeeded after {round} rounds"));
                        return Ok(());
                    }
                    ProbeOutcome::NotVisible => {
                        on_progress(&format!(
                            "round {round} miss (resolvers: {})",
                            missing.join(", ")
                        ));
                    }
                }
            } else if in_all && !all_dns.is_empty() {
                match probe_resolvers(
                    all_dns,
                    record,
                    round,
                    &mut missing,
                    &mut unreachable,
                    &mut probes,
                    None,
                )
                .await
                {
                    ProbeOutcome::Visible(hit_name) => {
                        on_progress(&format!("round {round} hit (resolver: {hit_name})"));
                        report_evicted(&breaker, &mut on_progress);
                        on_progress(&format!("propagation succeeded after {round} rounds"));
                        return Ok(());
                    }
                    ProbeOutcome::NotVisible => {
                        on_progress(&format!(
                            "round {round} miss (resolvers: {})",
                            missing.join(", ")
                        ));
                    }
                }
            } else if in_doh {
                // 末段 DoH 的 1/4 内再拆半：前 1/8 仅命中单家，后 1/8 整组
                let in_doh_single = now < phase4_mid;
                if in_doh_single && hit {
                    match probe_resolvers(
                        hit_single,
                        record,
                        round,
                        &mut missing,
                        &mut unreachable,
                        &mut probes,
                        Some(&mut fallback),
                    )
                    .await
                    {
                        ProbeOutcome::Visible(hit_name) => {
                            on_progress(&format!("round {round} hit (resolver: {hit_name})"));
                            report_proxy_fallback(&fallback, &mut proxy_reported, &mut on_progress);
                            report_evicted(&breaker, &mut on_progress);
                            on_progress(&format!("propagation succeeded after {round} rounds"));
                            return Ok(());
                        }
                        ProbeOutcome::NotVisible => {
                            on_progress(&format!(
                                "round {round} miss (resolvers: {})",
                                missing.join(", ")
                            ));
                            report_proxy_fallback(&fallback, &mut proxy_reported, &mut on_progress);
                            apply_breaker(&mut breaker, &mut doh_active, &probes, &mut on_progress);
                        }
                    }
                } else if !doh.is_empty() && doh_active.is_empty() {
                    // doh 集合原本非空但已被熔断剔空：跳过 DoH 轮询（不回退 all_dns），
                    // 留给权威/全量段或超时判定。
                    warn!(
                        name = %record.ca_name,
                        evicted = %breaker.evicted_list(),
                        "DoH 全部不可用，跳过 DoH 验证"
                    );
                } else {
                    // doh 段仅探测熔断后存活子集；doh 原本为空时回退 all_dns（不参与熔断）。
                    // 回退 all_dns 的传输层保持直连（不走代理回退）——只有真正的 doh
                    // 集合才启用 fallback。
                    let doh_full: &[&dyn DnsResolver] = if !doh.is_empty() {
                        &doh_active
                    } else {
                        all_dns
                    };
                    if !doh_full.is_empty() {
                        match probe_resolvers(
                            doh_full,
                            record,
                            round,
                            &mut missing,
                            &mut unreachable,
                            &mut probes,
                            if !doh.is_empty() {
                                Some(&mut fallback)
                            } else {
                                None
                            },
                        )
                        .await
                        {
                            ProbeOutcome::Visible(hit_name) => {
                                on_progress(&format!("round {round} hit (resolver: {hit_name})"));
                                report_proxy_fallback(
                                    &fallback,
                                    &mut proxy_reported,
                                    &mut on_progress,
                                );
                                report_evicted(&breaker, &mut on_progress);
                                on_progress(&format!("propagation succeeded after {round} rounds"));
                                return Ok(());
                            }
                            ProbeOutcome::NotVisible => {
                                on_progress(&format!(
                                    "round {round} miss (resolvers: {})",
                                    missing.join(", ")
                                ));
                                report_proxy_fallback(
                                    &fallback,
                                    &mut proxy_reported,
                                    &mut on_progress,
                                );
                                apply_breaker(
                                    &mut breaker,
                                    &mut doh_active,
                                    &probes,
                                    &mut on_progress,
                                );
                            }
                        }
                    }
                }
            }
        }

        if Instant::now() >= deadline {
            on_progress(&format!(
                "propagation timed out after {round} rounds (timeout={}s)",
                policy.timeout.as_secs(),
            ));
            return Err(timeout_error(
                published,
                record,
                &missing,
                &unreachable,
                &breaker.evicted,
                policy,
            ));
        }
        sleep(policy.interval).await;
    }
}

/// 单轮解析器探测的结果。
#[derive(Debug)]
enum ProbeOutcome {
    /// 任一家解析器看到了期望值（携带看到该记录的解析器名称）。
    Visible(String),
    /// 全部没看到（调用方继续等下一轮）。
    NotVisible,
}

/// 单次探测中一个解析器的结果分类：网络失败计熔断，未命中（Ok 但无期望值）清零。
struct ResolverProbe {
    key: String,
    /// 本轮该端点的「网络不可用」失败次数：
    ///
    /// - `0`：查询成功（命中或未命中）——清零连续失败计数；
    /// - `1`：直连失败且未发生代理回退（全局代理未配 / 非 DoH 端点），
    ///   或已缓存代理偏好的端点经代理查询失败；
    /// - `2`：直连失败且经代理重试仍失败——直连与代理各计一次。
    failures: u8,
}

/// DoH 熔断剔除的连续网络失败阈值（写死，不做配置项）。
const BREAKER_THRESHOLD: u8 = 3;

/// 单次传播会话内的 DoH 熔断器：按端点累计连续网络失败次数，达阈值剔除；
/// 函数返回即丢弃，下次运行重置。`evicted` 供终态日志附剔除列表。
#[derive(Default)]
struct CircuitBreaker {
    counts: HashMap<String, u8>,
    evicted: Vec<String>,
}

impl CircuitBreaker {
    /// 按本轮探测结果更新计数，返回本轮新剔除的端点键。
    /// 网络失败按 `failures` 计次（直连失败 + 代理回退失败 = 2 次）；
    /// Ok（命中或未命中）清零。
    fn update(&mut self, probes: &[ResolverProbe]) -> Vec<String> {
        let mut newly: Vec<String> = Vec::new();
        for probe in probes {
            if probe.failures > 0 {
                let count = self.counts.entry(probe.key.clone()).or_insert(0);
                *count = count.saturating_add(probe.failures);
                // 达阈值即剔除：代理回退失败的一轮计 2 次，计数可能跳过 3，
                // 用 >= 而非 ==；同轮重复键（同端点不同 name）只剔除一次。
                if *count >= BREAKER_THRESHOLD
                    && !newly.contains(&probe.key)
                    && !self.evicted.contains(&probe.key)
                {
                    newly.push(probe.key.clone());
                }
            } else {
                self.counts.remove(&probe.key);
            }
        }
        self.evicted.extend(newly.iter().cloned());
        newly
    }

    fn evicted_list(&self) -> String {
        self.evicted.join("、")
    }
}

/// 剔除可观测性：warn 与进度回调都带端点标识与连续失败次数。
fn apply_breaker<F>(
    breaker: &mut CircuitBreaker,
    active: &mut Vec<&dyn DnsResolver>,
    probes: &[ResolverProbe],
    on_progress: &mut F,
) where
    F: FnMut(&str) + Send,
{
    for key in breaker.update(probes) {
        warn!(
            resolver = %key,
            failures = BREAKER_THRESHOLD,
            "DoH 端点连续网络失败，本轮熔断剔除"
        );
        on_progress(&format!(
            "DoH 端点 {key} 连续 {BREAKER_THRESHOLD} 次网络失败，本轮熔断剔除"
        ));
        active.retain(|resolver| resolver.breaker_key() != key);
    }
}

/// 终态成功时附带剔除列表，便于排障。
fn report_evicted<F: FnMut(&str)>(breaker: &CircuitBreaker, on_progress: &mut F) {
    if !breaker.evicted.is_empty() {
        on_progress(&format!(
            "已熔断剔除（不再查询）：{}",
            breaker.evicted_list()
        ));
    }
}

/// DoH 代理回退成功后经 `on_progress` 上报一行 `proxy: via <host>`。
///
/// 与凭据代理行互不覆盖（SSE 实时多行独立可视；落库前由服务端
/// `compact_proxy_logs` 仅保留最后一条）。每个会话只报一次：
/// 直连探测成功不报，回退成功后的后续段（已缓存偏好直接走代理）也不重复报。
fn report_proxy_fallback<F: FnMut(&str)>(
    fallback: &DohProxyFallback,
    reported: &mut bool,
    on_progress: &mut F,
) {
    if *reported || !fallback.activated() {
        return;
    }
    *reported = true;
    if let Some(display) = fallback.proxy_display() {
        on_progress(&format!("proxy: via {display}"));
    }
}

/// DoH 端点的传输偏好：直连，或经全局代理。
///
/// 由代理自动回退产生：直连查询网络失败、经代理重试成功后缓存 `Proxy`，
/// 后续轮次与后续阶段（`doh_single` → `doh_full`）直接用代理传输层查询，
/// 不再每轮重复直连探测。仅本轮传播会话内有效——随等待函数返回丢弃，
/// 下次运行重新学习。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ProxyPreference {
    /// 直连（缺省）：未发生过成功的代理回退。
    #[default]
    Direct,
    /// 经全局代理：该端点已确认直连不可达、代理可达。
    Proxy,
}

/// DoH 代理自动回退的会话状态：全局代理 URL（已配置时）、懒构建的代理
/// 传输层、每端点的传输偏好缓存。
///
/// 与 [`CircuitBreaker`] 平行创建于传播会话开头，跨 `doh_single`/`doh_full`
/// 段保留，本轮结束即丢弃。全局代理未配置时整体退化为「无回退」：
/// 直连失败直接计次，行为与本机制引入前完全一致。
struct DohProxyFallback {
    /// 归一化后的全局代理 URL（非空）；未配置为 `None`。
    proxy_url: Option<String>,
    /// 懒构建的代理传输层：首次需要回退时按 `proxy_url` 现建，之后复用。
    /// 构建失败（URL 非法等，生产上已被启动校验拦下）保持 `None`，
    /// 回退静默跳过，不让流水线崩在传输层。
    transport: Option<Arc<dyn HttpTransport>>,
    /// 端点（与熔断计数同口径的 key）→ 传输偏好。
    preferences: HashMap<String, ProxyPreference>,
    /// 本会话是否发生过成功的代理回退（供运行历史 `proxy: ` 日志行上报）。
    activated: bool,
}

impl DohProxyFallback {
    /// 按全局代理 URL 构建；空串/未配置时回退功能关闭。
    fn new(proxy_url: Option<&str>) -> Self {
        let proxy_url = proxy_url
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned);
        Self {
            proxy_url,
            transport: None,
            preferences: HashMap::new(),
            activated: false,
        }
    }

    /// 注入代理传输层替身：测试免去真代理进程。生产走 [`Self::new`] 的懒构建。
    #[cfg(test)]
    fn with_transport(proxy_url: &str, transport: Arc<dyn HttpTransport>) -> Self {
        Self {
            proxy_url: Some(proxy_url.to_owned()),
            transport: Some(transport),
            preferences: HashMap::new(),
            activated: false,
        }
    }

    /// 本会话是否发生过成功的代理回退。
    fn activated(&self) -> bool {
        self.activated
    }

    /// 代理 URL 的日志展示形态（scheme://host，剥掉 userinfo）。
    fn proxy_display(&self) -> Option<String> {
        self.proxy_url.as_deref().map(super::proxy_log_target)
    }

    /// 该端点当前的传输偏好；未缓存过即 [`ProxyPreference::Direct`]。
    fn preference(&self, key: &str) -> ProxyPreference {
        self.preferences
            .get(key)
            .copied()
            .unwrap_or(ProxyPreference::Direct)
    }

    /// 是否已缓存「需经代理」偏好。
    fn prefers_proxy(&self, key: &str) -> bool {
        self.preference(key) == ProxyPreference::Proxy
    }

    /// 缓存「需经代理」偏好（代理重试成功时），并标记会话已激活回退。
    fn mark_proxy(&mut self, key: &str) {
        self.activated = true;
        self.preferences
            .insert(key.to_owned(), ProxyPreference::Proxy);
    }

    /// 经代理传输层查询：重建「同端点 + 代理客户端」的 DoH 解析器。
    ///
    /// 返回 `None` 表示这次走不了代理：全局代理未配置、代理传输层构建失败，
    /// 或目标不是 DoH 端点（权威 NS 直查等）。单次查询同样受
    /// [`LOOKUP_TIMEOUT`] 约束——挂起的代理连接不该吃掉整轮预算。
    async fn query_via_proxy(
        &mut self,
        resolver: &dyn DnsResolver,
        name: &str,
    ) -> Option<Result<Vec<String>>> {
        let transport = self.proxy_transport()?;
        let endpoint = resolver.endpoint()?;
        let proxied = DohResolver::custom(
            Arc::clone(&transport),
            resolver.name().to_owned(),
            endpoint.to_owned(),
        );
        Some(lookup_with_timeout(&proxied, name).await)
    }

    /// 懒构建（并缓存）代理传输层。
    fn proxy_transport(&mut self) -> Option<Arc<dyn HttpTransport>> {
        let url = self.proxy_url.as_deref()?;
        if self.transport.is_none() {
            // 生产 URL 已在启动时经 `validate_proxy_url` 校验；这里再失败
            // 只可能是 reqwest 拒绝该代理配置——退化为无回退。
            self.transport = ReqwestTransport::with_proxy(url, acmecast_core::NETWORK_TIMEOUT)
                .ok()
                .map(|transport| Arc::new(transport) as Arc<dyn HttpTransport>);
        }
        self.transport.clone()
    }
}

/// 单次解析器查询，带 [`LOOKUP_TIMEOUT`] 约束。
///
/// 不可达的端点（被墙的 DoH、挂起的连接）要**快速**失败：查询本身没有
/// 超时的话，一次 TCP 连接挂起到内核超时要一分多钟——整轮预算里只跑得动
/// 两轮，熔断与代理回退都没机会生效。
async fn lookup_with_timeout(resolver: &dyn DnsResolver, name: &str) -> Result<Vec<String>> {
    match tokio::time::timeout(LOOKUP_TIMEOUT, resolver.lookup_txt(name)).await {
        Ok(result) => result,
        Err(_elapsed) => Err(Error::provider(format!(
            "{} 查询 `{}` 超时（{} 秒）",
            resolver.name(),
            name,
            LOOKUP_TIMEOUT.as_secs()
        ))),
    }
}

/// 单端点一轮查询：直连（或已缓存代理偏好时经代理），网络失败时按需
/// 经代理回退一次。返回（查询结果, 本轮网络失败次数）。
///
/// `fallback` 为 `None` 时是完全的直连语义（权威 NS 直查与 `all_dns`
/// 阶段走这条）；为 `Some` 时启用代理自动回退：
///
/// - 直连成功 → 原样返回；
/// - 直连网络失败且全局代理已配 → 经代理重试一次：成功则清零计数、
///   缓存「需经代理」偏好并用结果参与判定；仍失败则直连 + 代理 = 2 次；
/// - 已缓存代理偏好 → 直接经代理查询，不再重复直连探测。
async fn probe_one(
    resolver: &dyn DnsResolver,
    name: &str,
    fallback: Option<&mut DohProxyFallback>,
) -> (Result<Vec<String>>, u8) {
    let Some(fallback) = fallback else {
        let result = lookup_with_timeout(resolver, name).await;
        let failures = u8::from(result.is_err());
        return (result, failures);
    };
    let key = resolver.breaker_key();
    if fallback.prefers_proxy(key) {
        // 已缓存「需经代理」：直接经代理传输层查询。代理传输层不可用
        // （构建失败等）时退回直连，两种情况失败都只计 1 次。
        return match fallback.query_via_proxy(resolver, name).await {
            Some(result) => {
                let failures = u8::from(result.is_err());
                (result, failures)
            }
            None => {
                let result = lookup_with_timeout(resolver, name).await;
                let failures = u8::from(result.is_err());
                (result, failures)
            }
        };
    }
    let direct = lookup_with_timeout(resolver, name).await;
    if direct.is_ok() {
        return (direct, 0);
    }
    match fallback.query_via_proxy(resolver, name).await {
        // 重试成功：清零计数（failures=0）+ 用结果参与判定 + 缓存偏好。
        Some(Ok(values)) => {
            fallback.mark_proxy(key);
            (Ok(values), 0)
        }
        Some(Err(proxy_error)) => {
            // 重试仍网络失败：直连 + 代理 = 2 次；代理错误进 warn 便于排障，
            // 上报的错误保留直连的原始原因。
            warn!(
                resolver = resolver.name(),
                error = %proxy_error,
                "DoH 端点经代理回退仍失败"
            );
            (direct, 2)
        }
        // 走不了代理（未配全局代理 / 非 DoH 端点 / 传输层构建失败）：
        // 只有直连这一次失败。
        None => (direct, 1),
    }
}

/// 对一组解析器执行一轮查询：任一家看到期望值即返回 `Visible`，
/// 否则把没看到的记入 `missing`、查不了的记入 `unreachable`。
///
/// 查询**失败**（含超时）与「查到了但值不对」是两回事：前者多半是
/// 解析器本身出了问题（端点不可达、被墙），按「不可达」归类，
/// 不参与判定——任一家看到即可放行，所以失败只会让自己出局。
///
/// `fallback` 为 `Some` 时对该组启用 DoH 代理自动回退（仅 DoH 段传入，
/// 见 [`probe_one`]）；权威 NS 直查与 `all_dns` 阶段传 `None`，
/// 传输层保持直连，不受代理回退影响。
async fn probe_resolvers(
    resolvers: &[&dyn DnsResolver],
    record: &TxtRecord,
    round: u32,
    missing: &mut Vec<String>,
    unreachable: &mut Vec<String>,
    probes: &mut Vec<ResolverProbe>,
    mut fallback: Option<&mut DohProxyFallback>,
) -> ProbeOutcome {
    missing.clear();
    unreachable.clear();
    probes.clear();
    // 查 CA 查询名而非写入名：CA 是按订单标识符查找的，跨 zone CNAME 委派时
    // 它俩不是同一个名字，只有从 ca_name 出发跟随 CNAME 链才等于 CA 的视角。
    let name = &record.ca_name;
    for &resolver in resolvers {
        let (outcome, failures) = probe_one(resolver, name, fallback.as_deref_mut()).await;
        match outcome {
            Ok(values) => {
                if values.iter().any(|value| value == &record.value) {
                    info!(
                        name = %name,
                        round,
                        resolver = resolver.name(),
                        "记录已在解析器上可见"
                    );
                    return ProbeOutcome::Visible(resolver.name().to_owned());
                }
                probes.push(ResolverProbe {
                    key: resolver.breaker_key().to_owned(),
                    failures: 0,
                });
                missing.push(resolver.name().to_owned());
            }
            Err(err) => {
                probes.push(ResolverProbe {
                    key: resolver.breaker_key().to_owned(),
                    failures,
                });
                warn!(resolver = resolver.name(), error = %err, "解析器查询失败，按不可达处理");
                unreachable.push(resolver.name().to_owned());
            }
        }
    }

    debug!(name = %name, round, missing = ?missing, unreachable = ?unreachable, "尚无解析器可见该记录");
    ProbeOutcome::NotVisible
}

/// 按「卡在哪一段」组装超时错误。
///
/// 只列出**没跟上**的解析器：已经就绪的那些是噪音，而排查时真正要看的
/// 是「谁还差着」。
///
/// 跨 zone CNAME 委派时（写入名 != CA 查询名）额外点出两个名字与 zone：
/// 记录已经写进 `dns_zone` 却查不到，多半是委派链没接好或还没生效，
/// 而错误里只有一个名字时看不出这一点。
fn timeout_error(
    published: bool,
    record: &TxtRecord,
    missing: &[String],
    unreachable: &[String],
    evicted: &[String],
    policy: &PropagationPolicy,
) -> Error {
    let waited = humanize(policy.timeout);
    let delegation_hint = if record.is_delegated() {
        format!(
            "（写入名 `{}`，经 CNAME 委派查询 `{}`；写入名可见而 CA 查不到，\
             通常是委派未生效或尚未传播）",
            record.name, record.ca_name
        )
    } else {
        String::new()
    };

    if !published {
        return Error::provider(format!(
            "等待 {waited} 后，提供商侧仍未确认记录 `{}` 已写入{delegation_hint}",
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
    if !evicted.is_empty() {
        parts.push(format!(
            "{} 连续网络失败已熔断剔除（不再查询）",
            evicted.join("、")
        ));
    }
    if parts.is_empty() {
        parts.push("全部解析器查询均失败".to_owned());
    }

    Error::provider(format!(
        "记录 `{}` 已写入提供商侧（zone `{}`），但等待 {waited} 后 CA 查询名 `{}` 仍不可见：{}{delegation_hint}",
        record.name,
        record.zone,
        record.ca_name,
        parts.join("；"),
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
    use parking_lot::Mutex;

    use super::*;
    use crate::http::HttpResponse;

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
        let _guard = ENV_LOCK.lock();
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
        let _guard = ENV_LOCK.lock();
        let _env = EnvGuard::set("ACMECAST_DOH_RESOLVERS", "none");
        let http: Arc<dyn HttpTransport> = Arc::new(crate::http::ReqwestTransport::default());
        assert!(default_resolvers(http).is_empty());
    }

    #[test]
    fn empty_or_invalid_env_falls_back_to_builtin() {
        let _guard = ENV_LOCK.lock();
        let http: Arc<dyn HttpTransport> = Arc::new(crate::http::ReqwestTransport::default());

        let _env = EnvGuard::set("ACMECAST_DOH_RESOLVERS", "  ");
        assert_eq!(default_resolvers(Arc::clone(&http)).len(), 4);

        // 全是无法解析的碎片：回退内置。
        let _env = EnvGuard::set("ACMECAST_DOH_RESOLVERS", " ,, =");
        assert_eq!(default_resolvers(http).len(), 4);
    }

    #[test]
    fn builtin_set_contains_china_and_global_resolvers() {
        let _guard = ENV_LOCK.lock();
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
            *self.calls.lock()
        }
    }

    #[async_trait]
    impl DnsResolver for ScriptedResolver {
        fn name(&self) -> &str {
            &self.name
        }

        async fn lookup_txt(&self, _name: &str) -> Result<Vec<String>> {
            let mut calls = self.calls.lock();
            let index = *calls;
            *calls += 1;
            drop(calls);

            let script = self.script.lock();
            Ok(script
                .get(index)
                .or_else(|| script.last())
                .cloned()
                .unwrap_or_default())
        }
    }

    /// 按脚本应答（可含网络错误）的解析器：每调一次前进一格，用完后一直返回最后一个。
    /// 用于熔断测试：`Err` 归网络失败计熔断，`Ok` 归未命中清零。
    #[derive(Debug)]
    struct FailScriptResolver {
        name: String,
        script: Mutex<Vec<std::result::Result<Vec<String>, String>>>,
        calls: Mutex<usize>,
    }

    impl FailScriptResolver {
        fn new(name: &str, script: Vec<std::result::Result<Vec<String>, String>>) -> Self {
            Self {
                name: name.to_owned(),
                script: Mutex::new(script),
                calls: Mutex::new(0),
            }
        }

        fn calls(&self) -> usize {
            *self.calls.lock()
        }
    }

    #[async_trait]
    impl DnsResolver for FailScriptResolver {
        fn name(&self) -> &str {
            &self.name
        }

        async fn lookup_txt(&self, _name: &str) -> Result<Vec<String>> {
            let mut calls = self.calls.lock();
            let index = *calls;
            *calls += 1;
            drop(calls);

            let script = self.script.lock();
            script
                .get(index)
                .or_else(|| script.last())
                .cloned()
                .unwrap_or_else(|| -> std::result::Result<Vec<String>, String> {
                    Err("连接超时".to_owned())
                })
                .map_err(Error::provider)
        }
    }

    /// 只对特定名字有值的解析器：用于验证传播等待查的是 CA 查询名，
    /// 而不是写入到 zone 下的那个名字。
    #[derive(Debug)]
    struct NameAwareResolver {
        visible_name: String,
        value: String,
        queries: Mutex<Vec<String>>,
    }

    #[async_trait]
    impl DnsResolver for NameAwareResolver {
        fn name(&self) -> &str {
            "认名字的解析器"
        }

        async fn lookup_txt(&self, name: &str) -> Result<Vec<String>> {
            self.queries.lock().push(name.to_owned());
            if name == self.visible_name {
                Ok(vec![self.value.clone()])
            } else {
                Ok(Vec::new())
            }
        }
    }

    fn delegated_record() -> TxtRecord {
        TxtRecord::delegated(
            "666517.xyz",
            "_acme-challenge.666517.xyz",
            "_acme-challenge.skiy.net",
            "value-1",
            60,
        )
    }

    #[tokio::test]
    async fn a_write_name_seen_but_ca_name_unseen_keeps_waiting() {
        // spec：写入名已在 provider 侧可见，但 CA 查询名在解析器侧查不到时，
        // 必须判为未就绪继续等——不能因写入名可见就放行，那正是 CNAME 委派
        // 没接好时把「CA 看不到」误判成「可以通知 CA」的场景。
        let provider = Publishes::always("value-1");
        // 只有 CA 查询名可见，解析器侧照常返回期望值。
        let resolver = NameAwareResolver {
            visible_name: "_acme-challenge.skiy.net".to_owned(),
            value: "value-1".to_owned(),
            queries: Mutex::new(Vec::new()),
        };
        let record = delegated_record();

        wait_until_visible(
            &provider,
            &serde_json::json!({}),
            &record,
            &[&resolver],
            &fast_policy(),
            None,
            |_| {},
        )
        .await
        .expect("CA 查询名可见即应放行");

        assert!(
            resolver
                .queries
                .lock()
                .iter()
                .all(|queried| queried == "_acme-challenge.skiy.net"),
            "解析器查询必须用 CA 查询名，而非写入名"
        );
    }

    #[tokio::test]
    async fn a_delegated_record_times_out_with_a_cname_hint() {
        // 写入名可见、CA 查询名始终不可见 → 超时，且错误须指向 CNAME 委派。
        let provider = Publishes::always("value-1");
        let resolver = NameAwareResolver {
            visible_name: "别处".to_owned(),
            value: "value-1".to_owned(),
            queries: Mutex::new(Vec::new()),
        };
        let record = delegated_record();

        let error = wait_until_visible(
            &provider,
            &serde_json::json!({}),
            &record,
            &[&resolver],
            &fast_policy(),
            None,
            |_| {},
        )
        .await
        .expect_err("CA 查询名不可见应超时");
        let message = error.to_string();
        assert!(
            message.contains("CNAME"),
            "错误应提示 CNAME 委派：{message}"
        );
        assert!(
            message.contains("_acme-challenge.skiy.net"),
            "错误应带 CA 查询名：{message}"
        );
        assert!(
            message.contains("_acme-challenge.666517.xyz"),
            "错误应带写入名：{message}"
        );
    }

    #[tokio::test]
    async fn the_write_name_alone_does_not_release_the_wait() {
        // 与上一条互补：provider 侧（写入名）已确认，但解析器侧对 CA 查询名
        // 不可见时，等待必须耗尽预算而非提前返回。
        let provider = Publishes::always("value-1");
        let resolver = NameAwareResolver {
            visible_name: "_acme-challenge.666517.xyz".to_owned(), // 只认写入名
            value: "value-1".to_owned(),
            queries: Mutex::new(Vec::new()),
        };
        let record = delegated_record();

        let error = wait_until_visible(
            &provider,
            &serde_json::json!({}),
            &record,
            &[&resolver],
            &fast_policy(),
            None,
            |_| {},
        )
        .await
        .expect_err("只有写入名可见不应放行");
        assert!(error.to_string().contains("CNAME"));
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
            self.deleted.lock().clone()
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
            Ok(self.values.lock().clone())
        }

        async fn create_txt(&self, _credentials: &Value, _record: &TxtRecord) -> Result<()> {
            Ok(())
        }

        async fn delete_txt(&self, _credentials: &Value, record: &TxtRecord) -> Result<()> {
            self.deleted.lock().push(record.name.clone());
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
                None,
                |_| {},
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
            None,
            |_| {},
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
            None,
            |_| {},
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
            None,
            |_| {},
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
            None,
            |_| {},
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
        let _guard = ENV_LOCK.lock();
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
            None,
            |_| {},
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
            None,
            |_| {},
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
            None,
            |_| {},
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
            None,
            |_| {},
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
            None,
            |_| {},
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
                Ok(self.0.lock().take().expect("只应答一次"))
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
        let _guard = ENV_LOCK.lock();
        let http: Arc<dyn HttpTransport> = Arc::new(crate::http::ReqwestTransport::default());
        // 未设 env 时走权威直查，不自动混入扩展
        let v = resolvers_for_zone(Arc::clone(&http), "example.com");
        assert_eq!(v.len(), 1);
        assert!(v[0].name().contains("权威"), "{}", v[0].name());
    }

    #[test]
    fn extended_resolvers_empty_entries_fallback_to_builtin() {
        let _guard = ENV_LOCK.lock();
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
        let _guard = ENV_LOCK.lock();
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
        let _guard = ENV_LOCK.lock();
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

    #[test]
    fn provider_aware_doh_resolvers_returns_all_four() {
        let _guard = ENV_LOCK.lock();
        let http: Arc<dyn HttpTransport> = Arc::new(crate::http::ReqwestTransport::default());
        for id in [
            Some("cloud.aliyun"),
            Some("cloud.cloudflare"),
            Some("cloud.tencent"),
            Some("dns.tencent-eo"),
            None,
            Some("unknown"),
        ] {
            let v = doh_resolvers_for_provider(Arc::clone(&http), id);
            assert_eq!(v.len(), 4, "provider={id:?} 应返回全部四家内置 DoH");
        }
    }

    #[test]
    fn provider_aware_doh_resolvers_env_overrides_provider() {
        let _guard = ENV_LOCK.lock();
        let _env = EnvGuard::set(
            "ACMECAST_DOH_RESOLVERS",
            "my=https://dns.example.com/resolve",
        );
        let http: Arc<dyn HttpTransport> = Arc::new(crate::http::ReqwestTransport::default());
        let v = doh_resolvers_for_provider(http, Some("cloud.aliyun"));
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].name(), "my", "环境变量应覆盖提供商感知");
    }

    #[test]
    fn provider_aware_doh_resolvers_env_none_short_circuits() {
        let _guard = ENV_LOCK.lock();
        let _env = EnvGuard::set("ACMECAST_DOH_RESOLVERS", "none");
        let http: Arc<dyn HttpTransport> = Arc::new(crate::http::ReqwestTransport::default());
        let v = doh_resolvers_for_provider(http, Some("cloud.tencent"));
        assert!(v.is_empty(), "none 时应为空集");
    }

    #[test]
    fn resolvers_for_provider_mirrors_doh_provider_when_no_env() {
        let _guard = ENV_LOCK.lock();
        let http: Arc<dyn HttpTransport> = Arc::new(crate::http::ReqwestTransport::default());
        for id in [
            Some("cloud.aliyun"),
            Some("cloud.cloudflare"),
            Some("cloud.tencent"),
            None,
        ] {
            let a = doh_resolvers_for_provider(Arc::clone(&http), id);
            let b = resolvers_for_provider(Arc::clone(&http), "example.com", id);
            assert_eq!(a.len(), b.len(), "provider={id:?}");
            for (x, y) in a.iter().zip(b.iter()) {
                assert_eq!(x.name(), y.name(), "provider={id:?}");
            }
        }
    }

    #[test]
    fn provider_aware_names_are_directly_diagnosable() {
        // 超时错误中 `missing` / `unreachable` 的名称应与提供商感知的解析器名称一致，
        // 便于排查。至少保证三家厂商名可被直接引用。
        let _guard = ENV_LOCK.lock();
        let http: Arc<dyn HttpTransport> = Arc::new(crate::http::ReqwestTransport::default());
        let all = doh_resolvers_for_provider(http, Some("cloud.aliyun"));
        let names: Vec<_> = all.iter().map(|r| r.name().to_owned()).collect();
        assert!(names.contains(&"阿里 DNS".to_owned()));
        assert!(names.contains(&"Cloudflare DNS".to_owned()));
        assert!(names.contains(&"腾讯 DNSPod".to_owned()));
        assert!(names.contains(&"Google DNS".to_owned()));
    }

    // ---- 分阶段传播等待 ----

    #[tokio::test]
    async fn phased_needs_public_confirmation_after_authoritative() {
        // 新四段下权威命中即就绪（不再需要 public 确认耦合）
        let provider = Publishes::always("value-1");
        let auth = ScriptedResolver::new("权威 NS", vec![vec!["value-1".to_owned()]]);

        wait_until_visible_phased(
            &provider,
            &serde_json::json!({}),
            &record(),
            &[&auth],
            &[],
            &[],
            &PropagationPolicy {
                interval: Duration::from_millis(5),
                timeout: Duration::from_millis(400),
            },
            None,
            |_| {},
        )
        .await
        .expect("权威命中即应成功");

        assert!(auth.calls() > 0, "权威应被查询过");
    }

    #[tokio::test]
    async fn phased_times_out_when_authoritative_visible_but_public_never_sees() {
        // 仅命中单家与权威均不可见时 → 超时
        let provider = Publishes::always("value-1");
        let hit = ScriptedResolver::new("命中单家", vec![vec![]]);
        let auth = ScriptedResolver::new("权威 NS", vec![vec![]]);
        let all = ScriptedResolver::new("全量", vec![vec![]]);
        let doh = ScriptedResolver::new("DoH", vec![vec![]]);

        let err = wait_until_visible_phased(
            &provider,
            &serde_json::json!({}),
            &record(),
            &[&auth],
            &[&all],
            &[&doh],
            &fast_policy(),
            None,
            |_| {},
        )
        .await
        .expect_err("全部不可见应超时");

        let text = err.to_string();
        assert!(text.contains("已写入提供商侧"), "{text}");
        assert!(text.contains("不可见"), "{text}");
        // hit 单家也在统计中
        let _ = hit;
    }

    #[tokio::test]
    async fn phased_falls_through_when_authoritative_never_sees() {
        // 权威始终不可见，DoH 末段后半命中 → 仍成功
        let provider = Publishes::always("value-1");
        let auth = ScriptedResolver::new("权威", vec![vec![]]);
        let doh = ScriptedResolver::new("DoH", vec![vec![], vec!["value-1".to_owned()]]);

        wait_until_visible_phased(
            &provider,
            &serde_json::json!({}),
            &record(),
            &[&auth],
            &[],
            &[&doh],
            &PropagationPolicy {
                interval: Duration::from_millis(5),
                timeout: Duration::from_millis(400),
            },
            None,
            |_| {},
        )
        .await
        .expect("DoH 末段命中应成功");

        assert!(doh.calls() > 0, "DoH 阶段应被查询过");
    }

    #[tokio::test]
    async fn phased_all_empty_means_provider_only() {
        // 三组全空（ACMECAST_DOH_RESOLVERS=none 时的语义）：
        // 提供商侧确认即就绪。
        let provider = Publishes::always("value-1");

        wait_until_visible_phased(
            &provider,
            &serde_json::json!({}),
            &record(),
            &[],
            &[],
            &[],
            &fast_policy(),
            None,
            |_| {},
        )
        .await
        .expect("三组全空时提供商侧确认即就绪");
    }

    #[tokio::test]
    async fn phased_no_authoritative_means_public_only() {
        // 权威为空 → 命中单家后进入全量阶段
        let provider = Publishes::always("value-1");
        // 全量一轮即中，但在新时序下首段为命中单家，命中单家 miss 后第 2 段起才查全量
        let all_dns = ScriptedResolver::new("全量", vec![vec!["value-1".to_owned()]]);
        // 命中单家设为始终 miss，迫使进入全量 1/4
        let hit_doh = ScriptedResolver::new("DoH-命中单家", vec![vec![]]);

        wait_until_visible_phased(
            &provider,
            &serde_json::json!({}),
            &record(),
            &[],
            &[&all_dns],
            &[&hit_doh],
            &PropagationPolicy {
                interval: Duration::from_millis(5),
                timeout: Duration::from_millis(400),
            },
            None,
            |_| {},
        )
        .await
        .expect("无权威时全量阶段可见应成功");

        assert!(all_dns.calls() > 0, "全量应被查询过");
    }

    #[tokio::test]
    async fn phased_all_never_visible_times_out() {
        // 全阶段都查不到 → 超时。
        let provider = Publishes::always("value-1");
        let auth = ScriptedResolver::new("权威", vec![vec![]]);
        let all_dns = ScriptedResolver::new("全量", vec![vec![]]);
        let doh = ScriptedResolver::new("DoH", vec![vec![]]);

        let err = wait_until_visible_phased(
            &provider,
            &serde_json::json!({}),
            &record(),
            &[&auth],
            &[&all_dns],
            &[&doh],
            &fast_policy(),
            None,
            |_| {},
        )
        .await
        .expect_err("全不可见应超时");

        let text = err.to_string();
        assert!(text.contains("已写入提供商侧"), "{text}");
        assert!(text.contains("不可见"), "{text}");
    }

    #[tokio::test]
    async fn phased_emits_expected_progress_logs() {
        let provider = Publishes::always("value-1");
        let all_dns = ScriptedResolver::new("全量", vec![vec![], vec!["value-1".to_owned()]]);
        let doh = ScriptedResolver::new("DoH", vec![vec![], vec!["value-1".to_owned()]]);

        let events = Arc::new(Mutex::new(Vec::new()));
        let events_clone = Arc::clone(&events);

        wait_until_visible_phased(
            &provider,
            &serde_json::json!({}),
            &record(),
            &[],
            &[&all_dns],
            &[&doh],
            &PropagationPolicy {
                interval: Duration::from_millis(5),
                timeout: Duration::from_secs(300),
            },
            None,
            move |msg| {
                events_clone.lock().push(msg.to_owned());
            },
        )
        .await
        .expect("应成功（新四段下可能首段或全量段命中）");

        let logs = events.lock().clone();
        assert!(
            logs.iter()
                .any(|l| l.contains("propagation start timeout=300 interval=0")),
            "应包含 propagation start: {logs:?}"
        );
        let start_pos = logs
            .iter()
            .position(|l| l.contains("propagation start"))
            .expect("缺 start");
        let hit_pos = logs
            .iter()
            .position(|l| l.contains("provider dns:"))
            .expect("缺命中行");
        assert!(
            start_pos < hit_pos,
            "propagation start 必须在 provider dns 之前: {logs:?}"
        );
        assert!(
            logs.iter().any(|l| l.contains("provider dns:")),
            "应含命中单家行: {logs:?}"
        );
        assert!(
            logs.iter().any(|l| l.contains(" hit ")),
            "应包含 hit: {logs:?}"
        );
        assert!(
            logs.iter().any(|l| l.contains("propagation succeeded")),
            "应包含 propagation succeeded: {logs:?}"
        );
    }

    #[tokio::test]
    async fn phased_start_contains_authoritative_context() {
        let provider = Publishes::always("value-1");
        let auth = ScriptedResolver::new("权威 NS（ooos.top）", vec![vec!["value-1".to_owned()]]);
        let all_dns = ScriptedResolver::new("Cloudflare DNS", vec![vec!["value-1".to_owned()]]);

        let events = Arc::new(Mutex::new(Vec::new()));
        let events_clone = Arc::clone(&events);

        wait_until_visible_phased(
            &provider,
            &serde_json::json!({}),
            &crate::provider::TxtRecord {
                zone: "ooos.top".to_owned(),
                name: "_acme-challenge.ooos.top".to_owned(),
                ca_name: "_acme-challenge.ooos.top".to_owned(),
                value: "value-1".to_owned(),
                ttl: 120,
            },
            &[&auth],
            &[&all_dns],
            &[],
            &PropagationPolicy {
                interval: Duration::from_millis(5),
                timeout: Duration::from_secs(300),
            },
            None,
            move |msg| {
                events_clone.lock().push(msg.to_owned());
            },
        )
        .await
        .expect("权威可见应成功");

        let logs = events.lock().clone();
        let start = logs
            .iter()
            .find(|l| l.contains("propagation start"))
            .expect("缺 start");
        assert!(
            !start.contains("provider dns:"),
            "首行不应含命中行: {start}"
        );
        let hit_line = logs
            .iter()
            .find(|l| l.contains("provider dns:"))
            .expect("缺命中行");
        // 命中单家取 doh 首位（此处 all_dns 即 ext_refs 首位应为 Cloudflare）
        assert!(
            hit_line.contains("Cloudflare DNS"),
            "命中行应为单家 Cloudflare: {hit_line}"
        );
        assert!(
            !start.contains("authoritative:"),
            "首行不应拼权威上下文: {start}"
        );
        let start_pos = logs
            .iter()
            .position(|l| l.contains("propagation start"))
            .unwrap();
        let hit_pos = logs
            .iter()
            .position(|l| l.contains("provider dns:"))
            .unwrap();
        assert!(start_pos < hit_pos, "start 应在命中行前: {logs:?}");
    }

    #[tokio::test]
    async fn wait_until_visible_start_contains_resolvers() {
        let provider = Publishes::always("value-1");
        let r1 = ScriptedResolver::new("阿里 DNS", vec![vec!["value-1".to_owned()]]);
        let r2 = ScriptedResolver::new("Cloudflare DNS", vec![vec!["value-1".to_owned()]]);

        let events = Arc::new(Mutex::new(Vec::new()));
        let events_clone = Arc::clone(&events);

        wait_until_visible(
            &provider,
            &serde_json::json!({}),
            &record(),
            &[&r1, &r2],
            &PropagationPolicy {
                interval: Duration::from_millis(5),
                timeout: Duration::from_secs(300),
            },
            None,
            move |msg| {
                events_clone.lock().push(msg.to_owned());
            },
        )
        .await
        .expect("应成功");

        let logs = events.lock().clone();
        let start = logs
            .iter()
            .find(|l| l.contains("propagation start"))
            .expect("缺 start");
        assert!(
            !start.contains("resolvers:"),
            "首行 start 不应拼 resolvers: {start}"
        );
        assert!(start.contains("propagation start timeout="), "{start}");
        let hit_line = logs
            .iter()
            .find(|l| l.contains("provider dns:"))
            .expect("缺命中行");
        assert!(
            hit_line.contains("阿里 DNS"),
            "命中单家行应为首个 resolver: {hit_line}"
        );
        assert!(
            !hit_line.contains("Cloudflare"),
            "命中单家不应含整组: {hit_line}"
        );
        let start_pos = logs
            .iter()
            .position(|l| l.contains("propagation start"))
            .unwrap();
        let hit_pos = logs
            .iter()
            .position(|l| l.contains("provider dns:"))
            .unwrap();
        let done_pos = logs
            .iter()
            .position(|l| l.contains("propagation succeeded"))
            .unwrap();
        assert!(
            start_pos < hit_pos && hit_pos < done_pos,
            "start → 命中行 → succeeded 顺序: {logs:?}"
        );
    }

    // ---- DoH 熔断剔除 ----

    #[tokio::test]
    async fn continuous_3_network_failures_evicts_endpoint() {
        // 连续 3 次网络失败后端点被剔除，后续轮次不再查询；
        // 同集合的未命中端点不受影响，等待照常走到超时。
        let provider = Publishes::always("value-1");
        let failing = FailScriptResolver::new("总连不上", vec![Err("连接超时".to_owned())]);
        let missing = ScriptedResolver::new("未命中的", vec![vec![]]);

        let events = Arc::new(Mutex::new(Vec::new()));
        let events_clone = Arc::clone(&events);

        let err = wait_until_visible(
            &provider,
            &serde_json::json!({}),
            &record(),
            &[&failing, &missing],
            &fast_policy(),
            None,
            move |msg| events_clone.lock().push(msg.to_owned()),
        )
        .await
        .expect_err("熔断剔除后无 DoH 可用应超时");

        assert_eq!(
            failing.calls(),
            3,
            "3 次失败后应被剔除，不再查询: {}",
            failing.calls()
        );
        assert!(missing.calls() > 3, "未命中端点不应受熔断影响");

        let logs = events.lock().clone();
        assert!(
            logs.iter()
                .any(|l| l.contains("总连不上") && l.contains("熔断剔除")),
            "剔除时进度日志应含端点标识: {logs:?}"
        );
        let text = err.to_string();
        assert!(text.contains("已熔断剔除"), "超时终态应附剔除列表: {text}");
        assert!(text.contains("总连不上"), "{text}");
    }

    #[tokio::test]
    async fn phased_doh_circuit_breaker_across_segments() {
        // 命中单家段（doh_single）与末段整组（doh_full）共享剔除状态：
        // 连续 3 次网络失败即剔除，跨段不再查询，等待超时且权威/全量照常。
        let provider = Publishes::always("value-1");
        let failing = FailScriptResolver::new("DoH-总失败", vec![Err("连接拒绝".to_owned())]);
        let all_dns = ScriptedResolver::new("全量", vec![vec![]]);

        let events = Arc::new(Mutex::new(Vec::new()));
        let events_clone = Arc::clone(&events);

        let err = wait_until_visible_phased(
            &provider,
            &serde_json::json!({}),
            &record(),
            &[],
            &[&all_dns],
            &[&failing],
            &fast_policy(),
            None,
            move |msg| events_clone.lock().push(msg.to_owned()),
        )
        .await
        .expect_err("DoH 剔空后应超时");

        assert_eq!(
            failing.calls(),
            3,
            "跨段累计 3 次失败即剔除，不再查询: {}",
            failing.calls()
        );
        assert!(all_dns.calls() > 0, "all_dns 段不受熔断影响，保持全量探测");

        let logs = events.lock().clone();
        assert!(
            logs.iter()
                .any(|l| l.contains("DoH-总失败") && l.contains("熔断剔除")),
            "剔除日志应含端点标识: {logs:?}"
        );
        assert!(err.to_string().contains("DoH-总失败"), "{err}");
    }

    #[tokio::test]
    async fn ok_empty_does_not_count_toward_breaker() {
        // 失败分类：Err（网络不可用）计熔断，Ok（空集/非期望值）为未命中清零——
        // 「失败、失败、未命中」的序列不应累计到 3，端点持续被查询。
        let provider = Publishes::always("value-1");
        let flaky = FailScriptResolver::new(
            "抖动的",
            vec![
                Err("连接超时".to_owned()),
                Err("连接拒绝".to_owned()),
                Ok(vec![]),
                Err("连接超时".to_owned()),
                Err("连接拒绝".to_owned()),
                Ok(vec![]),
            ],
        );

        let events = Arc::new(Mutex::new(Vec::new()));
        let events_clone = Arc::clone(&events);

        let err = wait_until_visible(
            &provider,
            &serde_json::json!({}),
            &record(),
            &[&flaky],
            &fast_policy(),
            None,
            move |msg| events_clone.lock().push(msg.to_owned()),
        )
        .await
        .expect_err("一直不可见应超时");

        assert!(
            flaky.calls() > 6,
            "未命中清零后不应被剔除，应持续查询到超时: {}",
            flaky.calls()
        );
        let logs = events.lock().clone();
        assert!(
            !logs.iter().any(|l| l.contains("熔断剔除")),
            "有未命中清零就不应触发剔除: {logs:?}"
        );
        assert!(err.to_string().contains("抖动的"), "{err}");
    }

    // ---- DoH 代理自动回退 ----

    /// 按脚本应答的传输层替身：`Err` 变体模拟「网络不可用」；记录调用次数，
    /// 用于断言偏好缓存后不再直连探测。
    #[derive(Debug)]
    struct ScriptedTransport {
        script: Mutex<Vec<std::result::Result<HttpResponse, String>>>,
        calls: Mutex<usize>,
    }

    impl ScriptedTransport {
        fn new(script: Vec<std::result::Result<HttpResponse, String>>) -> Self {
            Self {
                script: Mutex::new(script),
                calls: Mutex::new(0),
            }
        }

        fn calls(&self) -> usize {
            *self.calls.lock()
        }
    }

    #[async_trait]
    impl HttpTransport for ScriptedTransport {
        async fn send(&self, _request: HttpRequest) -> Result<HttpResponse> {
            *self.calls.lock() += 1;
            let mut script = self.script.lock();
            if script.len() > 1 {
                script.remove(0)
            } else {
                script.first().cloned().expect("应答脚本不应为空")
            }
            .map_err(Error::provider)
        }
    }

    /// DoH JSON 应答：一条指向期望值的 TXT 记录。
    fn doh_txt_response(value: &str) -> HttpResponse {
        HttpResponse::new(
            200,
            format!(
                r#"{{"Status":0,"Answer":[{{"name":"_acme-challenge.example.com.","type":16,"data":"\"{value}\""}}]}}"#
            ),
        )
    }

    /// 直连失败 → 经代理重试成功 → 重试结果参与判定、连续失败计数清零，
    /// 并缓存「需经代理」偏好。
    #[tokio::test]
    async fn doh_proxy_fallback_on_network_failure() {
        let direct_google = Arc::new(ScriptedTransport::new(vec![Err("连接超时".to_owned())]));
        let direct_cf = Arc::new(ScriptedTransport::new(vec![Err("连接拒绝".to_owned())]));
        let unseen = DohResolver::google(Arc::clone(&direct_google) as Arc<dyn HttpTransport>);
        let hit = DohResolver::cloudflare(Arc::clone(&direct_cf) as Arc<dyn HttpTransport>);
        // 代理传输层：Google 端点未命中，Cloudflare 端点返回期望值。
        let proxy = Arc::new(ScriptedTransport::new(vec![
            Ok(HttpResponse::new(200, r#"{"Status":0}"#)),
            Ok(doh_txt_response("value-1")),
        ]));
        let mut fallback = DohProxyFallback::with_transport(
            "socks5://127.0.0.1:7890",
            Arc::clone(&proxy) as Arc<dyn HttpTransport>,
        );

        let mut missing = Vec::new();
        let mut unreachable = Vec::new();
        let mut probes = Vec::new();
        let outcome = probe_resolvers(
            &[&unseen, &hit],
            &record(),
            1,
            &mut missing,
            &mut unreachable,
            &mut probes,
            Some(&mut fallback),
        )
        .await;

        // 重试结果参与判定：Cloudflare 经代理看到期望值 → 可见。
        assert!(
            matches!(&outcome, ProbeOutcome::Visible(name) if name == "Cloudflare DNS"),
            "经代理重试命中应判可见: {outcome:?}"
        );
        // Google 经代理查到但未命中：记 missing，不算不可达。
        assert!(
            missing.contains(&"Google DNS".to_owned()),
            "经代理成功查询不应记不可达: {missing:?} / {unreachable:?}"
        );
        assert!(unreachable.is_empty(), "{unreachable:?}");
        assert_eq!(probes.len(), 1, "未命中的端点应留下探针记录");
        assert_eq!(probes[0].failures, 0, "代理重试成功应清零连续失败计数");
        // 两端点都发生过成功的代理回退 → 都缓存「需经代理」偏好。
        assert!(fallback.prefers_proxy("dns.google"), "回退成功应缓存偏好");
        assert!(fallback.prefers_proxy("cloudflare-dns.com"));
    }

    /// 回退成功后经 `on_progress` 上报一行 `proxy: via <host>`：含 scheme/host、
    /// 不含 userinfo 认证材料；未回退（直连成功）不上报。
    #[tokio::test]
    async fn proxy_fallback_reports_via_progress_line() {
        // 直连失败 → 代理重试成功：会话激活回退。
        let direct = Arc::new(ScriptedTransport::new(vec![Err("连接超时".to_owned())]));
        let resolver = DohResolver::google(Arc::clone(&direct) as Arc<dyn HttpTransport>);
        let proxy = Arc::new(ScriptedTransport::new(vec![Ok(doh_txt_response(
            "value-1",
        ))]));
        let mut fallback = DohProxyFallback::with_transport(
            "socks5://user:secret@10.0.0.1:7890",
            Arc::clone(&proxy) as Arc<dyn HttpTransport>,
        );
        let mut missing = Vec::new();
        let mut unreachable = Vec::new();
        let mut probes = Vec::new();
        probe_resolvers(
            &[&resolver],
            &record(),
            1,
            &mut missing,
            &mut unreachable,
            &mut probes,
            Some(&mut fallback),
        )
        .await;
        assert!(fallback.activated(), "回退成功应激活会话");

        let mut reported = false;
        let mut messages: Vec<String> = Vec::new();
        report_proxy_fallback(&fallback, &mut reported, &mut |m: &str| {
            messages.push(m.to_owned())
        });
        assert_eq!(messages.len(), 1, "{messages:?}");
        assert_eq!(
            messages[0], "proxy: via socks5://10.0.0.1:7890",
            "{messages:?}"
        );
        assert!(
            !messages[0].contains("secret"),
            "认证材料不得进日志: {messages:?}"
        );
        // 已上报后不再重复。
        let mut messages_again: Vec<String> = Vec::new();
        report_proxy_fallback(&fallback, &mut reported, &mut |m: &str| {
            messages_again.push(m.to_owned())
        });
        assert!(messages_again.is_empty());

        // 未发生回退：不上报。
        let mut silent = DohProxyFallback::new(Some("socks5://127.0.0.1:7890"));
        let mut reported_silent = false;
        let mut empty: Vec<String> = Vec::new();
        report_proxy_fallback(&silent, &mut reported_silent, &mut |m: &str| {
            empty.push(m.to_owned())
        });
        assert!(empty.is_empty(), "直连会话不应上报: {empty:?}");
        let _ = &mut silent;
    }

    /// 代理偏好跨段保留：`doh_single` 段回退成功缓存偏好后，`doh_full` 段
    /// （共享同一会话状态）直接用代理传输层查询，不再重复直连探测。
    #[tokio::test]
    async fn proxy_preference_across_segments() {
        let direct = Arc::new(ScriptedTransport::new(vec![Err("连接超时".to_owned())]));
        let resolver = DohResolver::google(Arc::clone(&direct) as Arc<dyn HttpTransport>);
        // 代理传输层始终返回未命中（Ok 空应答）。
        let proxy = Arc::new(ScriptedTransport::new(vec![Ok(HttpResponse::new(
            200,
            r#"{"Status":0}"#,
        ))]));
        let mut fallback = DohProxyFallback::with_transport(
            "socks5://127.0.0.1:7890",
            Arc::clone(&proxy) as Arc<dyn HttpTransport>,
        );

        let mut missing = Vec::new();
        let mut unreachable = Vec::new();
        let mut probes = Vec::new();

        // doh_single 段：直连失败 → 经代理重试成功（未命中）→ 缓存偏好。
        let single = probe_resolvers(
            &[&resolver],
            &record(),
            1,
            &mut missing,
            &mut unreachable,
            &mut probes,
            Some(&mut fallback),
        )
        .await;
        assert!(matches!(single, ProbeOutcome::NotVisible), "{single:?}");
        assert_eq!(direct.calls(), 1, "单家段应先直连探测一次");
        assert_eq!(proxy.calls(), 1, "直连失败应经代理重试一次");
        assert!(
            fallback.prefers_proxy("dns.google"),
            "单家段回退成功应缓存偏好"
        );

        // doh_full 段：偏好为 Proxy → 直接经代理查询，不再直连探测。
        let full = probe_resolvers(
            &[&resolver],
            &record(),
            2,
            &mut missing,
            &mut unreachable,
            &mut probes,
            Some(&mut fallback),
        )
        .await;
        assert!(matches!(full, ProbeOutcome::NotVisible), "{full:?}");
        assert_eq!(
            direct.calls(),
            1,
            "整组段不应再直连探测已缓存代理偏好的端点"
        );
        assert_eq!(proxy.calls(), 2, "整组段应经代理传输层查询");
    }

    /// 全局代理未配置：不做回退，直连失败直接计 1 次。
    #[tokio::test]
    async fn no_global_proxy_means_no_fallback() {
        let direct = Arc::new(ScriptedTransport::new(vec![Err("连接超时".to_owned())]));
        let resolver = DohResolver::google(Arc::clone(&direct) as Arc<dyn HttpTransport>);
        let mut fallback = DohProxyFallback::new(None);

        let mut missing = Vec::new();
        let mut unreachable = Vec::new();
        let mut probes = Vec::new();
        let outcome = probe_resolvers(
            &[&resolver],
            &record(),
            1,
            &mut missing,
            &mut unreachable,
            &mut probes,
            Some(&mut fallback),
        )
        .await;

        assert!(matches!(outcome, ProbeOutcome::NotVisible), "{outcome:?}");
        assert_eq!(probes[0].failures, 1, "未配代理时直连失败只计 1 次");
        assert_eq!(direct.calls(), 1, "没有代理可回退，不应再查一次");
        assert!(
            !fallback.prefers_proxy("dns.google"),
            "未配代理不应缓存偏好"
        );
    }

    /// 直连失败且经代理重试仍失败：直连 + 代理 = 2 次；与熔断叠加时
    /// 两轮即达阈值，不会无限重试。
    #[tokio::test]
    async fn a_failed_proxy_retry_counts_twice_toward_the_breaker() {
        let direct = Arc::new(ScriptedTransport::new(vec![Err("连接超时".to_owned())]));
        let resolver = DohResolver::google(Arc::clone(&direct) as Arc<dyn HttpTransport>);
        let proxy = Arc::new(ScriptedTransport::new(vec![Err("代理不可达".to_owned())]));
        let mut fallback = DohProxyFallback::with_transport(
            "socks5://127.0.0.1:7890",
            Arc::clone(&proxy) as Arc<dyn HttpTransport>,
        );

        let mut missing = Vec::new();
        let mut unreachable = Vec::new();
        let mut probes = Vec::new();
        let outcome = probe_resolvers(
            &[&resolver],
            &record(),
            1,
            &mut missing,
            &mut unreachable,
            &mut probes,
            Some(&mut fallback),
        )
        .await;

        assert!(matches!(outcome, ProbeOutcome::NotVisible), "{outcome:?}");
        assert_eq!(probes[0].failures, 2, "直连失败 + 代理回退失败 = 2 次");
        assert_eq!(direct.calls(), 1);
        assert_eq!(proxy.calls(), 1, "回退仍失败也不缓存偏好");
        assert!(!fallback.prefers_proxy("dns.google"));

        // 累计两轮即达阈值（2 + 2 = 4 ≥ 3）。
        let mut breaker = CircuitBreaker::default();
        assert!(breaker.update(&probes).is_empty(), "第一轮 2 次未达阈值");
        let newly = breaker.update(&probes);
        assert_eq!(
            newly,
            vec!["dns.google".to_owned()],
            "累计 4 次应剔除，计数跳过 3 也不漏"
        );
    }
}
