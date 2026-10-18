//! SSH 主机档案凭据字段。
//!
//! 一份档案 = 一台主机的连接方式：连接信息（主机、端口、用户）、认证材料，
//! 以及主机侧运维惯例的文件权限缺省值。部署输入引用档案后由它提供连接与
//! 认证；远端路径与重载命令绑定的是「主机上跑哪个服务」而不是「主机是谁」，
//! 留在部署输入里，不属于档案。
//!
//! 字段结构同时服务两端：SSH 目标按它合并出执行配置，凭据系统按它渲染
//! 创建表单与校验字段值。认证材料恰好其一的约定在 [`SshHostFields::validate`]
//! 里钉住，两端共用一份逻辑。

use schemars::JsonSchema;
use schemars::schema::RootSchema;
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::targets::ssh::SshAuthSource;

/// SSH 主机档案的凭据类型标识。
///
/// 与 SSH 部署目标的 `type_id()` 对齐（沿 DNS 凭据的先例：
/// `cloud.cloudflare` / `cloud.aliyun` 凭据类型也与各自的提供商标识对齐）。
pub const SSH_HOST_TYPE_ID: &str = "deploy.ssh";
fn default_port() -> u16 {
    22
}

/// 登录用户的缺省值；schema 携带它供凭据表单预填。
fn default_user() -> String {
    "root".to_owned()
}

/// 档案字段 Schema：`private_key` 标注 `x-multiline`，前端据此渲染多行文本框。
///
/// 布局（`x-field-order` 钉顺序，前端 24 栅格流式两列）：
/// `cert_mode|key_mode` / `host|port` / `user|password` / `private_key` 整行 /
/// `proxy_mode|proxy_url`（`proxy_url` 仅 `custom` 时显隐）。
/// `private_key` 为整行多行域，`proxy` 行自然另起一行，无需前驱 `x-end-row`。
///
/// schemars 0.8 的宏不支持任意扩展属性，注入在这里统一做——凭据页与测试
/// 共用这一份 schema，不会出现「实现改了、标注丢了」的漂移。
#[must_use]
pub fn ssh_host_fields_schema() -> RootSchema {
    use schemars::schema::Schema;

    let mut schema = schemars::schema_for!(SshHostFields);
    // serde_json 的 Map 会把 properties 按字母序重排（cert_mode 会抢到
    // host 前面）——用 x-field-order 钉住业务渲染顺序，同时它是代理
    // 字段前驱（key_mode 挂 x-end-row）的判定依据。
    schema.schema.extensions.insert(
        "x-field-order".to_owned(),
        serde_json::json!([
            "cert_mode",
            "key_mode",
            "host",
            "port",
            "user",
            "password",
            "private_key",
            "proxy_mode",
            "proxy_url"
        ]),
    );
    if let Some(Schema::Object(private_key)) = schema
        .schema
        .object
        .as_mut()
        .and_then(|object| object.properties.get_mut("private_key"))
    {
        private_key
            .extensions
            .insert("x-multiline".to_owned(), serde_json::Value::Bool(true));
    }
    // 代理字段独占一行：枚举下拉 + 联动输入框；前驱是否为整行/行尾由
    // inject 内部按栅格模拟判定，`private_key` 整行后已在新行首列，无需补 `x-end-row`。
    acmecast_core::inject_proxy_field_annotations(&mut schema);
    schema
}

/// SSH 主机档案的字段定义。
///
/// `Debug` 手写：`private_key` 与 `password` 是认证材料，打印时只能
/// 标记「有一份」，不能露出内容——日志与错误信息都会经过这里。
#[derive(Clone, Serialize, Deserialize, JsonSchema)]
pub struct SshHostFields {
    /// 目标主机。
    pub host: String,

    /// SSH 端口。
    #[serde(default = "default_port")]
    pub port: u16,

    /// 登录用户。
    #[serde(default = "default_user")]
    #[schemars(default = "default_user")]
    pub user: String,

    /// OpenSSH / PEM 格式的私钥。与 `password` 恰好其一。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub private_key: Option<String>,

    /// 登录口令。与 `private_key` 恰好其一。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub password: Option<String>,

    /// 远端证书文件权限；schema 缺省值供表单预填，留空时由系统缺省兜底。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(default = "default_cert_mode")]
    pub cert_mode: Option<String>,

    /// 远端私钥文件权限；schema 缺省值供表单预填，留空时由系统缺省兜底。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(default = "default_key_mode")]
    pub key_mode: Option<String>,

    /// 出网代理模式：`direct` 直连 / `inherit` 继承全局 / `custom` 自定义。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proxy_mode: Option<String>,

    /// 自定义出网代理（`proxy_mode=custom` 时生效）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proxy_url: Option<String>,
}

/// 证书权限的表单缺省值，与部署目标的系统缺省（`default_cert_mode`）一致。
fn default_cert_mode() -> Option<String> {
    Some("0644".to_owned())
}

/// 私钥权限的表单缺省值，与部署目标的系统缺省（`default_key_mode`）一致。
fn default_key_mode() -> Option<String> {
    Some("0600".to_owned())
}

impl std::fmt::Debug for SshHostFields {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SshHostFields")
            .field("host", &self.host)
            .field("port", &self.port)
            .field("user", &self.user)
            .field(
                "private_key",
                &acmecast_core::redact_presence(&self.private_key),
            )
            .field(
                "password",
                &acmecast_core::redact_presence(&self.password),
            )
            .field("cert_mode", &self.cert_mode)
            .field("key_mode", &self.key_mode)
            .field("proxy_url", &acmecast_core::redact_presence(&self.proxy_url))
            .finish()
    }
}

impl SshHostFields {
    /// 校验一份档案字段值是否可用。
    ///
    /// 凭据创建/更新与部署前合并共用这一份规则，错误一律带字段名，
    /// 前端据此高亮出错的输入框。
    pub fn validate(&self) -> acmecast_core::Result<()> {
        if self.host.trim().is_empty() {
            return Err(acmecast_core::Error::validation("host", "不能为空白"));
        }
        if self.user.trim().is_empty() {
            return Err(acmecast_core::Error::validation("user", "不能为空白"));
        }
        if self.port == 0 {
            return Err(acmecast_core::Error::validation("port", "端口不能为 0"));
        }
        // 认证材料按「非空才视为已提供」归一化，与 auth_material 同口径：
        // 编辑场景把 private_key 清空后提交上来的 Some("") 按未提供处理，
        // 不再误报「两者皆有」。
        let key = self
            .private_key
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty());
        let password = self
            .password
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty());
        match (key, password) {
            (None, None) => Err(acmecast_core::Error::validation(
                "private_key",
                "私钥与口令必须恰好其一，当前两者皆缺",
            )),
            (Some(_), Some(_)) => Err(acmecast_core::Error::validation(
                "private_key",
                "私钥与口令必须恰好其一，当前两者皆有",
            )),
            _ => Ok(()),
        }?;
        // custom 模式必须给出合法代理 URL（保存/部署前拦截）。
        let proxy_mode = acmecast_core::ProxyMode::from_credentials(
            &serde_json::json!({
                "proxy_mode": self.proxy_mode,
                "proxy_url": self.proxy_url,
            }),
        );
        if proxy_mode == acmecast_core::ProxyMode::Custom {
            let Some(url) = self.proxy_url.as_deref().map(str::trim).filter(|u| !u.is_empty())
            else {
                return Err(acmecast_core::Error::validation(
                    "proxy_url",
                    "proxy_mode=custom 时必须填写代理 URL",
                ));
            };
            acmecast_core::validate_proxy_url(url)
                .map_err(|e| acmecast_core::Error::validation("proxy_url", e.to_string()))?;
        }
        Ok(())
    }

    /// 取出档案自带的认证材料，转成部署可用的来源。
    ///
    /// 档案通过校验后材料必然恰好其一；字段形态坏掉（材料缺失或空白）
    /// 时报错而不是猜——静默跳过会把「配置错了」变成「部署到一半才炸」。
    pub fn auth_material(&self) -> Result<SshAuthSource> {
        if let Some(key) = self.private_key.as_deref().filter(|k| !k.trim().is_empty()) {
            return Ok(SshAuthSource::PrivateKey(key.to_owned()));
        }
        if let Some(password) = self.password.as_deref().filter(|p| !p.trim().is_empty()) {
            return Ok(SshAuthSource::Password(password.to_owned()));
        }
        Err(Error::invalid_input(
            "credential_id",
            format!(
                "SSH 主机档案 `{}` 里没有可用的私钥或口令",
                self.host_for_message()
            ),
        ))
    }

    /// 错误信息里代表这份档案的名字：主机优先，退化到用户。
    fn host_for_message(&self) -> &str {
        if self.host.trim().is_empty() {
            self.user.as_str()
        } else {
            self.host.as_str()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fields() -> SshHostFields {
        SshHostFields {
            host: "web-1.example.com".to_owned(),
            port: 22,
            user: "deploy".to_owned(),
            private_key: Some("PRIVATE-KEY".to_owned()),
            password: None,
            cert_mode: None,
            key_mode: None,
            proxy_mode: None,
            proxy_url: None,
        }
    }

    #[test]
    fn a_well_formed_profile_validates() {
        fields().validate().expect("完整档案应通过校验");
    }

    #[test]
    fn blank_host_or_user_is_rejected_with_the_field_name() {
        let mut bad = fields();
        bad.host = "   ".to_owned();
        let err = bad.validate().expect_err("空白主机应被拒绝");
        match err {
            acmecast_core::Error::Validation { field, .. } => assert_eq!(field, "host"),
            other => panic!("期望 Validation，实际 {other:?}"),
        }

        let mut bad = fields();
        bad.user = String::new();
        let err = bad.validate().expect_err("空白用户应被拒绝");
        match err {
            acmecast_core::Error::Validation { field, .. } => assert_eq!(field, "user"),
            other => panic!("期望 Validation，实际 {other:?}"),
        }
    }

    #[test]
    fn auth_materials_must_be_exactly_one() {
        let mut bad = fields();
        bad.private_key = None;
        let err = bad.validate().expect_err("缺认证材料应被拒绝");
        match err {
            acmecast_core::Error::Validation { field, .. } => assert_eq!(field, "private_key"),
            other => panic!("期望 Validation，实际 {other:?}"),
        }

        let mut bad = fields();
        bad.password = Some("s3cret".to_owned());
        let err = bad.validate().expect_err("两种材料同给应被拒绝");
        match err {
            acmecast_core::Error::Validation { field, .. } => assert_eq!(field, "private_key"),
            other => panic!("期望 Validation，实际 {other:?}"),
        }

        let mut bad = fields();
        bad.private_key = Some("   ".to_owned());
        let err = bad.validate().expect_err("纯空白私钥按皆缺拒绝");
        match err {
            acmecast_core::Error::Validation { field, .. } => assert_eq!(field, "private_key"),
            other => panic!("期望 Validation，实际 {other:?}"),
        }

        let mut bad = fields();
        bad.private_key = None;
        bad.password = Some("   ".to_owned());
        let err = bad.validate().expect_err("纯空白口令按皆缺拒绝");
        match err {
            // 「皆缺」统一指向 private_key（与既有口径一致），不再单独报 password 空白。
            acmecast_core::Error::Validation { field, .. } => assert_eq!(field, "private_key"),
            other => panic!("期望 Validation，实际 {other:?}"),
        }
    }

    #[test]
    fn a_blank_string_material_counts_as_absent() {
        // 编辑场景：private_key 清空后提交上来的是 Some("")，按未提供处理，
        // 与 auth_material 的过滤口径一致——「恰好其一」通过，不误报「两者皆有」。
        let mut ok = fields();
        ok.private_key = Some(String::new());
        ok.password = Some("s3cret".to_owned());
        ok.validate().expect("空串私钥 + 非空口令应按恰好其一通过");

        let mut ok = fields();
        ok.password = Some(String::new());
        ok.validate().expect("非空私钥 + 空串口令应按恰好其一通过");
    }

    #[test]
    fn port_zero_is_rejected() {
        let mut bad = fields();
        bad.port = 0;
        let err = bad.validate().expect_err("端口 0 应被拒绝");
        match err {
            acmecast_core::Error::Validation { field, .. } => assert_eq!(field, "port"),
            other => panic!("期望 Validation，实际 {other:?}"),
        }
    }

    #[test]
    fn port_defaults_to_22_when_missing_from_json() {
        let parsed: SshHostFields =
            serde_json::from_value(serde_json::json!({ "host": "h", "user": "u", "private_key": "K" }))
                .expect("应能解析");
        assert_eq!(parsed.port, 22, "端口缺省应为 22");
    }

    #[test]
    fn user_defaults_to_root_when_missing_from_json() {
        let parsed: SshHostFields =
            serde_json::from_value(serde_json::json!({ "host": "h", "private_key": "K" }))
                .expect("应能解析");
        assert_eq!(parsed.user, "root", "用户缺省应为 root");
    }

    #[test]
    fn the_field_schema_carries_form_defaults() {
        // 表单预填的缺省值由 schema 的 default 携带：用户 root、证书 0644、私钥 0600；
        // 档案不含远端路径与重载命令字段。
        let rendered = serde_json::to_value(ssh_host_fields_schema()).expect("Schema 应可序列化");
        let properties = &rendered["properties"];
        assert_eq!(properties["user"]["default"], "root", "用户缺省应为 root");
        assert_eq!(properties["cert_mode"]["default"], "0644", "证书权限缺省应为 0644");
        assert_eq!(properties["key_mode"]["default"], "0600", "私钥权限缺省应为 0600");
        for removed in ["cert_path", "key_path", "reload_command"] {
            assert!(
                properties.get(removed).is_none(),
                "档案不应再有 {removed} 字段"
            );
        }
    }

    #[test]
    fn debug_output_never_contains_the_materials() {
        let profile = fields();
        let rendered = format!("{profile:?}");
        assert!(!rendered.contains("PRIVATE-KEY"), "{rendered}");
    }

    #[test]
    fn the_field_schema_marks_the_private_key_as_multiline() {
        let rendered = serde_json::to_value(ssh_host_fields_schema()).expect("Schema 应可序列化");
        assert_eq!(
            rendered["properties"]["private_key"]["x-multiline"],
            serde_json::Value::Bool(true),
            "私钥字段应标注多行: {}",
            rendered["properties"]["private_key"]
        );
    }

    #[test]
    fn the_field_schema_lays_out_proxy_on_its_own_row_and_keeps_host_port_together() {
        // 目标布局：cert_mode|key_mode / host|port / user|password / private_key 整行 / proxy_mode|proxy_url
        let rendered = serde_json::to_value(ssh_host_fields_schema()).expect("Schema 应可序列化");
        let order = rendered["x-field-order"].as_array().expect("应有 x-field-order");
        let order: Vec<&str> = order.iter().map(|v| v.as_str().unwrap()).collect();
        assert_eq!(
            order,
            ["cert_mode", "key_mode", "host", "port", "user", "password", "private_key", "proxy_mode", "proxy_url"],
            "字段顺序应满足 cert_mode|key_mode / host|port / user|password / private_key 整行 / proxy 行"
        );
        // private_key 整行后 proxy 自然新行首列，前驱不应补 x-end-row。
        assert!(
            rendered["properties"]["private_key"]["x-end-row"].is_null(),
            "private_key 为整行，proxy 行已在新行首列，不应补 x-end-row: {}",
            rendered["properties"]["private_key"]
        );
        // host/port、user/password 为同行半宽，不做 x-full-width/x-multiline/x-end-row。
        for name in ["host", "port", "user", "password", "cert_mode", "key_mode"] {
            let node = &rendered["properties"][name];
            assert!(node["x-full-width"].is_null(), "{name} 不应 x-full-width: {node}");
            assert!(node["x-multiline"].is_null(), "{name} 不应 x-multiline: {node}");
        }
        // 代理下拉与联动仍注入。
        assert!(rendered.to_string().contains(r#""enum":["direct","inherit","custom"]"#));
        assert!(rendered.to_string().contains(r#""x-visible-when":{"Equals":{"field":"proxy_mode","values":["custom"]}}"#));
    }

    #[test]
    fn auth_material_picks_the_private_key_first_and_reports_when_missing() {
        let profile = fields();
        match profile.auth_material().expect("应取到材料") {
            SshAuthSource::PrivateKey(key) => assert_eq!(key, "PRIVATE-KEY"),
            other => panic!("期望 PrivateKey，实际 {other:?}"),
        }

        let mut profile = fields();
        profile.private_key = None;
        profile.password = Some("s3cret".to_owned());
        match profile.auth_material().expect("应取到材料") {
            SshAuthSource::Password(password) => assert_eq!(password, "s3cret"),
            other => panic!("期望 Password，实际 {other:?}"),
        }

        let mut profile = fields();
        profile.private_key = None;
        let err = profile.auth_material().expect_err("无材料应报错");
        assert!(matches!(err, Error::InvalidInput { .. }), "{err:?}");
        assert!(err.to_string().contains("web-1.example.com"), "{err}");
    }

    #[test]
    fn the_type_id_aligns_with_the_ssh_deploy_target() {
        assert_eq!(SSH_HOST_TYPE_ID, "deploy.ssh");
    }
}
