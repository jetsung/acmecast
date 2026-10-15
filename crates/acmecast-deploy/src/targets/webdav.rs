//! WebDAV 部署：把证书链与私钥 PUT 到 WebDAV 资源。

use acmecast_access::CredentialStore;
use async_trait::async_trait;
use schemars::schema_for;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::error::{Error, Result};
use acmecast_cert::parse_pem_leaf;
use crate::target::{CertMaterials, DeployMode, DeployOutcome, DeploymentTarget, parse_input};

const ACCEPTED_CREDENTIAL_TYPES: [&str; 1] = ["deploy.webdav"];

#[derive(Debug)]
pub struct WebdavTarget {
    global_proxy_url: Option<String>,
    network_timeout: std::time::Duration,
}

impl Default for WebdavTarget {
    fn default() -> Self {
        Self { global_proxy_url: None, network_timeout: acmecast_core::NETWORK_TIMEOUT }
    }
}

impl WebdavTarget {
    #[must_use]
    pub fn new(proxy_url: Option<&str>, network_timeout: std::time::Duration) -> Self {
        let global_proxy_url = proxy_url.map(str::trim).filter(|s| !s.is_empty()).map(str::to_owned);
        Self { global_proxy_url, network_timeout }
    }
    #[must_use]
    pub fn with_timeout(network_timeout: std::time::Duration) -> Self {
        Self { global_proxy_url: None, network_timeout }
    }
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct WebdavInput {
    /// 引用一张 `webdav` 类型凭据（凭据中含 url/base_path）。
    pub credential_id: i64,
    /// 基路径覆盖：留空则使用凭据中的 `base_path`。
    #[serde(default)]
    pub base_path: Option<String>,
    /// 完整链对象路径模板；可选，含 `{domain}` 占位，未填时派生 `<prefix>.fullchain.cer`。
    #[serde(default)]
    pub fullchain_path: Option<String>,
    /// （存量别名）证书链路径模板：`fullchain_path` 未配置时生效，视为 fullchain。
    #[serde(default)]
    pub cert_path: Option<String>,
    /// 中间链路径模板；可选，未填时派生 `<prefix>.ca.cer`，显式留空则跳过上传。
    #[serde(default)]
    pub ca_path: Option<String>,
    /// 私钥对象路径模板；可选，未填时跟随 cert.store 的本地文件名。
    #[serde(default)]
    pub key_path: Option<String>,
    /// 跳过 TLS 证书校验。
    #[serde(default)]
    pub insecure_skip_verify: bool,
}

fn normalize_base(raw: &Option<String>) -> String {
    let s = raw.as_deref().unwrap_or("").trim().trim_matches('/').to_owned();
    if s.is_empty() {
        String::new()
    } else {
        format!("{s}/")
    }
}

fn render_path(template: &Option<String>, domain: &str, suffix: &str) -> String {
    let safe = acmecast_cert::domain_file_prefix(domain);
    if let Some(t) = template.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        t.replace("{domain}", &safe)
    } else {
        format!("{safe}.{suffix}")
    }
}

#[async_trait]
impl DeploymentTarget for WebdavTarget {
    fn type_id(&self) -> &'static str {
        "webdav"
    }

    fn display_name(&self) -> &'static str {
        "WebDAV"
    }

    fn input_schema(&self) -> schemars::schema::RootSchema {
        let mut schema = schema_for!(WebdavInput);
        use schemars::schema::Schema as S;
        if let Some(S::Object(credential_id)) = schema
            .schema
            .object
            .as_mut()
            .and_then(|o| o.properties.get_mut("credential_id"))
        {
            credential_id
                .extensions
                .insert("x-credential-types".into(), json!(["deploy.webdav"]));
        }
        schema.schema.extensions.insert(
            "x-field-order".into(),
            json!(["credential_id", "base_path", "fullchain_path", "ca_path", "key_path", "insecure_skip_verify", "cert_path"]),
        );
        schema
    }

    fn example_input(&self) -> Value {
        json!({
            "credential_id": 1,
            "base_path": "certs/"
        })
    }

    async fn deploy(
        &self,
        input: &Value,
        materials: &CertMaterials,
        credentials: &CredentialStore<'_>,
        mode: DeployMode,
    ) -> Result<DeployOutcome> {
        let input: WebdavInput = parse_input(input)?;
        let resolved = credentials
            .resolve(input.credential_id)
            .await
            .map_err(|e| Error::Remote(format!("取凭据 `{}` 失败: {e}", input.credential_id)))?;
        if !ACCEPTED_CREDENTIAL_TYPES.contains(&resolved.type_id.as_str()) {
            return Err(Error::invalid_input(
                "credential_id",
                format!("凭据 `{}`（{}）不是 deploy.webdav 类型", input.credential_id, resolved.type_id),
            ));
        }
        let fields = resolved.fields.clone();
        let vault_url = fields.get("url").and_then(|v| v.as_str()).unwrap_or("").to_owned();
        let vault_base = fields.get("base_path").and_then(|v| v.as_str()).map(str::to_owned);
        let base = normalize_base(&input.base_path.clone().or(vault_base));
        let url = if vault_url.trim().is_empty() { return Err(Error::invalid_input("credential_id", "凭据缺少 url")); } else { vault_url };
        let domain = primary_domain_of(materials)
            .unwrap_or_else(|_| "certificate".to_owned());
        // 3 对象：fullchain 原样、ca（首块之后剩余，单张时空对象，显式留空跳过）、key。
        let fullchain_template = input.fullchain_path.clone().or(input.cert_path.clone());
        let fullchain_name = render_path(&fullchain_template, &domain, "fullchain.cer");
        let ca_name = match input.ca_path.as_deref() {
            // 显式留空：跳过 ca 上传。
            Some(raw) if raw.trim().is_empty() => None,
            Some(raw) => Some(render_path(&Some(raw.to_owned()), &domain, "ca.cer")),
            None => Some(render_path(&None, &domain, "ca.cer")),
        };
        let key_name = render_path(&input.key_path, &domain, "key");
        let base_url = url.trim_end_matches('/');
        let fullchain_url = format!("{base_url}/{base}{fullchain_name}");
        let key_url = format!("{base_url}/{base}{key_name}");
        let ca_content = acmecast_cert::ca_pem_of(&materials.chain_pem);
        let ca_url = ca_name.map(|ca| format!("{base_url}/{base}{ca}"));
        let mut uploads: Vec<(&str, &str)> = vec![
            (&fullchain_url, &materials.chain_pem),
            (&key_url, &materials.key_pem),
        ];
        if let Some(ca) = &ca_url {
            uploads.insert(1, (ca.as_str(), &ca_content));
        }
        let paths: Vec<String> = uploads.iter().map(|(url, _)| (*url).to_owned()).collect();
        if mode == DeployMode::SkipWrite {
            return Ok(DeployOutcome::skipped(paths));
        }
        let username = fields.get("username").and_then(|v| v.as_str()).unwrap_or("").to_owned();
        let password = fields.get("password").and_then(|v| v.as_str()).unwrap_or("").to_owned();
        if username.is_empty() || password.is_empty() {
            return Err(Error::invalid_input("credential_id", "凭据缺少 username/password"));
        }

        let proxy_url = credential_proxy(&fields, self.global_proxy_url.as_deref());
        for (upload_url, content) in uploads {
            webdav_put(
                upload_url,
                &username,
                &password,
                content,
                input.insecure_skip_verify,
                proxy_url.as_deref(),
                self.network_timeout,
            )
            .await?;
        }
        Ok(DeployOutcome::written(paths))
    }
}


/// 从部署凭据字段取真实生效的代理 URL：`custom` 自带 URL 生效；
/// `direct`/`inherit`（deploy 链路无全局代理来源）返回 `None` 直连。
fn credential_proxy(fields: &Value, global_proxy_url: Option<&str>) -> Option<String> {
    acmecast_core::effective_proxy_url(fields, global_proxy_url)
}

async fn webdav_put(
    url: &str,
    username: &str,
    password: &str,
    content: &str,
    _insecure_skip_verify: bool,
    proxy_url: Option<&str>,
    timeout: std::time::Duration,
) -> Result<()> {
    let mut builder = reqwest::Client::builder();
    if let Some(proxy_url) = proxy_url {
        // 统一口径校验 scheme（reqwest 的 Proxy::all 不查 scheme 支持度）。
        if let Err(e) = acmecast_core::validate_proxy_url(proxy_url) {
            return Err(Error::invalid_input("proxy_url", e.to_string()));
        }
        let proxy = reqwest::Proxy::all(proxy_url)
            .map_err(|e| Error::invalid_input("proxy_url", format!("代理 URL 非法: {e}")))?;
        builder = builder.proxy(proxy);
    }
    // 请求级超时：MKCOL 与 PUT 都经这个客户端，无响应时以含目标 URL 的错误失败。
    let client = builder
        .timeout(timeout)
        .build()
        .map_err(|e| Error::Remote(format!("构造 HTTP 客户端失败: {e}")))?;
    // Ensure parent collection exists (MKCOL)
    if let Some(slash) = url.rfind('/') {
        let parent = &url[..slash];
        let mkcol = client.request(reqwest::Method::from_bytes(b"MKCOL").unwrap(), parent)
            .basic_auth(username, Some(password))
            .send()
            .await;
        // MKCOL 失败忽略（已存在或不支持），继续 PUT
        let _ = mkcol;
    }
    let resp = client
        .put(url)
        .basic_auth(username, Some(password))
        .body(content.to_owned())
        .header("Content-Type", "application/x-pem-file")
        .send()
        .await
        .map_err(|e| Error::Remote(format!("WebDAV PUT {url} 失败: {e}")))?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(Error::Remote(format!("WebDAV PUT {url} 返回 {status}: {}", body.chars().take(300).collect::<String>())));
    }
    Ok(())
}

fn primary_domain_of(materials: &CertMaterials) -> Result<String> {
    let leaf = parse_pem_leaf(&materials.chain_pem).map_err(|e| Error::invalid_input("materials", format!("证书链解析失败: {e}")))?;
    leaf.primary_domain().map(str::to_owned).ok_or_else(|| Error::invalid_input("materials", "证书没有主域名"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credential_proxy_only_custom_applies() {
        let custom = json!({"proxy_mode": "custom", "proxy_url": "socks5://10.0.0.1:1080"});
        assert_eq!(credential_proxy(&custom, None).as_deref(), Some("socks5://10.0.0.1:1080"));
        assert_eq!(credential_proxy(&json!({"proxy_mode": "direct", "proxy_url": "socks5://1.2.3.4:7890"}), Some("http://proxy:8080")), None);
        assert_eq!(credential_proxy(&json!({"proxy_mode": "inherit"}), None), None);
        assert_eq!(credential_proxy(&json!({"proxy_mode": "inherit"}), Some("http://proxy:8080")).as_deref(), Some("http://proxy:8080"));
        assert_eq!(credential_proxy(&json!({"proxy_url": "http://10.0.0.2:8080"}), None).as_deref(), Some("http://10.0.0.2:8080"));
    }

    #[test]
    fn default_paths_align_three_file_layout() {
        assert_eq!(render_path(&None, "example.com", "fullchain.cer"), "example.com.fullchain.cer");
        assert_eq!(render_path(&None, "example.com", "ca.cer"), "example.com.ca.cer");
        assert_eq!(render_path(&None, "example.com", "key"), "example.com.key");
    }

    #[test]
    fn wildcard_uses_wildcard_prefix() {
        assert_eq!(
            render_path(&None, "*.example.com", "ca.cer"),
            "_wildcard.example.com.ca.cer"
        );
    }

    #[test]
    fn cert_path_aliases_fullchain() {
        let input: WebdavInput = serde_json::from_value(json!({
            "credential_id": 1,
            "cert_path": "{domain}.cer"
        }))
        .unwrap();
        let resolved = input.fullchain_path.clone().or(input.cert_path.clone());
        assert_eq!(render_path(&resolved, "example.com", "fullchain.cer"), "example.com.cer");
        // ca 仍按默认派生。
        assert_eq!(render_path(&None, "example.com", "ca.cer"), "example.com.ca.cer");
    }
}
