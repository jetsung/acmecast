//! S3 兼容存储部署：把证书链与私钥写入对象存储。

use acmecast_access::CredentialStore;
use async_trait::async_trait;
use schemars::schema::RootSchema;
use schemars::schema_for;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::error::{Error, Result};
use crate::target::{CertMaterials, DeployMode, DeployOutcome, DeploymentTarget, parse_input};
use acmecast_cert::{ca_pem_of, parse_pem_leaf};

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
    /// 完整证书链对象键模板；可选，含 `{domain}` 占位。
    ///
    /// 未填时按主域派生 `<主域>.fullchain.cer`（`*` 已归一为 `_wildcard`）。
    /// 以 `/` 结尾视为目录，保存时按当前证书主域补为 `<prefix>.fullchain.cer`
    /// 等（通配 `*.example.com` → `_wildcard.example.com.*`；无主域时以
    /// `{domain}` 占位，部署时展开）。旧字段名 `cert_filename` 经 serde alias 兼容。
    #[serde(default, alias = "cert_filename")]
    pub fullchain_filename: Option<String>,
    /// 中间链对象键模板；可选，含 `{domain}` 占位，未填时按主域派生 `<主域>.ca.cer`。
    ///
    /// 以 `/` 结尾视为目录，保存时按当前证书主域补为 `<prefix>.ca.cer` 等
    /// （无主域时以 `{domain}` 占位，部署时展开）。
    #[serde(default)]
    pub ca_filename: Option<String>,
    /// 私钥对象键模板；可选，含 `{domain}` 占位，未填时按主域派生 `<主域>.key`。
    ///
    /// 以 `/` 结尾视为目录，保存时按当前证书主域补为 `<prefix>.key` 等
    /// （无主域时以 `{domain}` 占位，部署时展开）。
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
    let p = raw
        .as_deref()
        .unwrap_or("")
        .trim()
        .trim_matches('/')
        .to_owned();
    if p.is_empty() {
        String::new()
    } else {
        format!("{p}/")
    }
}

/// 渲染对象键模板；未填时按主域派生默认文件名。
///
/// 仍以 `/` 结尾的模板（历史配置未重保存）按目录兜底：拼上默认文件名，
/// 与保存时补全后的路径一致。`suffix` 用完整文件名（`fullchain.cer` 等），
/// 保持与 `certs/ssl/` 落盘命名同源。
fn render_filename(template: &Option<String>, domain: &str, suffix: &str) -> String {
    let safe = domain_file_prefix(domain);
    match template.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        Some(template) if template.ends_with('/') => format!("{template}{safe}.{suffix}"),
        Some(template) => template.replace("{domain}", &safe),
        None => format!("{safe}.{suffix}"),
    }
}

/// 补全 S3 对象键字段的目录写法。
///
/// 保存时由仓储层做同一补全；这里作为未重新保存的历史配置的执行期兜底。
/// 主域前缀已知时补 `<prefix>.fullchain.cer` 等带前缀对象键；未知时补成含
/// `{domain}` 的占位模板，真实前缀在部署时按主域展开。
pub(crate) fn normalize_s3_config(value: &mut Value, prefix: Option<&str>) {
    let Some(object) = value.as_object_mut() else {
        return;
    };
    super::normalize_joined_prefix(object, "prefix");
    let [fullchain_name, ca_name, key_name] = match prefix {
        Some(prefix) => [
            format!("{prefix}.fullchain.cer"),
            format!("{prefix}.ca.cer"),
            format!("{prefix}.key"),
        ],
        None => [
            "{domain}.fullchain.cer".to_owned(),
            "{domain}.ca.cer".to_owned(),
            "{domain}.key".to_owned(),
        ],
    };
    for (field, default_name) in [
        ("fullchain_filename", fullchain_name.as_str()),
        ("cert_filename", fullchain_name.as_str()),
        ("ca_filename", ca_name.as_str()),
        ("key_filename", key_name.as_str()),
    ] {
        super::fill_directory_field(object, field, default_name);
    }
}

/// 兼容存量输入：`fullchain_filename` 与旧别名 `cert_filename` 同时出现时
/// 丢弃旧键，双键同交会让 serde alias 报 `duplicate field`。
fn without_legacy_alias(input: &Value) -> Value {
    let mut value = input.clone();
    if let Some(object) = value.as_object_mut()
        && object
            .get("fullchain_filename")
            .is_some_and(|value| !value.is_null())
    {
        object.remove("cert_filename");
    }
    value
}

/// 要写入的对象：对象键（不含 prefix）与内容。
///
/// 单张链没有中间链可写时不含 ca 对象；`paths` 仍单独列出 3 个预期键，
/// 让调用方看到这次部署「本应」碰哪些对象。
fn s3_objects(input: &S3Input, materials: &CertMaterials, domain: &str) -> Vec<(String, String)> {
    let mut objects = vec![(
        render_filename(&input.fullchain_filename, domain, "fullchain.cer"),
        materials.chain_pem.clone(),
    )];
    let ca_pem = ca_pem_of(&materials.chain_pem);
    if !ca_pem.is_empty() {
        objects.push((
            render_filename(&input.ca_filename, domain, "ca.cer"),
            ca_pem,
        ));
    }
    objects.push((
        render_filename(&input.key_filename, domain, "key"),
        materials.key_pem.clone(),
    ));
    objects
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
    if cleaned.is_empty() {
        "certificate".to_owned()
    } else {
        cleaned
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
            json!([
                "credential_id",
                "prefix",
                "fullchain_filename",
                "ca_filename",
                "key_filename",
                "force_path_style",
                "insecure_skip_verify"
            ]),
        );
        schema
    }

    fn example_input(&self) -> Value {
        json!({
            "credential_id": 1,
            "prefix": "acme/",
            "fullchain_filename": "certs/",
            "ca_filename": "certs/",
            "key_filename": "certs/"
        })
    }

    async fn deploy(
        &self,
        input: &Value,
        materials: &CertMaterials,
        credentials: &CredentialStore<'_>,
        mode: DeployMode,
    ) -> Result<DeployOutcome> {
        // 目录写法在保存时已归一化；这里再补一次，覆盖未重新保存的历史配置。
        // 主域取本次部署的证书材料，与保存时按 `cert.apply` 主域补全同源。
        let mut normalized = input.clone();
        normalize_s3_config(
            &mut normalized,
            super::primary_domain_prefix(materials).as_deref(),
        );
        let input: S3Input = parse_input(&without_legacy_alias(&normalized))?;

        let resolved = credentials
            .resolve(input.credential_id)
            .await
            .map_err(|e| Error::Remote(format!("取凭据 `{}` 失败: {e}", input.credential_id)))?;
        if !ACCEPTED_CREDENTIAL_TYPES.contains(&resolved.type_id.as_str()) {
            return Err(Error::invalid_input(
                "credential_id",
                format!(
                    "凭据 `{}`（{}）不是 deploy.s3 类型",
                    input.credential_id, resolved.type_id
                ),
            ));
        }
        let fields: serde_json::Value = resolved.fields.clone();
        let domain = primary_domain_of(materials).unwrap_or_else(|_| "certificate".to_owned());
        let resolved_prefix = input.prefix.clone().or_else(|| {
            fields
                .get("prefix")
                .and_then(|v| v.as_str())
                .map(str::to_owned)
        });
        let prefix = normalize_prefix(&resolved_prefix);
        let endpoint = fields
            .get("endpoint")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_owned();
        let region = fields
            .get("region")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_owned();
        let bucket = fields
            .get("bucket")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_owned();
        if endpoint.trim().is_empty() || region.trim().is_empty() || bucket.trim().is_empty() {
            return Err(Error::invalid_input(
                "credential_id",
                "凭据缺少 endpoint/region/bucket",
            ));
        }
        // `prefix` 的尾部 `/` 与对象键的头部 `/` 由 `join_path` 统一归一：
        // 衔接处只有且仅有一个 `/`，协议部分（`s3://`）在下方单独组装。
        let fullchain_key = super::join_path(
            &prefix,
            &render_filename(&input.fullchain_filename, &domain, "fullchain.cer"),
        );
        let ca_key = super::join_path(
            &prefix,
            &render_filename(&input.ca_filename, &domain, "ca.cer"),
        );
        let key_key = super::join_path(
            &prefix,
            &render_filename(&input.key_filename, &domain, "key"),
        );
        let paths = vec![
            format!("s3://{bucket}/{fullchain_key}"),
            format!("s3://{bucket}/{ca_key}"),
            format!("s3://{bucket}/{key_key}"),
        ];
        if mode == DeployMode::SkipWrite {
            return Ok(DeployOutcome::skipped(paths));
        }
        let ak = fields
            .get("access_key")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_owned();
        let sk = fields
            .get("secret_key")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_owned();
        if ak.is_empty() || sk.is_empty() {
            return Err(Error::invalid_input(
                "credential_id",
                "凭据缺少 access_key/secret_key",
            ));
        }

        // 真实 S3 PUT，失败时由上层标记 pipeline 失败。单张链没有中间链可写：
        // 跳过 ca 对象（与 local/ssh 一致），但仍把预期键列在 paths 里。
        for (key, content) in s3_objects(&input, materials, &domain) {
            s3_put(&fields, &bucket, &super::join_path(&prefix, &key), &content)
                .await
                .map_err(|e| Error::Remote(e.to_string()))?;
        }
        Ok(DeployOutcome::written(paths))
    }
}

fn primary_domain_of(materials: &CertMaterials) -> Result<String> {
    let leaf = parse_pem_leaf(&materials.chain_pem)
        .map_err(|e| Error::invalid_input("materials", format!("证书链解析失败: {e}")))?;
    leaf.primary_domain()
        .map(str::to_owned)
        .ok_or_else(|| Error::invalid_input("materials", "证书没有主域名"))
}

async fn s3_put(
    fields: &serde_json::Value,
    bucket: &str,
    key: &str,
    content: &str,
) -> std::result::Result<(), String> {
    use aws_config::Region;
    use aws_credential_types::Credentials;
    use aws_sdk_s3::Client;
    use aws_sdk_s3::config::Builder as S3ConfigBuilder;
    use aws_smithy_types::body::SdkBody;
    let endpoint = fields
        .get("endpoint")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_owned();
    let region = fields
        .get("region")
        .and_then(|v| v.as_str())
        .unwrap_or("auto")
        .to_owned();
    let ak = fields
        .get("access_key")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_owned();
    let sk = fields
        .get("secret_key")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_owned();
    let session = fields
        .get("session_token")
        .and_then(|v| v.as_str())
        .map(str::to_owned);
    let region = Region::new(region);
    let creds = Credentials::new(ak, sk, session, None, "acmecast-deploy");
    let mut builder = S3ConfigBuilder::new()
        .region(region)
        .credentials_provider(creds)
        .behavior_version_latest();
    if !endpoint.is_empty() {
        builder = builder.endpoint_url(endpoint).force_path_style(true);
    }
    // 出网代理：仅 custom 自带 URL 生效（deploy 链路无全局代理来源）。
    if let Some(proxy) = acmecast_access::credential::s3_http::s3_http_client(
        acmecast_core::effective_proxy_url(fields, None).as_deref(),
    ) {
        builder = builder.http_client(proxy);
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

#[cfg(test)]
mod tests {
    use super::*;

    fn chained_materials() -> CertMaterials {
        CertMaterials::new(
            concat!(
                "-----BEGIN CERTIFICATE-----\n叶子\n-----END CERTIFICATE-----\n",
                "-----BEGIN CERTIFICATE-----\n中间\n-----END CERTIFICATE-----\n"
            ),
            "-----BEGIN PRIVATE KEY-----\n钥\n-----END PRIVATE KEY-----\n",
            "sha256:chain",
        )
    }

    fn single_cert_materials() -> CertMaterials {
        CertMaterials::new(
            "-----BEGIN CERTIFICATE-----\n单张\n-----END CERTIFICATE-----\n",
            "-----BEGIN PRIVATE KEY-----\n钥\n-----END PRIVATE KEY-----\n",
            "sha256:single",
        )
    }

    fn input(config: Value) -> S3Input {
        parse_input(&config).expect("配置应可解析")
    }

    #[test]
    fn default_keys_align_with_the_three_file_layout() {
        let input = input(json!({ "credential_id": 1 }));
        let keys: Vec<String> = s3_objects(&input, &chained_materials(), "example.com")
            .into_iter()
            .map(|(key, _)| key)
            .collect();

        assert_eq!(
            keys,
            vec![
                "example.com.fullchain.cer",
                "example.com.ca.cer",
                "example.com.key"
            ]
        );
    }

    #[test]
    fn wildcard_domains_use_the_wildcard_prefix() {
        let input = input(json!({ "credential_id": 1 }));
        let keys: Vec<String> = s3_objects(&input, &chained_materials(), "*.example.com")
            .into_iter()
            .map(|(key, _)| key)
            .collect();

        assert_eq!(keys[0], "_wildcard.example.com.fullchain.cer");
        assert_eq!(keys[1], "_wildcard.example.com.ca.cer");
        assert_eq!(keys[2], "_wildcard.example.com.key");
    }

    /// 绝对对象键（`/` 开头）与 prefix 衔接时只允许一个 `/`。
    #[test]
    fn absolute_keys_join_with_the_prefix_via_a_single_slash() {
        use crate::targets::join_path;
        assert_eq!(
            join_path("acme/", "/tmp/t3/_wildcard.w.idev.top.fullchain.cer"),
            "acme/tmp/t3/_wildcard.w.idev.top.fullchain.cer"
        );
        assert_eq!(
            format!(
                "s3://{}/{}",
                "backup",
                join_path("acme/", "/tmp/t3/_wildcard.w.idev.top.key")
            ),
            "s3://backup/acme/tmp/t3/_wildcard.w.idev.top.key"
        );
    }

    #[test]
    fn directory_templates_are_filled_with_domain_placeholders() {
        let mut raw = json!({
            "credential_id": 1,
            "prefix": "acme",
            "fullchain_filename": "certs/",
            "ca_filename": "certs/",
            "key_filename": "my-keys/"
        });

        normalize_s3_config(&mut raw, None);

        assert_eq!(raw["prefix"], "acme/");
        assert_eq!(raw["fullchain_filename"], "certs/{domain}.fullchain.cer");
        assert_eq!(raw["ca_filename"], "certs/{domain}.ca.cer");
        assert_eq!(raw["key_filename"], "my-keys/{domain}.key");

        let keys: Vec<String> = s3_objects(&input(raw), &chained_materials(), "example.com")
            .into_iter()
            .map(|(key, _)| key)
            .collect();
        assert_eq!(
            keys,
            vec![
                "certs/example.com.fullchain.cer",
                "certs/example.com.ca.cer",
                "my-keys/example.com.key"
            ]
        );
    }

    #[test]
    fn a_known_primary_domain_prefills_prefixed_object_keys() {
        let mut raw = json!({
            "credential_id": 1,
            "prefix": "acme",
            "fullchain_filename": "certs/",
            "ca_filename": "certs/",
            "key_filename": "my-keys/"
        });

        normalize_s3_config(&mut raw, Some("_wildcard.example.com"));

        assert_eq!(raw["prefix"], "acme/");
        assert_eq!(
            raw["fullchain_filename"],
            "certs/_wildcard.example.com.fullchain.cer"
        );
        assert_eq!(raw["ca_filename"], "certs/_wildcard.example.com.ca.cer");
        assert_eq!(raw["key_filename"], "my-keys/_wildcard.example.com.key");
    }

    #[test]
    fn a_single_cert_chain_skips_the_ca_object() {
        let input = input(json!({ "credential_id": 1 }));
        let objects = s3_objects(&input, &single_cert_materials(), "example.com");

        assert_eq!(objects.len(), 2, "单张链不应产生 ca 对象");
        assert_eq!(objects[0].0, "example.com.fullchain.cer");
        assert_eq!(objects[1].0, "example.com.key");
    }

    #[test]
    fn the_legacy_cert_filename_alias_still_parses() {
        let mut raw = json!({
            "credential_id": 1,
            "cert_filename": "backups/"
        });
        normalize_s3_config(&mut raw, None);
        let input: S3Input = parse_input(&without_legacy_alias(&raw)).unwrap();

        assert_eq!(
            input.fullchain_filename.as_deref(),
            Some("backups/{domain}.fullchain.cer")
        );
        assert_eq!(input.ca_filename, None);
    }

    #[test]
    fn the_schema_exposes_the_new_fields_and_directory_help() {
        let rendered = serde_json::to_string(&S3Target.input_schema()).unwrap();

        assert!(rendered.contains("fullchain_filename"), "{rendered}");
        assert!(rendered.contains("ca_filename"), "{rendered}");
        assert!(rendered.contains("key_filename"), "{rendered}");
        assert!(rendered.contains("以 `/` 结尾视为目录"), "{rendered}");
    }
}
