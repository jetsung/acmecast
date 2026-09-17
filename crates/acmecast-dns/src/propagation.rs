//! 记录的传播等待。
//!
//! 写完 TXT 记录并不等于它能被查到：厂商的 API 立刻承认，但公共解析器还要
//! 等自己的缓存与权威链走完。CA 校验时用的是**公共解析器**的视角，所以
//! 我们必须按同样的视角确认一遍。
//!
//! 分两段：先向提供商侧确认记录确实收下了（它都不认，下游无从谈起），
//! 再向若干公共解析器查询——**全部**看到期望值才算就绪。

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

/// 一个公共递归解析器。
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

/// 默认的解析器集合：Google 与 Cloudflare 两家。
///
/// 用两家而不是一家：单个解析器可能因为自身缓存或线路问题给出滞后的答案，
/// 而 CA 用的是哪个解析器我们无从知道——多问几家能显著降低
/// 「本地已经生效、CA 那边还没有」的概率。
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
    vec![
        Box::new(DohResolver::google(Arc::clone(&http))),
        Box::new(DohResolver::cloudflare(http)),
    ]
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

/// 等这条记录在**全部**解析器上都可见。
///
/// 未可见时按 `policy.interval` 重试，直到 `policy.timeout`——
/// 超时返回的错误会指明卡在哪一段：是提供商侧就没认，还是解析器还没看到。
/// 两者要查的方向完全不同，混成一句「超时了」等于没说。
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
    // 跨轮保留，供超时错误指明「是谁没跟上」。
    let mut missing: Vec<String> = Vec::new();

    loop {
        round += 1;

        // 第一段：提供商侧确认。它都不认，下游无从谈起。
        if !published {
            let found = provider.find_txt(credentials, record).await?;
            published = found.iter().any(|value| value == &record.value);
        }

        if published {
            // 第二段：逐个解析器查，收集**还没看到**的那些。
            missing.clear();
            for resolver in resolvers {
                let values = match resolver.lookup_txt(&record.name).await {
                    Ok(values) => values,
                    Err(err) => {
                        // 查询**失败**与「查到了但值不对」是两回事：后者是传播
                        // 还没到，前者多半是这个解析器本身出了问题。两者都按
                        // 「未传播」处理（继续等），但失败的值得留一条 warn。
                        warn!(
                            resolver = resolver.name(),
                            error = %err,
                            "解析器查询失败，按未传播处理"
                        );
                        Vec::new()
                    }
                };

                if !values.iter().any(|value| value == &record.value) {
                    missing.push(resolver.name().to_owned());
                }
            }

            if missing.is_empty() {
                info!(
                    name = %record.name,
                    round,
                    resolvers = resolvers.len(),
                    "记录已在全部解析器上可见"
                );
                return Ok(());
            }

            debug!(name = %record.name, round, missing = ?missing, "尚未在全部解析器上可见");
        }

        if Instant::now() >= deadline {
            return Err(timeout_error(published, record, &missing, policy));
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
    policy: &PropagationPolicy,
) -> Error {
    let waited = humanize(policy.timeout);

    if !published {
        return Error::provider(format!(
            "等待 {waited} 后，提供商侧仍未确认记录 `{}` 已写入",
            record.name
        ));
    }

    Error::provider(format!(
        "记录 `{}` 已写入提供商侧，但等待 {waited} 后仍未在 {} 上可见",
        record.name,
        missing.join("、")
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
        assert!(err.to_string().contains("未在"), "{err}");

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
    async fn every_resolver_must_see_the_value() {
        // spec 场景：多个解析器不一致时等全部返回期望值。
        let provider = Publishes::always("value-1");
        let ready = ScriptedResolver::new("先就绪的", vec![vec!["value-1".to_owned()]]);
        let lagging =
            ScriptedResolver::new("慢的", vec![vec![], vec![], vec!["value-1".to_owned()]]);

        wait_until_visible(
            &provider,
            &serde_json::json!({}),
            &record(),
            &[&ready, &lagging],
            &fast_policy(),
        )
        .await
        .expect("全部可见才返回");
        assert_eq!(lagging.calls(), 3);
    }

    #[tokio::test]
    async fn verification_waits_until_every_resolver_agrees() {
        // 快的那家第一轮就一致了，但它后面还要被再问一次——说明等待以
        // 「全部一致」为准，而不是先到先得就放行。
        let provider = Publishes::always("value-1");
        let ready = ScriptedResolver::new("快的", vec![vec!["value-1".to_owned()]]);
        let lagging = ScriptedResolver::new("慢的", vec![vec![], vec!["value-1".to_owned()]]);

        wait_until_visible(
            &provider,
            &serde_json::json!({}),
            &record(),
            &[&ready, &lagging],
            &fast_policy(),
        )
        .await
        .expect("全部一致后才返回");

        assert_eq!(ready.calls(), 2, "早就一致的那家也要陪跑到最后");
        assert_eq!(lagging.calls(), 2, "等到它一致为止");
    }

    #[test]
    fn the_default_set_covers_two_public_resolvers() {
        use crate::http::HttpResponse;

        #[derive(Debug)]
        struct Unused;

        #[async_trait]
        impl HttpTransport for Unused {
            async fn send(&self, _request: HttpRequest) -> Result<HttpResponse> {
                unreachable!("本用例只构造解析器，不发请求")
            }
        }

        let resolvers = default_resolvers(Arc::new(Unused));
        let names: Vec<&str> = resolvers.iter().map(|resolver| resolver.name()).collect();

        assert_eq!(names.len(), 2, "至少两家才谈得上「多解析器」");
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
        let ready = ScriptedResolver::new("快的", vec![vec!["value-1".to_owned()]]);
        let stuck = ScriptedResolver::new("卡住的", vec![vec![]]);

        let err = wait_until_visible(
            &provider,
            &serde_json::json!({}),
            &record(),
            &[&ready, &stuck],
            &fast_policy(),
        )
        .await
        .expect_err("有解析器看不到应超时");

        let text = err.to_string();
        assert!(text.contains("已写入提供商侧"), "{text}");
        assert!(text.contains("卡住的"), "应指出是哪个解析器没跟上: {text}");
        // 已经就绪的那个不必再提。
        assert!(!text.contains("快的"), "{text}");
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
