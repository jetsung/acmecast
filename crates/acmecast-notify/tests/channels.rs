//! 渠道适配器的 HTTP 行为断言（wiremock 进程内 mock）。
//!
//! 每个内置适配器都要证明三件事：请求打到配置的地址、请求体符合平台
//! 格式、签名按平台规则生成。generic 还要证明统一请求方案的可配置项
//! （方法、请求头、模板）真实生效。状态码非 2xx 视为失败。

use std::collections::BTreeMap;

use acmecast_notify::sign::hmac_sha256_base64;
use acmecast_notify::{
    ChannelConfig, DingTalkChannel, FeishuChannel, GenericChannel, NotificationChannel,
    NotificationMessage, SignKind,
};
use chrono::Utc;
use serde_json::Value;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn message() -> NotificationMessage {
    NotificationMessage {
        event: "cert.apply".to_owned(),
        title: "证书申请成功".to_owned(),
        pipeline: "example-pipeline".to_owned(),
        trigger: "cron".to_owned(),
        occurred_at: Utc::now(),
        domains: vec!["a.example.com".to_owned()],
        target: None,
    }
}

fn config(url: &str) -> ChannelConfig {
    ChannelConfig {
        name: "测试渠道".to_owned(),
        provider: "generic".to_owned(),
        url: url.to_owned(),
        secret: None,
        events: vec!["cert.apply".to_owned()],
        enabled: true,
        sign: None,
        method: "POST".to_owned(),
        headers: BTreeMap::new(),
        body_template: None,
    }
}

async fn ok_server() -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/hook"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;
    server
}

#[tokio::test]
async fn feishu_posts_text_body_without_sign() {
    let server = ok_server().await;
    let mut channel = config(&format!("{}/hook", server.uri()));
    channel.provider = "feishu".to_owned();
    FeishuChannel
        .send(&reqwest::Client::new(), &channel, &message())
        .await
        .expect("无密钥投递应成功");

    let requests = server.received_requests().await.expect("应记录请求");
    assert_eq!(requests.len(), 1);
    let body: Value = serde_json::from_slice(&requests[0].body).expect("应为 JSON");
    assert_eq!(body["msg_type"], "text");
    let text = body["content"]["text"].as_str().expect("应有文本");
    assert!(text.contains("证书申请成功"), "{text}");
    assert!(text.contains("example-pipeline"), "{text}");
    assert!(body.get("timestamp").is_none(), "无密钥不应带签名");
    assert!(body.get("sign").is_none(), "无密钥不应带签名");
}

#[tokio::test]
async fn feishu_with_secret_signs_timestamp_and_hmac() {
    let server = ok_server().await;
    let mut channel = config(&format!("{}/hook", server.uri()));
    channel.provider = "feishu".to_owned();
    channel.secret = Some("my-secret".to_owned());
    FeishuChannel
        .send(&reqwest::Client::new(), &channel, &message())
        .await
        .expect("签名投递应成功");

    let body: Value =
        serde_json::from_slice(&server.received_requests().await.unwrap()[0].body).unwrap();
    let timestamp = body["timestamp"].as_i64().expect("应有秒级时间戳");
    let sign = body["sign"].as_str().expect("应有签名");
    // 飞书规则：以 `timestamp\nsecret` 为 HMAC 密钥对空串签名。
    let string_to_sign = format!("{timestamp}\nmy-secret");
    assert_eq!(sign, hmac_sha256_base64(string_to_sign.as_bytes(), b""));
}

#[tokio::test]
async fn dingtalk_posts_text_body_and_unsigned_when_no_secret() {
    let server = ok_server().await;
    let mut channel = config(&format!("{}/hook", server.uri()));
    channel.provider = "dingtalk".to_owned();
    DingTalkChannel
        .send(&reqwest::Client::new(), &channel, &message())
        .await
        .expect("无密钥投递应成功");

    let requests = server.received_requests().await.expect("应记录请求");
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].url.path(), "/hook");
    let body: Value = serde_json::from_slice(&requests[0].body).expect("应为 JSON");
    assert_eq!(body["msgtype"], "text");
    assert!(
        body["text"]["content"]
            .as_str()
            .unwrap()
            .contains("证书申请成功")
    );
}

#[tokio::test]
async fn dingtalk_with_secret_appends_encoded_sign_query() {
    let server = ok_server().await;
    let mut channel = config(&format!("{}/hook", server.uri()));
    channel.provider = "dingtalk".to_owned();
    channel.secret = Some("SECxxxxxxxx".to_owned());
    DingTalkChannel
        .send(&reqwest::Client::new(), &channel, &message())
        .await
        .expect("加签投递应成功");

    let request = &server.received_requests().await.unwrap()[0];
    let query = request.url.query().expect("应带查询参数");
    // 钉钉规则：`timestamp + "\n" + secret` 为待签内容、密钥为 secret。
    let timestamp = query
        .split('&')
        .find(|pair| pair.starts_with("timestamp="))
        .and_then(|pair| pair.strip_prefix("timestamp="))
        .and_then(|value| value.parse::<i64>().ok())
        .expect("timestamp 应为毫秒数");
    let sign = query
        .split('&')
        .find(|pair| pair.starts_with("sign="))
        .and_then(|pair| pair.strip_prefix("sign="))
        .expect("应有 sign 参数");
    let expected = hmac_sha256_base64(
        b"SECxxxxxxxx",
        format!("{timestamp}\nSECxxxxxxxx").as_bytes(),
    );
    assert_eq!(sign, acmecast_notify::sign::percent_encode(&expected));
    // `+` 必须被编码，否则服务端会把它解析成空格导致校验失败。
    assert!(!sign.contains('+'), "签名应做百分号编码：{sign}");
}

#[tokio::test]
async fn generic_posts_unified_payload() {
    let server = ok_server().await;
    GenericChannel
        .send(
            &reqwest::Client::new(),
            &config(&format!("{}/hook", server.uri())),
            &message(),
        )
        .await
        .expect("通用投递应成功");

    let requests = server.received_requests().await.expect("应记录请求");
    let content_type = requests[0]
        .headers
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .expect("应声明 Content-Type");
    assert!(
        content_type.starts_with("application/json"),
        "{content_type}"
    );
    let body: Value = serde_json::from_slice(&requests[0].body).expect("应为 JSON");
    assert_eq!(body["event"], "cert.apply");
    assert_eq!(body["pipeline"], "example-pipeline");
    assert_eq!(body["trigger"], "cron");
    assert_eq!(body["domains"][0], "a.example.com");
    assert!(body.get("target").is_none());
}

#[tokio::test]
async fn generic_renders_template_with_method_and_headers() {
    let server = MockServer::start().await;
    Mock::given(method("PUT"))
        .and(path("/hook"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;

    let mut channel = config(&format!("{}/hook", server.uri()));
    channel.method = "PUT".to_owned();
    channel
        .headers
        .insert("X-API-Key".to_owned(), "sk-123".to_owned());
    channel.body_template = Some(r#"{"text": "{{title}}｜{{pipeline}}｜{{domains}}"}"#.to_owned());
    GenericChannel
        .send(&reqwest::Client::new(), &channel, &message())
        .await
        .expect("自定义模板投递应成功");

    let request = &server.received_requests().await.expect("应记录请求")[0];
    assert_eq!(
        request
            .headers
            .get("x-api-key")
            .and_then(|value| value.to_str().ok()),
        Some("sk-123")
    );
    let body: Value = serde_json::from_slice(&request.body).expect("模板应渲染为 JSON");
    assert_eq!(
        body["text"],
        "证书申请成功｜example-pipeline｜a.example.com"
    );
}

#[tokio::test]
async fn generic_applies_feishu_sign_scheme() {
    let server = ok_server().await;
    let mut channel = config(&format!("{}/hook", server.uri()));
    channel.sign = Some(SignKind::Feishu);
    channel.secret = Some("my-secret".to_owned());
    GenericChannel
        .send(&reqwest::Client::new(), &channel, &message())
        .await
        .expect("飞书签名方案投递应成功");

    let body: Value =
        serde_json::from_slice(&server.received_requests().await.unwrap()[0].body).unwrap();
    // 未配模板：走飞书缺省 text 格式，签名自动合并进请求体。
    assert_eq!(body["msg_type"], "text");
    let timestamp = body["timestamp"].as_i64().expect("应有秒级时间戳");
    assert_eq!(
        body["sign"].as_str().expect("应有签名"),
        hmac_sha256_base64(format!("{timestamp}\nmy-secret").as_bytes(), b"")
    );
}

#[tokio::test]
async fn generic_applies_dingtalk_sign_scheme() {
    let server = ok_server().await;
    let mut channel = config(&format!("{}/hook", server.uri()));
    channel.sign = Some(SignKind::DingTalk);
    channel.secret = Some("SECxxxxxxxx".to_owned());
    GenericChannel
        .send(&reqwest::Client::new(), &channel, &message())
        .await
        .expect("钉钉签名方案投递应成功");

    let request = &server.received_requests().await.expect("应记录请求")[0];
    // 未配模板：走钉钉缺省 text 格式，签名自动追加到查询参数。
    let body: Value = serde_json::from_slice(&request.body).unwrap();
    assert_eq!(body["msgtype"], "text");
    assert!(
        request.url.query().unwrap().contains("sign="),
        "签名应进 URL"
    );
}

#[tokio::test]
async fn generic_template_sign_placeholder_is_used_verbatim() {
    let server = ok_server().await;
    let mut channel = config(&format!("{}/hook", server.uri()));
    channel.sign = Some(SignKind::Feishu);
    channel.secret = Some("my-secret".to_owned());
    channel.body_template =
        Some(r#"{"text": "{{title}}", "sign": "{{sign}}", "timestamp": {{timestamp}}}"#.to_owned());
    GenericChannel
        .send(&reqwest::Client::new(), &channel, &message())
        .await
        .expect("显式签名占位符投递应成功");

    let body: Value =
        serde_json::from_slice(&server.received_requests().await.unwrap()[0].body).unwrap();
    let timestamp = body["timestamp"].as_i64().expect("应有时间戳");
    // 模板里的 {{sign}} 就是最终签名，自动附加不再覆盖。
    assert_eq!(
        body["sign"].as_str().expect("应有签名"),
        hmac_sha256_base64(format!("{timestamp}\nmy-secret").as_bytes(), b"")
    );
}

#[tokio::test]
async fn http_error_surfaces_status_and_body() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(400).set_body_string("sign不匹配"))
        .mount(&server)
        .await;

    let channel = config(&format!("{}/hook", server.uri()));
    let error = FeishuChannel
        .send(&reqwest::Client::new(), &channel, &message())
        .await
        .expect_err("非 2xx 应报错");
    let text = error.to_string();
    assert!(text.contains("400"), "{text}");
    assert!(text.contains("sign不匹配"), "错误应带响应片段：{text}");
}
