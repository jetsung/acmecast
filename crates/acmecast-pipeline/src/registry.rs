//! 任务类型注册表。

use std::collections::HashMap;

use crate::error::{Error, Result};
use crate::step::PipelineStep;

/// 已注册的流水线步骤集合。
///
/// 注册是**显式**的：启动时由 `acmecast-server` 一次性把所有内置步骤装进来。
/// 沿用 design 决策 3 的做法——不用 `inventory` / `linkme` 那类链接器魔法，
/// 它在部分平台与 WASM 上不可靠，也会让「到底注册了什么」变得无法 grep。
#[derive(Debug, Default)]
pub struct StepRegistry {
    steps: HashMap<&'static str, Box<dyn PipelineStep>>,
}

impl StepRegistry {
    /// 建一个空注册表。
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// 注册一个步骤实现。
    ///
    /// 类型标识重复时返回 [`Error::DuplicateStepType`]：重复注册只可能是编程错误，
    /// 静默覆盖会让「注册了两个实现、只有一个生效」一直藏着。
    pub fn register(&mut self, step: impl PipelineStep) -> Result<()> {
        let type_id = step.type_id();
        if self.steps.contains_key(type_id) {
            return Err(Error::DuplicateStepType(type_id.to_owned()));
        }
        self.steps.insert(type_id, Box::new(step));
        Ok(())
    }

    /// 按类型标识查找；未注册时返回 `None`。
    #[must_use]
    pub fn get(&self, type_id: &str) -> Option<&dyn PipelineStep> {
        self.steps.get(type_id).map(|boxed| boxed.as_ref())
    }

    /// 按类型标识查找；未注册时报错。
    ///
    /// 这是执行器解析步骤定义的入口——spec 要求「引用未登记的类型标识时
    /// 流水线执行失败并返回指明未知类型的错误」。
    pub fn require(&self, type_id: &str) -> Result<&dyn PipelineStep> {
        self.get(type_id).ok_or_else(|| Error::UnknownStepType {
            type_id: type_id.to_owned(),
            known: self.type_ids(),
        })
    }

    /// 全部已注册的类型标识，升序。
    #[must_use]
    pub fn type_ids(&self) -> Vec<String> {
        let mut ids: Vec<String> = self.steps.keys().map(|id| (*id).to_owned()).collect();
        ids.sort();
        ids
    }

    /// 全部已注册的步骤，按标识升序。
    #[must_use]
    pub fn list(&self) -> Vec<&dyn PipelineStep> {
        let mut entries: Vec<_> = self.steps.iter().collect();
        entries.sort_by_key(|(type_id, _)| *type_id);
        entries.into_iter().map(|(_, step)| step.as_ref()).collect()
    }

    /// 已注册步骤的数量。
    #[must_use]
    pub fn len(&self) -> usize {
        self.steps.len()
    }

    /// 是否还没有任何已注册步骤。
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.steps.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::step::{StepContext, StepOutput};

    /// 一个最小实现：只回显自己的类型标识。
    #[derive(Debug)]
    struct Echo(&'static str);

    #[async_trait::async_trait]
    impl PipelineStep for Echo {
        fn type_id(&self) -> &'static str {
            self.0
        }

        async fn execute(&self, _ctx: &mut StepContext<'_>) -> Result<StepOutput> {
            Ok(StepOutput::empty())
        }
    }

    #[test]
    fn a_registered_step_can_be_looked_up() {
        let mut registry = StepRegistry::new();
        registry
            .register(Echo("cert.apply"))
            .expect("首次注册应成功");

        let found = registry.get("cert.apply").expect("应能查到已注册步骤");
        assert_eq!(found.type_id(), "cert.apply");
        // 未声明输入结构的步骤，Schema 为 None——这是 trait 的默认行为。
        assert!(found.input_schema().is_none());
    }

    #[test]
    fn an_unregistered_step_is_not_found() {
        let registry = StepRegistry::new();
        assert!(registry.get("cert.apply").is_none());
    }

    #[test]
    fn requiring_an_unregistered_step_fails_with_the_known_list() {
        let mut registry = StepRegistry::new();
        registry.register(Echo("cert.apply")).unwrap();
        registry.register(Echo("cert.deploy")).unwrap();

        let err = registry
            .require("cert.renew")
            .expect_err("未注册的类型应被拒绝");

        match &err {
            Error::UnknownStepType { type_id, known } => {
                assert_eq!(type_id, "cert.renew");
                assert_eq!(
                    known,
                    &vec!["cert.apply".to_owned(), "cert.deploy".to_owned()]
                );
            }
            other => panic!("期望 UnknownStepType，实际 {other:?}"),
        }

        // 错误文本要能直接告诉用户「有哪些可用」。
        let text = err.to_string();
        assert!(text.contains("cert.renew"), "{text}");
        assert!(text.contains("cert.apply"), "{text}");
    }

    #[test]
    fn requiring_a_registered_step_succeeds() {
        let mut registry = StepRegistry::new();
        registry.register(Echo("cert.apply")).unwrap();

        assert_eq!(
            registry.require("cert.apply").unwrap().type_id(),
            "cert.apply"
        );
    }

    #[test]
    fn duplicate_registration_is_rejected() {
        let mut registry = StepRegistry::new();
        registry.register(Echo("cert.apply")).unwrap();

        let err = registry
            .register(Echo("cert.apply"))
            .expect_err("重复注册应被拒绝");
        assert!(matches!(err, Error::DuplicateStepType(ref id) if id == "cert.apply"));
        assert_eq!(registry.len(), 1, "被拒绝的注册不应改变注册表");
    }

    #[test]
    fn listing_is_sorted_by_type_id() {
        let mut registry = StepRegistry::new();
        // 刻意乱序注册。
        registry.register(Echo("cert.deploy")).unwrap();
        registry.register(Echo("cert.apply")).unwrap();

        assert_eq!(
            registry.type_ids(),
            vec!["cert.apply".to_owned(), "cert.deploy".to_owned()]
        );
        let listed: Vec<&str> = registry.list().iter().map(|step| step.type_id()).collect();
        assert_eq!(listed, vec!["cert.apply", "cert.deploy"]);
    }

    #[test]
    fn an_empty_registry_is_empty() {
        let registry = StepRegistry::new();
        assert!(registry.is_empty());
        assert_eq!(registry.len(), 0);
        assert!(registry.list().is_empty());
    }
}
