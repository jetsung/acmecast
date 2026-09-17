//! DNS 提供商注册表。

use std::collections::HashMap;

use crate::error::{Error, Result};
use crate::provider::DnsProvider;

/// 已注册的 DNS 提供商集合。
///
/// 与前两处注册表（凭据类型、流水线步骤）同构，刻意保持一致：显式注册、
/// 重复拒绝、查找失败时带上已知清单。**没有**把它们抽成泛型容器——三处的
/// 值类型与错误语义各不相同，抽出来只会得到一个薄包装，省下的行数不足以
/// 抵消那层间接。
#[derive(Debug, Default)]
pub struct DnsProviderRegistry {
    providers: HashMap<&'static str, Box<dyn DnsProvider>>,
}

impl DnsProviderRegistry {
    /// 建一个空注册表。
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// 注册一家提供商。
    ///
    /// 类型标识重复时返回 [`Error::DuplicateProviderType`]：重复注册只可能是
    /// 编程错误，静默覆盖会让「注册了两个实现、只有一个生效」一直藏着。
    pub fn register(&mut self, provider: impl DnsProvider) -> Result<()> {
        let type_id = provider.type_id();
        if self.providers.contains_key(type_id) {
            return Err(Error::DuplicateProviderType(type_id.to_owned()));
        }
        self.providers.insert(type_id, Box::new(provider));
        Ok(())
    }

    /// 按类型标识查找；未注册时返回 `None`。
    #[must_use]
    pub fn get(&self, type_id: &str) -> Option<&dyn DnsProvider> {
        self.providers.get(type_id).map(|boxed| boxed.as_ref())
    }

    /// 按类型标识查找；未注册时报错。
    ///
    /// 挑战任务解析提供商的入口——spec 要求「指定未登记的提供商类型时
    /// 任务失败并返回未知提供商类型的错误」。
    pub fn require(&self, type_id: &str) -> Result<&dyn DnsProvider> {
        self.get(type_id).ok_or_else(|| Error::UnknownProviderType {
            type_id: type_id.to_owned(),
            known: self.type_ids(),
        })
    }

    /// 全部已注册的类型标识，升序。
    #[must_use]
    pub fn type_ids(&self) -> Vec<String> {
        let mut ids: Vec<String> = self.providers.keys().map(|id| (*id).to_owned()).collect();
        ids.sort();
        ids
    }

    /// 全部已注册的提供商，按标识升序。
    #[must_use]
    pub fn list(&self) -> Vec<&dyn DnsProvider> {
        let mut entries: Vec<_> = self.providers.iter().collect();
        entries.sort_by_key(|(type_id, _)| *type_id);
        entries
            .into_iter()
            .map(|(_, provider)| provider.as_ref())
            .collect()
    }

    /// 已注册提供商的数量。
    #[must_use]
    pub fn len(&self) -> usize {
        self.providers.len()
    }

    /// 是否还没有任何已注册提供商。
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.providers.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use schemars::schema::RootSchema;
    use schemars::schema_for;
    use serde::Deserialize;
    use serde_json::{Value, json};

    use super::*;
    use crate::provider::TxtRecord;

    /// 测试用提供商的凭据字段。
    #[derive(Debug, Deserialize, schemars::JsonSchema)]
    // 字段只供 schemars 推导定义，代码里不逐个读取。
    #[allow(dead_code)]
    struct FakeCredentials {
        api_token: String,
    }

    /// 一个把调用记下来的假提供商。
    #[derive(Debug, Default)]
    struct FakeProvider {
        calls: Mutex<Vec<String>>,
    }

    impl FakeProvider {
        fn calls(&self) -> Vec<String> {
            self.calls.lock().expect("锁不应中毒").clone()
        }
    }

    #[async_trait::async_trait]
    impl DnsProvider for FakeProvider {
        fn type_id(&self) -> &'static str {
            "fake.dns"
        }

        fn display_name(&self) -> &'static str {
            "假 DNS 服务"
        }

        fn credential_fields(&self) -> RootSchema {
            schema_for!(FakeCredentials)
        }

        async fn find_txt(&self, _credentials: &Value, _record: &TxtRecord) -> Result<Vec<String>> {
            Ok(Vec::new())
        }

        async fn create_txt(&self, credentials: &Value, record: &TxtRecord) -> Result<()> {
            // 凭据不合法时应报出字段名，而不是笼统的「失败了」。
            let _: FakeCredentials = crate::provider::parse_credentials(credentials)?;
            self.calls
                .lock()
                .expect("锁不应中毒")
                .push(format!("create {}", record.name));
            Ok(())
        }

        async fn delete_txt(&self, credentials: &Value, record: &TxtRecord) -> Result<()> {
            let _: FakeCredentials = crate::provider::parse_credentials(credentials)?;
            self.calls
                .lock()
                .expect("锁不应中毒")
                .push(format!("delete {}", record.name));
            Ok(())
        }
    }

    /// 另一个提供商，用于验证列表排序。
    #[derive(Debug)]
    struct AnotherProvider;

    #[async_trait::async_trait]
    impl DnsProvider for AnotherProvider {
        fn type_id(&self) -> &'static str {
            "fake.another"
        }

        fn display_name(&self) -> &'static str {
            "另一个假 DNS 服务"
        }

        fn credential_fields(&self) -> RootSchema {
            schema_for!(FakeCredentials)
        }

        async fn find_txt(&self, _credentials: &Value, _record: &TxtRecord) -> Result<Vec<String>> {
            Ok(Vec::new())
        }

        async fn create_txt(&self, _credentials: &Value, _record: &TxtRecord) -> Result<()> {
            Ok(())
        }

        async fn delete_txt(&self, _credentials: &Value, _record: &TxtRecord) -> Result<()> {
            Ok(())
        }
    }

    #[test]
    fn a_registered_provider_can_be_looked_up() {
        let mut registry = DnsProviderRegistry::new();
        registry
            .register(FakeProvider::default())
            .expect("首次注册应成功");

        let found = registry.get("fake.dns").expect("应能查到已注册的提供商");
        assert_eq!(found.type_id(), "fake.dns");
        assert_eq!(found.display_name(), "假 DNS 服务");
    }

    #[test]
    fn an_unregistered_provider_is_not_found() {
        let registry = DnsProviderRegistry::new();
        assert!(registry.get("cloudflare").is_none());
    }

    #[test]
    fn requiring_an_unregistered_provider_fails_with_the_known_list() {
        let mut registry = DnsProviderRegistry::new();
        registry.register(FakeProvider::default()).unwrap();
        registry.register(AnotherProvider).unwrap();

        let err = registry
            .require("no-such-dns")
            .expect_err("未注册的类型应被拒绝");

        match &err {
            Error::UnknownProviderType { type_id, known } => {
                assert_eq!(type_id, "no-such-dns");
                assert_eq!(
                    known,
                    &vec!["fake.another".to_owned(), "fake.dns".to_owned()]
                );
            }
            other => panic!("期望 UnknownProviderType，实际 {other:?}"),
        }

        let text = err.to_string();
        assert!(text.contains("no-such-dns"), "{text}");
        assert!(text.contains("fake.dns"), "应列出可用的提供商: {text}");
    }

    #[test]
    fn duplicate_registration_is_rejected() {
        let mut registry = DnsProviderRegistry::new();
        registry.register(FakeProvider::default()).unwrap();

        let err = registry
            .register(FakeProvider::default())
            .expect_err("重复注册应被拒绝");
        assert!(matches!(err, Error::DuplicateProviderType(ref id) if id == "fake.dns"));
        assert_eq!(registry.len(), 1, "被拒绝的注册不应改变注册表");
    }

    #[test]
    fn listing_is_sorted_by_type_id() {
        let mut registry = DnsProviderRegistry::new();
        registry.register(FakeProvider::default()).unwrap();
        registry.register(AnotherProvider).unwrap();

        assert_eq!(
            registry.type_ids(),
            vec!["fake.another".to_owned(), "fake.dns".to_owned()]
        );
        let listed: Vec<&str> = registry
            .list()
            .iter()
            .map(|provider| provider.type_id())
            .collect();
        assert_eq!(listed, vec!["fake.another", "fake.dns"]);
    }

    #[test]
    fn an_empty_registry_is_empty() {
        let registry = DnsProviderRegistry::new();
        assert!(registry.is_empty());
        assert_eq!(registry.len(), 0);
        assert!(registry.list().is_empty());
    }

    // ---- Trait 本身可用 ----

    #[tokio::test]
    async fn a_provider_creates_and_deletes_records() {
        let mut registry = DnsProviderRegistry::new();
        registry.register(FakeProvider::default()).unwrap();
        let provider = registry.require("fake.dns").unwrap();

        let credentials = json!({ "api_token": "token-abc" });
        let record = TxtRecord::new(
            "example.com",
            "_acme-challenge.example.com",
            "digest-value",
            60,
        );

        provider
            .create_txt(&credentials, &record)
            .await
            .expect("应能创建记录");
        provider
            .delete_txt(&credentials, &record)
            .await
            .expect("应能删除记录");

        // 记录名与内容原样传给了实现。
        assert_eq!(record.name, "_acme-challenge.example.com");
        assert_eq!(record.value, "digest-value");
    }

    #[tokio::test]
    async fn bad_credentials_name_the_offending_field() {
        let provider = FakeProvider::default();
        let record = TxtRecord::new("example.com", "_acme-challenge.example.com", "v", 60);

        // 缺 api_token。
        let err = provider
            .create_txt(&json!({}), &record)
            .await
            .expect_err("凭据不合法应被拒绝");
        assert!(matches!(&err, Error::InvalidCredentials { .. }), "{err:?}");
        // serde 的原始描述形如 `missing field \`api_token\``，字段名就在其中。
        assert!(
            err.to_string().contains("api_token"),
            "错误里应指明缺哪个字段: {err}"
        );

        // 没走到真正调用那一步。
        assert!(provider.calls().is_empty());
    }

    #[test]
    fn the_credential_definition_is_exported() {
        let rendered = serde_json::to_string(&FakeProvider::default().credential_fields()).unwrap();
        assert!(rendered.contains("api_token"), "{rendered}");
        assert!(rendered.contains("required"), "{rendered}");
    }
}
