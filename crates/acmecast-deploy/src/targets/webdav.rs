//! WebDAV 部署：把证书链与私钥 PUT 到 WebDAV 资源。

use acmecast_access::CredentialStore;
use async_trait::async_trait;
use schemars::schema_for;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::error::{Error, Result};
use crate::target::{CertMaterials, DeployMode, DeployOutcome, DeploymentTarget, parse_input};
use acmecast_cert::{ca_pem_of, parse_pem_leaf};

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
    /// 完整证书链路径模板；可选，含 `{domain}`，未填时按主域派生 `<主域>.fullchain.cer`。
    ///
    /// 以 `/` 结尾视为目录，保存时按当前证书主域补为 `<prefix>.fullchain.cer`
    /// 等（通配 `*.example.com` → `_wildcard.example.com.*`；无主域时以
    /// `{domain}` 占位，部署时展开）。旧字段名 `cert_path` 经 serde alias 兼容。
    #[serde(default, alias = "cert_path")]
    pub fullchain_path: Option<String>,
    /// 中间链路径模板；可选，含 `{domain}`，未填时按主域派生 `<主域>.ca.cer`。
    ///
    /// 以 `/` 结尾视为目录，保存时按当前证书主域补为 `<prefix>.ca.cer` 等
    /// （无主域时以 `{domain}` 占位，部署时展开）。
    #[serde(default)]
    pub ca_path: Option<String>,
    /// 私钥路径模板；可选，含 `{domain}`，未填时按主域派生 `<主域>.key`。
    ///
    /// 以 `/` 结尾视为目录，保存时按当前证书主域补为 `<prefix>.key` 等
    /// （无主域时以 `{domain}` 占位，部署时展开）。
    #[serde(default)]
    pub key_path: Option<String>,
    /// 跳过 TLS 证书校验。
    #[serde(default)]
    pub insecure_skip_verify: bool,
}

fn normalize_base(raw: &Option<String>) -> String {
    let s = raw
        .as_deref()
        .unwrap_or("")
        .trim()
        .trim_matches('/')
        .to_owned();
    if s.is_empty() {
        String::new()
    } else {
        format!("{s}/")
    }
}

/// 渲染路径模板；未填时按主域派生默认文件名。
///
/// 仍以 `/` 结尾的模板（历史配置未重保存）按目录兜底：拼上默认文件名，
/// 与保存时补全后的路径一致。`suffix` 用完整文件名（`fullchain.cer` 等），
/// 保持与 `certs/ssl/` 落盘命名同源。
fn render_path(template: &Option<String>, domain: &str, suffix: &str) -> String {
    let safe = domain_file_prefix(domain);
    match template.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        Some(template) if template.ends_with('/') => format!("{template}{safe}.{suffix}"),
        Some(template) => template.replace("{domain}", &safe),
        None => format!("{safe}.{suffix}"),
    }
}

/// 补全 WebDAV 路径字段的目录写法。
///
/// 保存时由仓储层做同一补全；这里作为未重新保存的历史配置的执行期兜底。
/// 主域前缀已知时补 `<prefix>.fullchain.cer` 等带前缀路径；未知时补成含
/// `{domain}` 的占位模板，真实前缀在部署时按主域展开。`cert_path` 是
/// `fullchain_path` 的旧别名。
pub(crate) fn normalize_webdav_config(value: &mut Value, prefix: Option<&str>) {
    let Some(object) = value.as_object_mut() else {
        return;
    };
    super::normalize_joined_prefix(object, "base_path");
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
        ("fullchain_path", fullchain_name.as_str()),
        ("cert_path", fullchain_name.as_str()),
        ("ca_path", ca_name.as_str()),
        ("key_path", key_name.as_str()),
    ] {
        super::fill_directory_field(object, field, default_name);
    }
}

/// 兼容存量输入：`fullchain_path` 与旧别名 `cert_path` 同时出现时丢弃旧键。
fn without_legacy_alias(input: &Value) -> Value {
    let mut value = input.clone();
    if let Some(object) = value.as_object_mut()
        && object
            .get("fullchain_path")
            .is_some_and(|value| !value.is_null())
    {
        object.remove("cert_path");
    }
    value
}

/// 三个预期 URL：fullchain / ca / key（无论 ca 是否有内容都列出）。
///
/// `base` 的尾部 `/` 与渲染后路径的头部 `/` 由 `join_path` 统一归一：
/// 衔接处只有且仅有一个 `/`，协议部分（`base_url` 里的 `https://`）不受影响。
fn webdav_paths(input: &WebdavInput, domain: &str, base: &str, base_url: &str) -> [String; 3] {
    let join = |template: &Option<String>, suffix: &str| {
        super::join_path(base, &render_path(template, domain, suffix))
    };
    [
        format!(
            "{base_url}/{}",
            join(&input.fullchain_path, "fullchain.cer")
        ),
        format!("{base_url}/{}", join(&input.ca_path, "ca.cer")),
        format!("{base_url}/{}", join(&input.key_path, "key")),
    ]
}

/// 实际要 PUT 的对象：单张链没有中间链时不含 ca，与 `local`/`ssh` 一致。
fn webdav_writes(
    input: &WebdavInput,
    materials: &CertMaterials,
    domain: &str,
    base: &str,
    base_url: &str,
) -> Vec<(String, String)> {
    let [fullchain_url, ca_url, key_url] = webdav_paths(input, domain, base, base_url);
    let mut writes = vec![(fullchain_url, materials.chain_pem.clone())];
    let ca_pem = ca_pem_of(&materials.chain_pem);
    if !ca_pem.is_empty() {
        writes.push((ca_url, ca_pem));
    }
    writes.push((key_url, materials.key_pem.clone()));
    writes
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
            json!([
                "credential_id",
                "base_path",
                "fullchain_path",
                "ca_path",
                "key_path",
                "insecure_skip_verify"
            ]),
        );
        schema
    }

    fn example_input(&self) -> Value {
        json!({
            "credential_id": 1,
            "base_path": "certs/",
            "fullchain_path": "live/",
            "ca_path": "live/",
            "key_path": "live/"
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
        normalize_webdav_config(
            &mut normalized,
            super::primary_domain_prefix(materials).as_deref(),
        );
        let input: WebdavInput = parse_input(&without_legacy_alias(&normalized))?;
        let resolved = credentials
            .resolve(input.credential_id)
            .await
            .map_err(|e| Error::Remote(format!("取凭据 `{}` 失败: {e}", input.credential_id)))?;
        if !ACCEPTED_CREDENTIAL_TYPES.contains(&resolved.type_id.as_str()) {
            return Err(Error::invalid_input(
                "credential_id",
                format!(
                    "凭据 `{}`（{}）不是 deploy.webdav 类型",
                    input.credential_id, resolved.type_id
                ),
            ));
        }
        let fields = resolved.fields.clone();
        let vault_url = fields
            .get("url")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_owned();
        let vault_base = fields
            .get("base_path")
            .and_then(|v| v.as_str())
            .map(str::to_owned);
        let base = normalize_base(&input.base_path.clone().or(vault_base));
        let url = if vault_url.trim().is_empty() {
            return Err(Error::invalid_input("credential_id", "凭据缺少 url"));
        } else {
            vault_url
        };
        let domain = primary_domain_of(materials).unwrap_or_else(|_| "certificate".to_owned());
        let base_url = url.trim_end_matches('/');
        let paths = webdav_paths(&input, &domain, &base, base_url).to_vec();
        if mode == DeployMode::SkipWrite {
            return Ok(DeployOutcome::skipped(paths));
        }
        let username = fields
            .get("username")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_owned();
        let password = fields
            .get("password")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_owned();
        if username.is_empty() || password.is_empty() {
            return Err(Error::invalid_input(
                "credential_id",
                "凭据缺少 username/password",
            ));
        }

        // 真实 WebDAV PUT：凭据 proxy_mode=custom 时经代理出网（deploy 链路
        // 无全局代理来源，inherit 在此处等同直连）。
        let proxy_url = credential_proxy(&fields);
        for (url, content) in webdav_writes(&input, materials, &domain, &base, base_url) {
            webdav_put(
                &url,
                &username,
                &password,
                &content,
                input.insecure_skip_verify,
                proxy_url.as_deref(),
            )
            .await?;
        }
        Ok(DeployOutcome::written(paths))
    }
}

/// 从部署凭据字段取真实生效的代理 URL：`custom` 自带 URL 生效；
/// `direct`/`inherit`（deploy 链路无全局代理来源）返回 `None` 直连。
fn credential_proxy(fields: &Value) -> Option<String> {
    if acmecast_core::ProxyMode::from_credentials(fields) != acmecast_core::ProxyMode::Custom {
        return None;
    }
    acmecast_core::proxy_url_from_credentials(fields)
}

async fn webdav_put(
    url: &str,
    username: &str,
    password: &str,
    content: &str,
    _insecure_skip_verify: bool,
    proxy_url: Option<&str>,
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
    let client = builder
        .build()
        .map_err(|e| Error::Remote(format!("构造 HTTP 客户端失败: {e}")))?;
    // Ensure parent collection exists (MKCOL)
    if let Some(slash) = url.rfind('/') {
        let parent = &url[..slash];
        let mkcol = client
            .request(reqwest::Method::from_bytes(b"MKCOL").unwrap(), parent)
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
        return Err(Error::Remote(format!(
            "WebDAV PUT {url} 返回 {status}: {}",
            body.chars().take(300).collect::<String>()
        )));
    }
    Ok(())
}

fn primary_domain_of(materials: &CertMaterials) -> Result<String> {
    let leaf = parse_pem_leaf(&materials.chain_pem)
        .map_err(|e| Error::invalid_input("materials", format!("证书链解析失败: {e}")))?;
    leaf.primary_domain()
        .map(str::to_owned)
        .ok_or_else(|| Error::invalid_input("materials", "证书没有主域名"))
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

    fn input(config: Value) -> WebdavInput {
        parse_input(&config).expect("配置应可解析")
    }

    #[test]
    fn default_paths_align_with_the_three_file_layout() {
        let input = input(json!({ "credential_id": 1 }));
        let paths = webdav_paths(&input, "example.com", "certs/", "https://dav.example.com");

        assert_eq!(
            paths,
            [
                "https://dav.example.com/certs/example.com.fullchain.cer".to_owned(),
                "https://dav.example.com/certs/example.com.ca.cer".to_owned(),
                "https://dav.example.com/certs/example.com.key".to_owned(),
            ]
        );
    }

    #[test]
    fn wildcard_domains_use_the_wildcard_prefix() {
        let input = input(json!({ "credential_id": 1 }));
        let paths = webdav_paths(&input, "*.example.com", "", "https://dav.example.com");

        assert!(paths[0].ends_with("/_wildcard.example.com.fullchain.cer"));
        assert!(paths[1].ends_with("/_wildcard.example.com.ca.cer"));
        assert!(paths[2].ends_with("/_wildcard.example.com.key"));
    }

    #[test]
    fn directory_templates_are_filled_with_domain_placeholders() {
        let mut raw = json!({
            "credential_id": 1,
            "base_path": "/dav/certs/",
            "fullchain_path": "live/",
            "ca_path": "live/",
            "key_path": "live/"
        });

        normalize_webdav_config(&mut raw, None);

        assert_eq!(raw["base_path"], "dav/certs/");
        assert_eq!(raw["fullchain_path"], "live/{domain}.fullchain.cer");
        assert_eq!(raw["ca_path"], "live/{domain}.ca.cer");
        assert_eq!(raw["key_path"], "live/{domain}.key");

        let input = input(raw);
        let paths = webdav_paths(
            &input,
            "example.com",
            "dav/certs/",
            "https://dav.example.com",
        );
        assert_eq!(
            paths,
            [
                "https://dav.example.com/dav/certs/live/example.com.fullchain.cer".to_owned(),
                "https://dav.example.com/dav/certs/live/example.com.ca.cer".to_owned(),
                "https://dav.example.com/dav/certs/live/example.com.key".to_owned(),
            ]
        );
    }

    /// 绝对路径（`/` 开头）与 base 衔接时只允许一个 `/`，协议部分保留。
    #[test]
    fn absolute_paths_join_with_a_single_slash() {
        let input = input(json!({
            "credential_id": 1,
            "base_path": "/dav/acmecast/",
            "fullchain_path": "/tmp/t3/_wildcard.w.idev.top.fullchain.cer",
            "ca_path": "/tmp/t3/_wildcard.w.idev.top.ca.cer",
            "key_path": "/tmp/t3/_wildcard.w.idev.top.key"
        }));

        let paths = webdav_paths(
            &input,
            "*.w.idev.top",
            "dav/acmecast/",
            "https://dav.asfd.cn/dav",
        );
        assert_eq!(
            paths,
            [
                "https://dav.asfd.cn/dav/dav/acmecast/tmp/t3/_wildcard.w.idev.top.fullchain.cer"
                    .to_owned(),
                "https://dav.asfd.cn/dav/dav/acmecast/tmp/t3/_wildcard.w.idev.top.ca.cer"
                    .to_owned(),
                "https://dav.asfd.cn/dav/dav/acmecast/tmp/t3/_wildcard.w.idev.top.key".to_owned(),
            ]
        );
    }

    #[test]
    fn a_known_primary_domain_prefills_prefixed_paths() {
        let mut raw = json!({
            "credential_id": 1,
            "base_path": "/dav/certs/",
            "fullchain_path": "/tmp/t1/",
            "ca_path": "/tmp/t1/",
            "key_path": "/tmp/t1/"
        });

        normalize_webdav_config(&mut raw, Some("_wildcard.example.com"));

        assert_eq!(raw["base_path"], "dav/certs/");
        assert_eq!(
            raw["fullchain_path"],
            "/tmp/t1/_wildcard.example.com.fullchain.cer"
        );
        assert_eq!(raw["ca_path"], "/tmp/t1/_wildcard.example.com.ca.cer");
        assert_eq!(raw["key_path"], "/tmp/t1/_wildcard.example.com.key");
    }

    #[test]
    fn a_single_cert_chain_puts_only_two_files() {
        let input = input(json!({ "credential_id": 1 }));
        let writes = webdav_writes(
            &input,
            &single_cert_materials(),
            "example.com",
            "",
            "https://dav.example.com",
        );

        assert_eq!(writes.len(), 2, "单张链不应 PUT ca 文件");
        assert!(writes[0].0.ends_with("example.com.fullchain.cer"));
        assert!(writes[1].0.ends_with("example.com.key"));
    }

    #[test]
    fn a_chained_chain_puts_three_files() {
        let input = input(json!({ "credential_id": 1 }));
        let writes = webdav_writes(
            &input,
            &chained_materials(),
            "example.com",
            "",
            "https://dav.example.com",
        );

        assert_eq!(writes.len(), 3);
        assert!(writes[1].1.contains("中间"), "ca 文件应是拆链剩余");
    }

    #[test]
    fn the_legacy_cert_path_alias_still_parses() {
        let mut raw = json!({
            "credential_id": 1,
            "cert_path": "backups/"
        });
        normalize_webdav_config(&mut raw, None);
        let input: WebdavInput = parse_input(&without_legacy_alias(&raw)).unwrap();

        assert_eq!(
            input.fullchain_path.as_deref(),
            Some("backups/{domain}.fullchain.cer")
        );
        assert_eq!(input.ca_path, None);
    }

    #[test]
    fn the_schema_exposes_the_new_fields_and_directory_help() {
        let rendered = serde_json::to_string(&WebdavTarget.input_schema()).unwrap();

        assert!(rendered.contains("fullchain_path"), "{rendered}");
        assert!(rendered.contains("ca_path"), "{rendered}");
        assert!(rendered.contains("以 `/` 结尾视为目录"), "{rendered}");
    }

    /// 部署侧代理派生：仅 `custom` 自带 URL 生效，其余直连。
    #[test]
    fn credential_proxy_only_custom_applies() {
        let custom = json!({"proxy_mode": "custom", "proxy_url": "socks5://10.0.0.1:1080"});
        assert_eq!(
            credential_proxy(&custom).as_deref(),
            Some("socks5://10.0.0.1:1080")
        );
        assert_eq!(
            credential_proxy(
                &json!({"proxy_mode": "direct", "proxy_url": "socks5://1.2.3.4:7890"})
            ),
            None
        );
        assert_eq!(credential_proxy(&json!({"proxy_mode": "inherit"})), None);
        // 存量兼容：无 proxy_mode 但旧 proxy_url 有值 → custom。
        assert_eq!(
            credential_proxy(&json!({"proxy_url": "http://10.0.0.2:8080"})).as_deref(),
            Some("http://10.0.0.2:8080")
        );
    }
}
