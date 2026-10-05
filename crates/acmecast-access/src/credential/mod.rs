//! 凭据类型定义。

pub mod acme_account;

use schemars::schema::RootSchema;

/// 连通性测试的结果。
///
/// 「不可用」与「无从测试」分开：前者是凭据本身有问题，
/// 后者只是这个类型没有对外的探测动作，不该被当成故障。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnectivityOutcome {
    /// 凭据可用：已用一次轻量调用验证过。
    Ok,
    /// 凭据不可用，附**已脱敏**的原因，可直接呈现给用户。
    Unavailable {
        /// 已脱敏的失败原因。
        reason: String,
    },
    /// 该类型的凭据没有外部服务可供探测。
    NotTestable,
}

impl ConnectivityOutcome {
    /// 凭据是否可用。
    #[must_use]
    pub fn is_ok(&self) -> bool {
        matches!(self, Self::Ok)
    }

    /// 失败原因；仅 [`Self::Unavailable`] 才有。
    #[must_use]
    pub fn reason(&self) -> Option<&str> {
        match self {
            Self::Unavailable { reason } => Some(reason),
            _ => None,
        }
    }
}

/// 一种凭据类型。
///
/// 实现者只描述「这类凭据长什么样」——类型标识、展示名称、字段定义与校验规则。
/// 凭据实例的**存储与加解密**不在这里：前者归持久层，后者归
/// [`acmecast_core::CredentialCipher`]。
///
/// 字段值以 JSON 为载体（库里存的就是它的密文），因此实现者把 JSON 反序列化到自己的
/// 结构体即可完成校验，不必手写逐字段检查。
#[async_trait::async_trait]
pub trait CredentialType: Send + Sync + std::fmt::Debug + 'static {
    /// 类型标识，如 `acme.account`。在同一个注册表内必须唯一。
    fn type_id(&self) -> &'static str;

    /// 展示名称，供前端下拉框使用。
    fn display_name(&self) -> &'static str;

    /// 字段定义的 JSON Schema，供前端据此渲染表单。
    ///
    /// 用 `schemars` 从字段结构体直接推导，避免「结构体改了、Schema 没改」的漂移。
    fn fields_schema(&self) -> RootSchema;

    /// 校验一份字段值是否可用于本类型。
    ///
    /// 失败时应当返回带字段名的 [`acmecast_core::Error::Validation`]，
    /// 这样前端能直接高亮出错的那个输入框。
    fn validate(&self, fields: &serde_json::Value) -> acmecast_core::Result<()>;

    /// 用一份已解密的凭据对外部服务发起一次**轻量**调用，判定它是否可用。
    ///
    /// 失败原因必须先用 [`Self::redact`] 处理再放进 [`ConnectivityOutcome::Unavailable`]——
    /// 外部服务的报错常常原样回显请求里携带的密钥，直接透出就等于把凭据打印给用户。
    ///
    /// 默认返回 [`ConnectivityOutcome::NotTestable`]：没有对外探测动作的类型
    /// 应当如实说「无从测试」，而不是谎报可用。
    async fn test_connectivity(&self, _fields: &serde_json::Value) -> ConnectivityOutcome {
        ConnectivityOutcome::NotTestable
    }

    /// 把文本中与字段值相同的片段抹掉，生成可安全呈现给用户的说明。
    ///
    /// 递归检查字段里的每个字符串值：只要原样出现在 `message` 中，就替换为 `***`。
    /// 这样即便外部服务回显了密钥，也不会穿过这一层。
    fn redact(&self, fields: &serde_json::Value, message: &str) -> String {
        let mut values = Vec::new();
        collect_strings(fields, &mut values);

        let mut redacted = message.to_owned();
        for secret in values {
            // 太短的值不做替换：否则会把正常文本里的常见词一并抹掉。
            if secret.len() >= 4 && redacted.contains(&secret) {
                redacted = redacted.replace(&secret, "***");
            }
        }
        redacted
    }
}

/// 递归收集 JSON 里的全部字符串值。
///
/// 只取字符串：数字与布尔在文本里太常见，把 `1` 或 `true` 当秘密去抹会毁掉整条消息。
fn collect_strings(value: &serde_json::Value, out: &mut Vec<String>) {
    match value {
        serde_json::Value::String(text) => {
            out.push(text.clone());
            // 字段值本身可能是一段 JSON（账号凭据就是），里面的每个字符串同样是秘密。
            // 只看外层的话，私钥被服务端回显时就漏过去了。
            if let Ok(nested) = serde_json::from_str::<serde_json::Value>(text) {
                collect_strings(&nested, out);
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                collect_strings(item, out);
            }
        }
        serde_json::Value::Object(map) => {
            for item in map.values() {
                collect_strings(item, out);
            }
        }
        serde_json::Value::Null | serde_json::Value::Bool(_) | serde_json::Value::Number(_) => {}
    }
}

#[cfg(test)]
mod tests {
    use schemars::schema::RootSchema;

    use super::*;

    /// 没有对外探测动作的类型：默认实现应当如实说「无从测试」。
    #[derive(Debug)]
    struct UntestableType;

    impl CredentialType for UntestableType {
        fn type_id(&self) -> &'static str {
            "fake.untestable"
        }

        fn display_name(&self) -> &'static str {
            "无从测试的类型"
        }

        fn fields_schema(&self) -> RootSchema {
            RootSchema::default()
        }

        fn validate(&self, _fields: &serde_json::Value) -> acmecast_core::Result<()> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn the_default_says_it_cannot_be_tested() {
        let outcome = UntestableType
            .test_connectivity(&serde_json::json!({}))
            .await;

        assert!(matches!(outcome, ConnectivityOutcome::NotTestable));
        assert!(!outcome.is_ok(), "无从测试不该被当成可用");
        assert!(outcome.reason().is_none());
    }

    #[test]
    fn redact_removes_the_secret_from_a_failure_message() {
        // 外部服务常把请求里的密钥原样回显在报错里。
        let fields = serde_json::json!({ "api_token": "sk-live-abcdef123456" });
        let message = "调用失败：认证被拒（token=sk-live-abcdef123456）";

        let redacted = UntestableType.redact(&fields, message);
        assert!(!redacted.contains("sk-live-abcdef123456"), "{redacted}");
        assert!(redacted.contains("***"), "{redacted}");
        // 有用的上下文应当保留下来。
        assert!(redacted.contains("认证被拒"), "{redacted}");
    }

    #[test]
    fn redact_leaves_short_values_alone() {
        // 太短的字段值不做替换，否则会把正常文本里的常见词一并抹掉。
        let fields = serde_json::json!({ "region": "cn" });
        let message = "区域 cn 当前不可用";

        assert_eq!(UntestableType.redact(&fields, message), message);
    }

    #[test]
    fn redact_walks_nested_values() {
        let fields = serde_json::json!({
            "outer": { "inner": "secret-value-1" },
            "list": ["another-secret-2"],
        });
        let message = "outer=secret-value-1 list=another-secret-2";

        let redacted = UntestableType.redact(&fields, message);
        assert!(!redacted.contains("secret-value-1"), "{redacted}");
        assert!(!redacted.contains("another-secret-2"), "{redacted}");
    }

    #[test]
    fn redact_looks_inside_nested_json_field_values() {
        // 字段值本身可能是一段 JSON（账号凭据就是），里面的字符串同样是秘密。
        let secret = "MHcCAQEEIPxWD-IqQz576pxTGEYV21gJAGBcOw";
        let inner = serde_json::json!({ "key_pkcs8": secret });
        let fields = serde_json::json!({ "credentials": inner.to_string() });

        let redacted = UntestableType.redact(&fields, &format!("签名失败：密钥 {secret} 不被接受"));
        assert!(!redacted.contains(secret), "{redacted}");
        assert!(redacted.contains("签名失败"), "{redacted}");
    }

    #[test]
    fn non_string_values_are_not_treated_as_secrets() {
        // 把数字或布尔当秘密去抹，会把「第 1 步」「成功=false」这类正常文本毁掉。
        let fields = serde_json::json!({ "port": 1, "enabled": true });
        let message = "第 1 步失败，成功=false";

        assert_eq!(UntestableType.redact(&fields, message), message);
    }

    #[test]
    fn outcome_helpers_reflect_the_variant() {
        assert!(ConnectivityOutcome::Ok.is_ok());
        assert!(ConnectivityOutcome::Ok.reason().is_none());

        let unavailable = ConnectivityOutcome::Unavailable {
            reason: "额度受限".to_owned(),
        };
        assert!(!unavailable.is_ok());
        assert_eq!(unavailable.reason(), Some("额度受限"));

        assert!(!ConnectivityOutcome::NotTestable.is_ok());
        assert!(ConnectivityOutcome::NotTestable.reason().is_none());
    }
}
