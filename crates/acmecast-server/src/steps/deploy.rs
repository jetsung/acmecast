//! `cert.deploy`：把证书材料部署到指定目标。
//!
//! 幂等判断、部署记录与「跳过写入但重载照走」的语义都在
//! [`Deployer`](acmecast_deploy::Deployer) 里，本步骤只负责把产物与输入
//! 交给它——编排层不重复实现幂等，两处实现必然漂移。

use std::sync::Arc;

use acmecast_deploy::{CertMaterials, Deployer, DeploymentRegistry, DeploymentStateStore};
use acmecast_pipeline::{PipelineStep, Result, StepContext, StepOutput};
use schemars::JsonSchema;
use serde::Deserialize;

use super::domain_error;

fn platform_summary(input: &CertDeployInput) -> String {
    let target = input.target.trim();
    let cfg = &input.config;
    let obj = cfg.as_object();
    let get = |k: &str| obj.and_then(|o| o.get(k)).and_then(|v| v.as_str()).map(|s| s.trim()).filter(|s| !s.is_empty()).map(|s| s.to_owned());
    match target {
        "s3" => {
            let bucket = get("bucket").or_else(|| get("Bucket"));
            let region = get("region").or_else(|| get("Region"));
            let endpoint = get("endpoint").or_else(|| get("Endpoint")).or_else(|| get("endpoint_url"));
            let host = endpoint.as_deref().and_then(|u| url_host(u));
            if let Some(b) = bucket { if let Some(r) = region { format!("{b}/{r}") } else if let Some(h) = host { format!("{b}@{h}") } else { b } } else if let Some(h) = host { h } else if let Some(r) = region { r } else { "unknown".to_owned() }
        }
        "ssh" => {
            let host = get("host").or_else(|| get("hostname"));
            let port = get("port");
            if let Some(h) = host { if let Some(p) = port { format!("{h}:{p}") } else { h } } else { "unknown".to_owned() }
        }
        "tencent-ssl" => {
            let cid = obj.and_then(|o| o.get("credential_id")).and_then(|v| v.as_i64()).map(|n| n.to_string());
            let region = get("region").or_else(|| get("Region"));
            match (cid, region) { (Some(c), Some(r)) => format!("credential={c} region={r}"), (Some(c), None) => format!("credential={c}"), (None, Some(r)) => r, _ => "unknown".to_owned() }
        }
        "local" => get("cert_path").or_else(|| get("certPath")).or_else(|| get("key_path")).unwrap_or_else(|| "unknown".to_owned()),
        "webdav" => { let url = get("url").or_else(|| get("endpoint")); url.as_deref().and_then(|u| url_host(u)).unwrap_or_else(|| "unknown".to_owned()) }
        _ => "unknown".to_owned(),
    }
}
fn url_host(raw: &str) -> Option<String> {
    let s = raw.trim(); if s.is_empty() { return None; }
    let without_scheme = s.split("://").last().unwrap_or(s);
    let host = without_scheme.split('/').next().unwrap_or(without_scheme).split('?').next().unwrap_or(without_scheme).trim();
    if host.is_empty() { None } else { Some(host.to_owned()) }
}

/// `cert.deploy` 的输入。
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct CertDeployInput {
    /// 部署目标类型（如 `local`、`ssh`）。
    pub target: String,
    /// 目标的输入配置，结构与所选目标声明的 schema 一致。
    pub config: serde_json::Value,
}

/// 部署步骤。
#[derive(Debug)]
pub struct CertDeployStep {
    registry: Arc<DeploymentRegistry>,
    state: Arc<dyn DeploymentStateStore>,
}

impl CertDeployStep {
    /// 用部署注册表与部署记录存储装配。
    #[must_use]
    pub fn new(registry: Arc<DeploymentRegistry>, state: Arc<dyn DeploymentStateStore>) -> Self {
        Self { registry, state }
    }
}

#[async_trait::async_trait]
impl PipelineStep for CertDeployStep {
    fn type_id(&self) -> &'static str {
        "cert.deploy"
    }

    fn input_schema(&self) -> Option<schemars::schema::RootSchema> {
        let mut schema = schemars::schema_for!(CertDeployInput);
        // `config` 的结构随 `target` 变化，静态 schema 只能是无类型的 object；
        // 把各部署目标的输入 schema 与示例注入成扩展，前端据此让 config 的
        // 占位提示跟随 target 联动（见 SchemaForm 的 `x-target-schemas`）。
        let targets: serde_json::Map<String, serde_json::Value> = self
            .registry
            .list()
            .into_iter()
            .map(|target| {
                (
                    target.type_id().to_owned(),
                    serde_json::json!({
                        "display_name": target.display_name(),
                        "example": target.example_input(),
                        "schema": target.input_schema(),
                    }),
                )
            })
            .collect();
        if let Some(schemars::schema::Schema::Object(config)) = schema
            .schema
            .object
            .as_mut()
            .and_then(|object| object.properties.get_mut("config"))
        {
            config.extensions.insert(
                "x-target-schemas".to_owned(),
                serde_json::Value::Object(targets),
            );
        }
        Some(schema)
    }

    fn required_artifacts(&self) -> &'static [&'static str] {
        &["cert_pem", "key_pem", "fingerprint"]
    }

    async fn execute(&self, ctx: &mut StepContext<'_>) -> Result<StepOutput> {
        let input: CertDeployInput = ctx
            .input_as()
            .map_err(|error| domain_error("cert.deploy 输入不合法", error))?;

        let cert_pem: String = serde_json::from_value(
            ctx.artifact("cert_pem")
                .map_err(|e| domain_error("读取产物", e))?
                .clone(),
        )
        .map_err(|e| domain_error("cert_pem 产物", e))?;
        let key_pem: String = serde_json::from_value(
            ctx.artifact("key_pem")
                .map_err(|e| domain_error("读取产物", e))?
                .clone(),
        )
        .map_err(|e| domain_error("key_pem 产物", e))?;
        let fingerprint: String = serde_json::from_value(
            ctx.artifact("fingerprint")
                .map_err(|e| domain_error("读取产物", e))?
                .clone(),
        )
        .map_err(|e| domain_error("fingerprint 产物", e))?;

        let target = self
            .registry
            .require(input.target.trim())
            .map_err(|e| domain_error("部署目标", e))?;
        let deployer = Deployer::new(Arc::clone(&self.state));
        // force 已移除：仅 tencent-ssl 走指纹幂等，其余每次都写入
        let force = false;
        let deploy_result = deployer
            .deploy(
                target,
                &input.config,
                &CertMaterials::new(cert_pem, key_pem, fingerprint.clone()),
                ctx.credentials(),
                force,
            )
            .await;
        let outcome = match deploy_result {
            Ok(outcome) => outcome,
            Err(error) => {
                ctx.log_error(format!(
                    "部署失败：target={}, error={}",
                    target.type_id(),
                    error
                ));
                return Err(domain_error("执行部署", error));
            }
        };

        let platform = platform_summary(&input);
        let platform_tag = format!(" platform={platform}");
        // 腾讯云 SSL：把云端证书 ID 以结构化输出写入运行历史，使单次运行可自包含回溯。
        // 跳过写入时与普通日志合并为一条带 output 的日志，避免"跳过提示 + 结构化结果"两条冗余日志。
        // 平台信息在同一行前后追加，始终脱敏（platform 仅为脱敏后摘要，空值为 unknown）。
        if target.type_id() == "tencent-ssl" {
            if outcome.skipped_write {
                ctx.log_info_with_output(
                    format!(
                        "目标 {} 指纹未变，跳过写入：target={}{}, skipped_write=true, reason=fingerprint unchanged, fingerprint={fingerprint}；重载已按配置执行",
                        target.type_id(), target.type_id(), platform_tag,
                    ),
                    serde_json::json!({ "cert_ids": outcome.paths, "skipped_write": true, "platform": platform }),
                );
            } else {
                ctx.log_info(format!(
                    "已部署到目标 {} (platform={})，写入 {} 个路径：target={}, platform={}, fingerprint={}",
                    target.type_id(), platform, outcome.paths.len(), target.type_id(), platform, fingerprint,
                ));
                ctx.log_info_with_output("腾讯云 SSL 部署结果", serde_json::json!({ "cert_ids": outcome.paths, "skipped_write": false, "platform": platform }));
            }
        } else if outcome.skipped_write {
            ctx.log_info_with_output(
                format!(
                    "目标 {} 指纹未变，跳过写入：target={}{}, skipped_write=true, reason=fingerprint unchanged, fingerprint={fingerprint}；重载已按配置执行",
                    target.type_id(), target.type_id(), platform_tag,
                ),
                serde_json::json!({ "skipped_write": true, "platform": platform }),
            );
        } else {
            ctx.log_info(format!(
                "已部署到目标 {} (platform={})，写入 {} 个路径：target={}, platform={}, fingerprint={}",
                target.type_id(), platform, outcome.paths.len(), target.type_id(), platform, fingerprint,
            ));
        }

        Ok(StepOutput::empty()
            .with_artifact("deployed_paths", serde_json::json!(outcome.paths))
            .with_artifact("skipped_write", serde_json::json!(outcome.skipped_write)))
    }
}

#[cfg(test)]
mod tests_platform {
    use super::*;

    fn input(target: &str, config: serde_json::Value) -> CertDeployInput {
        CertDeployInput { target: target.to_owned(), config }
    }

    #[test]
    fn s3_platform_summary_variants() {
        assert_eq!(platform_summary(&input("s3", serde_json::json!({"bucket":"my-bucket","region":"auto"}))), "my-bucket/auto");
        assert_eq!(platform_summary(&input("s3", serde_json::json!({"bucket":"my-bucket","endpoint":"https://r2.cloudflarestorage.com"}))), "my-bucket@r2.cloudflarestorage.com");
        assert_eq!(platform_summary(&input("s3", serde_json::json!({"region":"us-east-1"}))), "us-east-1");
        assert_eq!(platform_summary(&input("s3", serde_json::json!({}))), "unknown");
    }

    #[test]
    fn ssh_and_tencent_and_webdav_platform() {
        assert_eq!(platform_summary(&input("ssh", serde_json::json!({"host":"10.0.0.1","port":"2222"}))), "10.0.0.1:2222");
        assert_eq!(platform_summary(&input("ssh", serde_json::json!({"host":"10.0.0.1"}))), "10.0.0.1");
        assert_eq!(platform_summary(&input("tencent-ssl", serde_json::json!({"credential_id": 7}))), "credential=7");
        assert_eq!(platform_summary(&input("tencent-ssl", serde_json::json!({"credential_id": 7, "region":"ap-guangzhou"}))), "credential=7 region=ap-guangzhou");
        assert_eq!(platform_summary(&input("webdav", serde_json::json!({"url":"https://dav.example.com/files"}))), "dav.example.com");
        assert_eq!(platform_summary(&input("webdav", serde_json::json!({}))), "unknown");
        assert_eq!(platform_summary(&input("local", serde_json::json!({"cert_path":"/etc/ssl/cert.pem"}))), "/etc/ssl/cert.pem");
        assert_eq!(platform_summary(&input("local", serde_json::json!({}))), "unknown");
        assert_eq!(platform_summary(&input("unknown-target", serde_json::json!({}))), "unknown");
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use acmecast_deploy::{DeploymentRegistry, InMemoryDeploymentState};

    use super::*;

    #[test]
    fn config_schema_carries_each_targets_example() {
        let mut registry = DeploymentRegistry::new();
        registry
            .register(acmecast_deploy::LocalTarget)
            .expect("首次注册应成功");
        registry
            .register(acmecast_deploy::SshTarget::live())
            .expect("首次注册应成功");
        registry
            .register(acmecast_deploy::TencentSslTarget::new())
            .expect("首次注册应成功");
        let step =
            CertDeployStep::new(Arc::new(registry), Arc::new(InMemoryDeploymentState::new()));

        let schema = step.input_schema().expect("cert.deploy 应声明输入结构");
        let rendered: serde_json::Value = serde_json::to_value(&schema).expect("序列化 schema");
        let targets = &rendered["properties"]["config"]["x-target-schemas"];

        // 每个已注册目标都要带 display_name、example 与 schema，
        // 前端的占位提示与未来的结构化校验都从这里取。
        assert_eq!(targets["local"]["display_name"], "本地文件系统");
        assert!(targets["local"]["example"]["cert_path"].is_string());
        assert_eq!(targets["local"]["schema"]["type"], "object");
        // SSH 的示例是档案式写法：一个 `credential_id` 打底，路径可选。
        assert!(targets["ssh"]["example"]["credential_id"].is_i64());
        assert_eq!(targets["ssh"]["schema"]["type"], "object");
        // 腾讯云 SSL 变体：凭据引用、备注、重复开关、替换开关、资源类型与地域，schema 走同一注入通道。
        assert_eq!(targets["tencent-ssl"]["display_name"], "腾讯云 SSL 证书");
        assert!(targets["tencent-ssl"]["example"]["credential_id"].is_i64());
        assert_eq!(
            targets["tencent-ssl"]["schema"]["properties"]["replace_existing"]["type"],
            "boolean"
        );
        assert_eq!(
            targets["tencent-ssl"]["schema"]["x-field-order"],
            serde_json::json!([
                "credential_id",
                "alias",
                "repeatable",
                "replace_existing",
                "resource_types",
                "resource_regions"
            ])
        );
    }

    // ---- 结构化输出测试 ----

    use std::collections::BTreeMap;
    use std::sync::Mutex;

    use acmecast_access::{CredentialRegistry, CredentialStore};
    use acmecast_core::CredentialCipher;
    use acmecast_deploy::{DeployMode, DeployOutcome, DeploymentTarget};
    use acmecast_pipeline::{Artifacts, PipelineStateStore, StepLogLevel};
    use acmecast_store::migrate;
    use sea_orm::{Database, DatabaseConnection};

    /// 一个 mock 部署目标：按 `type_id` 与返回值参数化，用于验证步骤日志的结构化输出。
    #[derive(Debug)]
    struct MockTarget {
        type_id_val: &'static str,
        cert_ids: Vec<String>,
    }

    #[async_trait::async_trait]
    impl DeploymentTarget for MockTarget {
        fn type_id(&self) -> &'static str {
            self.type_id_val
        }
        fn display_name(&self) -> &'static str {
            "mock"
        }
        fn input_schema(&self) -> schemars::schema::RootSchema {
            schemars::schema::RootSchema::default()
        }
        fn example_input(&self) -> serde_json::Value {
            serde_json::json!({})
        }
        async fn deploy(
            &self,
            _input: &serde_json::Value,
            _materials: &CertMaterials,
            _credentials: &CredentialStore<'_>,
            _mode: DeployMode,
        ) -> acmecast_deploy::Result<DeployOutcome> {
            Ok(DeployOutcome::written(self.cert_ids.clone()))
        }
    }

    /// 内存版的流水线级存储，够本测试用。
    #[derive(Debug, Default)]
    struct MemoryState(Mutex<BTreeMap<(i64, String), serde_json::Value>>);

    #[async_trait::async_trait]
    impl PipelineStateStore for MemoryState {
        async fn get(
            &self,
            pipeline_id: i64,
            key: &str,
        ) -> acmecast_pipeline::Result<Option<serde_json::Value>> {
            Ok(self
                .0
                .lock()
                .expect("锁不应中毒")
                .get(&(pipeline_id, key.to_owned()))
                .cloned())
        }

        async fn set(
            &self,
            pipeline_id: i64,
            key: &str,
            value: serde_json::Value,
        ) -> acmecast_pipeline::Result<()> {
            self.0
                .lock()
                .expect("锁不应中毒")
                .insert((pipeline_id, key.to_owned()), value);
            Ok(())
        }
    }

    /// 建一个跑完迁移的内存库，供 CredentialStore 使用。
    async fn mem_db() -> DatabaseConnection {
        let db = Database::connect("sqlite::memory:")
            .await
            .expect("应能连上内存库");
        migrate(&db).await.expect("迁移应成功");
        db
    }

    fn credential_store(db: &DatabaseConnection) -> CredentialStore<'_> {
        let cipher = CredentialCipher::from_base64(&CredentialCipher::generate_key_base64())
            .expect("密钥应可用");
        CredentialStore::new(db, Arc::new(CredentialRegistry::new()), Arc::new(cipher))
    }

    fn cert_artifacts() -> Artifacts {
        let mut artifacts = Artifacts::new();
        let produced: BTreeMap<String, serde_json::Value> = [
            (
                "cert_pem".to_string(),
                serde_json::json!("-----BEGIN CERTIFICATE-----"),
            ),
            (
                "key_pem".to_string(),
                serde_json::json!("-----BEGIN PRIVATE KEY-----"),
            ),
            ("fingerprint".to_string(), serde_json::json!("sha256:abc")),
        ]
        .into_iter()
        .collect();
        artifacts.merge(0, "cert.apply", produced);
        artifacts
    }

    #[tokio::test]
    async fn tencent_ssl_deploy_writes_cert_ids_to_log_output() {
        let mut registry = DeploymentRegistry::new();
        registry
            .register(MockTarget {
                type_id_val: "tencent-ssl",
                cert_ids: vec!["cert-abc123".into()],
            })
            .expect("注册应成功");

        let state = Arc::new(InMemoryDeploymentState::new());
        let step = CertDeployStep::new(Arc::new(registry), state);

        let db = mem_db().await;
        let credentials = credential_store(&db);
        let pipeline_state = MemoryState::default();
        let artifacts = cert_artifacts();
        let input = serde_json::json!({
            "target": "tencent-ssl",
            "config": {},
            "force": false,
        });

        let mut ctx = StepContext::new(1, 1, 0, &input, &artifacts, &credentials, &pipeline_state);
        let _output = step.execute(&mut ctx).await.expect("部署应成功");
        let logs = ctx.take_logs();

        // 结构化输出日志应在最后一条 info 日志上。
        let structured = logs
            .iter()
            .find(|log| log.output.is_some())
            .expect("应有一条带结构化输出的日志");
        let out = structured.output.as_ref().expect("output 不为空");
        assert_eq!(out["cert_ids"][0], "cert-abc123");
        assert_eq!(out["skipped_write"], false);
        assert_eq!(structured.level, StepLogLevel::Info);
    }

    #[tokio::test]
    async fn non_tencent_ssl_target_has_no_structured_output() {
        let mut registry = DeploymentRegistry::new();
        registry
            .register(MockTarget {
                type_id_val: "local",
                cert_ids: vec!["/etc/ssl/cert.pem".into()],
            })
            .expect("注册应成功");

        let state = Arc::new(InMemoryDeploymentState::new());
        let step = CertDeployStep::new(Arc::new(registry), state);

        let db = mem_db().await;
        let credentials = credential_store(&db);
        let pipeline_state = MemoryState::default();
        let artifacts = cert_artifacts();
        let input = serde_json::json!({
            "target": "local",
            "config": {},
            "force": false,
        });

        let mut ctx = StepContext::new(1, 1, 0, &input, &artifacts, &credentials, &pipeline_state);
        let _output = step.execute(&mut ctx).await.expect("部署应成功");
        let logs = ctx.take_logs();

        // 非 tencent-ssl 目标不应有结构化输出。
        assert!(
            logs.iter().all(|log| log.output.is_none()),
            "非 tencent-ssl 目标不应产生结构化输出"
        );
    }

    #[tokio::test]
    async fn tencent_ssl_writes_multiple_cert_ids_for_update() {
        let mut registry = DeploymentRegistry::new();
        registry
            .register(MockTarget {
                type_id_val: "tencent-ssl",
                cert_ids: vec!["cert-1".into(), "cert-2".into(), "cert-3".into()],
            })
            .expect("注册应成功");

        let state = Arc::new(InMemoryDeploymentState::new());
        let step = CertDeployStep::new(Arc::new(registry), state);

        let db = mem_db().await;
        let credentials = credential_store(&db);
        let pipeline_state = MemoryState::default();
        let artifacts = cert_artifacts();
        let input = serde_json::json!({
            "target": "tencent-ssl",
            "config": {},
            "force": false,
        });

        let mut ctx = StepContext::new(1, 1, 0, &input, &artifacts, &credentials, &pipeline_state);
        step.execute(&mut ctx).await.expect("部署应成功");
        let logs = ctx.take_logs();
        let structured = logs
            .iter()
            .find(|log| log.output.is_some())
            .expect("应有一条带结构化输出的日志");
        let out = structured.output.as_ref().unwrap();
        assert_eq!(out["cert_ids"].as_array().unwrap().len(), 3);
        assert_eq!(out["cert_ids"][0], "cert-1");
        assert_eq!(out["cert_ids"][1], "cert-2");
        assert_eq!(out["cert_ids"][2], "cert-3");
        assert_eq!(out["skipped_write"], false);
        // output 不含凭据明文。
        let serialized = out.to_string();
        assert!(
            !serialized.contains("secret"),
            "output 不应包含凭据相关字段"
        );
    }

    #[tokio::test]
    async fn tencent_ssl_skipped_write_has_empty_cert_ids_and_single_log() {
        #[derive(Debug)]
        struct SkipTarget;
        #[async_trait::async_trait]
        impl DeploymentTarget for SkipTarget {
            fn type_id(&self) -> &'static str {
                "tencent-ssl"
            }
            fn display_name(&self) -> &'static str {
                "mock-skip"
            }
            fn input_schema(&self) -> schemars::schema::RootSchema {
                schemars::schema::RootSchema::default()
            }
            fn example_input(&self) -> serde_json::Value {
                serde_json::json!({})
            }
            fn supports_skip_write(&self) -> bool {
                true
            }
            async fn deploy(
                &self,
                _input: &serde_json::Value,
                _materials: &CertMaterials,
                _credentials: &CredentialStore<'_>,
                mode: DeployMode,
            ) -> acmecast_deploy::Result<DeployOutcome> {
                // Deployer 在 fingerprint 一致时会传入 SkipWrite；此时应返回 skipped。
                if mode == DeployMode::SkipWrite {
                    return Ok(DeployOutcome::skipped(Vec::new()));
                }
                Ok(DeployOutcome::written(vec!["cert-abc123".into()]))
            }
        }

        let mut registry = DeploymentRegistry::new();
        registry.register(SkipTarget).expect("注册应成功");

        let state = Arc::new(InMemoryDeploymentState::new());
        // 预置已部署指纹，使下一次同指纹部署走 SkipWrite。
        state
            .record(acmecast_deploy::state::DeploymentEntry {
                target: acmecast_deploy::state::target_ref_of("tencent-ssl", &serde_json::json!({})),
                fingerprint: "sha256:abc".to_owned(),
                skipped_write: false,
                deployed_at: chrono::Utc::now(),
                paths: vec!["cert-abc123".into()],
                reload_output: None,
            })
            .await
            .expect("预置部署记录应成功");

        let db = mem_db().await;
        let state: Arc<dyn acmecast_deploy::DeploymentStateStore> = state;
        let step = CertDeployStep::new(Arc::new(registry), Arc::clone(&state));
        let credentials = credential_store(&db);
        let pipeline_state = MemoryState::default();
        let artifacts = cert_artifacts();
        let input = serde_json::json!({
            "target": "tencent-ssl",
            "config": {},
            "force": false,
        });

        let mut ctx = StepContext::new(1, 1, 0, &input, &artifacts, &credentials, &pipeline_state);
        step.execute(&mut ctx).await.expect("部署应成功");
        let logs = ctx.take_logs();

        // 跳过写入时应只有一条带 output 的日志，且内容为空列表 + skipped_write=true。
        let structured: Vec<_> = logs.iter().filter(|l| l.output.is_some()).collect();
        assert_eq!(structured.len(), 1, "跳过写入时应只有一条结构化日志");
        let out = structured[0].output.as_ref().unwrap();
        assert_eq!(out["cert_ids"].as_array().unwrap().len(), 0);
        assert_eq!(out["skipped_write"], true);
        assert!(
            structured[0].message.contains("跳过写入"),
            "跳过写入的结构化日志应包含跳过提示"
        );
    }

    #[tokio::test]
    async fn tencent_ssl_deploy_failure_produces_no_cert_ids_output() {
        #[derive(Debug)]
        struct FailingTarget;
        #[async_trait::async_trait]
        impl DeploymentTarget for FailingTarget {
            fn type_id(&self) -> &'static str {
                "tencent-ssl"
            }
            fn display_name(&self) -> &'static str {
                "mock-failing"
            }
            fn input_schema(&self) -> schemars::schema::RootSchema {
                schemars::schema::RootSchema::default()
            }
            fn example_input(&self) -> serde_json::Value {
                serde_json::json!({})
            }
            async fn deploy(
                &self,
                _input: &serde_json::Value,
                _materials: &CertMaterials,
                _credentials: &CredentialStore<'_>,
                _mode: DeployMode,
            ) -> acmecast_deploy::Result<DeployOutcome> {
                Err(acmecast_deploy::Error::Cloud {
                    service: "ssl".into(),
                    reason: "模拟 tencent-ssl 部署失败".into(),
                })
            }
        }

        let mut registry = DeploymentRegistry::new();
        registry.register(FailingTarget).expect("注册应成功");
        let state = Arc::new(InMemoryDeploymentState::new());
        let step = CertDeployStep::new(Arc::new(registry), state);

        let db = mem_db().await;
        let credentials = credential_store(&db);
        let pipeline_state = MemoryState::default();
        let artifacts = cert_artifacts();
        let input = serde_json::json!({
            "target": "tencent-ssl",
            "config": {},
            "force": false,
        });

        let mut ctx = StepContext::new(1, 1, 0, &input, &artifacts, &credentials, &pipeline_state);
        let result = step.execute(&mut ctx).await;
        assert!(result.is_err(), "部署失败应返回错误");
        let logs = ctx.take_logs();
        assert!(
            logs.iter().all(|l| l.output.is_none() || l.output.as_ref().unwrap().get("cert_ids").is_none()),
            "失败路径不应产生含 cert_ids 的成功回显"
        );
    }

}
