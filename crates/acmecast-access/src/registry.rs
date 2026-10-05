//! 凭据类型注册表。

use std::collections::HashMap;

use crate::credential::CredentialType;
use crate::error::{Error, Result};

/// 已注册凭据类型的集合。
///
/// 注册是**显式**的：启动时由 `acmecast-server` 调用一次性把所有实现装进来。
/// 不用 `inventory` / `linkme` 那类链接器魔法——它依赖链接器 section 布局，
/// 在部分平台与 WASM 上不可靠，也会让「到底注册了什么」变得无法 grep。
/// 多写几行换启动链路完全可读。
#[derive(Debug, Default)]
pub struct CredentialRegistry {
    types: HashMap<&'static str, Box<dyn CredentialType>>,
}

impl CredentialRegistry {
    /// 建一个空注册表。
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// 注册一种凭据类型。
    ///
    /// 类型标识重复时返回 [`Error::DuplicateType`]：重复注册只可能是编程错误，
    /// 静默覆盖会让「注册了两个实现、只有一个生效」一直藏着。
    pub fn register(&mut self, credential_type: impl CredentialType) -> Result<()> {
        let type_id = credential_type.type_id();
        if self.types.contains_key(type_id) {
            return Err(Error::DuplicateType(type_id.to_owned()));
        }
        self.types.insert(type_id, Box::new(credential_type));
        Ok(())
    }

    /// 按类型标识查找；未注册时返回 `None`。
    #[must_use]
    pub fn get(&self, type_id: &str) -> Option<&dyn CredentialType> {
        self.types.get(type_id).map(|boxed| boxed.as_ref())
    }

    /// 按类型标识查找；未注册时报错。
    ///
    /// 这是创建凭据时的入口——spec 要求「指定未知类型时拒绝创建并返回未知凭据类型错误」。
    pub fn require(&self, type_id: &str) -> Result<&dyn CredentialType> {
        self.get(type_id).ok_or_else(|| Error::UnknownType {
            requested: type_id.to_owned(),
            known: self.type_ids(),
        })
    }

    /// 全部已注册的类型标识，升序。
    #[must_use]
    pub fn type_ids(&self) -> Vec<String> {
        let mut ids: Vec<String> = self.types.keys().map(|id| (*id).to_owned()).collect();
        ids.sort();
        ids
    }

    /// 全部已注册的类型，按标识升序。
    #[must_use]
    pub fn list(&self) -> Vec<&dyn CredentialType> {
        let mut entries: Vec<_> = self.types.iter().collect();
        entries.sort_by_key(|(type_id, _)| *type_id);
        entries
            .into_iter()
            .map(|(_, credential_type)| credential_type.as_ref())
            .collect()
    }

    /// 已注册类型的数量。
    #[must_use]
    pub fn len(&self) -> usize {
        self.types.len()
    }

    /// 是否没有任何已注册类型。
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.types.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use schemars::JsonSchema;
    use schemars::schema::RootSchema;
    use schemars::schema_for;
    use serde::Deserialize;

    use super::*;

    /// 测试用凭据类型的字段定义。
    #[derive(Debug, Deserialize, JsonSchema)]
    struct FakeApiKeyFields {
        /// API 令牌。
        token: String,
        /// 可选的区域。
        region: Option<String>,
    }

    /// 一个最小的凭据类型实现，用于验证注册表的契约。
    #[derive(Debug)]
    struct FakeApiKey;

    impl CredentialType for FakeApiKey {
        fn type_id(&self) -> &'static str {
            "fake.api_key"
        }

        fn display_name(&self) -> &'static str {
            "假 API 密钥"
        }

        fn fields_schema(&self) -> RootSchema {
            schema_for!(FakeApiKeyFields)
        }

        fn validate(&self, fields: &serde_json::Value) -> acmecast_core::Result<()> {
            // 先落到结构体——形状不对（缺字段、类型不符）在这一步就被挡下。
            let parsed: FakeApiKeyFields =
                serde_json::from_value(fields.clone()).map_err(|err| {
                    acmecast_core::Error::validation("token", format!("字段不合法: {err}"))
                })?;

            // 再做结构体表达不了的语义校验。
            if parsed.token.trim().is_empty() {
                return Err(acmecast_core::Error::validation("token", "不能为空白"));
            }
            if let Some(region) = &parsed.region
                && region.trim().is_empty()
            {
                return Err(acmecast_core::Error::validation("region", "不能为空白"));
            }
            Ok(())
        }
    }

    /// 另一个类型，用于验证「列表按标识升序」。
    #[derive(Debug)]
    struct AnotherFake;

    impl CredentialType for AnotherFake {
        fn type_id(&self) -> &'static str {
            "fake.another"
        }

        fn display_name(&self) -> &'static str {
            "另一个假凭据"
        }

        fn fields_schema(&self) -> RootSchema {
            schema_for!(FakeApiKeyFields)
        }

        fn validate(&self, _fields: &serde_json::Value) -> acmecast_core::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn a_registered_type_can_be_looked_up() {
        let mut registry = CredentialRegistry::new();
        registry.register(FakeApiKey).expect("首次注册应成功");

        let found = registry.get("fake.api_key").expect("应能查到已注册类型");
        assert_eq!(found.type_id(), "fake.api_key");
        assert_eq!(found.display_name(), "假 API 密钥");
    }

    #[test]
    fn an_unregistered_type_is_not_found() {
        let registry = CredentialRegistry::new();
        assert!(registry.get("nobody").is_none());
    }

    #[test]
    fn requiring_an_unregistered_type_fails_with_the_known_list() {
        let mut registry = CredentialRegistry::new();
        registry.register(FakeApiKey).unwrap();
        registry.register(AnotherFake).unwrap();

        let err = registry
            .require("fake.missing")
            .expect_err("未注册类型应被拒绝");

        match &err {
            Error::UnknownType { requested, known } => {
                assert_eq!(requested, "fake.missing");
                assert_eq!(
                    known,
                    &vec!["fake.another".to_owned(), "fake.api_key".to_owned()]
                );
            }
            other => panic!("期望 UnknownType，实际 {other:?}"),
        }

        // 错误文本要能直接告诉用户「有哪些可用」。
        let text = err.to_string();
        assert!(text.contains("fake.missing"), "{text}");
        assert!(text.contains("fake.api_key"), "{text}");
    }

    #[test]
    fn requiring_a_registered_type_succeeds() {
        let mut registry = CredentialRegistry::new();
        registry.register(FakeApiKey).unwrap();

        assert_eq!(
            registry.require("fake.api_key").unwrap().type_id(),
            "fake.api_key"
        );
    }

    #[test]
    fn duplicate_registration_is_rejected() {
        let mut registry = CredentialRegistry::new();
        registry.register(FakeApiKey).unwrap();

        let err = registry.register(FakeApiKey).expect_err("重复注册应被拒绝");
        assert!(matches!(err, Error::DuplicateType(ref id) if id == "fake.api_key"));
        assert_eq!(registry.len(), 1, "被拒绝的注册不应改变注册表");
    }

    #[test]
    fn listing_is_sorted_by_type_id() {
        let mut registry = CredentialRegistry::new();
        // 刻意乱序注册。
        registry.register(FakeApiKey).unwrap();
        registry.register(AnotherFake).unwrap();

        assert_eq!(
            registry.type_ids(),
            vec!["fake.another".to_owned(), "fake.api_key".to_owned()]
        );
        let listed: Vec<&str> = registry
            .list()
            .iter()
            .map(|credential_type| credential_type.type_id())
            .collect();
        assert_eq!(listed, vec!["fake.another", "fake.api_key"]);
    }

    #[test]
    fn an_empty_registry_is_empty() {
        let registry = CredentialRegistry::new();
        assert!(registry.is_empty());
        assert_eq!(registry.len(), 0);
        assert!(registry.list().is_empty());
    }

    #[test]
    fn the_field_schema_describes_the_required_fields() {
        let schema = FakeApiKey.fields_schema();
        let rendered = serde_json::to_string(&schema).expect("Schema 应可序列化");

        assert!(
            rendered.contains("token"),
            "Schema 应含字段 token: {rendered}"
        );
        assert!(rendered.contains("region"), "Schema 应含字段 region");
    }

    #[test]
    fn validation_delegates_to_the_field_struct() {
        let valid = serde_json::json!({ "token": "abc" });
        FakeApiKey.validate(&valid).expect("合法字段应通过");

        let missing_required = serde_json::json!({ "region": "cn" });
        let err = FakeApiKey
            .validate(&missing_required)
            .expect_err("缺必填字段应被拒绝");
        assert!(
            matches!(err, acmecast_core::Error::Validation { .. }),
            "{err:?}"
        );

        let blank = serde_json::json!({ "token": "   " });
        let err = FakeApiKey
            .validate(&blank)
            .expect_err("空白 token 应被拒绝");
        match err {
            acmecast_core::Error::Validation { field, .. } => {
                assert_eq!(field, "token", "错误应指出出错字段");
            }
            other => panic!("期望 Validation，实际 {other:?}"),
        }
    }
}
