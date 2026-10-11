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
#[derive(Debug, Default)]
pub struct WebdavTarget;

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct WebdavInput {
    /// 引用一张 `webdav` 类型凭据（凭据中含 url/base_path）。
    pub credential_id: i64,
    /// 基路径覆盖：留空则使用凭据中的 `base_path`。
    #[serde(default)]
    pub base_path: Option<String>,
    /// 证书对象路径模板；可选，含 `{domain}`，未填时跟随 cert.store 的本地文件名。
    #[serde(default)]
    pub cert_path: Option<String>,
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
    let safe = domain_file_prefix(domain);
    if let Some(t) = template.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        t.replace("{domain}", &safe)
    } else {
        format!("{safe}.{suffix}")
    }
}

fn domain_file_prefix(domain: &str) -> String {
    let normalized = if let Some(stripped) = domain.strip_prefix("*.") {
        format!("_wildcard.{stripped}")
    } else if domain == "*" {
        "_wildcard".to_owned()
    } else {
        domain.to_owned()
    };
    let cleaned: String = normalized
        .chars()
        .map(|c| match c {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '.' | '-' | '_' => c,
            _ => '_',
        })
        .collect();
    if cleaned.is_empty() { "certificate".to_owned() } else { cleaned }
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
            json!(["credential_id", "base_path", "cert_path", "key_path", "insecure_skip_verify"]),
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
        let cert_name = render_path(&input.cert_path, &domain, "cer");
        let key_name = render_path(&input.key_path, &domain, "key");
        let base_url = url.trim_end_matches('/');
        let cert_url = format!("{base_url}/{base}{cert_name}");
        let key_url = format!("{base_url}/{base}{key_name}");
        let paths = vec![cert_url.clone(), key_url.clone()];
        if mode == DeployMode::SkipWrite {
            return Ok(DeployOutcome::skipped(paths));
        }
        let username = fields.get("username").and_then(|v| v.as_str()).unwrap_or("").to_owned();
        let password = fields.get("password").and_then(|v| v.as_str()).unwrap_or("").to_owned();
        if username.is_empty() || password.is_empty() {
            return Err(Error::invalid_input("credential_id", "凭据缺少 username/password"));
        }

        // 真实 WebDAV PUT
        webdav_put(&cert_url, &username, &password, &materials.chain_pem, input.insecure_skip_verify).await?;
        webdav_put(&key_url, &username, &password, &materials.key_pem, input.insecure_skip_verify).await?;
        Ok(DeployOutcome::written(paths))
    }
}


async fn webdav_put(url: &str, username: &str, password: &str, content: &str, _insecure_skip_verify: bool) -> Result<()> {
    let client = reqwest::Client::builder()
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
