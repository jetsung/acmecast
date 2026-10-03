//! 部署目标注册表。

use std::collections::HashMap;

use crate::error::{Error, Result};
use crate::target::DeploymentTarget;

/// 已注册的部署目标集合。
///
/// 与前几处注册表（凭据类型、流水线步骤、DNS 提供商）同构，刻意保持一致。
#[derive(Debug, Default)]
pub struct DeploymentRegistry {
    targets: HashMap<&'static str, Box<dyn DeploymentTarget>>,
}

impl DeploymentRegistry {
    /// 建一个空注册表。
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// 注册一个部署目标。
    ///
    /// 类型标识重复时返回 [`Error::DuplicateTargetType`]：重复注册只可能是
    /// 编程错误，静默覆盖会让「注册了两个实现、只有一个生效」一直藏着。
    pub fn register(&mut self, target: impl DeploymentTarget) -> Result<()> {
        let type_id = target.type_id();
        if self.targets.contains_key(type_id) {
            return Err(Error::DuplicateTargetType(type_id.to_owned()));
        }
        self.targets.insert(type_id, Box::new(target));
        Ok(())
    }

    /// 按类型标识查找；未注册时返回 `None`。
    #[must_use]
    pub fn get(&self, type_id: &str) -> Option<&dyn DeploymentTarget> {
        self.targets.get(type_id).map(|boxed| boxed.as_ref())
    }

    /// 按类型标识查找；未注册时报错。
    ///
    /// 部署步骤解析目标的入口——spec 要求「指定未登记的目标类型时
    /// 步骤失败并返回未知目标类型错误」。
    pub fn require(&self, type_id: &str) -> Result<&dyn DeploymentTarget> {
        self.get(type_id).ok_or_else(|| Error::UnknownTargetType {
            type_id: type_id.to_owned(),
            known: self.type_ids(),
        })
    }

    /// 全部已注册的类型标识，升序。
    #[must_use]
    pub fn type_ids(&self) -> Vec<String> {
        let mut ids: Vec<String> = self.targets.keys().map(|id| (*id).to_owned()).collect();
        ids.sort();
        ids
    }

    /// 全部已注册的目标，按标识升序。
    #[must_use]
    pub fn list(&self) -> Vec<&dyn DeploymentTarget> {
        let mut entries: Vec<_> = self.targets.iter().collect();
        entries.sort_by_key(|(type_id, _)| *type_id);
        entries
            .into_iter()
            .map(|(_, target)| target.as_ref())
            .collect()
    }

    /// 已注册目标的数量。
    #[must_use]
    pub fn len(&self) -> usize {
        self.targets.len()
    }

    /// 是否还没有任何已注册目标。
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.targets.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use acmecast_access::CredentialStore;
    use schemars::schema::RootSchema;
    use schemars::schema_for;
    use serde::Deserialize;
    use serde_json::json;

    use super::*;
    use crate::target::{CertMaterials, DeployMode, DeployOutcome, parse_input};

    /// 测试用目标的输入字段。
    #[derive(Debug, Deserialize, schemars::JsonSchema)]
    // 字段只供 schemars 推导定义与 serde 校验，代码里不逐个读取。
    #[allow(dead_code)]
    struct FakeInput {
        /// 写入路径。
        cert_path: String,
    }

    /// 一个最小的假目标。
    #[derive(Debug)]
    struct FakeTarget;

    #[async_trait::async_trait]
    impl DeploymentTarget for FakeTarget {
        fn type_id(&self) -> &'static str {
            "fake"
        }

        fn display_name(&self) -> &'static str {
            "假部署目标"
        }

        fn input_schema(&self) -> RootSchema {
            schema_for!(FakeInput)
        }

        fn example_input(&self) -> serde_json::Value {
            json!({ "cert_path": "/tmp/fake.crt" })
        }

        async fn deploy(
            &self,
            input: &serde_json::Value,
            materials: &CertMaterials,
            _credentials: &CredentialStore<'_>,
            _mode: DeployMode,
        ) -> Result<DeployOutcome> {
            // 输入不合法时就该在这里被拦下，而不是带着半个配置去写文件。
            let parsed: FakeInput = parse_input(input)?;
            assert!(!materials.fingerprint.is_empty());
            Ok(DeployOutcome::written(vec![parsed.cert_path]))
        }
    }

    /// 另一个目标，用于验证列表排序。
    #[derive(Debug)]
    struct AnotherTarget;

    #[async_trait::async_trait]
    impl DeploymentTarget for AnotherTarget {
        fn type_id(&self) -> &'static str {
            "another"
        }

        fn display_name(&self) -> &'static str {
            "另一个假目标"
        }

        fn input_schema(&self) -> RootSchema {
            schema_for!(FakeInput)
        }

        fn example_input(&self) -> serde_json::Value {
            json!({ "cert_path": "/tmp/another.crt" })
        }

        async fn deploy(
            &self,
            _input: &serde_json::Value,
            _materials: &CertMaterials,
            _credentials: &CredentialStore<'_>,
            _mode: DeployMode,
        ) -> Result<DeployOutcome> {
            Ok(DeployOutcome::written(Vec::new()))
        }
    }

    #[test]
    fn a_registered_target_can_be_looked_up() {
        let mut registry = DeploymentRegistry::new();
        registry.register(FakeTarget).expect("首次注册应成功");

        let found = registry.get("fake").expect("应能查到已注册的目标");
        assert_eq!(found.type_id(), "fake");
        assert_eq!(found.display_name(), "假部署目标");
    }

    #[test]
    fn an_unregistered_target_is_not_found() {
        let registry = DeploymentRegistry::new();
        assert!(registry.get("local").is_none());
    }

    #[test]
    fn requiring_an_unregistered_target_fails_with_the_known_list() {
        let mut registry = DeploymentRegistry::new();
        registry.register(FakeTarget).unwrap();
        registry.register(AnotherTarget).unwrap();

        let err = registry.require("s3").expect_err("未注册的类型应被拒绝");

        match &err {
            Error::UnknownTargetType { type_id, known } => {
                assert_eq!(type_id, "s3");
                assert_eq!(known, &vec!["another".to_owned(), "fake".to_owned()]);
            }
            other => panic!("期望 UnknownTargetType，实际 {other:?}"),
        }

        let text = err.to_string();
        assert!(text.contains("s3"), "{text}");
        assert!(text.contains("fake"), "应列出可用的目标: {text}");
    }

    #[test]
    fn duplicate_registration_is_rejected() {
        let mut registry = DeploymentRegistry::new();
        registry.register(FakeTarget).unwrap();

        let err = registry.register(FakeTarget).expect_err("重复注册应被拒绝");
        assert!(matches!(err, Error::DuplicateTargetType(ref id) if id == "fake"));
        assert_eq!(registry.len(), 1, "被拒绝的注册不应改变注册表");
    }

    #[test]
    fn listing_is_sorted_by_type_id() {
        let mut registry = DeploymentRegistry::new();
        registry.register(FakeTarget).unwrap();
        registry.register(AnotherTarget).unwrap();

        assert_eq!(
            registry.type_ids(),
            vec!["another".to_owned(), "fake".to_owned()]
        );
        let listed: Vec<&str> = registry
            .list()
            .iter()
            .map(|target| target.type_id())
            .collect();
        assert_eq!(listed, vec!["another", "fake"]);
    }

    #[test]
    fn an_empty_registry_is_empty() {
        let registry = DeploymentRegistry::new();
        assert!(registry.is_empty());
        assert_eq!(registry.len(), 0);
        assert!(registry.list().is_empty());
    }

    #[test]
    fn the_input_definition_is_exported() {
        let rendered = serde_json::to_string(&FakeTarget.input_schema()).unwrap();
        assert!(rendered.contains("cert_path"), "{rendered}");
        assert!(rendered.contains("required"), "{rendered}");
    }

    #[test]
    fn bad_input_names_the_offending_field() {
        // 输入不合法时应当明确到字段，而不是笼统地「配置错误」。
        let err = parse_input::<FakeInput>(&json!({ "cert_paths": "/x" }))
            .expect_err("缺必填字段应被拒绝");
        assert!(err.to_string().contains("cert_path"), "{err}");
    }

    #[test]
    fn the_outcome_distinguishes_written_from_skipped() {
        let written = DeployOutcome::written(vec!["/etc/ssl/a.pem".to_owned()]);
        assert!(!written.skipped_write);

        let skipped = DeployOutcome::skipped(vec!["/etc/ssl/a.pem".to_owned()])
            .with_reload_output("reloaded");
        assert!(skipped.skipped_write);
        assert_eq!(skipped.reload_output.as_deref(), Some("reloaded"));
    }
}
