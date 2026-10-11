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
#[derive(Debug, Default)]
pub struct S3Target;

/// S3 部署的输入。
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct S3Input {
    /// 引用一张 `s3` 类型凭据（凭据中含 endpoint/region/bucket/prefix）。
    pub credential_id: i64,
    /// 对象键前缀覆盖：留空则使用凭据中的 `prefix`。
    #[serde(default)]
    pub prefix: Option<String>,
    /// 证书链对象键模板；可选，含 `{domain}` 占位，未填时跟随 cert.store 的本地文件名 `<prefix>/<主域>.cer`（`*` 已归一为 `_wildcard`）。
    #[serde(default)]
    pub cert_filename: Option<String>,
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
            json!(["credential_id", "prefix", "cert_filename", "key_filename", "force_path_style", "insecure_skip_verify"]),
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
        let cert_key = format!("{}{}", prefix, render_filename(&input.cert_filename, &domain, "cer"));
        let key_key = format!("{}{}", prefix, render_filename(&input.key_filename, &domain, "key"));
        let paths = vec![format!("s3://{bucket}/{cert_key}"), format!("s3://{bucket}/{key_key}")];
        if mode == DeployMode::SkipWrite {
            return Ok(DeployOutcome::skipped(paths));
        }
        let ak = fields.get("access_key").and_then(|v| v.as_str()).unwrap_or("").to_owned();
        let sk = fields.get("secret_key").and_then(|v| v.as_str()).unwrap_or("").to_owned();
        if ak.is_empty() || sk.is_empty() {
            return Err(Error::invalid_input("credential_id", "凭据缺少 access_key/secret_key"));
        }

        // 真实 S3 PUT，失败时由上层标记 pipeline 失败
        s3_put(&fields, &bucket, &cert_key, &materials.chain_pem).await.map_err(|e| Error::Remote(e.to_string()))?;
        s3_put(&fields, &bucket, &key_key, &materials.key_pem).await.map_err(|e| Error::Remote(e.to_string()))?;
        Ok(DeployOutcome::written(paths))
    }
}

fn primary_domain_of(materials: &CertMaterials) -> Result<String> {
    let leaf = parse_pem_leaf(&materials.chain_pem).map_err(|e| Error::invalid_input("materials", format!("证书链解析失败: {e}")))?;
    leaf.primary_domain().map(str::to_owned).ok_or_else(|| Error::invalid_input("materials", "证书没有主域名"))
}

async fn s3_put(fields: &serde_json::Value, bucket: &str, key: &str, content: &str) -> std::result::Result<(), String> {
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
    let config = builder.build();
    let client = Client::from_conf(config);
    let body = SdkBody::from(content.to_owned());
    client
        .put_object()
        .bucket(bucket.to_owned())
        .key(key.to_owned())
        .body(body.into())
        .send()
        .await
        .map_err(|e| format!("S3 PUT s3://{bucket}/{key} 失败: {e:?}"))?;
    Ok(())
}

