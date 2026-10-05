//! DNS 提供商抽象。
//!
//! 挑战流程只认「创建一条 TXT 记录、删掉它」这两件事，因此各家 DNS 服务的
//! 差异全部收敛在这个 Trait 背后。

use async_trait::async_trait;
use schemars::schema::RootSchema;
use serde_json::Value;

use crate::error::{Error, Result};

/// 一条 TXT 记录。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TxtRecord {
    /// 记录所属的域名（zone），如 `example.com`。
    ///
    /// 厂商 API 都要它：Cloudflare 用它换 zone_id，阿里云用它当 DomainName。
    /// 刻意**不从记录名推导**——公共后缀（`example.co.uk`、`example.com.cn`）
    /// 会让「取后两段」这类推导出错，而错的结果是记录写到别的域名下去。
    /// 谁是 zone 只有调用方知道，就由调用方给。
    pub zone: String,
    /// 完整的记录名，如 `_acme-challenge.example.com`。
    pub name: String,
    /// 记录内容。ACME 的 DNS-01 放的是密钥授权的摘要。
    pub value: String,
    /// TTL 秒数。
    pub ttl: u32,
}

impl TxtRecord {
    /// 建一条记录。
    #[must_use]
    pub fn new(
        zone: impl Into<String>,
        name: impl Into<String>,
        value: impl Into<String>,
        ttl: u32,
    ) -> Self {
        Self {
            zone: zone.into(),
            name: name.into(),
            value: value.into(),
            ttl,
        }
    }

    /// 记录名相对于 zone 的部分：`_acme-challenge.example.com` + `example.com`
    /// 得到 `_acme-challenge`。阿里云这类要求分开给「主机记录」的 API 需要它。
    pub fn relative_name(&self) -> Result<String> {
        self.name
            .strip_suffix(&format!(".{}", self.zone))
            .map(str::to_owned)
            .ok_or_else(|| {
                Error::provider(format!("记录名 `{}` 不属于域名 `{}`", self.name, self.zone))
            })
    }
}

/// 一家 DNS 服务。
///
/// 实现者只管「怎么在某家厂商的 API 上增删记录」，不关心 ACME 流程、
/// 也不接触凭据的存储与解密——拿到的 [`Value`] 已经是解密后的字段值。
#[async_trait]
pub trait DnsProvider: Send + Sync + std::fmt::Debug + 'static {
    /// 类型标识，如 `cloudflare`。在同一个注册表内必须唯一。
    fn type_id(&self) -> &'static str;

    /// 展示名称。
    fn display_name(&self) -> &'static str;

    /// 本提供商需要的凭据字段定义，供前端渲染表单。
    ///
    /// 与流水线步骤的输入定义同理：用 `schemars` 从字段结构体推导，
    /// 实现者再在方法内把 `Value` 反序列化成同一个结构体完成校验。
    fn credential_fields(&self) -> RootSchema;

    /// 列出这条记录名下的现有 TXT 记录值。
    ///
    /// 挑战流程靠它避免「同一个名字下堆了两条记录」：CA 校验时可能读到
    /// 旧的那条，表现出来只是「校验值不匹配」，看不出是为什么。
    ///
    /// 入参是整条记录（而非只有名字）是因为多数厂商要先按 zone 定位：
    /// Cloudflare 得先换 zone_id 才能查记录。
    async fn find_txt(&self, credentials: &Value, record: &TxtRecord) -> Result<Vec<String>>;

    /// 创建一条 TXT 记录。
    ///
    /// `credentials` 是**解密后的**凭据字段值。
    async fn create_txt(&self, credentials: &Value, record: &TxtRecord) -> Result<()>;

    /// 删除一条 TXT 记录。
    ///
    /// 应当**幂等**：记录本就不存在时也算成功。清理路径上为了「它本来就不在」
    /// 而报错，只会让失败现场更乱——而那时真正要紧的是原始错误。
    async fn delete_txt(&self, credentials: &Value, record: &TxtRecord) -> Result<()>;
}

/// 把凭据字段反序列化成提供商自己的结构体。
///
/// 做成自由函数而非 trait 方法：带泛型参数的方法会让 [`DnsProvider`] 失去
/// dyn 兼容性，而注册表恰恰需要 `Box<dyn DnsProvider>`。
///
/// 失败时描述里会带上 serde 的原始信息——它形如
/// ``missing field `api_token` ``，字段名就在其中。
///
/// 字符串字段在解析前统一去除首尾空白：凭据往往从剪贴板粘贴进来，
/// 拖一个换行或空格进去，各家 API 的表现是「认证失败」而非「格式错误」
/// （Cloudflare 回 Invalid request headers，阿里云回 InvalidAccessKeyId），
/// 很难定位到是凭据值脏了。
pub fn parse_credentials<T>(credentials: &Value) -> Result<T>
where
    T: serde::de::DeserializeOwned,
{
    let cleaned = trim_strings(credentials);
    serde_json::from_value(cleaned).map_err(|e| {
        Error::invalid_credentials("(凭据字段)", format!("内容不符合本提供商的凭据定义: {e}"))
    })
}

/// 递归去掉 JSON 里所有字符串的首尾空白。
fn trim_strings(value: &Value) -> Value {
    match value {
        Value::String(text) => Value::String(text.trim().to_owned()),
        Value::Array(items) => Value::Array(items.iter().map(trim_strings).collect()),
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(key, value)| (key.clone(), trim_strings(value)))
                .collect(),
        ),
        other => other.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;
    use serde_json::json;

    #[derive(Debug, Deserialize, PartialEq)]
    struct FakeCredentials {
        api_token: String,
    }

    #[test]
    fn string_fields_are_trimmed_on_parse() {
        let credentials: FakeCredentials =
            parse_credentials(&json!({ "api_token": "  tok\n" })).expect("应解析成功");
        assert_eq!(credentials.api_token, "tok");
    }

    #[test]
    fn non_string_fields_are_untouched() {
        let cleaned = trim_strings(&json!({ "id": 3, "flag": true, "n": null }));
        assert_eq!(cleaned, json!({ "id": 3, "flag": true, "n": null }));
    }

    #[test]
    fn nested_strings_are_trimmed() {
        let cleaned = trim_strings(&json!({ "a": [" x "], "b": { "c": " y\n" } }));
        assert_eq!(cleaned, json!({ "a": ["x"], "b": { "c": "y" } }));
    }
}
