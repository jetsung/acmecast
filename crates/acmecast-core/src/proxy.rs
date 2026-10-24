//! 代理工具：凭据级代理模式、代理 URL 校验与凭据 schema 注入。
//!
//! 所有凭据类型（DNS 提供商、部署目标、ACME 账号）共用的代理原语都在这里，
//! 保证「保存校验」与「运行时选传输层」口径单一来源。

use serde_json::Value;

use crate::error::{Error, Result};

/// 从凭据 JSON 中取 `proxy_url`；字段缺失或为空视为直连，返回 `None`。
#[must_use]
pub fn proxy_url_from_credentials(credentials: &Value) -> Option<String> {
    let raw = credentials.get("proxy_url")?.as_str()?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_owned())
    }
}

/// 凭据级的代理模式（凭据的 `proxy_mode` 字段）。
///
/// 三态：直连、继承全局代理、凭据自定义代理覆盖全局。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ProxyMode {
    /// 直连，不继承全局代理（缺省）。
    #[default]
    Direct,
    /// 继承全局代理（`[proxy] url` 或 `ACMECAST_PROXY_URL`）；全局未配则直连。
    Inherit,
    /// 走凭据 `proxy_url`，覆盖全局。
    Custom,
}

impl ProxyMode {
    /// 凭据 JSON 中该字段的键名。
    pub const FIELD: &'static str = "proxy_mode";

    /// schema 里注入的三个候选值（前端下拉按序渲染）。
    pub const VARIANTS: [&'static str; 3] = ["direct", "inherit", "custom"];

    /// 数据库明文列的文本形态：`proxy_mode` 冗余列存的就是它。
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Direct => "direct",
            Self::Inherit => "inherit",
            Self::Custom => "custom",
        }
    }

    /// 从凭据 JSON 解析模式，做存量兼容归一化：
    ///
    /// - `proxy_mode` 缺省但旧 `proxy_url` 有值 → `Custom` + 该值；
    /// - 两者都缺（或 `proxy_url` 为空）→ `Direct`。
    ///
    /// 归一化在读时做而非改库数据：存量凭据因此不必迁移。
    #[must_use]
    pub fn from_credentials(credentials: &Value) -> Self {
        match credentials.get(Self::FIELD).and_then(Value::as_str) {
            Some("direct") => Self::Direct,
            Some("inherit") => Self::Inherit,
            Some("custom") => Self::Custom,
            // 存量凭据无 `proxy_mode`：旧 `proxy_url` 有值视为 custom，无值视为 direct。
            _ if proxy_url_from_credentials(credentials).is_some() => Self::Custom,
            _ => Self::Direct,
        }
    }
}

/// 代理 URL 的日志展示形态：`scheme://host:port`，剥掉 userinfo。
///
/// 代理 URL 允许 `socks5://user:pass@host:port` 形态，运行历史日志行
/// 只落 scheme 与主机——认证材料不进日志。
#[must_use]
pub fn proxy_log_target(proxy_url: &str) -> String {
    let trimmed = proxy_url.trim();
    let (scheme, rest) = match trimmed.split_once("://") {
        Some((scheme, rest)) => (scheme, rest),
        None => ("", trimmed),
    };
    // userinfo 可能内嵌 `@`，取最后一个 `@` 之后的部分即主机。
    let host = rest.rsplit('@').next().unwrap_or(rest);
    if scheme.is_empty() {
        host.to_owned()
    } else {
        format!("{scheme}://{host}")
    }
}

/// 凭据真实生效的代理 URL：`direct`→`None`；`inherit`→非空全局代理；
/// `custom`→凭据自带 `proxy_url`（校验后）。所有凭据类型共用这一口径，
/// 与运行时传输层选择（`transport_for_credentials` 等）同源。
#[must_use]
pub fn effective_proxy_url(credentials: &Value, global_proxy_url: Option<&str>) -> Option<String> {
    match ProxyMode::from_credentials(credentials) {
        ProxyMode::Direct => None,
        ProxyMode::Inherit => global_proxy_url
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned),
        ProxyMode::Custom => proxy_url_from_credentials(credentials),
    }
}

/// 校验凭据的代理字段（保存入口与各凭据类型 `validate` 的单一口径）：
/// `proxy_mode=custom` 时要求 `proxy_url` 非空且 scheme 合法；
/// `direct` / `inherit` 放行（`proxy_url` 可空，配了也不拦）。
pub fn validate_proxy_field(credentials: &Value) -> Result<()> {
    if ProxyMode::from_credentials(credentials) != ProxyMode::Custom {
        return Ok(());
    }
    let raw = credentials
        .get("proxy_url")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();
    if raw.is_empty() {
        return Err(Error::validation(
            "proxy_url",
            "proxy_mode=custom 时 proxy_url 不能为空",
        ));
    }
    validate_proxy_url(raw).map_err(|e| Error::validation("proxy_url", e.to_string()))
}

/// 校验代理 URL：scheme 必须是 `socks5` / `http` / `https`，且整体可解析。
///
/// 供运行时传输层构建与凭据保存校验共用，保证两处口径一致。
pub fn validate_proxy_url(proxy_url: &str) -> Result<()> {
    let parsed = url::Url::parse(proxy_url).map_err(|_| Error::Validation {
        field: "proxy_url".to_owned(),
        reason: format!("代理 URL `{proxy_url}` 无法解析（支持 socks5://、http://、https://）"),
    })?;
    match parsed.scheme() {
        "socks5" | "http" | "https" => Ok(()),
        other => Err(Error::Validation {
            field: "proxy_url".to_owned(),
            reason: format!(
                "代理 URL scheme `{other}` 不支持（支持 socks5://、http://、https://）"
            ),
        }),
    }
}

/// 把 `proxy_mode` / `proxy_url` 的前端渲染标注注入凭据 schema。
///
/// 入参为 `RootSchema`——需读顶层 `x-field-order` 确定 proxy_mode 的前驱字段。
///
/// - `proxy_mode`：枚举下拉（`enum_values` + `x-enum-labels` 中文展示名）；
/// - `proxy_url`：`x-visible-when` 与 `proxy_mode=custom` 联动显隐；
/// - `proxy_mode` 前一个字段：`x-end-row` 使 proxy_mode 从新行首列开始，
///   与 proxy_url 自然成对同行（左下拉、右输入框）。
///
/// schemars 0.8 的宏不支持任意扩展属性，本函数在各凭据类型的
/// `*_fields_schema()` 里统一调用，不会出现「实现改了、联动标注丢了」的漂移。
pub fn inject_proxy_field_annotations(schema: &mut schemars::schema::RootSchema) {
    let Some(validation) = schema.schema.object.as_mut() else {
        return;
    };
    if let Some(schemars::schema::Schema::Object(mode)) =
        validation.properties.get_mut("proxy_mode")
    {
        // schemars 对 `Option<String>` 不产 `enum` 数组——手动注入，
        // 否则前端 `SchemaForm` 的 `node.enum.length > 0` 分支不命中，
        // 会退化成普通文本输入框而非 `Select` 下拉。
        mode.enum_values = Some(vec![
            serde_json::Value::String("direct".to_owned()),
            serde_json::Value::String("inherit".to_owned()),
            serde_json::Value::String("custom".to_owned()),
        ]);
        mode.extensions.insert(
            "x-enum-labels".to_owned(),
            serde_json::json!({
                "direct": "直连",
                "inherit": "继承全局",
                "custom": "自定义",
            }),
        );
    }
    if let Some(schemars::schema::Schema::Object(url)) = validation.properties.get_mut("proxy_url")
    {
        url.extensions.insert(
            "x-visible-when".to_owned(),
            serde_json::json!({ "Equals": { "field": "proxy_mode", "values": ["custom"] } }),
        );
    }
    // 布局：proxy_mode 与 proxy_url 独占一行（proxy_mode 左、proxy_url 右）。
    // 仅当 proxy_mode 前一个字段处于行首（左列）时，才给它注入 x-end-row
    // 补空占位列把 proxy_mode 推到新行；若前一字段已在行尾（右列/整行），
    // 自然换行无需补列，否则空列会把 proxy_mode 挤到行尾右侧、proxy_url 另起一行。
    let order: Vec<String> = schema
        .schema
        .extensions
        .get("x-field-order")
        .and_then(|v| serde_json::from_value(v.clone()).ok())
        .unwrap_or_default();
    if let Some(pos) = order.iter().position(|f| f == "proxy_mode")
        && pos > 0
    {
        // 模拟 SchemaForm 的流式布局：半宽 12、整行 24，x-end-row 在半宽左列时补空列占满。
        let mut col: u8 = 0; // 当前行已占用宽度 0/12，24 即换行归零
        for name in &order[..pos] {
            let field = validation.properties.get(name);
            // 凭据字段均为字符串标量，仅 x-full-width / x-multiline 视为整行。
            let is_full = field.is_some_and(|f| match f {
                schemars::schema::Schema::Object(o) => {
                    o.extensions.contains_key("x-full-width")
                        || o.extensions.contains_key("x-multiline")
                }
                schemars::schema::Schema::Bool(_) => false,
            });
            let span: u8 = if is_full { 24 } else { 12 };
            if span == 24 {
                // 整行独占：若当前行有半列残留，先换行再占整行，行满后归零。
                if col == 12 {
                    col = 0;
                }
                col = 0;
            } else {
                // 半宽：若放不下则换行
                if col + span > 24 {
                    col = 0;
                }
                col += span;
                let has_end_row = field.is_some_and(|f| match f {
                    schemars::schema::Schema::Object(o) => {
                        o.extensions.get("x-end-row") == Some(&serde_json::Value::Bool(true))
                    }
                    schemars::schema::Schema::Bool(_) => false,
                });
                if has_end_row && col == 12 {
                    col = 24;
                }
                if col == 24 {
                    col = 0;
                }
            }
        }
        // col==12 意味着前驱占据左列，proxy_mode 会挤在同一行右侧——需补空列推新行。
        // col==0 意味着前驱已在行尾或整行后，proxy_mode 已在新行首列，无需补。
        if col == 12 {
            let prev = &order[pos - 1];
            if let Some(schemars::schema::Schema::Object(field)) =
                validation.properties.get_mut(prev)
            {
                field
                    .extensions
                    .insert("x-end-row".to_owned(), serde_json::Value::Bool(true));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proxy_mode_normalizes_legacy_credentials() {
        assert_eq!(
            ProxyMode::from_credentials(&serde_json::json!({"proxy_mode": "direct"})),
            ProxyMode::Direct
        );
        assert_eq!(
            ProxyMode::from_credentials(&serde_json::json!({"proxy_mode": "inherit"})),
            ProxyMode::Inherit
        );
        assert_eq!(
            ProxyMode::from_credentials(
                &serde_json::json!({"proxy_mode": "custom", "proxy_url": "http://10.0.0.1:3128"})
            ),
            ProxyMode::Custom
        );
        // 存量兼容：无 proxy_mode 但旧 proxy_url 有值 → custom；都无 → direct。
        assert_eq!(
            ProxyMode::from_credentials(&serde_json::json!({"proxy_url": "http://10.0.0.2:8080"})),
            ProxyMode::Custom
        );
        assert_eq!(
            ProxyMode::from_credentials(&serde_json::json!({})),
            ProxyMode::Direct
        );
    }

    #[test]
    fn proxy_url_from_credentials_treats_blank_as_none() {
        assert_eq!(
            proxy_url_from_credentials(&serde_json::json!({"proxy_url": "  "})),
            None
        );
        assert_eq!(
            proxy_url_from_credentials(&serde_json::json!({"proxy_url": "socks5://1.2.3.4:1080"})),
            Some("socks5://1.2.3.4:1080".to_owned())
        );
        assert_eq!(proxy_url_from_credentials(&serde_json::json!({})), None);
    }

    #[test]
    fn validate_proxy_url_accepts_supported_schemes_only() {
        for url in [
            "socks5://127.0.0.1:7890",
            "socks5://user:pass@10.0.0.1:1080",
            "http://10.0.0.1:3128",
            "https://proxy.example.com",
        ] {
            assert!(validate_proxy_url(url).is_ok(), "{url} 应合法");
        }
        // 非法：不支持的 scheme 与不可解析的值。
        for url in ["ftp://127.0.0.1:21", "not-a-url", ""] {
            let error = validate_proxy_url(url).expect_err("{url} 应被拒");
            assert!(error.to_string().contains("socks5"), "{error}");
        }
    }

    #[test]
    fn proxy_log_target_strips_userinfo() {
        assert_eq!(
            proxy_log_target("socks5://user:pass@10.0.0.1:7890"),
            "socks5://10.0.0.1:7890"
        );
        assert_eq!(
            proxy_log_target("http://10.0.0.1:3128"),
            "http://10.0.0.1:3128"
        );
        assert!(!proxy_log_target("socks5://user:secret@1.2.3.4:1080").contains("secret"));
    }

    /// 用最小 RootSchema 验证注入：enum、中文标签、联动与前驱换行。
    #[test]
    fn inject_annotations_covers_enum_labels_visibility_and_end_row() {
        let mut schema = schemars::schema_for!(Def);
        schema.schema.extensions.insert(
            "x-field-order".to_owned(),
            serde_json::json!(["token", "proxy_mode", "proxy_url"]),
        );
        inject_proxy_field_annotations(&mut schema);
        let rendered = serde_json::to_string(&schema).unwrap();
        assert!(
            rendered.contains(r#""enum":["direct","inherit","custom"]"#),
            "应注入枚举候选: {rendered}"
        );
        for label in ["直连", "继承全局", "自定义"] {
            assert!(
                rendered.contains(label),
                "应含中文展示名 {label}: {rendered}"
            );
        }
        assert!(
            rendered.contains(
                r#""x-visible-when":{"Equals":{"field":"proxy_mode","values":["custom"]}}"#
            ),
            "proxy_url 应联动 custom: {rendered}"
        );
        let value: serde_json::Value = serde_json::from_str(&rendered).unwrap();
        assert_eq!(
            value["properties"]["token"]["x-end-row"],
            serde_json::json!(true),
            "前驱字段应带 x-end-row: {rendered}"
        );
    }

    #[derive(schemars::JsonSchema)]
    struct Def {
        /// 前驱字段。
        #[allow(dead_code)]
        token: String,
        /// 代理模式。
        #[allow(dead_code)]
        proxy_mode: Option<String>,
        /// 自定义代理 URL。
        #[allow(dead_code)]
        proxy_url: Option<String>,
    }
}
