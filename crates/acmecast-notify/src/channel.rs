//! 渠道适配器契约与注册表。
//!
//! 适配器只回答「这个平台的消息长什么样、怎么签名」，把统一事件负载
//! [`NotificationMessage`] 翻译成平台请求；超时与重试由 [`crate::delivery`]
//! 统一处理，适配器不自己兜。

use std::collections::BTreeMap;
use std::sync::Arc;

use serde_json::{Value, json};

use crate::error::{Error, Result};
use crate::message::NotificationMessage;
use crate::sign::{SignKind, percent_encode};

/// 一个通知渠道的运行时配置。
///
/// 由服务端配置解析而来；`enabled = false` 的渠道保留在列表里但不投递。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelConfig {
    /// 渠道名，用于日志与测试端点定位。
    pub name: String,
    /// 适配器类型标识（`feishu` / `dingtalk` / `generic`）。
    pub provider: String,
    /// 机器人 webhook 地址。
    pub url: String,
    /// 签名密钥；平台支持签名校验时启用。
    pub secret: Option<String>,
    /// 订阅的事件标识列表。
    pub events: Vec<String>,
    /// 是否启用。
    pub enabled: bool,
    /// 签名方式（写死在代码中的方案，见 [`SignKind`]）；`None` 表示不签名。
    pub sign: Option<SignKind>,
    /// 请求方法：`POST` / `PUT` / `PATCH`，缺省 `POST`。
    pub method: String,
    /// 额外静态请求头（如自建端点的 `X-API-Key`）。
    pub headers: BTreeMap<String, String>,
    /// 自定义请求体模板；`None` 时按适配器缺省格式构造。
    pub body_template: Option<String>,
}

/// 渠道适配器：把统一消息翻译成某个平台的 webhook 请求。
#[async_trait::async_trait]
pub trait NotificationChannel: Send + Sync + std::fmt::Debug {
    /// 适配器类型标识，与渠道配置的 `provider` 对应。
    fn provider(&self) -> &'static str;

    /// 发送一条消息。
    ///
    /// 只做一次请求尝试，不重试——重试语义在投递执行器里统一实现。
    async fn send(
        &self,
        client: &reqwest::Client,
        channel: &ChannelConfig,
        message: &NotificationMessage,
    ) -> Result<()>;
}

/// 渠道适配器注册表。
///
/// 与 DNS 提供商、部署目标的注册表同型：启动时注册内置实现，配置里的
/// `provider` 字符串在这里查到适配器。
#[derive(Debug, Default)]
pub struct ChannelRegistry {
    channels: BTreeMap<String, Arc<dyn NotificationChannel>>,
}

impl ChannelRegistry {
    /// 空注册表。
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// 注册一个适配器；类型重复时报错。
    pub fn register(&mut self, channel: impl NotificationChannel + 'static) -> Result<()> {
        let provider = channel.provider().to_owned();
        if self.channels.contains_key(&provider) {
            return Err(Error::DuplicateProvider { provider });
        }
        self.channels.insert(provider, Arc::new(channel));
        Ok(())
    }

    /// 按类型标识查适配器。
    #[must_use]
    pub fn get(&self, provider: &str) -> Option<Arc<dyn NotificationChannel>> {
        self.channels.get(provider).cloned()
    }

    /// 已注册的适配器类型列表。
    #[must_use]
    pub fn type_ids(&self) -> Vec<String> {
        self.channels.keys().cloned().collect()
    }
}

/// 注册全部内置适配器的注册表。
#[must_use]
pub fn default_registry() -> ChannelRegistry {
    let mut registry = ChannelRegistry::new();
    registry
        .register(crate::feishu::FeishuChannel)
        .expect("内置适配器不应重复注册");
    registry
        .register(crate::dingtalk::DingTalkChannel)
        .expect("内置适配器不应重复注册");
    registry
        .register(crate::generic::GenericChannel)
        .expect("内置适配器不应重复注册");
    registry
}

/// 一次投递的已计算签名。
///
/// 签名值在渲染与附加之间可能被使用两处（模板变量与自动追加），
/// 只算一次，避免两处时间戳跨秒不一致。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Signature {
    kind: SignKind,
    timestamp: i64,
    value: String,
}

impl Signature {
    /// 按方案以当前时刻计算签名。
    pub(crate) fn compute(kind: SignKind, secret: &str) -> Self {
        let (timestamp, value) = kind.sign(secret);
        Self {
            kind,
            timestamp,
            value,
        }
    }

    /// 签名值，供模板变量填充。
    pub(crate) fn value(&self) -> &str {
        &self.value
    }

    /// 时间戳，供模板变量填充。
    pub(crate) fn timestamp(&self) -> i64 {
        self.timestamp
    }
}

/// 把签名附加到请求上：飞书合并进请求体顶层字段，钉钉追加到地址查询参数。
///
/// 请求体已含 `sign` 字段（模板里显式写了 `{{sign}}`）时飞书路径不再
/// 覆盖——模板里那份签名就是唯一签名。
pub(crate) fn apply_sign(
    url: &str,
    mut body: Value,
    signature: Option<&Signature>,
) -> (String, Value) {
    let Some(signature) = signature else {
        return (url.to_owned(), body);
    };
    match signature.kind {
        SignKind::Feishu => {
            if body.is_object() && body.get("sign").is_none() {
                body["timestamp"] = json!(signature.timestamp);
                body["sign"] = json!(signature.value);
            }
            (url.to_owned(), body)
        }
        SignKind::DingTalk => {
            let separator = if url.contains('?') { '&' } else { '?' };
            let url = format!(
                "{url}{separator}timestamp={}&sign={}",
                signature.timestamp,
                percent_encode(&signature.value)
            );
            (url, body)
        }
    }
}

/// 统一请求出口：方法 + 自定义请求头 + JSON 请求体，要求 2xx 响应。
///
/// 三种适配器共用一条发送路径，签名附加（[`apply_sign`]]）在构造请求前
/// 完成。失败时把状态码与响应开头一段带回错误信息——IM 平台的错误说明
/// （如钉钉的「sign不匹配」）就藏在响应体里，排障时第一个要看的就是它。
pub(crate) async fn send_request(
    client: &reqwest::Client,
    url: &str,
    method: &str,
    headers: &BTreeMap<String, String>,
    body: &Value,
) -> Result<()> {
    let method =
        reqwest::Method::from_bytes(method.as_bytes()).map_err(|error| Error::Delivery {
            name: String::new(),
            cause: format!("非法请求方法 {method:?}: {error}"),
        })?;
    let mut request = client.request(method, url).json(body);
    for (name, value) in headers {
        request = request.header(name, value);
    }
    let response = request.send().await.map_err(|error| Error::Delivery {
        name: String::new(),
        cause: format!("请求失败: {error}"),
    })?;
    let status = response.status();
    if !status.is_success() {
        let snippet = response.text().await.unwrap_or_default();
        let snippet = snippet.chars().take(200).collect::<String>();
        return Err(Error::Delivery {
            name: String::new(),
            cause: format!("HTTP {status}: {snippet}"),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug)]
    struct Dummy;

    #[async_trait::async_trait]
    impl NotificationChannel for Dummy {
        fn provider(&self) -> &'static str {
            "dummy"
        }

        async fn send(
            &self,
            _client: &reqwest::Client,
            _channel: &ChannelConfig,
            _message: &NotificationMessage,
        ) -> Result<()> {
            Ok(())
        }
    }

    #[test]
    fn register_and_lookup() {
        let mut registry = ChannelRegistry::new();
        registry.register(Dummy).expect("首次注册应成功");
        assert!(registry.get("dummy").is_some());
        assert!(registry.get("missing").is_none());
        assert_eq!(registry.type_ids(), vec!["dummy".to_owned()]);
    }

    #[test]
    fn duplicate_provider_rejected() {
        let mut registry = ChannelRegistry::new();
        registry.register(Dummy).expect("首次注册应成功");
        let error = registry.register(Dummy).unwrap_err();
        assert!(error.to_string().contains("dummy"), "{error}");
    }

    #[test]
    fn default_registry_has_builtin_providers() {
        let registry = default_registry();
        for provider in ["feishu", "dingtalk", "generic"] {
            assert!(
                registry.get(provider).is_some(),
                "缺少内置适配器 {provider}"
            );
        }
    }

    #[test]
    fn feishu_sign_merges_into_object_body_once() {
        let signature = Signature {
            kind: SignKind::Feishu,
            timestamp: 1_700_000_000,
            value: "sig==".to_owned(),
        };
        let body = json!({"msg_type": "text"});
        let (url, body) = apply_sign("https://hook", body, Some(&signature));
        assert_eq!(url, "https://hook");
        assert_eq!(body["timestamp"], 1_700_000_000);
        assert_eq!(body["sign"], "sig==");

        // 模板已自带 sign 字段时不覆盖。
        let body = json!({"sign": "from-template"});
        let (_, body) = apply_sign("https://hook", body, Some(&signature));
        assert_eq!(body["sign"], "from-template");
        assert!(body.get("timestamp").is_none());
    }

    #[test]
    fn dingtalk_sign_appends_to_query() {
        let signature = Signature {
            kind: SignKind::DingTalk,
            timestamp: 1_700_000_000_000,
            value: "a+z/9=".to_owned(),
        };
        let (url, body) = apply_sign("https://hook", json!({}), Some(&signature));
        assert_eq!(
            url,
            "https://hook?timestamp=1700000000000&sign=a%2Bz%2F9%3D"
        );
        assert!(body.get("sign").is_none(), "钉钉签名只进 URL");
    }

    #[test]
    fn dingtalk_sign_reuses_existing_query_separator() {
        let signature = Signature {
            kind: SignKind::DingTalk,
            timestamp: 1,
            value: "s".to_owned(),
        };
        let (url, _) = apply_sign("https://hook?a=1", json!({}), Some(&signature));
        assert!(url.contains("&timestamp=1&sign=s"), "{url}");
    }
}
