//! 通知凭据类型的行为：字段校验与凭据 → 渠道配置映射。
//!
//! 校验断言「缺必填 / 非法取值报错、合法字段通过」；映射断言 D5 的字段
//! 对应关系（provider 去前缀、sign 按 secret 推导、events 恒空、enabled 恒真）。

use acmecast_access::{CredentialType, ResolvedCredential};
use acmecast_notify::{
    DINGTALK_TYPE_ID, DingtalkCredentialType, EMAIL_TYPE_ID, EmailCredentialType, FEISHU_TYPE_ID,
    FeishuCredentialType, GENERIC_TYPE_ID, GenericCredentialType, SignKind, TlsMode,
    channel_config_from_credential, is_notify_type_id,
};
use serde_json::json;

/// 构造一份已解密凭据（映射函数的输入）。
fn credential(type_id: &str, name: &str, fields: serde_json::Value) -> ResolvedCredential {
    ResolvedCredential {
        id: 7,
        name: name.to_owned(),
        type_id: type_id.to_owned(),
        remark: String::new(),
        capabilities: Vec::new(),
        fields,
    }
}

#[test]
fn every_type_id_is_recognized_as_notify() {
    for type_id in [
        FEISHU_TYPE_ID,
        DINGTALK_TYPE_ID,
        GENERIC_TYPE_ID,
        EMAIL_TYPE_ID,
    ] {
        assert!(is_notify_type_id(type_id), "{type_id} 应为通知类型");
    }
    assert!(!is_notify_type_id("deploy.ssh"));
    assert!(!is_notify_type_id("cloud.tencent"));
}

#[test]
fn missing_required_fields_are_rejected_with_field_located_errors() {
    // 飞书缺 url。
    let error = FeishuCredentialType::default()
        .validate(&json!({}))
        .expect_err("缺 url 应被拒绝");
    assert!(
        matches!(&error, acmecast_core::Error::Validation { field, .. } if field == "fields"),
        "缺字段应在反序列化阶段报出: {error:?}"
    );

    // 钉钉缺 url。
    assert!(
        DingtalkCredentialType::default()
            .validate(&json!({}))
            .is_err()
    );

    // 通用 webhook 缺 url。
    assert!(
        GenericCredentialType::default()
            .validate(&json!({}))
            .is_err()
    );

    // 邮件缺 from。
    assert!(
        EmailCredentialType::default()
            .validate(&json!({ "host": "smtp.example.com", "to": ["a@example.com"] }))
            .is_err()
    );
}

#[test]
fn valid_fields_pass_validation_for_all_types() {
    FeishuCredentialType::default()
        .validate(&json!({
            "url": "https://open.feishu.cn/open-apis/bot/v2/hook/x",
            "secret": "s",
        }))
        .expect("飞书合法字段应通过");
    DingtalkCredentialType::default()
        .validate(&json!({
            "url": "https://oapi.dingtalk.com/robot/send?access_token=x",
        }))
        .expect("钉钉合法字段应通过");
    GenericCredentialType::default()
        .validate(&json!({
            "url": "https://ops.example.com/hook",
            "method": "PATCH",
            "headers": { "X-API-Key": "sk" },
            "body_template": "{\"text\": \"{{title}}\"}",
        }))
        .expect("通用 webhook 合法字段应通过");
    EmailCredentialType::default()
        .validate(&json!({
            "host": "smtp.exmail.qq.com",
            "port": 465,
            "tls": "implicit",
            "from": "no-reply@example.com",
            "to": ["ops@example.com"],
            "username": "no-reply@example.com",
            "password": "x",
        }))
        .expect("邮件合法字段应通过");
}

#[test]
fn feishu_credential_maps_to_channel_config() {
    let config = channel_config_from_credential(&credential(
        FEISHU_TYPE_ID,
        "生产飞书群",
        json!({
            "url": "https://open.feishu.cn/open-apis/bot/v2/hook/x",
            "secret": "sign-key",
        }),
    ), None)
    .expect("映射应成功");

    assert_eq!(config.name, "生产飞书群", "渠道名取凭据记录展示名");
    assert_eq!(config.provider, "feishu");
    assert_eq!(config.url, "https://open.feishu.cn/open-apis/bot/v2/hook/x");
    assert_eq!(config.secret.as_deref(), Some("sign-key"));
    assert_eq!(config.sign, Some(SignKind::Feishu));
    assert!(config.events.is_empty(), "events 不再参与过滤");
    assert!(config.enabled, "凭据渠道恒启用");
    assert!(config.smtp.is_none());
}

#[test]
fn feishu_without_secret_has_no_signature() {
    let config = channel_config_from_credential(&credential(
        FEISHU_TYPE_ID,
        "无签名群",
        json!({ "url": "https://open.feishu.cn/x", "secret": "  " }),
    ), None)
    .expect("映射应成功");
    assert!(config.secret.is_none(), "空白 secret 应归一为无签名");
    assert!(config.sign.is_none());
}

#[test]
fn dingtalk_credential_maps_to_signed_channel() {
    let config = channel_config_from_credential(&credential(
        DINGTALK_TYPE_ID,
        "钉钉群",
        json!({
            "url": "https://oapi.dingtalk.com/robot/send?access_token=x",
            "secret": "SECxxxx",
        }),
    ), None)
    .expect("映射应成功");
    assert_eq!(config.provider, "dingtalk");
    assert_eq!(config.sign, Some(SignKind::DingTalk));
}

#[test]
fn generic_credential_maps_method_headers_and_template() {
    let config = channel_config_from_credential(&credential(
        GENERIC_TYPE_ID,
        "自建",
        json!({
            "url": "https://ops.example.com/hook",
            "method": "put",
            "headers": { "X-API-Key": "sk" },
            "body_template": "{\"text\": \"{{title}}\"}",
        }),
    ), None)
    .expect("映射应成功");
    assert_eq!(config.provider, "generic");
    assert_eq!(config.method, "PUT", "方法应归一为大写");
    assert_eq!(
        config.headers.get("X-API-Key").map(String::as_str),
        Some("sk")
    );
    assert_eq!(
        config.body_template.as_deref(),
        Some("{\"text\": \"{{title}}\"}")
    );
    assert!(config.secret.is_none());
    assert!(config.sign.is_none());
}

#[test]
fn generic_credential_defaults_method_to_post() {
    let config = channel_config_from_credential(&credential(
        GENERIC_TYPE_ID,
        "自建",
        json!({ "url": "https://ops.example.com/hook" }),
    ), None)
    .expect("映射应成功");
    assert_eq!(config.method, "POST");
    assert!(config.headers.is_empty());
    assert!(config.body_template.is_none());
}

#[test]
fn email_credential_maps_smtp_settings() {
    let config = channel_config_from_credential(&credential(
        EMAIL_TYPE_ID,
        "邮件群",
        json!({
            "host": "smtp.exmail.qq.com",
            "port": 587,
            "tls": "starttls",
            "from": "no-reply@example.com",
            "to": ["ops@example.com", " backup@example.com "],
            "username": "no-reply@example.com",
            "password": "x",
        }),
    ), None)
    .expect("映射应成功");
    assert_eq!(config.provider, "email");
    assert!(config.url.is_empty(), "邮件渠道无 webhook 地址");
    let smtp = config.smtp.expect("邮件渠道应带 SMTP 设置");
    assert_eq!(smtp.host, "smtp.exmail.qq.com");
    assert_eq!(smtp.port, Some(587));
    assert_eq!(smtp.tls, TlsMode::StartTls);
    assert_eq!(
        smtp.to,
        vec![
            "ops@example.com".to_owned(),
            "backup@example.com".to_owned()
        ],
        "收件人应去空白"
    );
    assert!(smtp.has_credentials());
}

#[test]
fn email_credential_defaults_tls_to_implicit() {
    let config = channel_config_from_credential(&credential(
        EMAIL_TYPE_ID,
        "邮件群",
        json!({
            "host": "smtp.example.com",
            "from": "a@example.com",
            "to": ["b@example.com"],
        }),
    ), None)
    .expect("映射应成功");
    let smtp = config.smtp.expect("邮件渠道应带 SMTP 设置");
    assert_eq!(smtp.tls, TlsMode::Implicit);
    assert_eq!(smtp.effective_port(), 465);
}

#[test]
fn email_credential_with_unknown_tls_mode_fails_mapping() {
    let error = channel_config_from_credential(&credential(
        EMAIL_TYPE_ID,
        "邮件群",
        json!({
            "host": "smtp.example.com",
            "tls": "ssl3",
            "from": "a@example.com",
            "to": ["b@example.com"],
        }),
    ), None)
    .expect_err("未知 tls 模式应被拒绝");
    assert!(error.to_string().contains("tls"), "{error}");
}

#[test]
fn non_notify_credential_cannot_map_to_channel() {
    let error = channel_config_from_credential(&credential("deploy.ssh", "主机", json!({})), None)
        .expect_err("非通知凭据应被拒绝");
    assert!(error.to_string().contains("deploy.ssh"), "{error}");
}

#[test]
fn proxy_mode_custom_rejected_when_proxy_url_missing_or_invalid() {
    for fields in [
        json!({
            "url": "https://open.feishu.cn/x",
            "proxy_mode": "custom",
            "proxy_url": "",
        }),
        json!({
            "url": "https://oapi.dingtalk.com/x",
            "proxy_mode": "custom",
            "proxy_url": "ftp://127.0.0.1:21",
        }),
        json!({
            "url": "https://ops.example.com/hook",
            "proxy_mode": "custom",
        }),
        json!({
            "host": "smtp.example.com",
            "from": "a@example.com",
            "to": ["b@example.com"],
            "proxy_mode": "custom",
            "proxy_url": "not-a-url",
        }),
    ] {
        let type_id = if fields.get("host").is_some() {
            EMAIL_TYPE_ID
        } else if fields.get("headers").is_some() || fields.get("body_template").is_some() || fields.get("method").is_some() || fields["url"].as_str().is_some_and(|u| u.contains("ops.example")) {
            GENERIC_TYPE_ID
        } else if fields["url"].as_str().is_some_and(|u| u.contains("dingtalk")) {
            DINGTALK_TYPE_ID
        } else {
            FEISHU_TYPE_ID
        };
        let credential_type: Box<dyn CredentialType> = match type_id {
            FEISHU_TYPE_ID => Box::new(FeishuCredentialType::default()),
            DINGTALK_TYPE_ID => Box::new(DingtalkCredentialType::default()),
            GENERIC_TYPE_ID => Box::new(GenericCredentialType::default()),
            _ => Box::new(EmailCredentialType::default()),
        };
        let error = credential_type
            .validate(&fields)
            .expect_err("proxy_mode=custom 时非法 proxy_url 应被拒绝");
        assert!(
            error.to_string().contains("proxy_url") || format!("{error:?}").contains("proxy_url"),
            "错误应定位到 proxy_url: {error:?} fields={fields}"
        );
    }
    // direct / inherit 放行（proxy_url 可空）
    FeishuCredentialType::default()
        .validate(&json!({
            "url": "https://open.feishu.cn/x",
            "proxy_mode": "direct",
        }))
        .expect("direct 放行");
    GenericCredentialType::default()
        .validate(&json!({
            "url": "https://ops.example.com/hook",
            "proxy_mode": "inherit",
        }))
        .expect("inherit 放行");
}

#[test]
fn channel_proxy_url_resolved_by_effective_proxy() {
    // direct → None
    let config = channel_config_from_credential(&credential(
        FEISHU_TYPE_ID,
        "群",
        json!({
            "url": "https://open.feishu.cn/x",
            "proxy_mode": "direct",
            "proxy_url": "socks5://127.0.0.1:1",
        }),
    ), Some("http://global:8080"))
    .expect("映射应成功");
    assert!(config.proxy_url.is_none(), "direct 忽略全局代理: {:?}", config.proxy_url);

    // inherit → 全局值
    let config = channel_config_from_credential(&credential(
        GENERIC_TYPE_ID,
        "自建",
        json!({ "url": "https://ops.example.com/hook", "proxy_mode": "inherit" }),
    ), Some("socks5://10.0.0.1:1080"))
    .expect("映射应成功");
    assert_eq!(
        config.proxy_url.as_deref(),
        Some("socks5://10.0.0.1:1080"),
        "inherit 应取全局代理"
    );

    // inherit 但全局为空 → None（直连）
    let config = channel_config_from_credential(&credential(
        FEISHU_TYPE_ID,
        "群",
        json!({ "url": "https://open.feishu.cn/x", "proxy_mode": "inherit" }),
    ), None)
    .expect("映射应成功");
    assert!(config.proxy_url.is_none());

    // custom → 凭据值
    let config = channel_config_from_credential(&credential(
        EMAIL_TYPE_ID,
        "邮件",
        json!({
            "host": "smtp.example.com",
            "from": "a@example.com",
            "to": ["b@example.com"],
            "proxy_mode": "custom",
            "proxy_url": "http://10.0.0.2:3128"
        }),
    ), Some("socks5://10.0.0.1:1080"))
    .expect("映射应成功");
    assert_eq!(
        config.proxy_url.as_deref(),
        Some("http://10.0.0.2:3128"),
        "custom 覆盖全局"
    );
    assert_eq!(
        config.smtp.as_ref().unwrap().proxy_url.as_deref(),
        Some("http://10.0.0.2:3128"),
        "SMTP 的 proxy_url 与渠道一致"
    );

    // 缺省 → None（直连）
    let config = channel_config_from_credential(&credential(
        DINGTALK_TYPE_ID,
        "钉钉群",
        json!({ "url": "https://oapi.dingtalk.com/x" }),
    ), Some("http://global:8080"))
    .expect("映射应成功");
    assert!(config.proxy_url.is_none(), "缺省视为 direct");
}

#[test]
fn notify_credential_schema_injects_proxy_annotations() {
    for (type_id, schema_fn) in [
        (FEISHU_TYPE_ID, acmecast_notify::notify_credential::feishu_fields_schema as fn() -> _),
        (DINGTALK_TYPE_ID, acmecast_notify::notify_credential::dingtalk_fields_schema as fn() -> _),
        (GENERIC_TYPE_ID, acmecast_notify::notify_credential::generic_fields_schema as fn() -> _),
        (EMAIL_TYPE_ID, acmecast_notify::notify_credential::email_fields_schema as fn() -> _),
    ] {
        let rendered = serde_json::to_string(&schema_fn()).unwrap();
        assert!(
            rendered.contains(r#""enum":["direct","inherit","custom"]"#),
            "{type_id} 应注入代理枚举: {rendered}"
        );
        let value: serde_json::Value = serde_json::from_str(&rendered).unwrap();
        assert!(
            value["properties"]["proxy_mode"]["x-enum-labels"]
                .to_string()
                .contains("自定义"),
            "{type_id} 代理下拉应含中文标签: {rendered}"
        );
        assert_eq!(
            value["properties"]["proxy_url"]["x-visible-when"],
            serde_json::json!({ "Equals": { "field": "proxy_mode", "values": ["custom"] } }),
            "{type_id} proxy_url 应联动 custom: {rendered}"
        );
    }
    // feishu 的 secret 是 proxy_mode 前驱——应带 x-end-row 使代理字段独占一行
    let feishu = serde_json::to_value(acmecast_notify::notify_credential::feishu_fields_schema()).unwrap();
    assert_eq!(
        feishu["properties"]["secret"]["x-end-row"],
        serde_json::json!(true),
        "feishu 前驱 secret 应带 x-end-row"
    );
}
