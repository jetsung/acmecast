//! 7.2 内置 DNS 提供商的请求构造与响应处理。
//!
//! 用注入的传输层替身验证：请求发去哪、带了什么参数、响应怎么解析——
//! 全程不碰网络。真机验证留给各厂商的测试环境（或 11.x 的端到端）。

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use acmecast_dns::{
    AliyunProvider, CloudflareProvider, DnsProvider, DnsProviderRegistry, Error, HttpRequest,
    HttpResponse, HttpTransport, Result, TxtRecord,
};
use async_trait::async_trait;
use serde_json::{Value, json};

/// 按脚本依次应答的传输层替身。
#[derive(Debug, Default)]
struct ScriptedTransport {
    responses: Mutex<VecDeque<HttpResponse>>,
    requests: Mutex<Vec<HttpRequest>>,
}

impl ScriptedTransport {
    fn new(responses: Vec<HttpResponse>) -> Arc<Self> {
        Arc::new(Self {
            responses: Mutex::new(responses.into()),
            requests: Mutex::new(Vec::new()),
        })
    }

    fn requests(&self) -> Vec<HttpRequest> {
        self.requests.lock().expect("锁不应中毒").clone()
    }
}

#[async_trait]
impl HttpTransport for ScriptedTransport {
    async fn send(&self, request: HttpRequest) -> Result<HttpResponse> {
        self.requests.lock().expect("锁不应中毒").push(request);

        self.responses
            .lock()
            .expect("锁不应中毒")
            .pop_front()
            .ok_or_else(|| Error::provider("脚本里没有更多响应了".to_owned()))
    }
}

/// 一份 Cloudflare 凭据。
fn cloudflare_token() -> Value {
    json!({ "api_token": "cf-token-abc" })
}

/// 一份阿里云凭据。
fn aliyun_keys() -> Value {
    json!({ "access_key_id": "AKID", "access_key_secret": "SECRET" })
}

fn record() -> TxtRecord {
    TxtRecord::new(
        "example.com",
        "_acme-challenge.example.com",
        "digest-value-xyz",
        60,
    )
}

/// Cloudflare 的「查 zone」响应。
fn cloudflare_zone() -> HttpResponse {
    HttpResponse::new(200, r#"{"success":true,"result":[{"id":"zone-1"}]}"#)
}

// ---- Cloudflare ----

#[tokio::test]
async fn cloudflare_creates_a_txt_record() {
    let transport = ScriptedTransport::new(vec![
        cloudflare_zone(),
        HttpResponse::new(200, r#"{"success":true,"result":{"id":"rec-1"}}"#),
    ]);
    let provider =
        CloudflareProvider::with_transport(Arc::clone(&transport) as Arc<dyn HttpTransport>);

    provider
        .create_txt(&cloudflare_token(), &record())
        .await
        .expect("应能创建记录");

    let requests = transport.requests();
    assert_eq!(requests.len(), 2, "先查 zone、再建记录");

    // 第一步：按域名换 zone_id，带上 Bearer 认证。
    assert_eq!(requests[0].method, "GET");
    assert!(
        requests[0].url.contains("/zones?name=example.com"),
        "{}",
        requests[0].url
    );
    assert_eq!(requests[0].headers["Authorization"], "Bearer cf-token-abc");

    // 第二步：用换来的 zone_id 建 TXT。
    let create = &requests[1];
    assert_eq!(create.method, "POST");
    assert!(
        create.url.ends_with("/zones/zone-1/dns_records"),
        "{}",
        create.url
    );
    let body = create.body.as_ref().expect("应带请求体");
    assert_eq!(body["type"], "TXT");
    assert_eq!(body["name"], "_acme-challenge.example.com");
    assert_eq!(body["content"], "digest-value-xyz");
    assert_eq!(body["ttl"], 60);
}

#[tokio::test]
async fn cloudflare_deletes_by_looking_the_record_up_first() {
    let transport = ScriptedTransport::new(vec![
        cloudflare_zone(),
        HttpResponse::new(
            200,
            r#"{"success":true,"result":[{"id":"rec-1","content":"digest-value-xyz"}]}"#,
        ),
        HttpResponse::new(200, r#"{"success":true,"result":{"id":"rec-1"}}"#),
    ]);
    let provider =
        CloudflareProvider::with_transport(Arc::clone(&transport) as Arc<dyn HttpTransport>);

    provider
        .delete_txt(&cloudflare_token(), &record())
        .await
        .expect("应能删除记录");

    let requests = transport.requests();
    assert_eq!(requests.len(), 3, "查 zone、查记录、删记录");

    // 按名字查出候选，再从中挑出值相同的那条来删——
    // 同名但不是我们写的记录不该被替人清掉。
    let find = &requests[1];
    assert!(
        find.url.contains("type=TXT") && find.url.contains("name=_acme-challenge.example.com"),
        "应按名称定位: {}",
        find.url
    );

    assert_eq!(requests[2].method, "DELETE");
    assert!(
        requests[2].url.ends_with("/dns_records/rec-1"),
        "{}",
        requests[2].url
    );
}

#[tokio::test]
async fn cloudflare_deletion_is_idempotent() {
    // 记录本就不在（比如上一次清理已经删过）时不该报错——
    // 清理路径上为这个报错只会盖住真正的失败原因。
    let transport = ScriptedTransport::new(vec![
        cloudflare_zone(),
        HttpResponse::new(200, r#"{"success":true,"result":[]}"#),
    ]);
    let provider =
        CloudflareProvider::with_transport(Arc::clone(&transport) as Arc<dyn HttpTransport>);

    provider
        .delete_txt(&cloudflare_token(), &record())
        .await
        .expect("找不到记录也算成功");
    assert_eq!(transport.requests().len(), 2, "不该发出删除请求");
}

#[tokio::test]
async fn cloudflare_surfaces_the_vendor_message() {
    let transport = ScriptedTransport::new(vec![HttpResponse::new(
        403,
        r#"{"success":false,"errors":[{"message":"token 缺少 DNS:Edit 权限"}]}"#,
    )]);
    let provider =
        CloudflareProvider::with_transport(Arc::clone(&transport) as Arc<dyn HttpTransport>);

    let err = provider
        .create_txt(&cloudflare_token(), &record())
        .await
        .expect_err("失败响应应报错");

    // 厂商的说明比一个光秃秃的 403 有用得多。
    assert!(err.to_string().contains("DNS:Edit"), "{err}");
    assert!(err.to_string().contains("403"), "{err}");
}

// ---- 阿里云 ----

#[tokio::test]
async fn aliyun_creates_a_txt_record() {
    let transport = ScriptedTransport::new(vec![HttpResponse::new(
        200,
        r#"{"RecordId":"rec-9","RequestId":"req-1"}"#,
    )]);
    let provider = AliyunProvider::with_transport(Arc::clone(&transport) as Arc<dyn HttpTransport>);

    provider
        .create_txt(&aliyun_keys(), &record())
        .await
        .expect("应能创建记录");

    let requests = transport.requests();
    assert_eq!(requests.len(), 1);
    let url = &requests[0].url;

    assert!(url.contains("Action=AddDomainRecord"), "{url}");
    // 记录名被拆成「域名 + 主机记录」两半。
    assert!(url.contains("DomainName=example.com"), "{url}");
    assert!(url.contains("RR=_acme-challenge"), "{url}");
    assert!(url.contains("Type=TXT"), "{url}");
    assert!(url.contains("Value=digest-value-xyz"), "{url}");
    // 阿里云只接受 600–86400 的 TTL，60s 会被钳到下限（见下一个用例）。
    assert!(url.contains("TTL=600"), "{url}");
    // 签名所需的公共参数一个都不能少。
    for expected in [
        "Signature=",
        "SignatureMethod=HMAC-SHA1",
        "AccessKeyId=AKID",
        "Timestamp=",
        "SignatureNonce=",
    ] {
        assert!(url.contains(expected), "请求里应含 {expected}: {url}");
    }
}

#[tokio::test]
async fn aliyun_clamps_ttl_into_the_vendor_range() {
    // run 24 的失败原因：上层惯用的 60/120s TTL 被原样发给
    // AddDomainRecord，阿里云回 400 QuotaExceeded.TTL（合法区间 600–86400）。
    for (input, expected) in [(60, "TTL=600"), (3600, "TTL=3600"), (100_000, "TTL=86400")] {
        let transport =
            ScriptedTransport::new(vec![HttpResponse::new(200, r#"{"RecordId":"rec-9"}"#)]);
        let provider =
            AliyunProvider::with_transport(Arc::clone(&transport) as Arc<dyn HttpTransport>);
        let record = TxtRecord::new("example.com", "_acme-challenge.example.com", "v", input);

        provider
            .create_txt(&aliyun_keys(), &record)
            .await
            .expect("应能创建记录");

        let url = &transport.requests()[0].url;
        assert!(
            url.contains(expected),
            "TTL={input} 应发出 {expected}: {url}"
        );
    }
}

#[tokio::test]
async fn aliyun_deletes_by_looking_the_record_up_first() {
    let transport = ScriptedTransport::new(vec![
        // 查询结果里给出候选记录；只有值与我们这条相同的才会被删。
        HttpResponse::new(
            200,
            r#"{"DomainRecords":{"Record":[{"RecordId":"rec-9","Value":"digest-value-xyz"}]}}"#,
        ),
        HttpResponse::new(200, r#"{"RecordId":"rec-9"}"#),
    ]);
    let provider = AliyunProvider::with_transport(Arc::clone(&transport) as Arc<dyn HttpTransport>);

    provider
        .delete_txt(&aliyun_keys(), &record())
        .await
        .expect("应能删除记录");

    let requests = transport.requests();
    assert_eq!(requests.len(), 2, "先查后删");
    assert!(
        requests[0].url.contains("Action=DescribeDomainRecords"),
        "{}",
        requests[0].url
    );
    assert!(
        requests[1].url.contains("Action=DeleteDomainRecord"),
        "{}",
        requests[1].url
    );
    assert!(
        requests[1].url.contains("RecordId=rec-9"),
        "{}",
        requests[1].url
    );
}

#[tokio::test]
async fn aliyun_reports_a_missing_host_from_the_domain() {
    // 记录名与 zone 对不上是配置错误，应当明确说出来——而不是发一个
    // 厂商会拒绝、但原因不明的请求出去。
    let transport = ScriptedTransport::new(vec![]);
    let provider = AliyunProvider::with_transport(Arc::clone(&transport) as Arc<dyn HttpTransport>);

    let mismatched = TxtRecord::new("example.com", "other.net", "v", 60);
    let err = provider
        .create_txt(&aliyun_keys(), &mismatched)
        .await
        .expect_err("记录名不属于该域名应报错");

    assert!(err.to_string().contains("不属于域名"), "{err}");
    assert!(transport.requests().is_empty(), "不该发出请求");
}

// ---- 注册表 ----

#[test]
fn both_providers_are_registered_and_looked_up_by_type_id() {
    // spec 场景：所有内置提供商可按类型标识查得。
    let mut registry = DnsProviderRegistry::new();
    registry
        .register(CloudflareProvider::new())
        .expect("应能注册");
    registry.register(AliyunProvider::new()).expect("应能注册");

    assert_eq!(
        registry.type_ids(),
        vec!["aliyun".to_owned(), "cloudflare".to_owned()]
    );
    assert_eq!(
        registry.require("cloudflare").unwrap().display_name(),
        "Cloudflare"
    );
    assert_eq!(
        registry.require("aliyun").unwrap().display_name(),
        "阿里云 DNS"
    );

    // 凭据定义随提供商一起导出，前端据此渲染表单。
    for type_id in ["cloudflare", "aliyun"] {
        let rendered =
            serde_json::to_string(&registry.require(type_id).unwrap().credential_fields()).unwrap();
        assert!(rendered.contains("required"), "{type_id}: {rendered}");
    }
    assert!(
        serde_json::to_string(&registry.require("aliyun").unwrap().credential_fields())
            .unwrap()
            .contains("access_key_secret")
    );
}

#[test]
fn an_unregistered_provider_is_rejected() {
    let registry = DnsProviderRegistry::new();
    let err = registry.require("route53").expect_err("未注册应被拒绝");
    assert!(err.to_string().contains("route53"), "{err}");
}
