//! 通知渠道凭据类型：渠道配置以凭据形式加密落库、UI 管理、连通性测试。
//!
//! 四种类型与四种适配器一一对应：`notify.feishu` / `notify.dingtalk` /
//! `notify.generic` / `notify.email`。凭据字段 JSON 到 [`ChannelConfig`] 的
//! 映射由 [`channel_config_from_credential`] 统一完成——事件投递与连通性
//! 测试共用同一条构造路径，字段语义只有一处定义。

use std::collections::BTreeMap;

use acmecast_access::{ConnectivityOutcome, CredentialType, ResolvedCredential};
use chrono::Utc;
use schemars::JsonSchema;
use schemars::schema::{RootSchema, Schema};
use schemars::schema_for;
use serde::{Deserialize, Serialize};

use crate::channel::{ChannelConfig, default_registry};
use crate::delivery::http_client_with_proxy;
use crate::email::{SmtpSettings, TlsMode};
use crate::error::{Error, Result};
use crate::generic::{ALLOWED_METHODS, validate_channel};
use crate::message::NotificationMessage;
use crate::sign::SignKind;

/// 飞书机器人凭据类型标识。
pub const FEISHU_TYPE_ID: &str = "notify.feishu";
/// 钉钉机器人凭据类型标识。
pub const DINGTALK_TYPE_ID: &str = "notify.dingtalk";
/// 通用 webhook 凭据类型标识。
pub const GENERIC_TYPE_ID: &str = "notify.generic";
/// 邮件凭据类型标识。
pub const EMAIL_TYPE_ID: &str = "notify.email";

/// 全部通知凭据类型标识；流水线 `notification_channels` 的校验与前端
/// 选项过滤都以这份清单为准。
pub const NOTIFY_TYPE_IDS: [&str; 4] = [
    FEISHU_TYPE_ID,
    DINGTALK_TYPE_ID,
    GENERIC_TYPE_ID,
    EMAIL_TYPE_ID,
];

/// 类型标识是否属于通知凭据类型。
#[must_use]
pub fn is_notify_type_id(type_id: &str) -> bool {
    NOTIFY_TYPE_IDS.contains(&type_id)
}

/// 飞书机器人凭据字段。
#[derive(Clone, Serialize, Deserialize, JsonSchema)]
pub struct FeishuFields {
    /// 机器人 webhook 地址。
    pub url: String,
    /// 签名密钥；机器人开启「签名校验」时填写（可选，不填即无签名）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secret: Option<String>,
    /// 出网代理模式：`direct`（缺省，直连）/ `inherit`（继承全局代理）/ `custom`（走下面 `proxy_url`，覆盖全局）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proxy_mode: Option<String>,
    /// 自定义代理 URL（`socks5://` / `http://` / `https://`）；仅 `proxy_mode=custom` 时生效。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proxy_url: Option<String>,
}

impl std::fmt::Debug for FeishuFields {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FeishuFields")
            .field("url", &self.url)
            .field("secret", &acmecast_core::redact_presence(&self.secret))
            .field("proxy_mode", &self.proxy_mode)
            .field("proxy_url", &self.proxy_url)
            .finish()
    }
}

/// 钉钉机器人凭据字段。
#[derive(Clone, Serialize, Deserialize, JsonSchema)]
pub struct DingtalkFields {
    /// 机器人 webhook 地址（含 `access_token`）。
    pub url: String,
    /// 加签密钥（`SEC` 开头）；机器人开启「加签」安全设置时填写（可选，不填即无签名）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secret: Option<String>,
    /// 出网代理模式：`direct`（缺省，直连）/ `inherit`（继承全局代理）/ `custom`（走下面 `proxy_url`，覆盖全局）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proxy_mode: Option<String>,
    /// 自定义代理 URL（`socks5://` / `http://` / `https://`）；仅 `proxy_mode=custom` 时生效。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proxy_url: Option<String>,
}

impl std::fmt::Debug for DingtalkFields {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DingtalkFields")
            .field("url", &self.url)
            .field("secret", &acmecast_core::redact_presence(&self.secret))
            .field("proxy_mode", &self.proxy_mode)
            .field("proxy_url", &self.proxy_url)
            .finish()
    }
}

/// 通用 webhook 凭据字段。
#[derive(Clone, Serialize, Deserialize, JsonSchema)]
pub struct GenericFields {
    /// 请求地址。
    pub url: String,
    /// 请求方法：`POST` / `PUT` / `PATCH`，缺省 `POST`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub method: Option<String>,
    /// 额外静态请求头（JSON 对象文本），如自建端点的 `X-API-Key`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub headers: Option<BTreeMap<String, String>>,
    /// 自定义请求体模板，占位符 `{{var}}` 填充事件素材（可选）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body_template: Option<String>,
    /// 出网代理模式：`direct`（缺省，直连）/ `inherit`（继承全局代理）/ `custom`（走下面 `proxy_url`，覆盖全局）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proxy_mode: Option<String>,
    /// 自定义代理 URL（`socks5://` / `http://` / `https://`）；仅 `proxy_mode=custom` 时生效。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proxy_url: Option<String>,
}

impl std::fmt::Debug for GenericFields {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GenericFields")
            .field("url", &self.url)
            .field("method", &self.method)
            .field("headers", &self.headers)
            .field("body_template", &self.body_template)
            .field("proxy_mode", &self.proxy_mode)
            .field("proxy_url", &self.proxy_url)
            .finish()
    }
}

/// 邮件凭据字段。
#[derive(Clone, Serialize, Deserialize, JsonSchema)]
pub struct EmailFields {
    /// SMTP 服务器地址（不含端口与协议前缀）。
    pub host: String,
    /// 端口；缺省随 `tls` 模式（`implicit`→465、`starttls`→587、`none`→25）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    /// TLS 连接模式：`implicit` / `starttls` / `none`，缺省 `implicit`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tls: Option<String>,
    /// 发件人地址。
    pub from: String,
    /// 收件人地址列表（每行一个）。
    pub to: Vec<String>,
    /// SMTP 认证用户名（可选）；与 `password` 同时填写才启用认证。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    /// SMTP 认证密码（可选）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub password: Option<String>,
    /// 出网代理模式：`direct`（缺省，直连）/ `inherit`（继承全局代理）/ `custom`（走下面 `proxy_url`，覆盖全局）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proxy_mode: Option<String>,
    /// 自定义代理 URL（`socks5://` / `http://` / `https://`）；仅 `proxy_mode=custom` 时生效。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proxy_url: Option<String>,
}

impl std::fmt::Debug for EmailFields {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EmailFields")
            .field("host", &self.host)
            .field("port", &self.port)
            .field("tls", &self.tls)
            .field("from", &self.from)
            .field("to", &self.to)
            .field("username", &self.username)
            .field("password", &acmecast_core::redact_presence(&self.password))
            .field("proxy_mode", &self.proxy_mode)
            .field("proxy_url", &self.proxy_url)
            .finish()
    }
}

/// 往字段节点插入一个扩展键。
///
/// `Option<T>` 字段的 schema 是 `anyOf: [T, null]` 外层节点；扩展键插在
/// 外层——前端解包可空字段时外层覆盖内层，插内层不会生效。
fn set_extension(schema: &mut RootSchema, field: &str, key: &str, value: serde_json::Value) {
    if let Some(Schema::Object(node)) = schema
        .schema
        .object
        .as_mut()
        .and_then(|object| object.properties.get_mut(field))
    {
        node.extensions.insert(key.to_owned(), value);
    }
}

/// 给字段注入枚举候选（前端渲染成下拉）。
fn set_enum(schema: &mut RootSchema, field: &str, values: &[&str]) {
    if let Some(Schema::Object(node)) = schema
        .schema
        .object
        .as_mut()
        .and_then(|object| object.properties.get_mut(field))
    {
        node.enum_values = Some(
            values
                .iter()
                .map(|value| serde_json::Value::String((*value).to_owned()))
                .collect(),
        );
    }
}

/// 飞书凭据字段 schema：`url` 独占整行，`secret` 左半独占一行。
#[must_use]
pub fn feishu_fields_schema() -> RootSchema {
    let mut schema = schema_for!(FeishuFields);
    schema.schema.extensions.insert(
        "x-field-order".to_owned(),
        serde_json::json!(["url", "secret", "proxy_mode", "proxy_url"]),
    );
    set_extension(
        &mut schema,
        "url",
        "x-full-width",
        serde_json::Value::Bool(true),
    );
    set_extension(
        &mut schema,
        "secret",
        "x-end-row",
        serde_json::Value::Bool(true),
    );
    acmecast_core::inject_proxy_field_annotations(&mut schema);
    schema
}

/// 钉钉凭据字段 schema：与飞书同构。
#[must_use]
pub fn dingtalk_fields_schema() -> RootSchema {
    let mut schema = schema_for!(DingtalkFields);
    schema.schema.extensions.insert(
        "x-field-order".to_owned(),
        serde_json::json!(["url", "secret", "proxy_mode", "proxy_url"]),
    );
    set_extension(
        &mut schema,
        "url",
        "x-full-width",
        serde_json::Value::Bool(true),
    );
    set_extension(
        &mut schema,
        "secret",
        "x-end-row",
        serde_json::Value::Bool(true),
    );
    acmecast_core::inject_proxy_field_annotations(&mut schema);
    schema
}

/// 通用 webhook 凭据字段 schema：`url` 整行、`method` 左半独占、`headers/body_template` 整行多行。
#[must_use]
pub fn generic_fields_schema() -> RootSchema {
    let mut schema = schema_for!(GenericFields);
    schema.schema.extensions.insert(
        "x-field-order".to_owned(),
        serde_json::json!(["url", "method", "headers", "body_template", "proxy_mode", "proxy_url"]),
    );
    set_enum(&mut schema, "method", &ALLOWED_METHODS);
    set_extension(
        &mut schema,
        "url",
        "x-full-width",
        serde_json::Value::Bool(true),
    );
    set_extension(
        &mut schema,
        "method",
        "x-end-row",
        serde_json::Value::Bool(true),
    );
    set_extension(
        &mut schema,
        "headers",
        "x-multiline",
        serde_json::Value::Bool(true),
    );
    set_extension(
        &mut schema,
        "body_template",
        "x-multiline",
        serde_json::Value::Bool(true),
    );
    acmecast_core::inject_proxy_field_annotations(&mut schema);
    schema
}

/// 邮件凭据字段 schema：`host|port`、`from|tls`、`to` 整行，`username|password` 同行。
#[must_use]
pub fn email_fields_schema() -> RootSchema {
    let mut schema = schema_for!(EmailFields);
    schema.schema.extensions.insert(
        "x-field-order".to_owned(),
        serde_json::json!([
            "host", "port", "from", "tls", "to", "username", "password", "proxy_mode",
            "proxy_url"
        ]),
    );
    set_enum(&mut schema, "tls", &["implicit", "starttls", "none"]);
    set_extension(
        &mut schema,
        "to",
        "x-multiline",
        serde_json::Value::Bool(true),
    );
    set_extension(
        &mut schema,
        "to",
        "x-full-width",
        serde_json::Value::Bool(true),
    );
    acmecast_core::inject_proxy_field_annotations(&mut schema);
    schema
}

/// 飞书机器人凭据类型。
#[derive(Debug, Default)]
pub struct FeishuCredentialType {
    /// 全局出网代理（`[proxy] url` / `ACMECAST_PROXY_URL`）；供
    /// `proxy_mode=inherit` 的凭据在连通性测试里继承。
    global_proxy_url: Option<String>,
}

impl FeishuCredentialType {
    /// 设置全局出网代理，供 `proxy_mode=inherit` 的凭据继承。
    #[must_use]
    pub fn with_global_proxy(mut self, proxy_url: Option<impl Into<String>>) -> Self {
        self.global_proxy_url = proxy_url.map(Into::into);
        self
    }
}

#[async_trait::async_trait]
impl CredentialType for FeishuCredentialType {
    fn type_id(&self) -> &'static str {
        FEISHU_TYPE_ID
    }

    fn display_name(&self) -> &'static str {
        "飞书机器人"
    }

    fn fields_schema(&self) -> RootSchema {
        feishu_fields_schema()
    }

    fn validate(&self, fields: &serde_json::Value) -> acmecast_core::Result<()> {
        let parsed: FeishuFields = serde_json::from_value(fields.clone())
            .map_err(|error| acmecast_core::Error::validation("fields", error.to_string()))?;
        validate_bot_fields(&parsed.url)?;
        acmecast_core::validate_proxy_field(fields)
    }

    async fn test_connectivity(&self, fields: &serde_json::Value) -> ConnectivityOutcome {
        let parsed: std::result::Result<FeishuFields, _> = serde_json::from_value(fields.clone());
        let Ok(parsed) = parsed else {
            return ConnectivityOutcome::Unavailable {
                reason: "字段不合法".to_owned(),
            };
        };
        // 外层凭据记录的展示名（ResolvedCredential.name）才是渠道标识，
        // 字段内已无 name；连通性测试取 url 作占位名即可（仅用于 ChannelConfig.name 日志）。
        probe(
            self,
            FEISHU_TYPE_ID,
            parsed.url.trim(),
            fields,
            self.global_proxy_url.as_deref(),
        )
        .await
    }
}

/// 钉钉机器人凭据类型。
#[derive(Debug, Default)]
pub struct DingtalkCredentialType {
    /// 全局出网代理（`[proxy] url` / `ACMECAST_PROXY_URL`）；供
    /// `proxy_mode=inherit` 的凭据在连通性测试里继承。
    global_proxy_url: Option<String>,
}

impl DingtalkCredentialType {
    /// 设置全局出网代理，供 `proxy_mode=inherit` 的凭据继承。
    #[must_use]
    pub fn with_global_proxy(mut self, proxy_url: Option<impl Into<String>>) -> Self {
        self.global_proxy_url = proxy_url.map(Into::into);
        self
    }
}

#[async_trait::async_trait]
impl CredentialType for DingtalkCredentialType {
    fn type_id(&self) -> &'static str {
        DINGTALK_TYPE_ID
    }

    fn display_name(&self) -> &'static str {
        "钉钉机器人"
    }

    fn fields_schema(&self) -> RootSchema {
        dingtalk_fields_schema()
    }

    fn validate(&self, fields: &serde_json::Value) -> acmecast_core::Result<()> {
        let parsed: DingtalkFields = serde_json::from_value(fields.clone())
            .map_err(|error| acmecast_core::Error::validation("fields", error.to_string()))?;
        validate_bot_fields(&parsed.url)?;
        acmecast_core::validate_proxy_field(fields)
    }

    async fn test_connectivity(&self, fields: &serde_json::Value) -> ConnectivityOutcome {
        let parsed: std::result::Result<DingtalkFields, _> = serde_json::from_value(fields.clone());
        let Ok(parsed) = parsed else {
            return ConnectivityOutcome::Unavailable {
                reason: "字段不合法".to_owned(),
            };
        };
        probe(
            self,
            DINGTALK_TYPE_ID,
            parsed.url.trim(),
            fields,
            self.global_proxy_url.as_deref(),
        )
        .await
    }
}

/// 通用 webhook 凭据类型。
#[derive(Debug, Default)]
pub struct GenericCredentialType {
    /// 全局出网代理（`[proxy] url` / `ACMECAST_PROXY_URL`）；供
    /// `proxy_mode=inherit` 的凭据在连通性测试里继承。
    global_proxy_url: Option<String>,
}

impl GenericCredentialType {
    /// 设置全局出网代理，供 `proxy_mode=inherit` 的凭据继承。
    #[must_use]
    pub fn with_global_proxy(mut self, proxy_url: Option<impl Into<String>>) -> Self {
        self.global_proxy_url = proxy_url.map(Into::into);
        self
    }
}

#[async_trait::async_trait]
impl CredentialType for GenericCredentialType {
    fn type_id(&self) -> &'static str {
        GENERIC_TYPE_ID
    }

    fn display_name(&self) -> &'static str {
        "通用 Webhook"
    }

    fn fields_schema(&self) -> RootSchema {
        generic_fields_schema()
    }

    fn validate(&self, fields: &serde_json::Value) -> acmecast_core::Result<()> {
        let parsed: GenericFields = serde_json::from_value(fields.clone())
            .map_err(|error| acmecast_core::Error::validation("fields", error.to_string()))?;
        validate_http_url(&parsed.url)?;
        if let Some(method) = normalized(&parsed.method) {
            let normalized = method.to_ascii_uppercase();
            if !ALLOWED_METHODS.contains(&normalized.as_str()) {
                return Err(acmecast_core::Error::validation(
                    "method",
                    format!("仅支持 {}", ALLOWED_METHODS.join(" / ")),
                ));
            }
        }
        // 适配器侧的规则（请求头合法性、模板占位符）在保存时同样拦截：
        // 配置错误不带进投递路径。错误文案里定位字段，与邮件类型同法。
        let config = ChannelConfig {
            name: String::new(),
            provider: "generic".to_owned(),
            url: parsed.url.trim().to_owned(),
            secret: None,
            sign: None,
            events: Vec::new(),
            enabled: true,
            method: normalized(&parsed.method)
                .map(|method| method.to_ascii_uppercase())
                .unwrap_or_else(|| "POST".to_owned()),
            headers: parsed.headers.unwrap_or_default(),
            body_template: normalized(&parsed.body_template),
            proxy_url: acmecast_core::effective_proxy_url(fields, None),
            smtp: None,
        };
        validate_channel(&config).map_err(|reason| {
            let field = if reason.contains("请求体模板") {
                "body_template"
            } else {
                ["headers", "method"]
                    .iter()
                    .find(|field| reason.contains(**field))
                    .copied()
                    .unwrap_or("fields")
            };
            acmecast_core::Error::validation(field, reason)
        })?;
        acmecast_core::validate_proxy_field(fields)
    }

    async fn test_connectivity(&self, fields: &serde_json::Value) -> ConnectivityOutcome {
        let parsed: std::result::Result<GenericFields, _> = serde_json::from_value(fields.clone());
        let Ok(parsed) = parsed else {
            return ConnectivityOutcome::Unavailable {
                reason: "字段不合法".to_owned(),
            };
        };
        probe(
            self,
            GENERIC_TYPE_ID,
            parsed.url.trim(),
            fields,
            self.global_proxy_url.as_deref(),
        )
        .await
    }
}

/// 邮件凭据类型。
#[derive(Debug, Default)]
pub struct EmailCredentialType {
    /// 全局出网代理（`[proxy] url` / `ACMECAST_PROXY_URL`）；供
    /// `proxy_mode=inherit` 的凭据在连通性测试里继承。
    global_proxy_url: Option<String>,
}

impl EmailCredentialType {
    /// 设置全局出网代理，供 `proxy_mode=inherit` 的凭据继承。
    #[must_use]
    pub fn with_global_proxy(mut self, proxy_url: Option<impl Into<String>>) -> Self {
        self.global_proxy_url = proxy_url.map(Into::into);
        self
    }
}

#[async_trait::async_trait]
impl CredentialType for EmailCredentialType {
    fn type_id(&self) -> &'static str {
        EMAIL_TYPE_ID
    }

    fn display_name(&self) -> &'static str {
        "邮件"
    }

    fn fields_schema(&self) -> RootSchema {
        email_fields_schema()
    }

    fn validate(&self, fields: &serde_json::Value) -> acmecast_core::Result<()> {
        let parsed: EmailFields = serde_json::from_value(fields.clone())
            .map_err(|error| acmecast_core::Error::validation("fields", error.to_string()))?;
        let settings = smtp_settings_from(parsed, acmecast_core::effective_proxy_url(fields, None))
            .map_err(|reason| acmecast_core::Error::validation("tls", reason))?;
        settings.validate().map_err(|reason| {
            // SmtpSettings 的错误文案以字段名开头（host/port/from/to/username），
            // 据此把错误定位到表单字段。
            let field = ["host", "port", "from", "to", "username"]
                .iter()
                .find(|field| reason.contains(**field))
                .copied()
                .unwrap_or("fields");
            acmecast_core::Error::validation(field, reason)
        })?;
        acmecast_core::validate_proxy_field(fields)
    }

    async fn test_connectivity(&self, fields: &serde_json::Value) -> ConnectivityOutcome {
        let parsed: std::result::Result<EmailFields, _> = serde_json::from_value(fields.clone());
        let Ok(parsed) = parsed else {
            return ConnectivityOutcome::Unavailable {
                reason: "字段不合法".to_owned(),
            };
        };
        probe(
            self,
            EMAIL_TYPE_ID,
            parsed.host.trim(),
            fields,
            self.global_proxy_url.as_deref(),
        )
        .await
    }
}

/// 从已解密凭据构造渠道配置（D5 映射）。
///
/// `name` 取凭据记录的展示名；`provider` 由 `type_id` 去掉 `notify.` 前缀
/// 得出；`events` 不再参与过滤（恒空数组），`enabled` 恒真——渠道是否接收
/// 通知由流水线的 `notification_channels` 选择决定。
///
/// `global_proxy_url` 是全局出网代理（`[proxy] url` / `ACMECAST_PROXY_URL`）：
/// 供 `proxy_mode=inherit` 的凭据继承。
pub fn channel_config_from_credential(
    credential: &ResolvedCredential,
    global_proxy_url: Option<&str>,
) -> Result<ChannelConfig> {
    channel_config_from_parts(
        &credential.type_id,
        &credential.name,
        &credential.fields,
        global_proxy_url,
    )
}

/// 从已解密凭据构造渠道配置（兼容旧签名，无代理真空）。
///
/// 已全局装配 `global_proxy_url` 的路径应直接调用
/// [`channel_config_from_credential`] 或四参版 [`channel_config_from_parts`]。
/// 存量测试等无代理场景使用此入口——`inherit` 视为直连。
pub fn channel_config_from_credential_no_proxy(
    credential: &ResolvedCredential,
) -> Result<ChannelConfig> {
    channel_config_from_parts(&credential.type_id, &credential.name, &credential.fields, None)
}

/// 字段 JSON → 渠道配置的映射本体。
fn channel_config_from_parts(
    type_id: &str,
    name: &str,
    fields: &serde_json::Value,
    global_proxy_url: Option<&str>,
) -> Result<ChannelConfig> {
    match type_id {
        FEISHU_TYPE_ID => {
            let parsed = parse_fields::<FeishuFields>(type_id, fields)?;
            let secret = normalized(&parsed.secret);
            Ok(ChannelConfig {
                name: name.to_owned(),
                provider: "feishu".to_owned(),
                url: parsed.url.trim().to_owned(),
                sign: secret.as_ref().map(|_| SignKind::Feishu),
                secret,
                events: Vec::new(),
                enabled: true,
                method: "POST".to_owned(),
                headers: BTreeMap::new(),
                body_template: None,
                proxy_url: acmecast_core::effective_proxy_url(fields, global_proxy_url),
                smtp: None,
            })
        }
        DINGTALK_TYPE_ID => {
            let parsed = parse_fields::<DingtalkFields>(type_id, fields)?;
            let secret = normalized(&parsed.secret);
            Ok(ChannelConfig {
                name: name.to_owned(),
                provider: "dingtalk".to_owned(),
                url: parsed.url.trim().to_owned(),
                sign: secret.as_ref().map(|_| SignKind::DingTalk),
                secret,
                events: Vec::new(),
                enabled: true,
                method: "POST".to_owned(),
                headers: BTreeMap::new(),
                body_template: None,
                proxy_url: acmecast_core::effective_proxy_url(fields, global_proxy_url),
                smtp: None,
            })
        }
        GENERIC_TYPE_ID => {
            let parsed = parse_fields::<GenericFields>(type_id, fields)?;
            let method = normalized(&parsed.method)
                .map(|method| method.to_ascii_uppercase())
                .unwrap_or_else(|| "POST".to_owned());
            Ok(ChannelConfig {
                name: name.to_owned(),
                provider: "generic".to_owned(),
                url: parsed.url.trim().to_owned(),
                secret: None,
                sign: None,
                events: Vec::new(),
                enabled: true,
                method,
                headers: parsed.headers.unwrap_or_default(),
                body_template: normalized(&parsed.body_template),
                proxy_url: acmecast_core::effective_proxy_url(fields, global_proxy_url),
                smtp: None,
            })
        }
        EMAIL_TYPE_ID => {
            let parsed = parse_fields::<EmailFields>(type_id, fields)?;
            let mut smtp =
                smtp_settings_from(parsed, acmecast_core::effective_proxy_url(fields, global_proxy_url))
                    .map_err(|reason| Error::InvalidConfig {
                        reason: format!("{EMAIL_TYPE_ID} {reason}"),
                    })?;
            // 存量邮件凭据缺 proxy_mode 时 effective_proxy_url 为 None（直连），
            // 与本次变更前行为一致。
            smtp.proxy_url = smtp.proxy_url.clone();
            Ok(ChannelConfig {
                name: name.to_owned(),
                provider: "email".to_owned(),
                url: String::new(),
                secret: None,
                sign: None,
                events: Vec::new(),
                enabled: true,
                method: "POST".to_owned(),
                headers: BTreeMap::new(),
                body_template: None,
                proxy_url: smtp.proxy_url.clone(),
                smtp: Some(smtp),
            })
        }
        other => Err(Error::InvalidConfig {
            reason: format!("不是通知凭据类型: {other}"),
        }),
    }
}

/// 解析凭据字段到具体结构体。
fn parse_fields<T>(type_id: &str, fields: &serde_json::Value) -> Result<T>
where
    T: serde::de::DeserializeOwned,
{
    serde_json::from_value(fields.clone()).map_err(|error| Error::InvalidConfig {
        reason: format!("{type_id} 字段不合法: {error}"),
    })
}

/// 去空白；空串归一为 `None`。
fn normalized(value: &Option<String>) -> Option<String> {
    value
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_owned)
}

/// 机器人凭据的公共校验：地址为 http(s)。
fn validate_bot_fields(url: &str) -> acmecast_core::Result<()> {
    validate_http_url(url)
}

/// http(s) 地址的形态校验：只查 scheme 前缀与非空 host，不做完整 URL 解析。
fn validate_http_url(url: &str) -> acmecast_core::Result<()> {
    let url = url.trim();
    if url.is_empty() {
        return Err(acmecast_core::Error::validation("url", "不能为空"));
    }
    if !(url.starts_with("https://") || url.starts_with("http://")) {
        return Err(acmecast_core::Error::validation(
            "url",
            "必须为 http(s) 地址",
        ));
    }
    Ok(())
}

/// 邮件字段 → SMTP 连接设置；`tls` 取值非法时返回原因。
fn smtp_settings_from(
    parsed: EmailFields,
    proxy_url: Option<String>,
) -> std::result::Result<SmtpSettings, String> {
    let tls = match normalized(&parsed.tls) {
        None => TlsMode::Implicit,
        Some(id) => TlsMode::from_id(&id)
            .ok_or_else(|| format!("未知 tls 模式: {id}（可用：implicit、starttls、none）"))?,
    };
    Ok(SmtpSettings {
        host: parsed.host.trim().to_owned(),
        port: parsed.port,
        from: parsed.from.trim().to_owned(),
        to: parsed
            .to
            .iter()
            .map(|addr| addr.trim().to_owned())
            .filter(|addr| !addr.is_empty())
            .collect(),
        username: normalized(&parsed.username),
        password: parsed.password.clone(),
        tls,
        proxy_url,
    })
}

/// 连通性测试的公共路径：字段 → 渠道配置 → 适配器发送一条测试消息。
///
/// 只发一次、不走投递重试——连通性测试要的是「现在通不通」的即时答案，
/// 退避重试只会拖慢交互响应。失败原因经 `redact` 脱敏后返回。
async fn probe(
    credential_type: &dyn CredentialType,
    type_id: &str,
    name: &str,
    fields: &serde_json::Value,
    global_proxy_url: Option<&str>,
) -> ConnectivityOutcome {
    let raw = fields.clone();
    let config = match channel_config_from_parts(type_id, name, fields, global_proxy_url) {
        Ok(config) => config,
        Err(error) => {
            return ConnectivityOutcome::Unavailable {
                reason: error.to_string(),
            };
        }
    };
    let Some(adapter) = default_registry().get(&config.provider) else {
        return ConnectivityOutcome::Unavailable {
            reason: format!("未知渠道类型: {}", config.provider),
        };
    };
    let proxy_url = config.proxy_url.clone();
    let client = match http_client_with_proxy(proxy_url.as_deref()) {
        Ok(client) => client,
        Err(error) => {
            let reason = with_proxy_host_hint(&proxy_url, &error.to_string());
            return ConnectivityOutcome::Unavailable {
                reason: credential_type.redact(&raw, &reason),
            };
        }
    };
    match adapter.send(&client, &config, &test_message()).await {
        Ok(()) => ConnectivityOutcome::Ok,
        Err(error) => {
            let reason = with_proxy_host_hint(&proxy_url, &error.to_string());
            ConnectivityOutcome::Unavailable {
                reason: credential_type.redact(&raw, &reason),
            }
        }
    }
}

/// 代理失败时在原因末尾补代理主机提示（若错误文本尚未包含代理地址）。
fn with_proxy_host_hint(proxy_url: &Option<String>, cause: &str) -> String {
    let Some(url) = proxy_url.as_deref() else {
        return cause.to_owned();
    };
    if cause.contains(&acmecast_core::proxy_log_target(url)) {
        return cause.to_owned();
    }
    format!("{cause}（经代理 {}）", acmecast_core::proxy_log_target(url))
}

/// 连通性测试用的消息素材（与凭据测试入口的文案一致）。
fn test_message() -> NotificationMessage {
    NotificationMessage {
        event: "test".to_owned(),
        title: "通知渠道测试".to_owned(),
        pipeline: "acmecast 服务".to_owned(),
        trigger: "manual".to_owned(),
        occurred_at: Utc::now(),
        domains: Vec::new(),
        failure: None,
        summary: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feishu_fields() -> serde_json::Value {
        serde_json::json!({
            "url": "https://open.feishu.cn/open-apis/bot/v2/hook/xxxx",
            "secret": "s3cret-sign-key",
        })
    }

    #[test]
    fn feishu_validate_requires_http_url() {
        FeishuCredentialType::default()
            .validate(&feishu_fields())
            .expect("合法字段应通过");

        let bad_url = serde_json::json!({ "url": "ftp://a.example.com" });
        let error = FeishuCredentialType::default().validate(&bad_url).unwrap_err();
        assert!(
            matches!(&error, acmecast_core::Error::Validation { field, .. } if field == "url"),
            "{error:?}"
        );

        let empty_url = serde_json::json!({ "url": "" });
        assert!(FeishuCredentialType::default().validate(&empty_url).is_err());
    }

    #[test]
    fn feishu_secret_is_optional() {
        let fields = serde_json::json!({
            "url": "https://open.feishu.cn/open-apis/bot/v2/hook/xxxx",
        });
        FeishuCredentialType::default()
            .validate(&fields)
            .expect("缺 secret 应通过");
    }

    #[test]
    fn dingtalk_validate_mirrors_feishu() {
        let fields = serde_json::json!({
            "url": "https://oapi.dingtalk.com/robot/send?access_token=x",
            "secret": "SECxxxx",
        });
        DingtalkCredentialType::default()
            .validate(&fields)
            .expect("合法字段应通过");
        let missing_url = serde_json::json!({ "url": "" });
        assert!(DingtalkCredentialType::default().validate(&missing_url).is_err());
    }

    #[test]
    fn generic_validate_checks_method_whitelist() {
        let valid = serde_json::json!({
            "url": "https://ops.example.com/hook",
            "method": "put",
            "headers": { "X-API-Key": "sk-xxx" },
            "body_template": "{\"text\": \"{{title}}\"}",
        });
        GenericCredentialType::default()
            .validate(&valid)
            .expect("合法字段应通过");

        let bad_method = serde_json::json!({
            "url": "https://ops.example.com/hook",
            "method": "GET",
        });
        let error = GenericCredentialType::default().validate(&bad_method).unwrap_err();
        assert!(
            matches!(&error, acmecast_core::Error::Validation { field, .. } if field == "method"),
            "{error:?}"
        );
    }

    #[test]
    fn generic_validate_rejects_illegal_headers_and_template() {
        // 请求头名含控制字符：保存时即拦截，不带进投递路径。
        let bad_header = serde_json::json!({
            "url": "https://ops.example.com/hook",
            "headers": { "bad header\nname": "v" },
        });
        let error = GenericCredentialType::default().validate(&bad_header).unwrap_err();
        assert!(
            matches!(&error, acmecast_core::Error::Validation { field, .. } if field == "headers"),
            "{error:?}"
        );

        // 模板含未知占位符：同样在保存时拒绝。
        let bad_template = serde_json::json!({
            "url": "https://ops.example.com/hook",
            "body_template": "{\"x\": \"{{nope}}\"}",
        });
        let error = GenericCredentialType::default().validate(&bad_template).unwrap_err();
        assert!(
            matches!(&error, acmecast_core::Error::Validation { field, .. } if field == "body_template"),
            "{error:?}"
        );
    }

    #[test]
    fn email_validate_requires_recipients_and_addresses() {
        let valid = serde_json::json!({
            "host": "smtp.exmail.qq.com",
            "tls": "starttls",
            "from": "no-reply@example.com",
            "to": ["ops@example.com"],
        });
        EmailCredentialType::default()
            .validate(&valid)
            .expect("合法字段应通过");

        let no_recipients = serde_json::json!({
            "host": "smtp.exmail.qq.com",
            "from": "no-reply@example.com",
            "to": [],
        });
        let error = EmailCredentialType::default().validate(&no_recipients).unwrap_err();
        assert!(
            matches!(&error, acmecast_core::Error::Validation { field, .. } if field == "to"),
            "{error:?}"
        );

        let bad_from = serde_json::json!({
            "host": "smtp.exmail.qq.com",
            "from": "not-an-address",
            "to": ["ops@example.com"],
        });
        assert!(EmailCredentialType::default().validate(&bad_from).is_err());
    }

    #[test]
    fn email_validate_rejects_unknown_tls_mode() {
        let fields = serde_json::json!({
            "host": "smtp.exmail.qq.com",
            "tls": "ssl3",
            "from": "no-reply@example.com",
            "to": ["ops@example.com"],
        });
        let error = EmailCredentialType::default().validate(&fields).unwrap_err();
        assert!(
            matches!(&error, acmecast_core::Error::Validation { field, .. } if field == "tls"),
            "{error:?}"
        );
    }

    #[test]
    fn schemas_pin_field_order_and_extensions() {
        // feishu/dingtalk: url 整行，secret 左半独占
        let rendered = serde_json::to_string(&feishu_fields_schema()).unwrap();
        assert!(
            rendered.contains(r#""x-field-order":["url","secret","proxy_mode","proxy_url"]"#),
            "{rendered}"
        );
        let value: serde_json::Value = serde_json::from_str(&rendered).unwrap();
        assert_eq!(value["properties"]["url"]["x-full-width"], serde_json::json!(true));
        assert_eq!(value["properties"]["secret"]["x-end-row"], serde_json::json!(true));
        assert_eq!(
            value["properties"]["proxy_mode"]["enum"],
            serde_json::json!(["direct", "inherit", "custom"])
        );
        assert!(
            value["properties"]["proxy_mode"]["x-enum-labels"]
                .to_string()
                .contains("自定义"),
            "应含中文标签: {rendered}"
        );
        assert_eq!(
            value["properties"]["proxy_url"]["x-visible-when"],
            serde_json::json!({ "Equals": { "field": "proxy_mode", "values": ["custom"] } })
        );

        // dingtalk 同构
        let rendered = serde_json::to_string(&dingtalk_fields_schema()).unwrap();
        assert!(
            rendered.contains(r#""x-field-order":["url","secret","proxy_mode","proxy_url"]"#),
            "{rendered}"
        );

        // generic: url 整行，method 左半独占，headers/body 整行多行
        let rendered = serde_json::to_string(&generic_fields_schema()).unwrap();
        assert!(
            rendered.contains(
                r#""x-field-order":["url","method","headers","body_template","proxy_mode","proxy_url"]"#
            ),
            "{rendered}"
        );
        assert!(
            rendered.contains(r#""enum":["POST","PUT","PATCH"]"#),
            "method 应注入枚举候选: {rendered}"
        );
        let value: serde_json::Value = serde_json::from_str(&rendered).unwrap();
        assert_eq!(value["properties"]["url"]["x-full-width"], serde_json::json!(true));
        assert_eq!(value["properties"]["method"]["x-end-row"], serde_json::json!(true));
        assert_eq!(
            value["properties"]["headers"]["x-multiline"],
            serde_json::json!(true)
        );
        assert_eq!(
            value["properties"]["body_template"]["x-multiline"],
            serde_json::json!(true)
        );

        // email: host|port / from|tls / to 整行 / username|password
        let rendered = serde_json::to_string(&email_fields_schema()).unwrap();
        assert!(
            rendered.contains(r#""x-field-order":["host","port","from","tls","to","username","password","proxy_mode","proxy_url"]"#),
            "{rendered}"
        );
        assert!(
            rendered.contains(r#""enum":["implicit","starttls","none"]"#),
            "tls 应注入枚举候选: {rendered}"
        );
        let value: serde_json::Value = serde_json::from_str(&rendered).unwrap();
        assert_eq!(value["properties"]["to"]["x-multiline"], serde_json::json!(true));
        assert_eq!(value["properties"]["to"]["x-full-width"], serde_json::json!(true));
        // name 已移除，不应再有 x-end-row 在 name 上
        assert!(
            value["properties"].get("name").is_none(),
            "email 已移除 name 字段: {rendered}"
        );
    }

    #[test]
    fn debug_impls_redact_secrets() {
        let fields = FeishuFields {
            url: "https://a.example.com".to_owned(),
            secret: Some("super-secret".to_owned()),
            proxy_mode: None,
            proxy_url: None,
        };
        let rendered = format!("{fields:?}");
        assert!(!rendered.contains("super-secret"), "{rendered}");

        let email = EmailFields {
            host: "smtp.example.com".to_owned(),
            port: None,
            tls: None,
            from: "a@example.com".to_owned(),
            to: vec!["b@example.com".to_owned()],
            username: None,
            password: Some("mail-password".to_owned()),
            proxy_mode: None,
            proxy_url: None,
        };
        let rendered = format!("{email:?}");
        assert!(!rendered.contains("mail-password"), "{rendered}");
    }
}
