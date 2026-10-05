//! 字段的显隐联动。
//!
//! 标准 JSON Schema 没有「显隐联动」这个概念，因此这里用扩展关键字
//! `x-visible-when` 把它带出去，由前端决定要不要渲染某个字段。
//!
//! 用扩展关键字而不是另立一套表单描述，是为了让**一份定义自包含**：
//! 类型、必填、枚举、默认值、显隐都在同一个 schema 里。分开两套的话，
//! 「结构体改了、表单描述没改」的漂移迟早会发生。

use schemars::schema::{RootSchema, Schema};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// 扩展关键字：字段的显隐条件。
pub const VISIBLE_WHEN_KEY: &str = "x-visible-when";
/// 扩展关键字：字段在当前输入下是否被隐藏。
pub const HIDDEN_KEY: &str = "x-hidden";

/// 一个字段的显隐条件。
///
/// 标注 `#[non_exhaustive]`：将来很可能需要「非空即可见」之类的变体，
/// 而加变体对 match 的调用方是破坏性的。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum VisibleWhen {
    /// 依赖字段取到这些值之一时，本字段可见。
    Equals {
        /// 依赖的字段名。
        field: String,
        /// 令本字段可见的取值。
        values: Vec<Value>,
    },
}

impl VisibleWhen {
    /// 便利构造：依赖某字段等于某个值。
    #[must_use]
    pub fn equals(field: impl Into<String>, value: impl Into<Value>) -> Self {
        Self::Equals {
            field: field.into(),
            values: vec![value.into()],
        }
    }

    /// 给定一份输入，判断条件是否满足。
    #[must_use]
    pub fn is_satisfied(&self, input: &Value) -> bool {
        match self {
            Self::Equals { field, values } => input
                .get(field)
                // 依赖字段缺失即视为不满足：条件里列的都是显式取值，
                // 连值都没有自然谈不上匹配。
                .is_some_and(|actual| values.contains(actual)),
        }
    }
}

/// 给某个字段标注显隐条件。
///
/// 字段名在 Schema 里不存在时**静默跳过**：Schema 由步骤自己给出，
/// 拼错字段名是它那边的问题，不该在导出这一层炸掉整个接口。
pub fn mark_visible_when(schema: &mut RootSchema, field: &str, condition: VisibleWhen) {
    let Some(object) = schema.schema.object.as_mut() else {
        return;
    };
    let Some(Schema::Object(field_schema)) = object.properties.get_mut(field) else {
        return;
    };

    if let Ok(encoded) = serde_json::to_value(&condition) {
        field_schema
            .extensions
            .insert(VISIBLE_WHEN_KEY.to_owned(), encoded);
    }
}

/// 按一份输入求值，把不可见的字段标记为隐藏。
///
/// 只处理**有条件**的字段：无条件字段不加任何标记，免得前端要区分
/// 「没有 x-hidden」与「x-hidden: false」两种等价状态。
pub fn mark_hidden_fields(schema: &mut RootSchema, input: &Value) {
    let Some(object) = schema.schema.object.as_mut() else {
        return;
    };

    for field_schema in object.properties.values_mut() {
        // 标量 Schema（如 `true`）不带扩展，跳过即可。
        let Schema::Object(field_schema) = field_schema else {
            continue;
        };
        let Some(raw) = field_schema.extensions.get(VISIBLE_WHEN_KEY) else {
            continue;
        };
        // 条件解不出来就当它不存在：宁可多显示一个字段，也不要因为
        // 一条读不懂的条件把字段藏起来——用户会以为功能坏了。
        let Ok(condition) = serde_json::from_value::<VisibleWhen>(raw.clone()) else {
            continue;
        };

        field_schema.extensions.insert(
            HIDDEN_KEY.to_owned(),
            Value::Bool(!condition.is_satisfied(input)),
        );
    }
}

#[cfg(test)]
mod tests {
    use schemars::schema_for;
    use serde::Deserialize;

    use super::*;

    /// 一份典型的联动输入：`ca` 选 `custom` 时才需要填 Directory URL。
    // 字段只供 schemars 推导 Schema，代码里不逐个读取——这正是「结构体即定义」。
    #[allow(dead_code)]
    #[derive(Debug, Deserialize, schemars::JsonSchema)]
    struct Input {
        ca: String,
        directory_url: Option<String>,
        email: Option<String>,
    }

    fn schema() -> RootSchema {
        let mut schema = schema_for!(Input);
        mark_visible_when(
            &mut schema,
            "directory_url",
            VisibleWhen::equals("ca", "custom"),
        );
        schema
    }

    /// 取出某字段的 `x-hidden`；没标记时返回 `None`。
    fn hidden_of(schema: &RootSchema, field: &str) -> Option<bool> {
        let object = schema.schema.object.as_ref()?;
        let Schema::Object(field_schema) = object.properties.get(field)? else {
            return None;
        };
        field_schema
            .extensions
            .get(HIDDEN_KEY)
            .and_then(Value::as_bool)
    }

    #[test]
    fn the_condition_is_exported_into_the_schema() {
        let rendered = serde_json::to_string(&schema()).expect("应能序列化");

        assert!(rendered.contains(VISIBLE_WHEN_KEY), "{rendered}");
        assert!(
            rendered.contains("custom"),
            "条件里的取值应可见: {rendered}"
        );
    }

    #[test]
    fn a_field_is_marked_hidden_when_the_condition_fails() {
        let mut schema = schema();
        mark_hidden_fields(&mut schema, &serde_json::json!({ "ca": "letsencrypt" }));

        assert_eq!(
            hidden_of(&schema, "directory_url"),
            Some(true),
            "条件不满足时应标记为隐藏"
        );
    }

    #[test]
    fn a_field_is_marked_visible_when_the_condition_holds() {
        let mut schema = schema();
        mark_hidden_fields(&mut schema, &serde_json::json!({ "ca": "custom" }));

        assert_eq!(hidden_of(&schema, "directory_url"), Some(false));
    }

    #[test]
    fn a_missing_dependency_means_hidden() {
        let mut schema = schema();
        // 连 ca 都没填，谈不上选中了 custom。
        mark_hidden_fields(&mut schema, &serde_json::json!({}));

        assert_eq!(hidden_of(&schema, "directory_url"), Some(true));
    }

    #[test]
    fn unconditioned_fields_carry_no_mark() {
        let mut schema = schema();
        mark_hidden_fields(&mut schema, &serde_json::json!({ "ca": "custom" }));

        // 没有条件的字段不加标记——否则前端要区分「没有 x-hidden」与
        // 「x-hidden: false」两种等价状态。
        assert_eq!(hidden_of(&schema, "ca"), None);
        assert_eq!(hidden_of(&schema, "email"), None);
    }

    #[test]
    fn marking_an_unknown_field_is_ignored() {
        let mut schema = schema();
        // 字段名拼错不该炸掉导出，也不该凭空造出一个字段。
        mark_visible_when(
            &mut schema,
            "no_such_field",
            VisibleWhen::equals("ca", "custom"),
        );

        let rendered = serde_json::to_string(&schema).unwrap();
        assert!(!rendered.contains("no_such_field"), "{rendered}");
    }

    #[test]
    fn a_schema_without_properties_is_left_alone() {
        // 标量 Schema 没有 properties，标记函数应当安静地什么都不做。
        let mut scalar = schema_for!(String);
        mark_hidden_fields(&mut scalar, &serde_json::json!({ "ca": "custom" }));
        assert!(scalar.schema.object.is_none());
    }

    #[test]
    fn multiple_values_can_enable_a_field() {
        let mut schema = schema_for!(Input);
        mark_visible_when(
            &mut schema,
            "directory_url",
            VisibleWhen::Equals {
                field: "ca".to_owned(),
                values: vec![serde_json::json!("custom"), serde_json::json!("internal")],
            },
        );

        for ca in ["custom", "internal"] {
            let mut marked = schema.clone();
            mark_hidden_fields(&mut marked, &serde_json::json!({ "ca": ca }));
            assert_eq!(hidden_of(&marked, "directory_url"), Some(false), "ca={ca}");
        }
    }
}
