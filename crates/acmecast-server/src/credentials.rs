//! 内置 DNS 提供商与 SSH 主机的凭据类型。
//!
//! 字段定义直接复用领域 crate 里的结构体，避免「结构体改了、
//! schema 没改」的漂移。注册进凭据注册表后，前端就能创建这类凭据，
//! 流水线里的 `dns_credential_id` / SSH 档案引用下拉也才有可选项。
//!
//! 连通性测试：SSH 主机档案有一次真实的探测动作（连上主机认证一次）；
//! DNS 提供商沿用 trait 默认的 `NotTestable`——一次有效探测需要具体
//! zone，这里不猜一个域名去试。

use acmecast_access::{ConnectivityOutcome, CredentialType};
use acmecast_deploy::{SshHostFields, probe_host, ssh_host_fields_schema};
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

/// SSH 主机凭据类型（SSH 主机档案：主机、用户、认证材料与远端默认值）。
#[derive(Debug, Default)]
pub struct SshHostCredentialType;

#[async_trait::async_trait]
impl CredentialType for SshHostCredentialType {
    fn type_id(&self) -> &'static str {
        // 与 SSH 部署目标的 type_id 对齐：`cert.deploy` 的 SSH 配置引用它。
        "ssh"
    }

    fn display_name(&self) -> &'static str {
        "SSH 主机（部署）"
    }

    fn fields_schema(&self) -> RootSchema {
        // `private_key` 的多行标注（`x-multiline`）在这份 schema 里统一注入。
        ssh_host_fields_schema()
    }

    fn validate(&self, fields: &serde_json::Value) -> acmecast_core::Result<()> {
        let parsed: SshHostFields = serde_json::from_value(fields.clone()).map_err(|error| {
            acmecast_core::Error::validation("fields", format!("字段不合法: {error}"))
        })?;
        parsed.validate()
    }

    async fn test_connectivity(&self, fields: &serde_json::Value) -> ConnectivityOutcome {
        let Ok(parsed) = serde_json::from_value::<SshHostFields>(fields.clone()) else {
            return ConnectivityOutcome::Unavailable {
                reason: "档案字段不合法，无法测试".to_owned(),
            };
        };
        // 探测的报错文本可能回显请求里的字段值，先过一遍脱敏再呈现。
        match probe_host(&parsed).await {
            Ok(()) => ConnectivityOutcome::Ok,
            Err(error) => ConnectivityOutcome::Unavailable {
                reason: self.redact(fields, &error.to_string()),
            },
        }
    }

    /// 只抹认证材料。
    ///
    /// 默认实现会把**所有**字段值原样出现的地方抹掉——host 也会中招，
    /// 「连不上哪台」就看不出来了。host/port/user 是用户刚填的连接信息，
    /// 不是秘密；真正要挡住的只有私钥与口令。
    fn redact(&self, fields: &serde_json::Value, message: &str) -> String {
        let mut redacted = message.to_owned();
        for secret in [
            fields
                .get("private_key")
                .and_then(serde_json::Value::as_str),
            fields.get("password").and_then(serde_json::Value::as_str),
        ]
        .into_iter()
        .flatten()
        {
            // 太短的值不做替换：否则会把正常文本里的常见词一并抹掉。
            if secret.len() >= 4 {
                redacted = redacted.replace(secret, "***");
            }
        }
        redacted
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

    /// SSH 档案的标识必须与 SSH 部署目标一致，前端据此对齐语义。
    #[test]
    fn the_ssh_host_type_id_aligns_with_the_deploy_target() {
        assert_eq!(SshHostCredentialType.type_id(), "ssh");
        assert_eq!(SshHostCredentialType.display_name(), "SSH 主机（部署）");
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

    /// 一份可直接创建的档案字段。
    fn profile_fields() -> serde_json::Value {
        serde_json::json!({
            "host": "web-1.example.com",
            "user": "deploy",
            "private_key": "PRIVATE-KEY-MATERIAL",
        })
    }

    #[test]
    fn the_ssh_host_type_reuses_the_field_validation() {
        SshHostCredentialType
            .validate(&profile_fields())
            .expect("完整档案应通过");

        let mut broken = profile_fields();
        broken["private_key"] = serde_json::Value::Null;
        let err = SshHostCredentialType
            .validate(&broken)
            .expect_err("缺认证材料应被拒绝");
        match err {
            acmecast_core::Error::Validation { field, .. } => assert_eq!(field, "private_key"),
            other => panic!("期望 Validation，实际 {other:?}"),
        }

        let mut broken = profile_fields();
        broken["host"] = serde_json::json!("   ");
        let err = SshHostCredentialType
            .validate(&broken)
            .expect_err("空白主机应被拒绝");
        match err {
            acmecast_core::Error::Validation { field, .. } => assert_eq!(field, "host"),
            other => panic!("期望 Validation，实际 {other:?}"),
        }

        let mut broken = profile_fields();
        broken["password"] = serde_json::json!("pw");
        SshHostCredentialType
            .validate(&broken)
            .expect_err("两种认证材料同给应被拒绝");
    }

    #[test]
    fn the_ssh_host_schema_marks_the_private_key_multiline() {
        let rendered =
            serde_json::to_value(SshHostCredentialType.fields_schema()).expect("Schema 应可序列化");
        assert_eq!(
            rendered["properties"]["private_key"]["x-multiline"],
            serde_json::Value::Bool(true)
        );
    }

    #[test]
    fn the_ssh_host_schema_carries_only_profile_fields_with_form_defaults() {
        let rendered =
            serde_json::to_value(SshHostCredentialType.fields_schema()).expect("Schema 应可序列化");
        let properties = rendered["properties"]
            .as_object()
            .expect("schema 应有 properties");
        let mut names: Vec<_> = properties.keys().map(String::as_str).collect();
        names.sort_unstable();
        assert_eq!(
            names,
            vec![
                "cert_mode",
                "host",
                "key_mode",
                "password",
                "port",
                "private_key",
                "user"
            ],
            "档案 schema 只应含档案字段（路径与重载命令属部署输入）: {names:?}"
        );
        for banned in ["cert_path", "key_path", "reload_command"] {
            assert!(
                !properties.contains_key(banned),
                "凭据 schema 不得出现部署字段 {banned}"
            );
        }
        // 表单缺省值随 schema 带出，供 SchemaForm 预填。
        assert_eq!(rendered["properties"]["user"]["default"], "root");
        assert_eq!(rendered["properties"]["cert_mode"]["default"], "0644");
        assert_eq!(rendered["properties"]["key_mode"]["default"], "0600");
    }

    #[tokio::test]
    async fn ssh_host_connectivity_reports_unreachable_targets_without_the_materials() {
        let mut fields = profile_fields();
        fields["host"] = serde_json::json!("127.0.0.1");
        fields["port"] = serde_json::json!(1);

        let outcome = SshHostCredentialType.test_connectivity(&fields).await;
        let ConnectivityOutcome::Unavailable { reason } = outcome else {
            panic!("不可达主机应报不可用，实际 {outcome:?}");
        };
        // host 不是秘密，保留它用户才知道测的是哪台；材料必须被抹掉。
        assert!(reason.contains("127.0.0.1:1"), "{reason}");
        assert!(
            !reason.contains("PRIVATE-KEY-MATERIAL"),
            "脱敏后的原因不得含认证材料: {reason}"
        );
    }

    #[test]
    fn the_ssh_host_redact_only_hides_auth_materials() {
        let fields = serde_json::json!({
            "host": "web-1.example.com",
            "user": "deploy",
            "password": "S3CRET-PASSWORD",
        });
        let message = "连接 web-1.example.com 失败，请求携 S3CRET-PASSWORD";
        let redacted = SshHostCredentialType.redact(&fields, message);
        assert!(redacted.contains("web-1.example.com"), "{redacted}");
        assert!(!redacted.contains("S3CRET-PASSWORD"), "{redacted}");
    }

    #[tokio::test]
    async fn ssh_host_connectivity_rejects_malformed_fields() {
        let outcome = SshHostCredentialType
            .test_connectivity(&serde_json::json!({ "host": 1 }))
            .await;
        assert!(
            matches!(outcome, ConnectivityOutcome::Unavailable { .. }),
            "形态坏掉的字段应报不可用: {outcome:?}"
        );
    }
}
