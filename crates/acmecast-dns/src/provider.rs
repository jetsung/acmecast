//! DNS 提供商抽象。
//!
//! 挑战流程只认「创建一条 TXT 记录、删掉它」这两件事，因此各家 DNS 服务的
//! 差异全部收敛在这个 Trait 背后。

use async_trait::async_trait;
use schemars::schema::RootSchema;
use serde_json::Value;

use crate::challenge::CHALLENGE_PREFIX;
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
    /// **写入名**：DNS 提供商实际创建记录时用的名字，必须属于 [`TxtRecord::zone`]。
    ///
    /// 同 zone 托管时它与 `ca_name` 相同；挑战记录经 CNAME 委派到另一个 zone
    /// 托管时（`_acme-challenge.a.example.com` → `b.example.net`），它按委派
    /// 目标 zone 计算为 `_acme-challenge.b.example.net`——**不是** CA 要查的那个名字。
    ///
    /// 四家 provider 都消费它：Cloudflare 与 EdgeOne 原样透传给厂商 API，
    /// 阿里云与腾讯云先经 [`TxtRecord::relative_name`] 转成相对主机记录。
    pub name: String,
    /// **CA 查询名**：CA 去查挑战记录时用的 FQDN，恒由授权域名派生，
    /// 不受 `zone` 影响——CA 是按订单标识符查找的，它不知道记录存在哪个 zone。
    ///
    /// 传播等待的解析器查询（[`crate::propagation`]）与面向用户的日志用它，
    /// 目的是让排查时看到的名字与 CA 侧现象对得上。跨 zone CNAME 委派时，
    /// 解析器需从这个名字出发跟随 CNAME 链，才能确认 CA 真的查得到。
    ///
    /// 与 [`TxtRecord::name`] 相同时表示无委派，两条判定线合一。
    pub ca_name: String,
    /// 记录内容。ACME 的 DNS-01 放的是密钥授权的摘要。
    pub value: String,
    /// TTL 秒数。
    pub ttl: u32,
}

impl TxtRecord {
    /// 建一条记录：写入名给定，CA 查询名由写入名反推。
    ///
    /// 仅适用于两个名字逐字相同的场景。CA 查询名与写入名不同时（无论是 CNAME
    /// 委派到别的 zone，还是同 zone 下的父级委派）一律用 [`TxtRecord::delegated`]
    /// 显式给出——两者都需要调用方明确表态，不该由构造器猜。
    ///
    /// 不对写入名与 zone 的关系做断言：字段是公开的，构造器断言拦不住直接构造，
    /// 只会打断「记录名越界」这类需要 [`TxtRecord::relative_name`] 报错的测试
    /// 路径。写入名是否属于 zone 由 `relative_name` 在运行时把关。
    #[must_use]
    pub fn new(
        zone: impl Into<String>,
        name: impl Into<String>,
        value: impl Into<String>,
        ttl: u32,
    ) -> Self {
        let name = name.into();
        Self {
            zone: zone.into(),
            ca_name: name.clone(),
            name,
            value: value.into(),
            ttl,
        }
    }

    /// 建一条记录，CA 查询名与写入名分别给定。
    ///
    /// 跨 zone 的 CNAME 委派：`_acme-challenge.skiy.net` CNAME 到
    /// `_acme-challenge.666517.xyz`，记录由 `666517.xyz` 托管——`name` 为后者、
    /// `ca_name` 为前者。
    ///
    /// 同 zone 的父级委派（如 zone 推导为 `example.com`、CA 查
    /// `_acme-challenge.a.example.com`）也走这里：两种情形都由
    /// [`TxtRecord::is_delegated`] 区分，不在构造期断言，故不必分两个构造器。
    pub fn delegated(
        zone: impl Into<String>,
        name: impl Into<String>,
        ca_name: impl Into<String>,
        value: impl Into<String>,
        ttl: u32,
    ) -> Self {
        Self {
            zone: zone.into(),
            name: name.into(),
            ca_name: ca_name.into(),
            value: value.into(),
            ttl,
        }
    }

    /// 是否处于 CNAME 委派场景（CA 查询名不在 [`TxtRecord::zone`] 之下）。
    ///
    /// 判定看的是「CA 查询名是否属于 zone」，而非两个名字是否逐字相同：
    /// `a.example.com` 的 zone 推导为 `example.com` 时，DNS 提供商写
    /// `_acme-challenge.example.com`、CA 查 `_acme-challenge.a.example.com`，
    /// 名字虽不同却同属一个 zone——那是普通委派（子域记录），不是 CNAME 委派。
    #[must_use]
    pub fn is_delegated(&self) -> bool {
        !self.ca_name.ends_with(&format!(".{}", self.zone))
    }

    /// CA 查询名所属的 zone，即 CNAME 委派的**委派源** zone。
    ///
    /// 权威 NS 直查的首跳必须从这里出发：委派场景下 CNAME 记录本身位于授权
    /// 域名一侧，问委派目标（[`TxtRecord::zone`]）的 NS 拿不到它，也就无从跟随。
    /// 非委派场景下它就是 `zone` 本身。
    #[must_use]
    pub fn ca_zone(&self) -> &str {
        if self.is_delegated() {
            // CA 查询名恒为 `_acme-challenge.<授权域名去通配前缀>`，故其所属 zone
            // 即该前缀之后的部分。解析不出时退回 `zone`——那是唯一已知的可用委派源。
            self.ca_name
                .strip_prefix(&format!("{CHALLENGE_PREFIX}."))
                .unwrap_or(&self.zone)
        } else {
            &self.zone
        }
    }

    /// 写入名相对于 zone 的部分：`_acme-challenge.example.com` + `example.com`
    /// 得到 `_acme-challenge`。阿里云这类要求分开给「主机记录」的 API 需要它。
    ///
    /// 作用在**写入名**上——委派场景下写入名本就属于 zone，校验依然成立。
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

    /// 探测 zone 是否托管在该提供商下，且凭据足以管理它。
    ///
    /// 自动选择 DNS 提供商时的候选筛选动作：`Ok(())` 即该候选可用。
    /// 「zone 未托管」与「凭据无效/无权限」不区分——统一 `Err`，
    /// 由调用方决定回退到下一个候选还是整体失败。
    ///
    /// 默认实现返回「不支持探测」：没有 zone 概念的提供商（如测试替身）
    /// 不必为此留桩。
    async fn probe_zone(&self, _credentials: &Value, _zone: &str) -> Result<()> {
        Err(Error::provider(format!(
            "提供商 `{}` 不支持 zone 探测",
            self.type_id()
        )))
    }
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

    #[test]
    fn a_plain_record_uses_one_name_for_both_roles() {
        let record = TxtRecord::new("example.com", "_acme-challenge.example.com", "v", 300);
        assert_eq!(record.ca_name, "_acme-challenge.example.com");
        assert!(!record.is_delegated());
        assert_eq!(
            record.relative_name().expect("写入名属于 zone"),
            "_acme-challenge"
        );
    }

    #[test]
    fn a_delegated_record_keeps_both_names_apart() {
        let record = TxtRecord::delegated(
            "666517.xyz",
            "_acme-challenge.666517.xyz",
            "_acme-challenge.skiy.net",
            "v",
            300,
        );
        assert_eq!(record.name, "_acme-challenge.666517.xyz");
        assert_eq!(record.ca_name, "_acme-challenge.skiy.net");
        assert!(record.is_delegated());
        // 写入名仍属于 zone，`relative_name` 在委派场景下照常成立。
        assert_eq!(
            record.relative_name().expect("写入名属于委派目标 zone"),
            "_acme-challenge"
        );
        // 权威首跳走委派源一侧：CNAME 记录本身在授权域名那里。
        assert_eq!(record.ca_zone(), "skiy.net");
    }

    #[test]
    fn a_parent_zone_record_is_delegated_but_not_cname_delegated() {
        // zone 推导为 example.com、CA 查 _acme-challenge.a.example.com：
        // 两个名字不同，但同属一个 zone，故不算 CNAME 委派，首跳仍用 zone。
        let record = TxtRecord::delegated(
            "example.com",
            "_acme-challenge.example.com",
            "_acme-challenge.a.example.com",
            "v",
            300,
        );
        assert!(!record.is_delegated());
        assert_eq!(record.ca_zone(), "example.com");
        assert_eq!(
            record.relative_name().expect("写入名属于 zone"),
            "_acme-challenge"
        );
    }

    #[test]
    fn a_name_outside_the_zone_has_no_relative_form() {
        // 构造器已拦住「写入名不属于 zone」，这里直接造结构体字段验证
        // `relative_name` 对越界写入名的兜底报错（release 下断言不生效时的防线）。
        let record = TxtRecord {
            zone: "666517.xyz".to_owned(),
            name: "_acme-challenge.skiy.net".to_owned(),
            ca_name: "_acme-challenge.skiy.net".to_owned(),
            value: "v".to_owned(),
            ttl: 300,
        };
        let error = record.relative_name().expect_err("写入名越界应报错");
        assert!(error.to_string().contains("不属于域名"));
    }
}
