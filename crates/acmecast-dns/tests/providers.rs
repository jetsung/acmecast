//! 7.2 内置 DNS 提供商的请求构造与响应处理。
//!
//! 用注入的传输层替身验证：请求发去哪、带了什么参数、响应怎么解析——
//! 全程不碰网络。真机验证留给各厂商的测试环境（或 11.x 的端到端）。

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use acmecast_dns::{
    AliyunProvider, CloudflareProvider, DnsProvider, DnsProviderRegistry, Error, HttpRequest,
    HttpResponse, HttpTransport, Result, TencentEoProvider, TencentProvider, TxtRecord,
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

// ---- 腾讯云（云解析与 EdgeOne） ----

/// 一份腾讯云凭据；`site` 传 `cn` 或 `intl`。
fn tencent_keys(site: &str) -> Value {
    json!({ "secret_id": "AKID", "secret_key": "tencent-secret", "account_site": site })
}

/// DNSPod 的「查记录」响应（同名 TXT：一条同值、一条不同值）。
fn dnspod_records() -> HttpResponse {
    HttpResponse::new(
        200,
        r#"{"Response":{"RecordList":[{"RecordId":7,"Value":"digest-value-xyz"},{"RecordId":8,"Value":"other-value"}],"RequestId":"req-1"}}"#,
    )
}

/// EdgeOne 的「查 zone」响应。
fn teo_zone() -> HttpResponse {
    HttpResponse::new(
        200,
        r#"{"Response":{"Zones":[{"ZoneId":"zone-9","ZoneName":"example.com"}],"RequestId":"req-1"}}"#,
    )
}

/// EdgeOne 的「查记录」响应（同名 TXT：一条同值、一条不同值）。
///
/// 国际站的 RecordId 是 `record-xxxx` 字符串（国内站为数字，
/// 数字形态的兼容由 [`teo_deletes_numeric_record_ids`] 覆盖）。
fn teo_records() -> HttpResponse {
    HttpResponse::new(
        200,
        r#"{"Response":{"DnsRecords":[{"RecordId":"record-5","Content":"digest-value-xyz"},{"RecordId":"record-6","Content":"other-content"}],"RequestId":"req-1"}}"#,
    )
}

#[tokio::test]
async fn tencent_creates_a_txt_record() {
    let transport = ScriptedTransport::new(vec![HttpResponse::new(
        200,
        r#"{"Response":{"RecordId":162,"RequestId":"req-1"}}"#,
    )]);
    let provider =
        TencentProvider::with_transport(Arc::clone(&transport) as Arc<dyn HttpTransport>);

    provider
        .create_txt(&tencent_keys("cn"), &record())
        .await
        .expect("应能创建记录");

    let requests = transport.requests();
    assert_eq!(requests.len(), 1);
    let request = &requests[0];

    // 国内站端点，一次 POST 带完整签名头。
    assert_eq!(request.method, "POST");
    assert_eq!(request.url, "https://dnspod.tencentcloudapi.com/");
    assert_eq!(request.headers["X-TC-Action"], "CreateRecord");
    assert_eq!(request.headers["X-TC-Version"], "2021-03-23");
    assert!(request.headers["X-TC-Timestamp"].parse::<i64>().is_ok());
    assert_eq!(
        request.headers["Content-Type"],
        "application/json; charset=utf-8"
    );
    let authorization = &request.headers["Authorization"];
    assert!(authorization.starts_with("TC3-HMAC-SHA256 Credential=AKID/"), "{authorization}");
    assert!(authorization.contains("/dnspod/tc3_request"), "{authorization}");
    assert!(authorization.contains("SignedHeaders=content-type;host;x-tc-action"), "{authorization}");

    // 记录名被拆成「域名 + 主机记录」两半，线路用默认。
    let body = request.body.as_ref().expect("应带请求体");
    assert_eq!(body["Domain"], "example.com");
    assert_eq!(body["SubDomain"], "_acme-challenge");
    assert_eq!(body["RecordType"], "TXT");
    assert_eq!(body["RecordLine"], "默认");
    assert_eq!(body["Value"], "digest-value-xyz");
    // 免费版套餐不支持 <600 的 TTL，上层惯用的 60/300s 会被钳到下限。
    assert_eq!(body["TTL"], 600);
}

#[tokio::test]
async fn tencent_uses_the_intl_endpoint_for_intl_credentials() {
    let transport = ScriptedTransport::new(vec![HttpResponse::new(
        200,
        r#"{"Response":{"RecordId":1}}"#,
    )]);
    let provider =
        TencentProvider::with_transport(Arc::clone(&transport) as Arc<dyn HttpTransport>);

    provider
        .create_txt(&tencent_keys("intl"), &record())
        .await
        .expect("应能创建记录");

    assert_eq!(
        transport.requests()[0].url,
        "https://dnspod.intl.tencentcloudapi.com/"
    );
}

#[tokio::test]
async fn tencent_clamps_ttl_into_the_vendor_range() {
    // 免费版套餐对 TTL 有下限（真机报 LimitExceeded.RecordTtlLimit，
    // 见 histories/28）：低于 600 的统一钳到 600，超上限的钳到 86400。
    for (input, expected) in [(60, 600), (300, 600), (3600, 3600), (100_000, 86_400)] {
        let transport = ScriptedTransport::new(vec![HttpResponse::new(
            200,
            r#"{"Response":{"RecordId":162}}"#,
        )]);
        let provider =
            TencentProvider::with_transport(Arc::clone(&transport) as Arc<dyn HttpTransport>);
        let record = TxtRecord::new("example.com", "_acme-challenge.example.com", "v", input);

        provider
            .create_txt(&tencent_keys("cn"), &record)
            .await
            .expect("应能创建记录");

        let requests = transport.requests();
        let body = requests[0].body.as_ref().expect("应带请求体");
        assert_eq!(body["TTL"], expected, "输入 {input}s 应钳到 {expected}s");
    }
}

#[tokio::test]
async fn tencent_deletes_by_looking_the_record_up_first() {
    let transport = ScriptedTransport::new(vec![
        dnspod_records(),
        HttpResponse::new(200, r#"{"Response":{"RequestId":"req-1"}}"#),
    ]);
    let provider =
        TencentProvider::with_transport(Arc::clone(&transport) as Arc<dyn HttpTransport>);

    provider
        .delete_txt(&tencent_keys("cn"), &record())
        .await
        .expect("应能删除记录");

    // 按值挑出候选再删——同名但不是我们写的记录不该被替人清掉。
    let requests = transport.requests();
    assert_eq!(requests.len(), 2, "查记录、删记录");
    assert_eq!(requests[0].headers["X-TC-Action"], "DescribeRecordFilterList");
    assert_eq!(requests[1].headers["X-TC-Action"], "DeleteRecord");
    let body = requests[1].body.as_ref().expect("应带请求体");
    assert_eq!(body["Domain"], "example.com");
    assert_eq!(body["RecordId"], 7, "只应删同值的那条");
}

#[tokio::test]
async fn tencent_deletion_is_idempotent() {
    let transport = ScriptedTransport::new(vec![HttpResponse::new(
        200,
        r#"{"Response":{"RecordList":[],"RequestId":"req-1"}}"#,
    )]);
    let provider =
        TencentProvider::with_transport(Arc::clone(&transport) as Arc<dyn HttpTransport>);

    provider
        .delete_txt(&tencent_keys("cn"), &record())
        .await
        .expect("找不到记录也算成功");
    assert_eq!(transport.requests().len(), 1, "不该发出删除请求");
}

#[tokio::test]
async fn tencent_surfaces_business_errors_over_http_200() {
    // 腾讯云 API 3.0 的业务错误也回 HTTP 200，错误在 Response.Error 里。
    let transport = ScriptedTransport::new(vec![HttpResponse::new(
        200,
        r#"{"Response":{"Error":{"Code":"AuthFailure.SignatureFailure","Message":"The signature does not match"},"RequestId":"req-1"}}"#,
    )]);
    let provider =
        TencentProvider::with_transport(Arc::clone(&transport) as Arc<dyn HttpTransport>);

    let err = provider
        .create_txt(&tencent_keys("cn"), &record())
        .await
        .expect_err("业务错误应报错");

    let text = err.to_string();
    assert!(text.contains("AuthFailure.SignatureFailure"), "{text}");
    // 错误来自服务端，密钥明文不该出现在任何路径上。
    assert!(!text.contains("tencent-secret"), "{text}");
}

#[tokio::test]
async fn teo_creates_a_txt_record_via_the_zone_lookup() {
    let transport = ScriptedTransport::new(vec![
        teo_zone(),
        HttpResponse::new(200, r#"{"Response":{"RecordId":162,"RequestId":"req-1"}}"#),
    ]);
    let provider =
        TencentEoProvider::with_transport(Arc::clone(&transport) as Arc<dyn HttpTransport>);

    provider
        .create_txt(&tencent_keys("cn"), &record())
        .await
        .expect("应能创建记录");

    let requests = transport.requests();
    assert_eq!(requests.len(), 2, "先查 zone、再建记录");

    assert_eq!(requests[0].url, "https://teo.tencentcloudapi.com/");
    assert_eq!(requests[0].headers["X-TC-Action"], "DescribeZones");
    let zone_query = requests[0].body.as_ref().expect("应带请求体");
    assert_eq!(zone_query["Filters"][0]["Name"], "zone-name");
    assert_eq!(zone_query["Filters"][0]["Values"][0], "example.com");

    assert_eq!(requests[1].headers["X-TC-Action"], "CreateDnsRecord");
    let body = requests[1].body.as_ref().expect("应带请求体");
    assert_eq!(body["ZoneId"], "zone-9");
    // EdgeOne 直接收完整记录名，不再拆主机记录。
    assert_eq!(body["Name"], "_acme-challenge.example.com");
    assert_eq!(body["Type"], "TXT");
    assert_eq!(body["Content"], "digest-value-xyz");
}

#[tokio::test]
async fn teo_reports_a_zone_outside_edgeone() {
    // 域名只托管在 DNSPod 时 DescribeZones 查不到 zone，错误要指到这一步。
    let transport = ScriptedTransport::new(vec![HttpResponse::new(
        200,
        r#"{"Response":{"Zones":[],"RequestId":"req-1"}}"#,
    )]);
    let provider =
        TencentEoProvider::with_transport(Arc::clone(&transport) as Arc<dyn HttpTransport>);

    let err = provider
        .create_txt(&tencent_keys("cn"), &record())
        .await
        .expect_err("无 zone 应报错");

    let text = err.to_string();
    assert!(text.contains("example.com"), "{text}");
    assert!(text.contains("EdgeOne"), "{text}");
}

#[tokio::test]
async fn teo_deletes_by_looking_the_record_up_first() {
    let transport = ScriptedTransport::new(vec![
        teo_zone(),
        teo_records(),
        HttpResponse::new(200, r#"{"Response":{"JobId":1,"RequestId":"req-1"}}"#),
    ]);
    let provider =
        TencentEoProvider::with_transport(Arc::clone(&transport) as Arc<dyn HttpTransport>);

    provider
        .delete_txt(&tencent_keys("cn"), &record())
        .await
        .expect("应能删除记录");

    let requests = transport.requests();
    assert_eq!(requests.len(), 3, "查 zone、查记录、删记录");
    assert_eq!(requests[1].headers["X-TC-Action"], "DescribeDnsRecords");
    let query = requests[1].body.as_ref().expect("应带请求体");
    assert_eq!(query["ZoneId"], "zone-9");
    assert_eq!(query["Filters"][0]["Name"], "name");
    assert_eq!(query["Filters"][0]["Values"][0], "_acme-challenge.example.com");
    assert_eq!(query["Filters"][1]["Name"], "type");
    assert_eq!(query["Filters"][1]["Values"][0], "TXT");
    assert_eq!(query["Limit"], 1000, "默认每页 20，应顶格避免分页截断");
    assert_eq!(requests[2].headers["X-TC-Action"], "DeleteDnsRecords");
    let body = requests[2].body.as_ref().expect("应带请求体");
    assert_eq!(body["ZoneId"], "zone-9");
    assert_eq!(body["RecordIds"], json!(["record-5"]), "只应删同值的那条");
}

#[tokio::test]
async fn teo_deletes_numeric_record_ids() {
    // 国内站的 RecordId 是数字：删除时必须原样传回数字，不得假定字符串。
    let transport = ScriptedTransport::new(vec![
        teo_zone(),
        HttpResponse::new(
            200,
            r#"{"Response":{"DnsRecords":[{"RecordId":5,"Content":"digest-value-xyz"}],"RequestId":"req-1"}}"#,
        ),
        HttpResponse::new(200, r#"{"Response":{"JobId":1,"RequestId":"req-1"}}"#),
    ]);
    let provider =
        TencentEoProvider::with_transport(Arc::clone(&transport) as Arc<dyn HttpTransport>);

    provider
        .delete_txt(&tencent_keys("cn"), &record())
        .await
        .expect("应能删除记录");

    let requests = transport.requests();
    let body = requests[2].body.as_ref().expect("应带请求体");
    assert_eq!(body["RecordIds"], json!([5]));
}

#[tokio::test]
async fn teo_finds_txt_records_via_the_real_query_action() {
    let transport = ScriptedTransport::new(vec![teo_zone(), teo_records()]);
    let provider =
        TencentEoProvider::with_transport(Arc::clone(&transport) as Arc<dyn HttpTransport>);

    let values = provider
        .find_txt(&tencent_keys("cn"), &record())
        .await
        .expect("应能查询记录");

    assert_eq!(values, vec!["digest-value-xyz", "other-content"]);

    let requests = transport.requests();
    assert_eq!(requests.len(), 2, "先查 zone、再查记录");
    assert_eq!(requests[1].headers["X-TC-Action"], "DescribeDnsRecords");
    let query = requests[1].body.as_ref().expect("应带请求体");
    assert_eq!(query["ZoneId"], "zone-9");
    assert_eq!(query["Filters"][0]["Values"][0], "_acme-challenge.example.com");
    assert_eq!(query["Filters"][1]["Values"][0], "TXT");
    assert_eq!(query["Limit"], 1000);
}

// ---- 凭据探测（凭据页「测试」按钮） ----

#[tokio::test]
async fn cloudflare_verify_credentials_accepts_an_active_token() {
    let transport = ScriptedTransport::new(vec![HttpResponse::new(
        200,
        r#"{"success":true,"result":{"id":"tok","status":"active"}}"#,
    )]);
    let provider =
        CloudflareProvider::with_transport(Arc::clone(&transport) as Arc<dyn HttpTransport>);

    provider
        .verify_credentials(&cloudflare_token())
        .await
        .expect("active 令牌应可用");

    let request = &transport.requests()[0];
    assert_eq!(request.url, "https://api.cloudflare.com/client/v4/user/tokens/verify");
    assert_eq!(request.headers["Authorization"], "Bearer cf-token-abc");
}

#[tokio::test]
async fn cloudflare_verify_credentials_reports_what_the_server_said() {
    let transport = ScriptedTransport::new(vec![HttpResponse::new(
        400,
        r#"{"success":false,"errors":[{"message":"Invalid request headers"}]}"#,
    )]);
    let provider =
        CloudflareProvider::with_transport(Arc::clone(&transport) as Arc<dyn HttpTransport>);

    let err = provider
        .verify_credentials(&cloudflare_token())
        .await
        .expect_err("失败应报错");
    let text = err.to_string();
    assert!(text.contains("Invalid request headers"), "{text}");
    assert!(!text.contains("cf-token-abc"), "不得回显令牌: {text}");
}

#[tokio::test]
async fn aliyun_verify_credentials_probes_sts_get_caller_identity() {
    let transport = ScriptedTransport::new(vec![HttpResponse::new(
        200,
        r#"{"RequestId":"req-1","AccountId":"1234"}"#,
    )]);
    let provider = AliyunProvider::with_transport(Arc::clone(&transport) as Arc<dyn HttpTransport>);

    provider
        .verify_credentials(&aliyun_keys())
        .await
        .expect("有效密钥应可用");

    let url = &transport.requests()[0].url;
    assert!(url.starts_with("https://sts.aliyuncs.com/"), "{url}");
    assert!(url.contains("Action=GetCallerIdentity"), "{url}");
    assert!(url.contains("Version=2015-04-01"), "{url}");
    assert!(url.contains("Signature="), "{url}");
}

#[tokio::test]
async fn aliyun_verify_credentials_surfaces_server_errors() {
    let transport = ScriptedTransport::new(vec![HttpResponse::new(
        200,
        r#"{"Code":"InvalidAccessKeyId.NotFound","Message":"The AccessKey ID does not exist"}"#,
    )]);
    let provider = AliyunProvider::with_transport(Arc::clone(&transport) as Arc<dyn HttpTransport>);

    let err = provider
        .verify_credentials(&aliyun_keys())
        .await
        .expect_err("无效密钥应报错");
    let text = err.to_string();
    assert!(text.contains("InvalidAccessKeyId.NotFound"), "{text}");
    assert!(!text.contains("SECRET"), "不得回显密钥: {text}");
}

#[tokio::test]
async fn tencent_verify_credentials_probes_sts_per_account_site() {
    // 国内站与国际站的凭据各探测一次，端点必须随 account_site 切换。
    for (site, expected_host) in [
        ("cn", "https://sts.tencentcloudapi.com/"),
        ("intl", "https://sts.intl.tencentcloudapi.com/"),
    ] {
        let transport = ScriptedTransport::new(vec![HttpResponse::new(
            200,
            r#"{"Response":{"AccountId":"1234","RequestId":"req-1"}}"#,
        )]);
        let provider =
            TencentProvider::with_transport(Arc::clone(&transport) as Arc<dyn HttpTransport>);

        provider
            .verify_credentials(&tencent_keys(site))
            .await
            .expect("有效密钥应可用");

        let request = &transport.requests()[0];
        assert_eq!(request.url, expected_host);
        assert_eq!(request.headers["X-TC-Action"], "GetCallerIdentity");
        assert_eq!(request.headers["X-TC-Version"], "2018-08-13");
        assert_eq!(request.body.as_ref().expect("应带请求体"), &json!({}));
    }
}

#[tokio::test]
async fn tencent_verify_credentials_surfaces_server_errors() {
    let transport = ScriptedTransport::new(vec![HttpResponse::new(
        200,
        r#"{"Response":{"Error":{"Code":"AuthFailure.SecretIdNotFound","Message":"SecretId 不存在"},"RequestId":"req-1"}}"#,
    )]);
    let provider =
        TencentProvider::with_transport(Arc::clone(&transport) as Arc<dyn HttpTransport>);

    let err = provider
        .verify_credentials(&tencent_keys("intl"))
        .await
        .expect_err("无效密钥应报错");
    let text = err.to_string();
    assert!(text.contains("AuthFailure.SecretIdNotFound"), "{text}");
    assert!(!text.contains("tencent-secret"), "不得回显密钥: {text}");
}

#[tokio::test]
async fn tencent_eo_verify_credentials_probes_sts_too() {
    let transport = ScriptedTransport::new(vec![HttpResponse::new(
        200,
        r#"{"Response":{"AccountId":"1234","RequestId":"req-1"}}"#,
    )]);
    let provider =
        TencentEoProvider::with_transport(Arc::clone(&transport) as Arc<dyn HttpTransport>);

    provider
        .verify_credentials(&tencent_keys("intl"))
        .await
        .expect("有效密钥应可用");

    assert_eq!(
        transport.requests()[0].url,
        "https://sts.intl.tencentcloudapi.com/"
    );
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
    registry.register(TencentProvider::new()).expect("应能注册");
    registry
        .register(TencentEoProvider::new())
        .expect("应能注册");

    assert_eq!(
        registry.type_ids(),
        vec![
            "aliyun".to_owned(),
            "cloudflare".to_owned(),
            "tencent".to_owned(),
            "tencent-eo".to_owned(),
        ]
    );
    assert_eq!(
        registry.require("cloudflare").unwrap().display_name(),
        "Cloudflare"
    );
    assert_eq!(
        registry.require("aliyun").unwrap().display_name(),
        "阿里云 DNS"
    );
    assert_eq!(
        registry.require("tencent").unwrap().display_name(),
        "腾讯云"
    );
    assert_eq!(
        registry.require("tencent-eo").unwrap().display_name(),
        "腾讯云 EdgeOne"
    );

    // 凭据定义随提供商一起导出，前端据此渲染表单。
    for type_id in ["cloudflare", "aliyun", "tencent", "tencent-eo"] {
        let rendered =
            serde_json::to_string(&registry.require(type_id).unwrap().credential_fields()).unwrap();
        assert!(rendered.contains("required"), "{type_id}: {rendered}");
    }
    assert!(
        serde_json::to_string(&registry.require("aliyun").unwrap().credential_fields())
            .unwrap()
            .contains("access_key_secret")
    );
    // 腾讯云体系的凭据字段：两家共用一份定义，含站点枚举。
    let tencent_fields =
        serde_json::to_string(&registry.require("tencent").unwrap().credential_fields()).unwrap();
    for field in ["secret_id", "secret_key", "account_site", "cn", "intl"] {
        assert!(tencent_fields.contains(field), "凭据 schema 应含 {field}: {tencent_fields}");
    }
}

#[test]
fn an_unregistered_provider_is_rejected() {
    let registry = DnsProviderRegistry::new();
    let err = registry.require("route53").expect_err("未注册应被拒绝");
    assert!(err.to_string().contains("route53"), "{err}");
}
