//! 6.3 输入定义导出与执行前校验。
//!
//! spec 的关键要求不是「能报错」，而是「**不进入任务执行逻辑**」——
//! 所以这里用一个会数自己被调用次数的步骤来验证，而不是只看错误信息。

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use acmecast_access::{CredentialRegistry, CredentialStore};
use acmecast_core::CredentialCipher;
use acmecast_pipeline::{
    Artifacts, PipelineStateStore, PipelineStep, Result, StepContext, StepOutput,
};
use acmecast_store::migrate;
use async_trait::async_trait;
use schemars::schema::RootSchema;
use schemars::schema_for;
use sea_orm::{Database, DatabaseConnection};
use serde::Deserialize;

// ---- 被测的步骤 ----

/// 输入结构体：Schema 由它推导，serde 也用它反序列化。
#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct ApplyInput {
    domains: Vec<String>,
    challenge: Challenge,
}

/// 挑战类型，枚举值会出现在导出的定义里。
///
/// 显式写出取值而不是靠 `rename_all`：`Dns01` 这种带数字的变体在
/// `kebab-case` 下会变成 `dns01`（数字前不插连字符），与 ACME 的
/// `dns-01` 对不上。
#[derive(Debug, Deserialize, schemars::JsonSchema)]
enum Challenge {
    #[serde(rename = "dns-01")]
    Dns01,
    #[serde(rename = "http-01")]
    Http01,
}

/// 一个声明了输入结构、并记录自己被调用次数的步骤。
#[derive(Debug, Default)]
struct CountingStep {
    executions: AtomicUsize,
}

#[async_trait]
impl PipelineStep for CountingStep {
    fn type_id(&self) -> &'static str {
        "test.counting"
    }

    fn input_schema(&self) -> Option<RootSchema> {
        Some(schema_for!(ApplyInput))
    }

    async fn execute(&self, ctx: &mut StepContext<'_>) -> Result<StepOutput> {
        // 真的执行了才计数。
        self.executions.fetch_add(1, Ordering::SeqCst);

        let input: ApplyInput = ctx.input_as()?;
        ctx.log_info(format!(
            "{:?} 挑战，{} 个域名",
            input.challenge,
            input.domains.len()
        ));
        Ok(StepOutput::empty())
    }
}

/// 没声明输入结构的步骤：校验应当直接放行。
#[derive(Debug)]
struct LooseStep;

#[async_trait]
impl PipelineStep for LooseStep {
    fn type_id(&self) -> &'static str {
        "test.loose"
    }

    async fn execute(&self, _ctx: &mut StepContext<'_>) -> Result<StepOutput> {
        Ok(StepOutput::empty())
    }
}

// ---- 脚手架 ----

#[derive(Debug, Default)]
struct NoState;

#[async_trait]
impl PipelineStateStore for NoState {
    async fn get(&self, _pipeline_id: i64, _key: &str) -> Result<Option<serde_json::Value>> {
        Ok(None)
    }

    async fn set(&self, _pipeline_id: i64, _key: &str, _value: serde_json::Value) -> Result<()> {
        Ok(())
    }
}

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

/// 执行器将来要做的事：**先校验，通过才执行**。
async fn validate_then_execute(
    step: &dyn PipelineStep,
    input: serde_json::Value,
    credentials: &CredentialStore<'_>,
) -> Result<StepOutput> {
    // 校验不通过就在这里返回，后面那行不会被执行。
    step.validate_input(&input)?;

    let artifacts = Artifacts::new();
    let state = NoState;
    let mut ctx = StepContext::new(1, 1, 0, &input, &artifacts, credentials, &state);
    step.execute(&mut ctx).await
}

// ---- Scenario: 导出任务输入定义 ----

#[test]
fn the_input_definition_carries_fields_and_constraints() {
    let schema = CountingStep::default()
        .input_schema()
        .expect("该步骤应声明输入结构");
    let rendered = serde_json::to_string(&schema).expect("应能序列化");

    // 字段、必填、枚举选项——前端渲染表单需要的信息都在里面。
    assert!(rendered.contains("domains"), "{rendered}");
    assert!(rendered.contains("challenge"), "{rendered}");
    assert!(rendered.contains("required"), "应标出必填: {rendered}");
    assert!(
        rendered.contains("dns-01"),
        "枚举选项应在定义里: {rendered}"
    );
    assert!(rendered.contains("http-01"), "{rendered}");
}

#[test]
fn a_step_without_a_declared_schema_exports_nothing() {
    assert!(LooseStep.input_schema().is_none());
}

// ---- Scenario: 缺失必填输入 ----

#[tokio::test]
async fn a_missing_required_field_stops_before_execution() {
    let db = database().await;
    let credentials = credential_store(&db);
    let step = CountingStep::default();

    let err = validate_then_execute(
        &step,
        serde_json::json!({ "challenge": "dns-01" }),
        &credentials,
    )
    .await
    .expect_err("缺必填应被拒绝");

    // 错误要指明字段名，前端才能高亮到具体输入框。
    assert!(err.to_string().contains("domains"), "{err}");
    // 核心断言：步骤一次也没被执行过。
    assert_eq!(
        step.executions.load(Ordering::SeqCst),
        0,
        "校验失败不该进入执行逻辑"
    );
}

#[tokio::test]
async fn a_value_outside_the_enum_stops_before_execution() {
    let db = database().await;
    let credentials = credential_store(&db);
    let step = CountingStep::default();

    let err = validate_then_execute(
        &step,
        serde_json::json!({ "domains": ["example.com"], "challenge": "tls-alpn-01" }),
        &credentials,
    )
    .await
    .expect_err("枚举外的值应被拒绝");

    assert!(err.to_string().contains("challenge"), "{err}");
    assert_eq!(step.executions.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn a_valid_input_does_reach_execution() {
    // 与上两条对照：校验通过时步骤**应当**被执行——
    // 否则「不进入执行逻辑」这个断言可能只是「从来没执行过」。
    let db = database().await;
    let credentials = credential_store(&db);
    let step = CountingStep::default();

    validate_then_execute(
        &step,
        serde_json::json!({ "domains": ["example.com"], "challenge": "dns-01" }),
        &credentials,
    )
    .await
    .expect("合法输入应通过");

    assert_eq!(step.executions.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn a_step_without_a_schema_accepts_any_input() {
    let db = database().await;
    let credentials = credential_store(&db);

    validate_then_execute(
        &LooseStep,
        serde_json::json!({ "任何": "东西" }),
        &credentials,
    )
    .await
    .expect("未声明结构的步骤应放行");
}
