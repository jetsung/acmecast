//! 6.4 输入定义导出与字段显隐联动。
//!
//! spec 的两个层面：
//! - 静态定义里带着**条件**（`x-visible-when`），供前端动态判断；
//! - 给定一份输入导出时，条件不满足的字段被**标记为隐藏**（`x-hidden`）。

use acmecast_pipeline::{HIDDEN_KEY, PipelineStep, Result, StepContext, StepOutput, VisibleWhen};
use async_trait::async_trait;
use schemars::schema::RootSchema;
use schemars::schema_for;
use serde::Deserialize;

/// 一份带联动的输入：`ca` 决定后两个字段该不该出现。
// 字段只供 schemars 推导 Schema，代码里不逐个读取。
#[allow(dead_code)]
#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct AcmeInput {
    ca: String,
    /// 只有 `ca = custom` 时才需要填。
    directory_url: Option<String>,
    /// 只有 `ca = sslcom` 时才有意义。
    eab_kid: Option<String>,
}

#[derive(Debug)]
struct AcmeStep;

#[async_trait]
impl PipelineStep for AcmeStep {
    fn type_id(&self) -> &'static str {
        "test.acme"
    }

    fn input_schema(&self) -> Option<RootSchema> {
        let mut schema = schema_for!(AcmeInput);
        acmecast_pipeline::visibility::mark_visible_when(
            &mut schema,
            "directory_url",
            VisibleWhen::equals("ca", "custom"),
        );
        acmecast_pipeline::visibility::mark_visible_when(
            &mut schema,
            "eab_kid",
            VisibleWhen::equals("ca", "sslcom"),
        );
        Some(schema)
    }

    async fn execute(&self, _ctx: &mut StepContext<'_>) -> Result<StepOutput> {
        Ok(StepOutput::empty())
    }
}

/// 没声明输入结构的步骤。
#[derive(Debug)]
struct LooseStep;

#[async_trait]
impl PipelineStep for LooseStep {
    fn type_id(&self) -> &'static str {
        "test.loose"
    }

    async fn execute(&self, _ctx: &mut StepContext<'_>) -> Result<StepOutput> {
        Ok(StepOutput::empty())
    }
}

/// 从导出的定义里取某字段的 `x-hidden`。
fn hidden_of(exported: &serde_json::Value, field: &str) -> Option<bool> {
    exported
        .get("properties")?
        .get(field)?
        .get(HIDDEN_KEY)?
        .as_bool()
}

fn export(input: serde_json::Value) -> serde_json::Value {
    AcmeStep
        .export_input_schema(&input)
        .expect("应能导出")
        .expect("该步骤应声明了输入结构")
}

// ---- 静态定义带条件 ----

#[test]
fn the_static_schema_carries_the_linkage_conditions() {
    let rendered = serde_json::to_string(&AcmeStep.input_schema().unwrap()).unwrap();

    // 前端要靠这份静态定义自己判断显隐，所以条件必须出现在里面。
    assert!(rendered.contains("x-visible-when"), "{rendered}");
    assert!(rendered.contains("directory_url"), "{rendered}");
    assert!(rendered.contains("custom"), "{rendered}");
    assert!(rendered.contains("sslcom"), "{rendered}");
}

// ---- 按输入求值后的导出 ----

#[test]
fn a_field_is_hidden_when_its_condition_fails() {
    let exported = export(serde_json::json!({ "ca": "letsencrypt" }));

    assert_eq!(
        hidden_of(&exported, "directory_url"),
        Some(true),
        "选了内置 CA，Directory URL 该藏起来"
    );
    assert_eq!(hidden_of(&exported, "eab_kid"), Some(true));
    // ca 自身没有条件，不该被标记——否则前端要区分两种等价状态。
    assert_eq!(hidden_of(&exported, "ca"), None);
}

#[test]
fn the_matching_field_stays_visible() {
    let exported = export(serde_json::json!({ "ca": "custom" }));

    assert_eq!(
        hidden_of(&exported, "directory_url"),
        Some(false),
        "选了自定义 CA，Directory URL 应出现"
    );
    assert_eq!(
        hidden_of(&exported, "eab_kid"),
        Some(true),
        "另一个条件仍不满足"
    );
}

#[test]
fn two_fields_gated_by_the_same_field_are_judged_independently() {
    // 同一个依赖字段、不同的取值，两个字段的显隐互不影响。
    let exported = export(serde_json::json!({ "ca": "sslcom" }));

    assert_eq!(hidden_of(&exported, "directory_url"), Some(true));
    assert_eq!(hidden_of(&exported, "eab_kid"), Some(false));
}

#[test]
fn an_empty_input_hides_every_conditioned_field() {
    // 用户还没填任何东西时，条件字段都该先藏起来——否则表单会一上来
    // 就展示一堆用不上的输入框。
    let exported = export(serde_json::json!({}));

    assert_eq!(hidden_of(&exported, "directory_url"), Some(true));
    assert_eq!(hidden_of(&exported, "eab_kid"), Some(true));
}

#[test]
fn the_export_still_carries_the_conditions() {
    // 求值后的导出不能把条件本身弄丢：前端在用户改选 CA 之后还要重新判断。
    let exported = export(serde_json::json!({ "ca": "custom" }));
    let rendered = serde_json::to_string(&exported).unwrap();

    assert!(rendered.contains("x-visible-when"), "{rendered}");
    assert!(rendered.contains("custom"), "{rendered}");
}

#[test]
fn a_step_without_a_schema_exports_nothing() {
    let exported = LooseStep
        .export_input_schema(&serde_json::json!({}))
        .expect("未声明结构不是错误");
    assert!(exported.is_none());
}
