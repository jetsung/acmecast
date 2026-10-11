//! 凭据能力标记（`dns` / `deploy`）的解析与归一化。
//!
//! `acmecast_credential.capabilities` 以 JSON 数组文本存储。本模块负责：
//! - 解析数据库文本为能力集合（缺省 / 空 / 非法 JSON 视为双能力，兼容存量）；
//! - 归一化（白名单过滤、去重、排序）后序列化回文本，保证
//!   `["deploy","dns"]` 与 `["dns","deploy"]` 在库中形式一致；
//! - 受控类型白名单：仅 `tencent` / `cloudflare` / `aliyun` 声明能力，
//!   其余类型（`tencent-eo`、`ssh`、`acme.account` 等）不暴露该字段。

/// 能力枚举：可用于 DNS-01 挑战。
pub const DNS: &str = "dns";
/// 能力枚举：可用于证书部署。
pub const DEPLOY: &str = "deploy";

/// 受控凭据类型：仅这些类型声明并持久化能力。
pub const CONTROLLED_TYPES: [&str; 3] = ["tencent", "cloudflare", "aliyun"];

/// 判断凭据类型是否受控（声明并持久化能力）。
#[must_use]
pub fn is_controlled(type_id: &str) -> bool {
    CONTROLLED_TYPES.contains(&type_id)
}

/// 存量与新建的默认能力集合（JSON 文本）。
pub const DEFAULT_CAPABILITIES_TEXT: &str = r#"["dns","deploy"]"#;

/// 把数据库中的能力文本解析为归一化能力集合。
///
/// 缺省 / 空 / 非法 JSON / 过滤后为空一律视为双能力 `["dns","deploy"]`，
/// 与迁移默认值一致，保证存量受控凭据在新过滤逻辑下不被误隐藏。
#[must_use]
pub fn parse(text: &str) -> Vec<String> {
    let parsed: Vec<String> = serde_json::from_str(text).unwrap_or_default();
    normalize(parsed)
}

/// 把 API 输入的能力集合归一化：白名单过滤、去重、排序。
///
/// 输入为空或过滤后为空时返回默认双能力。注意：API 层的「空数组 /
/// 非法值拒绝」校验在 `acmecast-server` 完成，本函数不做拒绝。
#[must_use]
pub fn normalize<I, S>(items: I) -> Vec<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    // 规范顺序按白名单固定为 dns → deploy（不是字母序：字母序里
    // "deploy" 会排到 "dns" 前面，与迁移默认文本及 API 契约不一致）。
    let mut seen: Vec<String> = Vec::with_capacity(2);
    let mut has_dns = false;
    let mut has_deploy = false;
    for item in items {
        match item.as_ref() {
            DNS => has_dns = true,
            DEPLOY => has_deploy = true,
            _ => {} // 白名单外直接过滤
        }
    }
    if has_dns {
        seen.push(DNS.to_owned());
    }
    if has_deploy {
        seen.push(DEPLOY.to_owned());
    }
    if seen.is_empty() {
        seen.push(DNS.to_owned());
        seen.push(DEPLOY.to_owned());
    }
    seen
}

/// 把能力集合序列化为库存储文本（先归一化）。
#[must_use]
pub fn to_text<I, S>(items: I) -> String
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let normalized = normalize(items);
    // normalize 只产出白名单内的合法值，序列化不会失败；万一失败回退默认值。
    serde_json::to_string(&normalized).unwrap_or_else(|_| DEFAULT_CAPABILITIES_TEXT.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn order_is_normalized_away() {
        // 等价性：不同顺序 / 重复元素归一化后一致。
        assert_eq!(normalize(["deploy", "dns"]), normalize(["dns", "deploy"]));
        assert_eq!(normalize(["dns", "dns", "deploy"]), vec![DNS, DEPLOY]);
        assert_eq!(parse(r#"["deploy","dns"]"#), vec![DNS, DEPLOY]);
    }

    #[test]
    fn illegal_values_are_filtered() {
        // 非法值被白名单过滤，仅剩合法项。
        assert_eq!(normalize(["dns", "admin", "ssh"]), vec![DNS]);
        assert_eq!(parse(r#"["dns","bogus"]"#), vec![DNS]);
    }

    #[test]
    fn legacy_default_is_both_capabilities() {
        // 存量缺省：空文本、非法 JSON、空数组、过滤后为空均视为双能力。
        assert_eq!(parse(""), vec![DNS, DEPLOY]);
        assert_eq!(parse("not json"), vec![DNS, DEPLOY]);
        assert_eq!(parse("[]"), vec![DNS, DEPLOY]);
        assert_eq!(parse(r#"["whatever"]"#), vec![DNS, DEPLOY]);
        assert_eq!(normalize(Vec::<String>::new()), vec![DNS, DEPLOY]);
    }

    #[test]
    fn text_round_trip_is_stable() {
        // 写入文本先归一化，等价集合产生相同存储文本。
        assert_eq!(to_text(["deploy", "dns"]), to_text(["dns", "deploy"]));
        assert_eq!(to_text(["dns"]), r#"["dns"]"#);
        assert_eq!(parse(&to_text(["deploy"])), vec![DEPLOY]);
    }

    #[test]
    fn controlled_types_match_whitelist() {
        assert!(is_controlled("tencent"));
        assert!(is_controlled("cloudflare"));
        assert!(is_controlled("aliyun"));
        assert!(!is_controlled("tencent-eo"));
        assert!(!is_controlled("ssh"));
        assert!(!is_controlled("acme.account"));
    }
}
