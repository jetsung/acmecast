//! S3 兼容存储部署：把证书链与私钥写入对象存储。

use acmecast_access::CredentialStore;
use async_trait::async_trait;
use schemars::schema::RootSchema;
use schemars::schema_for;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::error::{Error, Result};
use acmecast_cert::parse_pem_leaf;
use crate::target::{CertMaterials, DeployMode, DeployOutcome, DeploymentTarget, parse_input};

/// S3 凭据可接受的类型。
const ACCEPTED_CREDENTIAL_TYPES: [&str; 1] = ["deploy.s3"];
/// S3 部署目标。
#[derive(Debug)]
pub struct S3Target {
    global_proxy_url: Option<String>,
    network_timeout: std::time::Duration,
}

impl Default for S3Target {
    fn default() -> Self {
        Self { global_proxy_url: None, network_timeout: acmecast_core::NETWORK_TIMEOUT }
    }
}

impl S3Target {
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

/// S3 部署的输入。
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct S3Input {
    /// 引用一张 `s3` 类型凭据（凭据中含 endpoint/region/bucket/prefix）。
    pub credential_id: i64,
    /// 对象键前缀覆盖：留空则使用凭据中的 `prefix`。
    #[serde(default)]
    pub prefix: Option<String>,
    /// 完整链对象键模板；可选，含 `{domain}` 占位，未填时按主域名派生 `<prefix>.fullchain.cer`（`*` 已归一为 `_wildcard`）。
    #[serde(default)]
    pub fullchain_filename: Option<String>,
    /// （存量别名）证书链对象键模板：`fullchain_filename` 未配置时生效，视为 fullchain。
    #[serde(default)]
    pub cert_filename: Option<String>,
    /// 中间链对象键模板；可选，未填时派生 `<prefix>.ca.cer`，显式留空则跳过上传。
    #[serde(default)]
    pub ca_filename: Option<String>,
    /// 私钥对象键模板；可选，未填时跟随 cert.store 的本地文件名 `<prefix>/<主域>.key`（`*` 已归一为 `_wildcard`）。
    #[serde(default)]
    pub key_filename: Option<String>,
    /// 是否强制 path-style（对 MinIO 等自建存储常用）。
    #[serde(default)]
    pub force_path_style: bool,
    /// 跳过 TLS 证书校验（自签 S3 网关）。
    #[serde(default)]
    pub insecure_skip_verify: bool,
}

fn normalize_prefix(raw: &Option<String>) -> String {
    let p = raw.as_deref().unwrap_or("").trim().trim_matches('/').to_owned();
    if p.is_empty() {
        String::new()
    } else {
        format!("{p}/")
    }
}

fn render_filename(template: &Option<String>, domain: &str, suffix: &str) -> String {
    let safe = acmecast_cert::domain_file_prefix(domain);
    if let Some(t) = template.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        t.replace("{domain}", &safe)
    } else {
        format!("{safe}.{suffix}")
    }
}

#[async_trait]
impl DeploymentTarget for S3Target {
    fn type_id(&self) -> &'static str {
        "s3"
    }

    fn display_name(&self) -> &'static str {
        "S3 兼容存储"
    }

    fn input_schema(&self) -> RootSchema {
        let mut schema = schema_for!(S3Input);
        use schemars::schema::Schema as S;
        if let Some(S::Object(credential_id)) = schema
            .schema
            .object
            .as_mut()
            .and_then(|o| o.properties.get_mut("credential_id"))
        {
            credential_id
                .extensions
                .insert("x-credential-types".into(), json!(["deploy.s3"]));
        }
        schema.schema.extensions.insert(
            "x-field-order".into(),
            json!(["credential_id", "prefix", "fullchain_filename", "ca_filename", "key_filename", "force_path_style", "insecure_skip_verify", "cert_filename"]),
        );
        schema
    }

    fn example_input(&self) -> Value {
        json!({
            "credential_id": 1,
            "prefix": "acme/"
        })
    }

    async fn deploy(
        &self,
        input: &Value,
        materials: &CertMaterials,
        credentials: &CredentialStore<'_>,
        mode: DeployMode,
    ) -> Result<DeployOutcome> {
        let input: S3Input = parse_input(input)?;

        let resolved = credentials
            .resolve(input.credential_id)
            .await
            .map_err(|e| Error::Remote(format!("取凭据 `{}` 失败: {e}", input.credential_id)))?;
        if !ACCEPTED_CREDENTIAL_TYPES.contains(&resolved.type_id.as_str()) {
            return Err(Error::invalid_input(
                "credential_id",
                format!("凭据 `{}`（{}）不是 deploy.s3 类型", input.credential_id, resolved.type_id),
            ));
        }
        let fields: serde_json::Value = resolved.fields.clone();
        let domain = primary_domain_of(materials)
            .unwrap_or_else(|_| "certificate".to_owned());
        let resolved_prefix = input.prefix.clone().or_else(|| fields.get("prefix").and_then(|v| v.as_str()).map(str::to_owned));
        let prefix = normalize_prefix(&resolved_prefix);
        let endpoint = fields.get("endpoint").and_then(|v| v.as_str()).unwrap_or("").to_owned();
        let region = fields.get("region").and_then(|v| v.as_str()).unwrap_or("").to_owned();
        let bucket = fields.get("bucket").and_then(|v| v.as_str()).unwrap_or("").to_owned();
        if endpoint.trim().is_empty() || region.trim().is_empty() || bucket.trim().is_empty() {
            return Err(Error::invalid_input("credential_id", "凭据缺少 endpoint/region/bucket"));
        }
        // 3 对象：fullchain 原样、ca（首块之后剩余，单张时空对象，显式留空跳过）、key。
        let fullchain_template = input.fullchain_filename.clone().or(input.cert_filename.clone());
        let fullchain_key = format!("{}{}", prefix, render_filename(&fullchain_template, &domain, "fullchain.cer"));
        let ca_key = match input.ca_filename.as_deref() {
            // 显式留空：跳过 ca 对象上传。
            Some(raw) if raw.trim().is_empty() => None,
            Some(raw) => Some(format!("{}{}", prefix, render_filename(&Some(raw.to_owned()), &domain, "ca.cer"))),
            None => Some(format!("{}{}", prefix, render_filename(&None, &domain, "ca.cer"))),
        };
        let key_key = format!("{}{}", prefix, render_filename(&input.key_filename, &domain, "key"));
        let ca_content = acmecast_cert::ca_pem_of(&materials.chain_pem);
        let mut uploads: Vec<(&str, &str)> = vec![
            (&fullchain_key, &materials.chain_pem),
            (&key_key, &materials.key_pem),
        ];
        if let Some(ca) = &ca_key {
            uploads.insert(1, (ca.as_str(), &ca_content));
        }
        let paths: Vec<String> = uploads
            .iter()
            .map(|(key, _)| format!("s3://{bucket}/{key}"))
            .collect();
        if mode == DeployMode::SkipWrite {
            return Ok(DeployOutcome::skipped(paths));
        }
        let ak = fields.get("access_key").and_then(|v| v.as_str()).unwrap_or("").to_owned();
        let sk = fields.get("secret_key").and_then(|v| v.as_str()).unwrap_or("").to_owned();
        if ak.is_empty() || sk.is_empty() {
            return Err(Error::invalid_input("credential_id", "凭据缺少 access_key/secret_key"));
        }

        for (object_key, content) in uploads {
            s3_put(&fields, &bucket, object_key, content, self.global_proxy_url.as_deref(), self.network_timeout)
                .await
                .map_err(|e| Error::Remote(e.to_string()))?;
        }
        Ok(DeployOutcome::written(paths))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_keys_align_three_file_layout() {
        assert_eq!(
            render_filename(&None, "example.com", "fullchain.cer"),
            "example.com.fullchain.cer"
        );
        assert_eq!(render_filename(&None, "example.com", "ca.cer"), "example.com.ca.cer");
        assert_eq!(render_filename(&None, "example.com", "key"), "example.com.key");
    }

    #[test]
    fn wildcard_uses_wildcard_prefix() {
        assert_eq!(
            render_filename(&None, "*.example.com", "fullchain.cer"),
            "_wildcard.example.com.fullchain.cer"
        );
    }

    #[test]
    fn cert_filename_aliases_fullchain() {
        // 存量 cert_filename 未配置 fullchain_filename 时生效。
        let input: S3Input = serde_json::from_value(json!({
            "credential_id": 1,
            "cert_filename": "{domain}.cer"
        }))
        .unwrap();
        let resolved = input.fullchain_filename.clone().or(input.cert_filename.clone());
        assert_eq!(render_filename(&resolved, "example.com", "fullchain.cer"), "example.com.cer");
        // ca 仍按默认派生。
        assert_eq!(render_filename(&None, "example.com", "ca.cer"), "example.com.ca.cer");
    }

    #[test]
    fn empty_ca_filename_marks_skip() {
        let input: S3Input = serde_json::from_value(json!({
            "credential_id": 1,
            "ca_filename": ""
        }))
        .unwrap();
        let ca_key = match input.ca_filename.as_deref() {
            Some(raw) if raw.trim().is_empty() => None,
            _ => Some("ca".to_owned()),
        };
        assert!(ca_key.is_none(), "显式留空 ca_filename 应跳过上传");
    }
}

fn primary_domain_of(materials: &CertMaterials) -> Result<String> {
    let leaf = parse_pem_leaf(&materials.chain_pem).map_err(|e| Error::invalid_input("materials", format!("证书链解析失败: {e}")))?;
    leaf.primary_domain().map(str::to_owned).ok_or_else(|| Error::invalid_input("materials", "证书没有主域名"))
}

async fn s3_put(
    fields: &serde_json::Value,
    bucket: &str,
    key: &str,
    content: &str,
    global_proxy_url: Option<&str>,
    timeout: std::time::Duration,
) -> std::result::Result<(), String> {
    use aws_config::Region;
    use aws_credential_types::Credentials;
    use aws_sdk_s3::config::Builder as S3ConfigBuilder;
    use aws_sdk_s3::Client;
    use aws_smithy_types::body::SdkBody;
    let endpoint = fields.get("endpoint").and_then(|v| v.as_str()).unwrap_or("").trim().to_owned();
    let region = fields.get("region").and_then(|v| v.as_str()).unwrap_or("auto").to_owned();
    let ak = fields.get("access_key").and_then(|v| v.as_str()).unwrap_or("").to_owned();
    let sk = fields.get("secret_key").and_then(|v| v.as_str()).unwrap_or("").to_owned();
    let session = fields.get("session_token").and_then(|v| v.as_str()).map(str::to_owned);
    let region = Region::new(region);
    let creds = Credentials::new(ak, sk, session, None, "acmecast-deploy");
    let mut builder = S3ConfigBuilder::new().region(region).credentials_provider(creds).behavior_version_latest();
    if !endpoint.is_empty() {
        builder = builder.endpoint_url(endpoint).force_path_style(true);
    }
    // 出网代理：`inherit` 继承全局，`custom` 自带 URL
    if let Some(proxy) = acmecast_access::credential::s3_http::s3_http_client(
        acmecast_core::effective_proxy_url(fields, global_proxy_url).as_deref(),
    ) {
        builder = builder.http_client(proxy);
    }
    let config = builder.build();
    let client = Client::from_conf(config);
    let body = SdkBody::from(content.to_owned());
    // AWS SDK 无内建请求超时：put_object 的 future 受 tokio 超时约束，
    // 端点无响应时以含 bucket 与对象键的可读错误失败，而非永久挂起。
    tokio::time::timeout(
        timeout,
        client
            .put_object()
            .bucket(bucket.to_owned())
            .key(key.to_owned())
            .body(body.into())
            .send(),
    )
    .await
    .map_err(|_| {
        format!(
            "S3 PUT s3://{bucket}/{key} 超时（{} 秒内无响应）",
            timeout.as_secs()
        )
    })?
    .map_err(|e| format!("S3 PUT s3://{bucket}/{key} 失败: {e:?}"))?;
    Ok(())
}

