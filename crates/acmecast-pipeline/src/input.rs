//! 步骤输入的校验。
//!
//! 覆盖范围是 `schemars` 实际会生成的常见约束：`required`、`type`、`enum`，
//! 并递归到对象的属性与数组的元素，`$ref` 也会顺着解析。
//!
//! 这不是完整的 JSON Schema 实现，也不打算是。执行前校验要解决的是
//! 「早失败、并把出错字段指给用户」；而**最终保证**来自步骤自己的
//! `ctx.input_as::<T>()`——serde 会完整地检查类型、必填与枚举。两层各司其职，
//! 不值得为前者引入一个完整的 Schema 引擎。

use serde_json::Value;

use crate::error::{Error, Result};

/// 按 Schema 校验一份输入。
pub fn validate(schema: &Value, input: &Value) -> Result<()> {
    check_object(schema, schema, input, "")
}

/// 顺着 `$ref` 找到它指向的定义。
///
/// 必须解析：`schemars` 把枚举与嵌套结构体一律放进 `definitions` 里再用 `$ref` 引用，
/// 不解析的话这些约束就全都落空了。
///
/// 只认 `#/...` 这种指向本文档内部的引用——`schemars` 也只生成这一种。
/// 解析不出来时原样返回：宁可少校验，也不要因为认不出引用而误伤合法输入。
fn deref<'a>(schema: &'a Value, root: &'a Value) -> &'a Value {
    let Some(reference) = schema.get("$ref").and_then(Value::as_str) else {
        return schema;
    };
    let Some(pointer) = reference.strip_prefix('#') else {
        return schema;
    };
    root.pointer(pointer).unwrap_or(schema)
}

/// 校验一个对象：先看必填，再逐字段递归。
fn check_object(schema: &Value, root: &Value, input: &Value, path: &str) -> Result<()> {
    let schema = deref(schema, root);
    let Some(object) = input.as_object() else {
        return Err(Error::InvalidInput {
            field: locate(path),
            reason: "应为对象".to_owned(),
        });
    };

    // 必填字段：这是本次校验最主要的价值所在——缺了它步骤通常跑不起来，
    // 而报错发生在执行前，用户不必去猜是哪一步失败的。
    if let Some(required) = schema.get("required").and_then(Value::as_array) {
        for name in required.iter().filter_map(Value::as_str) {
            if !object.contains_key(name) {
                return Err(Error::InvalidInput {
                    field: join(path, name),
                    reason: "是必填字段".to_owned(),
                });
            }
        }
    }

    if let Some(properties) = schema.get("properties").and_then(Value::as_object) {
        for (name, property_schema) in properties {
            // 非必填字段缺失是合法的，跳过。
            let Some(value) = object.get(name) else {
                continue;
            };
            check_value(property_schema, root, value, &join(path, name))?;
        }
    }

    Ok(())
}

/// 校验一个值：枚举、类型，以及容器内部的递归。
fn check_value(schema: &Value, root: &Value, value: &Value, path: &str) -> Result<()> {
    let schema = deref(schema, root);

    if let Some(options) = schema.get("enum").and_then(Value::as_array)
        && !options.contains(value)
    {
        let rendered: Vec<String> = options.iter().map(Value::to_string).collect();
        return Err(Error::InvalidInput {
            field: path.to_owned(),
            reason: format!("只能是 {} 之一", rendered.join("、")),
        });
    }

    if let Some(expected) = schema.get("type").and_then(Value::as_str)
        && !type_matches(expected, value)
    {
        return Err(Error::InvalidInput {
            field: path.to_owned(),
            reason: format!("应为 {expected}"),
        });
    }

    if let (Some(items), Some(array)) = (schema.get("items"), value.as_array()) {
        for (index, item) in array.iter().enumerate() {
            check_value(items, root, item, &format!("{path}[{index}]"))?;
        }
    }

    // 对象属性继续往下走；数组项已在上面处理。
    if value.is_object() && schema.get("properties").is_some() {
        check_object(schema, root, value, path)?;
    }

    Ok(())
}

/// 值是否符合声明的类型。
///
/// 认不出的类型名一律放行：Schema 的方言不少，把「看不懂」当成「不合法」
/// 会误伤合法输入，而那种错误最难排查。
fn type_matches(expected: &str, value: &Value) -> bool {
    match expected {
        "string" => value.is_string(),
        "integer" => value.is_i64() || value.is_u64(),
        "number" => value.is_number(),
        "boolean" => value.is_boolean(),
        "array" => value.is_array(),
        "object" => value.is_object(),
        "null" => value.is_null(),
        _ => true,
    }
}

/// 拼出定位串，形如 `dns.provider` 或 `domains[0]`。
fn join(prefix: &str, name: &str) -> String {
    if prefix.is_empty() {
        name.to_owned()
    } else {
        format!("{prefix}.{name}")
    }
}

/// 根级错误的字段名——此时还没有具体字段可指。
fn locate(path: &str) -> String {
    if path.is_empty() {
        "<input>".to_owned()
    } else {
        path.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// 一份典型的结构化输入定义：一个必填数组、一个带枚举的必填字段、
    /// 一个可选字段。
    fn schema() -> Value {
        json!({
            "type": "object",
            "required": ["domains", "challenge"],
            "properties": {
                "domains": { "type": "array", "items": { "type": "string" } },
                "challenge": { "type": "string", "enum": ["dns-01", "http-01"] },
                "email": { "type": "string" },
            }
        })
    }

    #[test]
    fn a_valid_input_passes() {
        let input = json!({ "domains": ["example.com"], "challenge": "dns-01" });
        validate(&schema(), &input).expect("合法输入应通过");
    }

    #[test]
    fn an_optional_field_may_be_absent() {
        // email 不在 required 里，缺席是合法的——这正是「必填」与「有声明」的区别。
        let input = json!({ "domains": ["example.com"], "challenge": "dns-01" });
        validate(&schema(), &input).expect("可选字段缺失应放行");
    }

    #[test]
    fn a_missing_required_field_names_itself() {
        let input = json!({ "challenge": "dns-01" });

        let err = validate(&schema(), &input).expect_err("缺必填应被拒绝");
        match &err {
            Error::InvalidInput { field, reason } => {
                assert_eq!(field, "domains");
                assert!(reason.contains("必填"), "{reason}");
            }
            other => panic!("期望 InvalidInput，实际 {other:?}"),
        }
        assert!(err.to_string().contains("domains"), "{err}");
    }

    #[test]
    fn a_wrong_type_names_the_field() {
        let input = json!({ "domains": "example.com", "challenge": "dns-01" });

        let err = validate(&schema(), &input).expect_err("类型不符应被拒绝");
        match &err {
            Error::InvalidInput { field, reason } => {
                assert_eq!(field, "domains");
                assert!(reason.contains("array"), "{reason}");
            }
            other => panic!("期望 InvalidInput，实际 {other:?}"),
        }
    }

    #[test]
    fn a_value_outside_the_enum_names_the_field_and_the_options() {
        let input = json!({ "domains": ["example.com"], "challenge": "tls-alpn-01" });

        let err = validate(&schema(), &input).expect_err("枚举外的值应被拒绝");
        let text = err.to_string();
        assert!(text.contains("challenge"), "{text}");
        assert!(text.contains("dns-01"), "应列出可选值: {text}");
    }

    #[test]
    fn an_array_element_is_located_by_index() {
        let input = json!({ "domains": ["example.com", 42], "challenge": "dns-01" });

        let err = validate(&schema(), &input).expect_err("元素类型不符应被拒绝");
        match &err {
            Error::InvalidInput { field, .. } => {
                assert_eq!(field, "domains[1]", "应指出是第几个元素");
            }
            other => panic!("期望 InvalidInput，实际 {other:?}"),
        }
    }

    #[test]
    fn a_nested_field_is_located_by_path() {
        let nested = json!({
            "type": "object",
            "required": ["dns"],
            "properties": {
                "dns": {
                    "type": "object",
                    "required": ["provider"],
                    "properties": { "provider": { "type": "string" } }
                }
            }
        });

        let err = validate(&nested, &json!({ "dns": {} })).expect_err("嵌套必填缺失应被拒绝");
        match &err {
            Error::InvalidInput { field, .. } => assert_eq!(field, "dns.provider"),
            other => panic!("期望 InvalidInput，实际 {other:?}"),
        }
    }

    #[test]
    fn a_non_object_input_is_rejected() {
        let err = validate(&schema(), &json!("不是对象")).expect_err("非对象应被拒绝");
        assert!(err.to_string().contains("<input>"), "{err}");
    }

    #[test]
    fn a_ref_is_resolved_so_its_constraints_still_apply() {
        // `schemars` 把枚举与嵌套结构体一律放进 `definitions` 再用 `$ref` 引用，
        // 因此引用**必须**解析——否则这些约束会全部落空。
        let with_ref = json!({
            "type": "object",
            "required": ["challenge"],
            "properties": { "challenge": { "$ref": "#/definitions/Challenge" } },
            "definitions": {
                "Challenge": { "type": "string", "enum": ["dns-01", "http-01"] }
            }
        });

        validate(&with_ref, &json!({ "challenge": "dns-01" })).expect("引用内的合法值应通过");

        let err = validate(&with_ref, &json!({ "challenge": "tls-alpn-01" }))
            .expect_err("引用内的枚举约束应当生效");
        assert!(err.to_string().contains("challenge"), "{err}");
    }

    #[test]
    fn an_unresolvable_ref_does_not_block() {
        // 认不出的引用原样放行：宁可少校验，也不要因为解析失败误伤合法输入。
        let broken = json!({
            "type": "object",
            "properties": { "x": { "$ref": "#/definitions/Missing" } }
        });
        validate(&broken, &json!({ "x": 1 })).expect("解析不出的引用不应拦下输入");
    }

    #[test]
    fn an_unknown_type_name_does_not_block() {
        // 认不出的类型名放行：把「看不懂」当「不合法」会误伤合法输入，
        // 而那种错误最难排查。
        let exotic =
            json!({ "type": "object", "properties": { "x": { "type": "some-future-type" } } });
        validate(&exotic, &json!({ "x": 1 })).expect("未知类型名不应拦下输入");
    }
}
