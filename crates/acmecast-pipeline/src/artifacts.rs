//! 运行期间的产物集合。
//!
//! 产物是步骤之间传递结果的唯一通道——后序步骤按**名称**读取前序产出，
//! 而不必回头去读磁盘或重算。

use std::collections::BTreeMap;

use crate::error::{Error, Result};

/// 一件产物，连同它的来源。
#[derive(Debug, Clone, PartialEq)]
pub struct Artifact {
    /// 产出它的步骤序号，从 0 起。
    pub step_order: i32,
    /// 产出它的任务类型标识。
    pub type_id: String,
    /// 内容。
    pub value: serde_json::Value,
}

/// 一次运行中的产物集合。
///
/// 顺序执行下「读最近产出的那份」语义自洽：每次读取时，最近产出的就是当前该用的。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Artifacts {
    values: BTreeMap<String, Artifact>,
}

impl Artifacts {
    /// 建一个空集合。
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// 并入一个步骤的全部产出，返回本次**被覆盖**的产物名。
    ///
    /// 同名时覆盖而非报错。理由是一条流水线管多个域名时，后一个「申请证书」步骤
    /// 产出的 `cert_pem` 本就该替换前者——若改成报错，用户只能把序号写进产物名
    /// （`cert_pem_1`、`cert_pem_2`），等于把「步骤顺序」这一信息冗余进名字里。
    ///
    /// 但覆盖**不会**以失败的形式暴露：部署步骤照样成功，只是内容可能不是本意。
    /// 所以每次覆盖都记一条 warn，那是这种情况唯一的线索。
    pub fn merge(
        &mut self,
        step_order: i32,
        type_id: &str,
        produced: BTreeMap<String, serde_json::Value>,
    ) -> Vec<String> {
        let mut overwritten = Vec::new();

        for (name, value) in produced {
            if let Some(previous) = self.values.get(&name) {
                tracing::warn!(
                    artifact = %name,
                    previous_step = previous.step_order,
                    previous_type = %previous.type_id,
                    current_step = step_order,
                    current_type = %type_id,
                    "产物重名，后产出的覆盖了先产出的"
                );
                overwritten.push(name.clone());
            }

            self.values.insert(
                name,
                Artifact {
                    step_order,
                    type_id: type_id.to_owned(),
                    value,
                },
            );
        }

        overwritten
    }

    /// 按名称读取产物。
    ///
    /// 缺失时返回的错误会列出当前可用的产物名——这个错误几乎总是步骤顺序写反
    /// 或产物名打错，此时「有哪些可用」比「你少了什么」更有用。
    pub fn get(&self, name: &str) -> Result<&serde_json::Value> {
        self.values
            .get(name)
            .map(|artifact| &artifact.value)
            .ok_or_else(|| Error::MissingArtifact {
                name: name.to_owned(),
                available: self.names(),
            })
    }

    /// 某件产物的来源；不存在时为 `None`。
    #[must_use]
    pub fn source_of(&self, name: &str) -> Option<&Artifact> {
        self.values.get(name)
    }

    /// 全部产物名，升序。
    #[must_use]
    pub fn names(&self) -> Vec<String> {
        self.values.keys().cloned().collect()
    }

    /// 产物数量。
    #[must_use]
    pub fn len(&self) -> usize {
        self.values.len()
    }

    /// 是否还没有任何产物。
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn produced(items: &[(&str, serde_json::Value)]) -> BTreeMap<String, serde_json::Value> {
        items
            .iter()
            .map(|(name, value)| ((*name).to_owned(), value.clone()))
            .collect()
    }

    #[test]
    fn an_artifact_can_be_read_back_by_name() {
        let mut artifacts = Artifacts::new();
        artifacts.merge(0, "cert.apply", produced(&[("cert_pem", json!("PEM"))]));

        assert_eq!(artifacts.get("cert_pem").unwrap(), &json!("PEM"));
        assert_eq!(artifacts.len(), 1);
        assert!(!artifacts.is_empty());
    }

    #[test]
    fn a_missing_artifact_names_itself_and_lists_what_is_available() {
        let mut artifacts = Artifacts::new();
        artifacts.merge(
            0,
            "cert.apply",
            produced(&[("cert_pem", json!("PEM")), ("key_pem", json!("KEY"))]),
        );

        let err = artifacts.get("certificate").expect_err("缺失应报错");
        let text = err.to_string();
        assert!(text.contains("certificate"), "应指出缺的是哪一个: {text}");
        assert!(text.contains("cert_pem"), "应列出可用的产物: {text}");
        assert!(text.contains("key_pem"), "{text}");
    }

    #[test]
    fn a_missing_artifact_on_an_empty_run_says_so() {
        let artifacts = Artifacts::new();
        let err = artifacts.get("anything").expect_err("空集合里什么都缺");
        assert!(err.to_string().contains("暂无"), "{err}");
    }

    // ---- 覆盖语义 ----

    #[test]
    fn a_later_step_overwrites_an_artifact_of_the_same_name() {
        let mut artifacts = Artifacts::new();
        artifacts.merge(0, "cert.apply", produced(&[("cert_pem", json!("第一份"))]));
        artifacts.merge(2, "cert.apply", produced(&[("cert_pem", json!("第二份"))]));

        // 顺序执行下「读最近产出的那份」——这正是多域名流水线依赖的语义。
        assert_eq!(artifacts.get("cert_pem").unwrap(), &json!("第二份"));
        assert_eq!(artifacts.len(), 1, "同名产物只占一个位置");
    }

    #[test]
    fn an_overwrite_is_reported_back_to_the_caller() {
        let mut artifacts = Artifacts::new();
        artifacts.merge(0, "cert.apply", produced(&[("cert_pem", json!("A"))]));

        let overwritten = artifacts.merge(
            1,
            "cert.import",
            produced(&[("cert_pem", json!("B")), ("extra", json!("C"))]),
        );

        assert_eq!(overwritten, vec!["cert_pem"], "只应报告被覆盖的那一个");
    }

    #[test]
    fn an_overwrite_leaves_a_trace_of_the_previous_source() {
        let mut artifacts = Artifacts::new();
        artifacts.merge(0, "cert.apply", produced(&[("cert_pem", json!("A"))]));
        artifacts.merge(3, "cert.import", produced(&[("cert_pem", json!("B"))]));

        // 覆盖之后，来源应指向新的那一步——warn 之外，这是排查时的第二条线索。
        let source = artifacts.source_of("cert_pem").expect("应有来源");
        assert_eq!(source.step_order, 3);
        assert_eq!(source.type_id, "cert.import");
    }

    #[test]
    fn distinct_names_do_not_interfere() {
        let mut artifacts = Artifacts::new();
        artifacts.merge(0, "a", produced(&[("one", json!(1))]));
        let overwritten = artifacts.merge(1, "b", produced(&[("two", json!(2))]));

        assert!(overwritten.is_empty(), "名字不同不该被当成覆盖");
        assert_eq!(artifacts.names(), vec!["one", "two"]);
    }
}
