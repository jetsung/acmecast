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

/// WebDAV 凭据类型。
#[derive(Debug, Default)]
pub struct WebdavCredentialType;

#[async_trait::async_trait]
impl CredentialType for WebdavCredentialType {
    fn type_id(&self) -> &'static str {
        TYPE_ID
    }

    fn display_name(&self) -> &'static str {
        "WebDAV"
    }

    fn fields_schema(&self) -> RootSchema {
        let mut schema = schema_for!(WebdavFields);
        if let Some(pw) = schema
            .schema
            .object
            .as_mut()
            .and_then(|o| o.properties.get_mut("password"))
        {
            if let schemars::schema::Schema::Object(obj) = pw {
                obj.extensions.insert("x-multiline".into(), serde_json::Value::Bool(false));
            }
        }
        schema
    }

    async fn test_connectivity(&self, fields: &serde_json::Value) -> ConnectivityOutcome {
        let raw = fields.clone();
        let parsed: Result<WebdavFields, _> = serde_json::from_value(fields.clone());
        let f = match parsed {
            Ok(v) if !v.username.trim().is_empty() && !v.password.is_empty() && !v.url.trim().is_empty() => v,
            Ok(_) => return ConnectivityOutcome::Unavailable { reason: "username/password/url 均不能为空".to_owned() },
            Err(e) => return ConnectivityOutcome::Unavailable { reason: e.to_string() },
        };
        match webdav_probe(&f).await {
            Ok(()) => ConnectivityOutcome::Ok,
            Err(e) => ConnectivityOutcome::Unavailable { reason: self.redact(&raw, &e) },
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
        Ok(())
    }
}

async fn webdav_probe(fields: &WebdavFields) -> Result<(), String> {
    let base = fields.base_path.as_deref().unwrap_or("").trim().trim_matches('/').to_owned();
    let url = fields.url.trim_end_matches('/').to_owned();
    let prefix = if base.is_empty() { String::new() } else { format!("{}/", base) };
    let key = format!("{}/{}{}", url, prefix, ".acmecast-probe/healthcheck.txt");
    // MKCOL parent
    if let Some(slash) = key.rfind('/') {
        let parent = &key[..slash];
        let client = reqwest::Client::new();
        let _ = client.request(reqwest::Method::from_bytes(b"MKCOL").unwrap(), parent)
            .basic_auth(&fields.username, Some(&fields.password))
            .send()
            .await;
    }
    let client = reqwest::Client::new();
    let resp = client
        .put(&key)
        .basic_auth(&fields.username, Some(&fields.password))
        .body("acmecast probe")
        .send()
        .await
        .map_err(|e| format!("WebDAV 上传探测失败: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("WebDAV 上传探测失败: {} {}", resp.status(), resp.text().await.unwrap_or_default()));
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

