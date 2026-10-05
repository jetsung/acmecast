//! 通用 webhook 适配器：统一请求方案。
//!
//! URL、签名方式、请求方法、请求头与请求体模板均可配置；飞书与钉钉
//! 两种签名方案写死在代码中（[`SignKind`]），配置只指定方式标识。
//! 不配置任何扩展字段时行为与最初版本一致：统一事件负载 JSON 原样 POST。
//!
//! 模板占位符 `{{var}}` 以字符串值填充并做 JSON 转义（供 `"{{var}}"`
//! 的带引号用法），`{{timestamp}}` 为纯数字不加引号；签名值既可由模板
//! 的 `{{sign}}`/`{{timestamp}}` 显式放置，也可由发送路径按方案自动附加
//! （飞书进请求体、钉钉进地址查询参数）。

use std::collections::BTreeMap;

use async_trait::async_trait;
use serde_json::Value;

use crate::channel::{ChannelConfig, NotificationChannel, Signature, apply_sign, send_request};
use crate::error::{Error, Result};
use crate::message::NotificationMessage;
use crate::sign::SignKind;

/// 允许的请求方法：通知场景 GET 无意义（无请求体），不放行。
pub const ALLOWED_METHODS: [&str; 3] = ["POST", "PUT", "PATCH"];

/// 请求体模板可用的占位符。
pub const TEMPLATE_VARIABLES: [&str; 10] = [
    "event",
    "title",
    "pipeline",
    "trigger",
    "trigger_label",
    "occurred_at",
    "domains",
    "target",
    "timestamp",
    "sign",
];

/// 通用 webhook。
#[derive(Debug, Default)]
pub struct GenericChannel;

#[async_trait]
impl NotificationChannel for GenericChannel {
    fn provider(&self) -> &'static str {
        "generic"
    }

    async fn send(
        &self,
        client: &reqwest::Client,
        channel: &ChannelConfig,
        message: &NotificationMessage,
    ) -> Result<()> {
        // 签名只算一次：模板变量与自动附加共用同一份值，避免跨秒不一致。
        let signature = channel
            .secret
            .as_deref()
            .zip(channel.sign)
            .map(|(secret, kind)| Signature::compute(kind, secret));
        let body = render_body(channel, message, signature.as_ref())?;
        let (url, body) = apply_sign(&channel.url, body, signature.as_ref());
        send_request(client, &url, &channel.method, &channel.headers, &body).await
    }
}

/// 渲染请求体：模板优先，缺省按签名方式选平台格式。
///
/// 无签名且未配置模板时保持最初行为——统一事件负载原样 POST。
fn render_body(
    channel: &ChannelConfig,
    message: &NotificationMessage,
    signature: Option<&Signature>,
) -> Result<Value> {
    let Some(template) = &channel.body_template else {
        return Ok(match channel.sign {
            Some(SignKind::Feishu) => message.feishu_text_json(),
            Some(SignKind::DingTalk) => message.dingtalk_text_json(),
            None => message.payload_json(),
        });
    };
    let text = render_template(template, &template_vars(message, signature));
    serde_json::from_str(&text).map_err(|error| Error::Delivery {
        name: channel.name.clone(),
        cause: format!("请求体模板渲染结果不是合法 JSON: {error}"),
    })
}

/// 组装模板变量表；文本值一律 JSON 转义，`timestamp` 为纯数字。
///
/// 签名值与时间戳来自 send 侧算好的 [`Signature`]（与自动附加共用同一
/// 份）；无签名时 `sign` 填空串、`timestamp` 取当前秒。
fn template_vars(
    message: &NotificationMessage,
    signature: Option<&Signature>,
) -> BTreeMap<String, String> {
    // JSON 字符串转义并剥掉外层引号：模板里以 `"{{var}}"` 带引号使用，
    // 值里的引号、换行等由这层转义保证不破坏 JSON 结构。
    let json_escaped = |value: &str| -> String {
        let encoded = serde_json::to_string(value).unwrap_or_default();
        encoded[1..encoded.len() - 1].to_owned()
    };

    let mut vars = BTreeMap::new();
    vars.insert("event".to_owned(), json_escaped(&message.event));
    vars.insert("title".to_owned(), json_escaped(&message.title));
    vars.insert("pipeline".to_owned(), json_escaped(&message.pipeline));
    vars.insert("trigger".to_owned(), json_escaped(&message.trigger));
    vars.insert(
        "trigger_label".to_owned(),
        json_escaped(crate::message::trigger_label(&message.trigger)),
    );
    vars.insert(
        "occurred_at".to_owned(),
        json_escaped(&message.occurred_at.to_rfc3339()),
    );
    vars.insert(
        "domains".to_owned(),
        json_escaped(&message.domains.join(", ")),
    );
    vars.insert(
        "target".to_owned(),
        json_escaped(message.target.as_deref().unwrap_or_default()),
    );
    let (timestamp, sign) = signature.map_or_else(
        || (chrono::Utc::now().timestamp(), String::new()),
        |signature| (signature.timestamp(), signature.value().to_owned()),
    );
    vars.insert("timestamp".to_owned(), timestamp.to_string());
    vars.insert("sign".to_owned(), sign);
    vars
}

/// 把模板中的 `{{var}}` 占位符替换为变量值。
///
/// 未知占位符原样保留：启动校验已拦截未知变量，投递路径上遇到只可能
/// 是防御场景，留原样比静默丢内容更容易排障；未闭合的 `{{` 同样原样。
fn render_template(template: &str, vars: &BTreeMap<String, String>) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(start) = rest.find("{{") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        match after.find("}}") {
            Some(end) => {
                let name = after[..end].trim();
                match vars.get(name) {
                    Some(value) => out.push_str(value),
                    None => out.push_str(&rest[start..start + end + 4]),
                }
                rest = &after[end + 2..];
            }
            None => {
                out.push_str("{{");
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// 扫描模板中的占位符名（供启动校验）。
#[must_use]
pub fn template_placeholders(template: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut rest = template;
    while let Some(start) = rest.find("{{") {
        let after = &rest[start + 2..];
        match after.find("}}") {
            Some(end) => {
                names.push(after[..end].trim().to_owned());
                rest = &after[end + 2..];
            }
            None => break,
        }
    }
    names
}

/// 校验渠道的扩展字段（方法、请求头、签名与请求体模板）。
///
/// 供服务端在解析 `[[notifications]]` 时调用：配置错误在启动日志里
/// 说清楚，不带进投递路径。
///
/// # Errors
///
/// 方法不在白名单、请求头名/值非法、签名方式缺 `secret`、模板占位符
/// 未知，或 `sign = feishu` 的模板既无 `{{sign}}` 占位符又渲染不出
/// JSON 对象（签名无处合并）时返回错误说明。
pub fn validate_channel(channel: &ChannelConfig) -> std::result::Result<(), String> {
    if !ALLOWED_METHODS.contains(&channel.method.as_str()) {
        return Err(format!(
            "method 仅支持 {}（收到 {:?}）",
            ALLOWED_METHODS.join(" / "),
            channel.method
        ));
    }
    for (name, value) in &channel.headers {
        if reqwest::header::HeaderName::from_bytes(name.as_bytes()).is_err() {
            return Err(format!("headers 含非法请求头名: {name:?}"));
        }
        if reqwest::header::HeaderValue::from_str(value).is_err() {
            return Err(format!("headers 含非法请求头值: {name:?}"));
        }
    }
    if channel.sign.is_some() && channel.secret.is_none() {
        return Err("配置了签名方式（sign）但未提供 secret".to_owned());
    }
    let Some(template) = &channel.body_template else {
        return Ok(());
    };
    let placeholders = template_placeholders(template);
    for name in &placeholders {
        if !TEMPLATE_VARIABLES.contains(&name.as_str()) {
            return Err(format!(
                "请求体模板含未知占位符 {{{{{name}}}}}（可用：{}）",
                TEMPLATE_VARIABLES.join("、")
            ));
        }
    }
    if channel.sign == Some(SignKind::Feishu) && !placeholders.iter().any(|name| name == "sign") {
        // 飞书签名合并进请求体顶层，模板必须渲染出 JSON 对象；
        // 用样例消息渲染一次，结构与真实投递一致。
        let sample = sample_message();
        let text = render_template(template, &template_vars(&sample, None));
        match serde_json::from_str::<Value>(&text) {
            Ok(value) if value.is_object() => {}
            _ => {
                return Err(
                    "sign = feishu 时请求体模板须渲染为 JSON 对象（签名自动合并进顶层字段），或在模板中显式使用 {{sign}} 占位符"
                        .to_owned(),
                );
            }
        }
    }
    Ok(())
}

/// 校验用的样例消息：覆盖全部可选字段，暴露模板对缺省素材的处理。
fn sample_message() -> NotificationMessage {
    NotificationMessage {
        event: crate::EVENT_CERT_APPLY.to_owned(),
        title: "证书申请成功".to_owned(),
        pipeline: "示例流水线".to_owned(),
        trigger: "manual".to_owned(),
        occurred_at: chrono::Utc::now(),
        domains: vec!["a.example.com".to_owned()],
        target: Some("SSH web.example.com".to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::EVENT_CERT_APPLY;
    use chrono::TimeZone;
    use serde_json::json;

    fn message() -> NotificationMessage {
        NotificationMessage {
            event: EVENT_CERT_APPLY.to_owned(),
            title: "证书申请成功".to_owned(),
            pipeline: "示例".to_owned(),
            trigger: "cron".to_owned(),
            occurred_at: chrono::Utc.with_ymd_and_hms(2026, 10, 4, 12, 0, 0).unwrap(),
            domains: vec!["a.example.com".to_owned()],
            target: None,
        }
    }

    fn channel(template: Option<&str>) -> ChannelConfig {
        ChannelConfig {
            name: "gen".to_owned(),
            provider: "generic".to_owned(),
            url: "http://127.0.0.1:9/hook".to_owned(),
            secret: None,
            events: vec![EVENT_CERT_APPLY.to_owned()],
            enabled: true,
            sign: None,
            method: "POST".to_owned(),
            headers: BTreeMap::new(),
            body_template: template.map(str::to_owned),
        }
    }

    #[test]
    fn default_body_follows_sign_kind() {
        let no_sign = render_body(&channel(None), &message(), None).unwrap();
        assert_eq!(no_sign, message().payload_json(), "无签名保持原负载");

        let feishu = render_body(
            &ChannelConfig {
                sign: Some(SignKind::Feishu),
                ..channel(None)
            },
            &message(),
            None,
        )
        .unwrap();
        assert_eq!(feishu["msg_type"], "text");

        let dingtalk = render_body(
            &ChannelConfig {
                sign: Some(SignKind::DingTalk),
                ..channel(None)
            },
            &message(),
            None,
        )
        .unwrap();
        assert_eq!(dingtalk["msgtype"], "text");
    }

    #[test]
    fn template_variables_are_json_escaped() {
        let mut msg = message();
        msg.pipeline = "引\"号\n换行".to_owned();
        let text = render_template(
            "{\"pipeline\": \"{{pipeline}}\", \"t\": {{timestamp}}}",
            &template_vars(&msg, None),
        );
        let value: Value = serde_json::from_str(&text).expect("转义后应为合法 JSON");
        assert_eq!(value["pipeline"], "引\"号\n换行");
        assert!(value["t"].is_number(), "timestamp 应为数字: {text}");
    }

    #[test]
    fn unknown_placeholder_stays_verbatim() {
        let text = render_template("{\"x\": \"{{nope}}\"}", &template_vars(&message(), None));
        assert_eq!(text, "{\"x\": \"{{nope}}\"}");
    }

    #[test]
    fn unclosed_brace_stays_verbatim() {
        let text = render_template("a {{b c", &template_vars(&message(), None));
        assert_eq!(text, "a {{b c");
    }

    #[test]
    fn placeholders_scan_trimmed_names() {
        let names = template_placeholders("{{a}} {{ b }} {{c");
        assert_eq!(names, vec!["a", "b"]);
    }

    #[test]
    fn validate_rejects_unknown_placeholder_and_method() {
        let error = validate_channel(&channel(Some("{\"x\": \"{{nope}}\"}"))).unwrap_err();
        assert!(error.contains("未知占位符"), "{error}");

        let error = validate_channel(&ChannelConfig {
            method: "GET".to_owned(),
            ..channel(None)
        })
        .unwrap_err();
        assert!(error.contains("method"), "{error}");
    }

    #[test]
    fn validate_rejects_sign_without_secret() {
        let error = validate_channel(&ChannelConfig {
            sign: Some(SignKind::Feishu),
            ..channel(None)
        })
        .unwrap_err();
        assert!(error.contains("secret"), "{error}");
    }

    #[test]
    fn validate_rejects_feishu_template_without_object_or_sign() {
        let error = validate_channel(&ChannelConfig {
            sign: Some(SignKind::Feishu),
            secret: Some("s".to_owned()),
            body_template: Some("plain text {{title}}".to_owned()),
            ..channel(None)
        })
        .unwrap_err();
        assert!(error.contains("JSON 对象"), "{error}");

        // 显式 {{sign}} 占位符后不再要求对象。
        assert!(
            validate_channel(&ChannelConfig {
                sign: Some(SignKind::Feishu),
                secret: Some("s".to_owned()),
                body_template: Some("plain {{sign}} {{timestamp}}".to_owned()),
                ..channel(None)
            })
            .is_ok()
        );
    }

    #[test]
    fn validate_accepts_valid_headers_and_template() {
        let mut headers = BTreeMap::new();
        headers.insert("X-API-Key".to_owned(), "sk-123".to_owned());
        assert!(
            validate_channel(&ChannelConfig {
                headers,
                body_template: Some(
                    json!({
                        "text": "{{title}}｜{{pipeline}}｜{{domains}}"
                    })
                    .to_string()
                ),
                ..channel(None)
            })
            .is_ok()
        );
    }

    #[test]
    fn ntfy_example_template_renders_valid_json() {
        // 文档（03-configuration.md）中的 ntfy.sh 示例模板原样在此渲染，
        // 防止示例随渲染逻辑演进而失效。TOML 单引号串与 Rust raw string
        // 一样按字面保留 `\n`，由 JSON 解析成换行。
        let template = concat!(
            r#"{"topic": "acmecast-ops", "title": "{{title}}", "#,
            r#""message": "{{pipeline}}（{{trigger_label}}）\n域名：{{domains}}", "#,
            r#""tags": ["white_check_mark"], "priority": 4}"#
        );
        let config = channel(Some(template));
        assert!(validate_channel(&config).is_ok());

        let body = render_body(&config, &message(), None).unwrap();
        assert_eq!(body["topic"], "acmecast-ops");
        assert_eq!(body["title"], "证书申请成功");
        let text = body["message"].as_str().expect("message 应为字符串");
        assert!(text.contains("示例（定时）"), "{text}");
        assert!(text.contains("a.example.com"), "{text}");
        assert!(text.contains('\n'), "换行应保留: {text}");
        assert_eq!(body["priority"], 4);
    }

    #[test]
    fn validate_rejects_illegal_header_name() {
        let mut headers = BTreeMap::new();
        headers.insert("bad header\nname".to_owned(), "v".to_owned());
        let error = validate_channel(&ChannelConfig {
            headers,
            ..channel(None)
        })
        .unwrap_err();
        assert!(error.contains("非法请求头名"), "{error}");
    }
}
