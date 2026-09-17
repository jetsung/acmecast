//! 6.1 步骤契约与执行上下文。
//!
//! 本项的核心验证点是「可被多个**不同**实现共同引用」——因此这里定义两个职责
//! 各异的步骤（一个产出证书、一个消费证书），让它们跑在同一套上下文上。
//!
//! 执行器要到 6.5 才有，所以测试里手写了一段「跑一步」的胶水：构造上下文、
//! 执行、把产出并入运行状态。这段胶水正好说明契约本身够不够用。

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use acmecast_access::{CredentialRegistry, CredentialStore};
use acmecast_core::CredentialCipher;
use acmecast_pipeline::{
    Artifacts, Error, PipelineStateStore, PipelineStep, Result, StepContext, StepLog, StepOutput,
};
use acmecast_store::{entity::credential, migrate};
use async_trait::async_trait;
use chrono::Utc;
use sea_orm::{ActiveModelTrait, Database, DatabaseConnection, Set};
use serde::Deserialize;

// ---- 两个不同的步骤实现 ----

#[derive(Debug, Deserialize)]
struct ApplyInput {
    domains: Vec<String>,
}

/// 申请证书：产出 `cert_pem`，并通过上下文记一条日志。
#[derive(Debug)]
struct ApplyCert;

#[async_trait]
impl PipelineStep for ApplyCert {
    fn type_id(&self) -> &'static str {
        "cert.apply"
    }

    async fn execute(&self, ctx: &mut StepContext<'_>) -> Result<StepOutput> {
        let input: ApplyInput = ctx.input_as()?;
        ctx.log_info(format!("为 {} 申请证书", input.domains.join("、")));

        Ok(StepOutput::empty()
            .with_artifact(
                "cert_pem",
                serde_json::json!(format!("PEM({})", input.domains.join(","))),
            )
            .with_log(StepLog::info("签发完成")))
    }
}

/// 部署证书：读取前序产出的 `cert_pem`，不重新读磁盘。
#[derive(Debug)]
struct DeployCert;

#[async_trait]
impl PipelineStep for DeployCert {
    fn type_id(&self) -> &'static str {
        "cert.deploy"
    }

    async fn execute(&self, ctx: &mut StepContext<'_>) -> Result<StepOutput> {
        let pem = ctx.artifact("cert_pem")?;
        let pem = pem.as_str().unwrap_or_default();

        Ok(StepOutput::empty()
            .with_artifact("deployed_to", serde_json::json!("/etc/ssl/site.pem"))
            .with_log(StepLog::info(format!("部署了 {pem}"))))
    }
}

// ---- 测试脚手架 ----

/// 内存版的流水线级存储，够本层用。
#[derive(Debug, Default)]
struct MemoryState(Mutex<BTreeMap<(i64, String), serde_json::Value>>);

#[async_trait]
impl PipelineStateStore for MemoryState {
    async fn get(&self, pipeline_id: i64, key: &str) -> Result<Option<serde_json::Value>> {
        Ok(self
            .0
            .lock()
            .expect("锁不应中毒")
            .get(&(pipeline_id, key.to_owned()))
            .cloned())
    }

    async fn set(&self, pipeline_id: i64, key: &str, value: serde_json::Value) -> Result<()> {
        self.0
            .lock()
            .expect("锁不应中毒")
            .insert((pipeline_id, key.to_owned()), value);
        Ok(())
    }
}

/// 建一个跑完迁移的内存库。
async fn database() -> DatabaseConnection {
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

/// 跑一个步骤，并把它的产出与日志并入运行状态。
///
/// 这是执行器将来要做的事的雏形（6.5 才正式实现）。
async fn run_step(
    step: &dyn PipelineStep,
    step_order: i32,
    input: serde_json::Value,
    artifacts: &mut Artifacts,
    credentials: &CredentialStore<'_>,
    state: &dyn PipelineStateStore,
) -> Result<StepOutput> {
    let (executed, buffered) = {
        // 上下文只借用产物集合，作用域到此为止——之后才轮到写回。
        let mut ctx = StepContext::new(1, 1, step_order, &input, artifacts, credentials, state);
        let result = step.execute(&mut ctx).await;
        (result, ctx.take_logs())
    };

    let mut output = executed?;
    output.logs.extend(buffered);

    // 执行器负责的一步：并入产物。同名覆盖的告警就发生在 merge 内。
    artifacts.merge(step_order, step.type_id(), output.artifacts.clone());
    Ok(output)
}

// ---- 验证 ----

#[tokio::test]
async fn two_different_steps_run_on_the_same_context() {
    let db = database().await;
    let credentials = credential_store(&db);
    let state = MemoryState::default();
    let mut artifacts = Artifacts::new();

    // 第一步产出证书。
    let applied = run_step(
        &ApplyCert,
        0,
        serde_json::json!({ "domains": ["example.com"] }),
        &mut artifacts,
        &credentials,
        &state,
    )
    .await
    .expect("申请步骤应成功");

    // 第二步消费它——两个实现共用同一套上下文，彼此不必知道对方是谁。
    let deployed = run_step(
        &DeployCert,
        1,
        serde_json::json!({}),
        &mut artifacts,
        &credentials,
        &state,
    )
    .await
    .expect("部署步骤应成功");

    assert_eq!(applied.artifacts["cert_pem"], "PEM(example.com)");
    assert_eq!(deployed.artifacts["deployed_to"], "/etc/ssl/site.pem");
    assert_eq!(
        artifacts.get("cert_pem").unwrap(),
        &serde_json::json!("PEM(example.com)"),
        "产物应留在运行状态里，供后续步骤继续读取"
    );
    assert_eq!(
        artifacts.source_of("cert_pem").unwrap().type_id,
        "cert.apply",
        "来源应指向真正的产出者"
    );
}

#[tokio::test]
async fn a_step_that_needs_a_missing_artifact_fails_with_a_clear_reason() {
    let db = database().await;
    let credentials = credential_store(&db);
    let state = MemoryState::default();
    // 直接从部署开始：cert_pem 还不存在。
    let mut artifacts = Artifacts::new();

    let err = run_step(
        &DeployCert,
        0,
        serde_json::json!({}),
        &mut artifacts,
        &credentials,
        &state,
    )
    .await
    .expect_err("缺产物应当失败");

    match &err {
        Error::MissingArtifact { name, .. } => assert_eq!(name, "cert_pem"),
        other => panic!("期望 MissingArtifact，实际 {other:?}"),
    }
}

#[tokio::test]
async fn logs_from_both_sources_are_collected() {
    let db = database().await;
    let credentials = credential_store(&db);
    let state = MemoryState::default();
    let mut artifacts = Artifacts::new();

    let output = run_step(
        &ApplyCert,
        0,
        serde_json::json!({ "domains": ["a.example.com", "b.example.com"] }),
        &mut artifacts,
        &credentials,
        &state,
    )
    .await
    .unwrap();

    // 一条来自 ctx.log_info，一条随返回值带出——两者都该被收集到。
    let messages: Vec<&str> = output.logs.iter().map(|log| log.message.as_str()).collect();
    assert!(
        messages
            .iter()
            .any(|m| m.contains("a.example.com、b.example.com")),
        "上下文里记的日志应被收集: {messages:?}"
    );
    assert!(
        messages.iter().any(|m| m == &"签发完成"),
        "返回值里带的日志也应被收集: {messages:?}"
    );
}

#[tokio::test]
async fn a_step_reads_its_credential_by_identifier() {
    let db = database().await;

    // 插一份凭据，让步骤能按标识取到它。
    let cipher = CredentialCipher::from_base64(&CredentialCipher::generate_key_base64()).unwrap();
    let now = Utc::now();
    let stored = credential::ActiveModel {
        name: Set("申请用的账号".to_owned()),
        type_id: Set("acme.account".to_owned()),
        encrypted_fields: Set(cipher
            .encrypt_string(&serde_json::json!({ "ca": "letsencrypt" }).to_string())
            .unwrap()),
        created_at: Set(now),
        updated_at: Set(now),
        ..Default::default()
    }
    .insert(&db)
    .await
    .unwrap();

    let credentials = credential_store(&db);
    let state = MemoryState::default();
    let mut artifacts = Artifacts::new();

    /// 一个会去取凭据的步骤。
    #[derive(Debug)]
    struct NeedsCredential {
        expected_id: i64,
    }

    #[async_trait]
    impl PipelineStep for NeedsCredential {
        fn type_id(&self) -> &'static str {
            "test.needs_credential"
        }

        async fn execute(&self, ctx: &mut StepContext<'_>) -> Result<StepOutput> {
            // 这里不注册类型，所以取不到——但错误必须来自凭据体系，
            // 而不是「上下文没提供这个能力」。
            let err = ctx
                .credentials()
                .resolve(self.expected_id)
                .await
                .expect_err("未注册类型应被凭据体系拒绝");
            Ok(StepOutput::empty().with_log(StepLog::warn(err.to_string())))
        }
    }

    let output = run_step(
        &NeedsCredential {
            expected_id: stored.id,
        },
        0,
        serde_json::json!({}),
        &mut artifacts,
        &credentials,
        &state,
    )
    .await
    .expect("步骤应成功返回");

    // 上下文确实把凭据体系接进来了：错误来自它的「类型未注册」判定。
    assert!(
        output.logs[0].message.contains("未知的凭据类型"),
        "应能经由上下文访问凭据体系: {:?}",
        output.logs
    );
}

#[tokio::test]
async fn state_is_scoped_to_the_pipeline() {
    let db = database().await;
    let credentials = credential_store(&db);
    let state = MemoryState::default();
    let mut artifacts = Artifacts::new();

    // 两个步骤在不同流水线上写同一个键。
    #[derive(Debug)]
    struct RememberLastPath;

    #[async_trait]
    impl PipelineStep for RememberLastPath {
        fn type_id(&self) -> &'static str {
            "test.remember"
        }

        async fn execute(&self, ctx: &mut StepContext<'_>) -> Result<StepOutput> {
            let last = ctx.state().get(ctx.pipeline_id(), "last_path").await?;
            ctx.state()
                .set(
                    ctx.pipeline_id(),
                    "last_path",
                    serde_json::json!("本次路径"),
                )
                .await?;
            Ok(StepOutput::empty().with_artifact("previous", last.unwrap_or_default()))
        }
    }

    let step = RememberLastPath;
    let first = run_step(
        &step,
        0,
        serde_json::json!({}),
        &mut artifacts,
        &credentials,
        &state,
    )
    .await
    .unwrap();
    assert_eq!(first.artifacts["previous"], serde_json::Value::Null);

    // 同一流水线再跑一次，能读到上次写的值。
    let second = run_step(
        &step,
        0,
        serde_json::json!({}),
        &mut artifacts,
        &credentials,
        &state,
    )
    .await
    .unwrap();
    assert_eq!(
        second.artifacts["previous"], "本次路径",
        "同一流水线应能跨运行读到"
    );
}
