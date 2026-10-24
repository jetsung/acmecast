//! WebDAV 凭据类型。

use schemars::JsonSchema;
use schemars::schema::RootSchema;
use schemars::schema_for;
use serde::{Deserialize, Serialize};

use super::{ConnectivityOutcome, CredentialType};

/// WebDAV 凭据类型标识。
pub const TYPE_ID: &str = "deploy.webdav";
/// WebDAV 凭据字段。
#[derive(Clone, Serialize, Deserialize, JsonSchema)]
pub struct WebdavFields {
    /// 用户名。
    pub username: String,
    /// 密码。
    pub password: String,
    /// WebDAV 基地址，如 `https://dav.example.com/`。
    pub url: String,
    /// 基路径（可选）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_path: Option<String>,
    /// 出网代理模式：`direct` 直连 / `inherit` 继承全局 / `custom` 自定义。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proxy_mode: Option<String>,
    /// 自定义出网代理（`proxy_mode=custom` 时生效）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proxy_url: Option<String>,
}

impl std::fmt::Debug for WebdavFields {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WebdavFields")
            .field("username", &self.username)
            .field("password", &acmecast_core::REDACTED)
            .field("url", &self.url)
            .field("base_path", &self.base_path)
            .finish()
    }
}

/// WebDAV 凭据字段 schema：固定录入顺序 `url | base_path` / `username | password`，
/// 代理字段独占一行（proxy_mode 左下拉、proxy_url 右输入框）。
#[must_use]
pub fn webdav_fields_schema() -> RootSchema {
    let mut schema = schema_for!(WebdavFields);
    schema.schema.extensions.insert(
        "x-field-order".to_owned(),
        serde_json::json!([
            "url",
            "base_path",
            "username",
            "password",
            "proxy_mode",
            "proxy_url"
        ]),
    );
    if let Some(schemars::schema::Schema::Object(obj)) = schema
        .schema
        .object
        .as_mut()
        .and_then(|o| o.properties.get_mut("password"))
    {
        obj.extensions
            .insert("x-multiline".into(), serde_json::Value::Bool(false));
    }
    acmecast_core::inject_proxy_field_annotations(&mut schema);
    // url|base_path、username|password 各占一行——password 已在行尾,
    // proxy 行自然新行起，无需 x-end-row（由 inject 内部判断）。
    // 这里无需再手动移除；保留仅为文档可见性，实际已由 inject 判定不注入。
    schema
}

/// 按凭据的代理模式取 `reqwest::Proxy`：`custom` 且 URL 合法时返回代理，
/// `inherit` 继承全局代理；其余（`direct` / 无全局来源 / URL 非法）直连。
fn webdav_proxy(fields: &WebdavFields, global_proxy_url: Option<&str>) -> Option<reqwest::Proxy> {
    let url =
        acmecast_core::effective_proxy_url(&serde_json::to_value(fields).ok()?, global_proxy_url)?;
    // 先过统一口径的 scheme 校验（reqwest 的 Proxy::all 接受 ftp:// 等 URL
    // 层面合法但出网不支持的 scheme）——非法 URL 忽略走直连。
    acmecast_core::validate_proxy_url(&url).ok()?;
    reqwest::Proxy::all(&url).ok()
}

/// 按凭据构建 WebDAV 客户端：`proxy_mode=custom` 时经代理出网，否则直连。
fn webdav_client(fields: &WebdavFields, global_proxy_url: Option<&str>) -> reqwest::Client {
    match webdav_proxy(fields, global_proxy_url) {
        Some(proxy) => reqwest::Client::builder()
            .proxy(proxy)
            .build()
            .unwrap_or_default(),
        None => reqwest::Client::new(),
    }
}

/// WebDAV 凭据类型。
#[derive(Debug, Default)]
pub struct WebdavCredentialType {
    /// 全局出网代理（`[proxy] url` / `ACMECAST_PROXY_URL`）；供
    /// `proxy_mode=inherit` 的凭据在连通性测试里继承。
    global_proxy_url: Option<String>,
}

impl WebdavCredentialType {
    /// 设置全局出网代理（`[proxy] url` / `ACMECAST_PROXY_URL`）。
    #[must_use]
    pub fn with_global_proxy(mut self, proxy_url: Option<impl Into<String>>) -> Self {
        self.global_proxy_url = proxy_url.map(Into::into);
        self
    }
}

#[async_trait::async_trait]
impl CredentialType for WebdavCredentialType {
    fn type_id(&self) -> &'static str {
        TYPE_ID
    }

    fn display_name(&self) -> &'static str {
        "WebDAV"
    }

    fn fields_schema(&self) -> RootSchema {
        webdav_fields_schema()
    }

    async fn test_connectivity(
        &self,
        fields: &serde_json::Value,
        _capabilities: &[String],
    ) -> ConnectivityOutcome {
        let raw = fields.clone();
        let parsed: Result<WebdavFields, _> = serde_json::from_value(fields.clone());
        let f = match parsed {
            Ok(v)
                if !v.username.trim().is_empty()
                    && !v.password.is_empty()
                    && !v.url.trim().is_empty() =>
            {
                v
            }
            Ok(_) => {
                return ConnectivityOutcome::Unavailable {
                    reason: "username/password/url 均不能为空".to_owned(),
                };
            }
            Err(e) => {
                return ConnectivityOutcome::Unavailable {
                    reason: e.to_string(),
                };
            }
        };
        match webdav_probe(&f, self.global_proxy_url.as_deref()).await {
            Ok(()) => ConnectivityOutcome::Ok,
            Err(e) => ConnectivityOutcome::Unavailable {
                reason: self.redact(&raw, &e),
            },
        }
    }

    fn validate(&self, fields: &serde_json::Value) -> acmecast_core::Result<()> {
        let parsed: WebdavFields = serde_json::from_value(fields.clone())
            .map_err(|e| acmecast_core::Error::validation("fields", e.to_string()))?;
        if parsed.username.trim().is_empty() {
            return Err(acmecast_core::Error::validation("username", "不能为空"));
        }
        if parsed.password.is_empty() {
            return Err(acmecast_core::Error::validation("password", "不能为空"));
        }
        if parsed.url.trim().is_empty() {
            return Err(acmecast_core::Error::validation("url", "不能为空"));
        }
        acmecast_core::validate_proxy_field(fields)
    }
}

async fn webdav_probe(fields: &WebdavFields, global_proxy_url: Option<&str>) -> Result<(), String> {
    let base = fields
        .base_path
        .as_deref()
        .unwrap_or("")
        .trim()
        .trim_matches('/')
        .to_owned();
    let url = fields.url.trim_end_matches('/').to_owned();
    let prefix = if base.is_empty() {
        String::new()
    } else {
        format!("{}/", base)
    };
    let key = format!("{}/{}{}", url, prefix, ".acmecast-probe/healthcheck.txt");
    // MKCOL parent
    if let Some(slash) = key.rfind('/') {
        let parent = &key[..slash];
        let client = webdav_client(fields, global_proxy_url);
        let _ = client
            .request(reqwest::Method::from_bytes(b"MKCOL").unwrap(), parent)
            .basic_auth(&fields.username, Some(&fields.password))
            .send()
            .await;
    }
    let client = webdav_client(fields, global_proxy_url);
    let resp = client
        .put(&key)
        .basic_auth(&fields.username, Some(&fields.password))
        .body("acmecast probe")
        .send()
        .await
        .map_err(|e| format!("WebDAV 上传探测失败: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!(
            "WebDAV 上传探测失败: {} {}",
            resp.status(),
            resp.text().await.unwrap_or_default()
        ));
    }
    let del = client
        .delete(&key)
        .basic_auth(&fields.username, Some(&fields.password))
        .send()
        .await
        .map_err(|e| format!("WebDAV 删除探测失败: {e}"))?;
    if !del.status().is_success() && del.status().as_u16() != 404 {
        return Err(format!("WebDAV 删除探测文件失败: {}", del.status()));
    }
    // 清理空目录 .acmecast-probe（若实现为空目录即隐藏，失败忽略）
    if let Some(slash) = key.rfind('/') {
        let parent = &key[..slash];
        let _ = client
            .delete(parent)
            .basic_auth(&fields.username, Some(&fields.password))
            .send()
            .await;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn webdav_fields_schema_injects_field_order() {
        let schema = webdav_fields_schema();
        let order = schema
            .schema
            .extensions
            .get("x-field-order")
            .expect("应注入 x-field-order");
        assert_eq!(
            order,
            &serde_json::json!([
                "url",
                "base_path",
                "username",
                "password",
                "proxy_mode",
                "proxy_url"
            ])
        );
        let object = schema.schema.object.expect("应为对象 schema");
        for key in [
            "url",
            "base_path",
            "username",
            "password",
            "proxy_mode",
            "proxy_url",
        ] {
            assert!(object.properties.contains_key(key), "缺少字段 {key}");
        }
    }

    /// 代理字段注入：枚举候选 + 中文标签 + custom 联动；代理行单独成行
    /// （`url|base_path` / `username|password` 已各占一行，password 不补 `x-end-row`，
    /// 否则空列会把 proxy_mode 挤到行尾右侧、proxy_url 另起一行）。
    #[test]
    fn webdav_fields_schema_injects_proxy_annotations() {
        let rendered = serde_json::to_string(&webdav_fields_schema()).unwrap();
        assert!(
            rendered.contains(r#""enum":["direct","inherit","custom"]"#),
            "proxy_mode 应注入枚举候选: {rendered}"
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
        assert!(
            value["properties"]["password"]["x-end-row"].is_null(),
            "password 不应带 x-end-row（已在行尾自然换行，补空列会错位）: {rendered}"
        );
    }

    /// 代理客户端构建：`custom` 合法 URL 走代理，其余分支直连。
    #[test]
    fn webdav_proxy_follows_proxy_mode() {
        let mut fields = WebdavFields {
            username: "u".into(),
            password: "p".into(),
            url: "https://dav.example.com/".into(),
            base_path: None,
            proxy_mode: None,
            proxy_url: None,
        };
        // 缺省 / direct：直连。
        assert!(webdav_proxy(&fields, Some("socks5://9.9.9.9:9")).is_none());
        fields.proxy_mode = Some("direct".into());
        fields.proxy_url = Some("socks5://1.2.3.4:7890".into());
        assert!(
            webdav_proxy(&fields, Some("socks5://9.9.9.9:9")).is_none(),
            "direct 应忽略 proxy_url"
        );
        // inherit：无全局来源 → 直连；有全局来源 → 继承。
        fields.proxy_mode = Some("inherit".into());
        assert!(
            webdav_proxy(&fields, None).is_none(),
            "inherit 无全局来源应直连"
        );
        assert!(
            webdav_proxy(&fields, Some("socks5://9.9.9.9:9")).is_some(),
            "inherit 应继承全局代理"
        );
        // custom 合法 URL：带代理。
        fields.proxy_mode = Some("custom".into());
        fields.proxy_url = Some("socks5://user:pass@10.0.0.1:1080".into());
        assert!(
            webdav_proxy(&fields, Some("socks5://9.9.9.9:9")).is_some(),
            "custom 合法 URL 应带代理"
        );
        // custom 非法 URL：忽略代理直连（保存时已被 validate 拦下）。
        fields.proxy_url = Some("ftp://127.0.0.1:21".into());
        assert!(
            webdav_proxy(&fields, None).is_none(),
            "custom 非法 URL 应直连"
        );
        // 客户端构建两条路径都不应 panic。
        fields.proxy_url = Some("socks5://10.0.0.1:1080".into());
        let _ = webdav_client(&fields, None);
        fields.proxy_mode = Some("direct".into());
        let _ = webdav_client(&fields, Some("socks5://9.9.9.9:9"));
    }
}
