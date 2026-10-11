//! S3 凭据类型。

use schemars::JsonSchema;
use schemars::schema::RootSchema;
use schemars::schema_for;
use serde::{Deserialize, Serialize};

use super::{ConnectivityOutcome, CredentialType};

/// S3 凭据类型标识。
pub const TYPE_ID: &str = "s3";

/// S3 凭据字段。
#[derive(Clone, Serialize, Deserialize, JsonSchema)]
pub struct S3Fields {
    /// Access Key。
    pub access_key: String,
    /// Secret Key。
    pub secret_key: String,
    /// Session Token（临时凭据，可选）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_token: Option<String>,
    /// S3 endpoint（含 scheme），如 `https://s3.example.com`。
    pub endpoint: String,
    /// 区域，如 `us-east-1`。
    pub region: String,
    /// 桶名。
    pub bucket: String,
    /// 对象键前缀（可选）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prefix: Option<String>,
}

impl std::fmt::Debug for S3Fields {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("S3Fields")
            .field("access_key", &self.access_key)
            .field("secret_key", &acmecast_core::REDACTED)
            .field("session_token", &acmecast_core::redact_presence(&self.session_token))
            .finish()
    }
}

/// S3 凭据类型。
#[derive(Debug, Default)]
pub struct S3CredentialType;

#[async_trait::async_trait]
impl CredentialType for S3CredentialType {
    fn type_id(&self) -> &'static str {
        TYPE_ID
    }

    fn display_name(&self) -> &'static str {
        "S3 兼容存储"
    }

    fn fields_schema(&self) -> RootSchema {
        s3_fields_schema()
    }

    async fn test_connectivity(&self, fields: &serde_json::Value) -> ConnectivityOutcome {
        let raw = fields.clone();
        let parsed: Result<S3Fields, _> = serde_json::from_value(fields.clone());
        let parsed_fields = match parsed {
            Ok(f) if !f.access_key.trim().is_empty() && !f.secret_key.trim().is_empty() && !f.endpoint.trim().is_empty() && !f.region.trim().is_empty() && !f.bucket.trim().is_empty() => f,
            Ok(_) => {
                return ConnectivityOutcome::Unavailable {
                    reason: "access_key/secret_key/endpoint/region/bucket 均不能为空".to_owned(),
                }
            }
            Err(e) => return ConnectivityOutcome::Unavailable { reason: e.to_string() },
        };
        match s3_probe(&parsed_fields).await {
            Ok(()) => ConnectivityOutcome::Ok,
            Err(e) => ConnectivityOutcome::Unavailable { reason: self.redact(&raw, &e) },
        }
    }

    fn validate(&self, fields: &serde_json::Value) -> acmecast_core::Result<()> {
        let parsed: S3Fields = serde_json::from_value(fields.clone())
            .map_err(|e| acmecast_core::Error::validation("fields", e.to_string()))?;
        if parsed.access_key.trim().is_empty() {
            return Err(acmecast_core::Error::validation("access_key", "不能为空"));
        }
        if parsed.secret_key.trim().is_empty() {
            return Err(acmecast_core::Error::validation("secret_key", "不能为空"));
        }
        if parsed.endpoint.trim().is_empty() {
            return Err(acmecast_core::Error::validation("endpoint", "不能为空"));
        }
        if parsed.region.trim().is_empty() {
            return Err(acmecast_core::Error::validation("region", "不能为空"));
        }
        if parsed.bucket.trim().is_empty() {
            return Err(acmecast_core::Error::validation("bucket", "不能为空"));
        }
        Ok(())
    }
}

/// S3 凭据字段 schema：注入 `x-field-order` 钉住渲染顺序（serde_json Map 会按
/// 字母序重排 `properties`），使三组成对字段按业务分组两两成行；不注入宽度扩展。
#[must_use]
pub fn s3_fields_schema() -> RootSchema {
    let mut schema = schema_for!(S3Fields);
    schema.schema.extensions.insert(
        "x-field-order".to_owned(),
        serde_json::json!([
            "access_key",
            "secret_key",
            "endpoint",
            "bucket",
            "region",
            "prefix",
            "session_token"
        ]),
    );
    schema
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn s3_fields_schema_pins_field_order() {
        let schema = s3_fields_schema();
        let order = schema
            .schema
            .extensions
            .get("x-field-order")
            .and_then(|v| v.as_array())
            .expect("应注入 x-field-order");
        let names: Vec<&str> = order
            .iter()
            .filter_map(|v| v.as_str())
            .collect();
        assert_eq!(
            names,
            [
                "access_key",
                "secret_key",
                "endpoint",
                "bucket",
                "region",
                "prefix",
                "session_token"
            ]
        );
        let props = schema
            .schema
            .object
            .expect("应为对象 schema")
            .properties;
        for key in names {
            assert!(props.contains_key(key), "properties 缺少字段 {key}");
        }
    }
}

async fn s3_probe(fields: &S3Fields) -> Result<(), String> {
    use aws_config::Region;
    use aws_credential_types::Credentials;
    use aws_sdk_s3::config::Builder as S3ConfigBuilder;
    use aws_sdk_s3::Client;
    use aws_smithy_types::body::SdkBody;

    let region = Region::new(fields.region.clone());
    let creds = Credentials::new(
        fields.access_key.clone(),
        fields.secret_key.clone(),
        fields.session_token.clone(),
        None,
        "acmecast-probe",
    );
    let mut s3_config = S3ConfigBuilder::new()
        .region(region)
        .credentials_provider(creds)
        .behavior_version_latest();
    // R2/MinIO 需要自定义 endpoint
    let endpoint = fields.endpoint.trim().trim_end_matches('/').to_owned();
    if !endpoint.is_empty() {
        s3_config = s3_config.endpoint_url(endpoint).force_path_style(true);
    }
    let config = s3_config.build();
    let client = Client::from_conf(config);

    let prefix = fields.prefix.as_deref().unwrap_or("").trim().trim_matches('/').to_owned();
    let key = if prefix.is_empty() {
        ".acmecast-probe/healthcheck.txt".to_owned()
    } else {
        format!("{}/.acmecast-probe/healthcheck.txt", prefix)
    };
    let body = SdkBody::from("acmecast probe");
    if let Err(e) = client
        .put_object()
        .bucket(fields.bucket.clone())
        .key(key.clone())
        .body(body.into())
        .send()
        .await
    {
        return Err(format!("S3 上传探测失败: {e:?}"));
    }
    if let Err(e) = client.delete_object().bucket(fields.bucket.clone()).key(key).send().await {
        return Err(format!("S3 删除探测文件失败: {e:?}"));
    }
    Ok(())
}

