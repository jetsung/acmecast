//! 内置 DNS 提供商的凭据类型。
//!
//! 字段定义直接复用 `acmecast-dns` 里各提供商的凭据结构体，避免「结构体改了、
//! schema 没改」的漂移。注册进凭据注册表后，前端就能创建这类凭据，
//! 流水线里的 `dns_credential_id` 下拉也才有可选项。
//!
//! 连通性测试沿用 trait 默认的 `NotTestable`：DNS 提供商的一次有效探测需要具体
//! zone，这里不猜一个域名去试。

use acmecast_access::CredentialType;
use acmecast_dns::providers::aliyun::AliyunCredentials;
use acmecast_dns::providers::cloudflare::CloudflareCredentials;
use schemars::schema::RootSchema;
use schemars::schema_for;

/// Cloudflare DNS 凭据类型（字段：`api_token`）。
#[derive(Debug, Default)]
pub struct CloudflareCredentialType;

#[async_trait::async_trait]
impl CredentialType for CloudflareCredentialType {
    fn type_id(&self) -> &'static str {
        // 与 `CloudflareProvider::type_id` 对齐：cert.apply 的 dns_provider 也用它。
        "cloudflare"
    }

    fn display_name(&self) -> &'static str {
        "Cloudflare（DNS）"
    }

    fn fields_schema(&self) -> RootSchema {
        schema_for!(CloudflareCredentials)
    }

    fn validate(&self, fields: &serde_json::Value) -> acmecast_core::Result<()> {
        let parsed: CloudflareCredentials =
            serde_json::from_value(fields.clone()).map_err(|error| {
                acmecast_core::Error::validation("api_token", format!("字段不合法: {error}"))
            })?;
        if parsed.api_token.trim().is_empty() {
            return Err(acmecast_core::Error::validation(
                "api_token",
                "API Token 不能为空白",
            ));
        }
        Ok(())
    }
}

/// 阿里云 DNS 凭据类型（字段：`access_key_id` / `access_key_secret`）。
#[derive(Debug, Default)]
pub struct AliyunCredentialType;

#[async_trait::async_trait]
impl CredentialType for AliyunCredentialType {
    fn type_id(&self) -> &'static str {
        // 与 `AliyunProvider::type_id` 对齐。
        "aliyun"
    }

    fn display_name(&self) -> &'static str {
        "阿里云（DNS）"
    }

    fn fields_schema(&self) -> RootSchema {
        schema_for!(AliyunCredentials)
    }

    fn validate(&self, fields: &serde_json::Value) -> acmecast_core::Result<()> {
        let parsed: AliyunCredentials =
            serde_json::from_value(fields.clone()).map_err(|error| {
                acmecast_core::Error::validation("access_key_id", format!("字段不合法: {error}"))
            })?;
        if parsed.access_key_id.trim().is_empty() {
            return Err(acmecast_core::Error::validation(
                "access_key_id",
                "AccessKey ID 不能为空白",
            ));
        }
        if parsed.access_key_secret.trim().is_empty() {
            return Err(acmecast_core::Error::validation(
                "access_key_secret",
                "AccessKey Secret 不能为空白",
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 类型标识必须与 DNS 提供商的 type_id 一致，否则 `dns_provider` 对不上。
    #[test]
    fn type_ids_match_dns_providers() {
        assert_eq!(CloudflareCredentialType.type_id(), "cloudflare");
        assert_eq!(AliyunCredentialType.type_id(), "aliyun");
    }

    #[test]
    fn cloudflare_requires_a_non_blank_token() {
        let err = CloudflareCredentialType
            .validate(&serde_json::json!({ "api_token": "   " }))
            .expect_err("空白 token 应被拒绝");
        match err {
            acmecast_core::Error::Validation { field, .. } => assert_eq!(field, "api_token"),
            other => panic!("期望 Validation，实际 {other:?}"),
        }

        CloudflareCredentialType
            .validate(&serde_json::json!({ "api_token": "token" }))
            .expect("非空 token 应通过");
    }

    #[test]
    fn aliyun_requires_both_halves() {
        AliyunCredentialType
            .validate(&serde_json::json!({ "access_key_id": "id" }))
            .expect_err("缺 secret 应被拒绝");
        let err = AliyunCredentialType
            .validate(&serde_json::json!({ "access_key_id": "id", "access_key_secret": " " }))
            .expect_err("空白 secret 应被拒绝");
        match err {
            acmecast_core::Error::Validation { field, .. } => {
                assert_eq!(field, "access_key_secret")
            }
            other => panic!("期望 Validation，实际 {other:?}"),
        }

        AliyunCredentialType
            .validate(&serde_json::json!({ "access_key_id": "id", "access_key_secret": "secret" }))
            .expect("两半齐全应通过");
    }
}
