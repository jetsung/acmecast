//! 6.5 按序执行与失败中止。
//!
//! spec 的两个场景：全部成功、中间失败（后续步骤不执行）。
//! 「后续步骤不执行」用**计数器**验证，而不是只看结果状态——状态为失败
//! 并不能说明第三及以后的步骤真的没跑。

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use acmecast_access::{CredentialRegistry, CredentialStore};
use acmecast_core::CredentialCipher;
use acmecast_pipeline::{
    Error, PipelineDefinition, PipelineRunner, PipelineStateStore, PipelineStep, Result, RunStatus,
    StepContext, StepDefinition, StepOutput, StepRegistry, StepStatus,
};
use acmecast_store::migrate;
use async_trait::async_trait;
use schemars::schema::RootSchema;
use schemars::schema_for;
use sea_orm::{Database, DatabaseConnection};
use serde::{Deserialize, Serialize};

// ---- 测试用的步骤实现 ----

/// 每次执行给计数器加一。
#[derive(Debug)]
struct Counted(Arc<AtomicUsize>);

#[async_trait]
impl PipelineStep for Counted {
    fn type_id(&self) -> &'static str {
        "test.counted"
    }

    async fn execute(&self, _ctx: &mut StepContext<'_>) -> Result<StepOutput> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(StepOutput::empty())
    }
}

/// 产出一件固定产物。
#[derive(Debug)]
struct Produce;

#[async_trait]
impl PipelineStep for Produce {
    fn type_id(&self) -> &'static str {
        "test.produce"
    }

    async fn execute(&self, ctx: &mut StepContext<'_>) -> Result<StepOutput> {
        ctx.log_info("产出中");
        Ok(StepOutput::empty().with_artifact("value", serde_json::json!("产出的值")))
    }
}

/// 消费前序产物。
#[derive(Debug)]
struct Consume;

#[async_trait]
impl PipelineStep for Consume {
    fn type_id(&self) -> &'static str {
        "test.consume"
    }

    async fn execute(&self, ctx: &mut StepContext<'_>) -> Result<StepOutput> {
        let value = ctx.artifact("value")?;
        ctx.log_info(format!("读到 {}", value.as_str().unwrap_or_default()));
        Ok(StepOutput::empty())
    }
}

/// 先记一条日志，再失败。
#[derive(Debug)]
struct Fails;

#[async_trait]
impl PipelineStep for Fails {
    fn type_id(&self) -> &'static str {
        "test.fails"
    }

    async fn execute(&self, ctx: &mut StepContext<'_>) -> Result<StepOutput> {
        ctx.log_warn("失败现场：这一步先记点什么");
        Err(Error::Core(acmecast_core::Error::Internal(
            "故意失败，用于验证中止".to_owned(),
        )))
    }
}

/// 声明了必填输入，但执行体从不被期待跑到。
#[derive(Debug)]
struct NeedsDomains;

#[derive(Debug, Deserialize, schemars::JsonSchema)]
// 字段只供 schemars 推导 Schema，代码里不读取。
#[allow(dead_code)]
struct DomainsInput {
    domains: Vec<String>,
}

#[async_trait]
impl PipelineStep for NeedsDomains {
    fn type_id(&self) -> &'static str {
        "test.needs_domains"
    }

    fn input_schema(&self) -> Option<RootSchema> {
        Some(schema_for!(DomainsInput))
    }

    async fn execute(&self, _ctx: &mut StepContext<'_>) -> Result<StepOutput> {
        panic!("校验不通过时不该走到这里");
    }
}

// ---- 产物传递用的步骤 ----

const CERT_PEM: &str = "-----BEGIN CERTIFICATE-----\nMIIB\n-----END CERTIFICATE-----\n";
const KEY_PEM: &str = "-----BEGIN PRIVATE KEY-----\nMIIE\n-----END PRIVATE KEY-----\n";

/// 申请证书：产出 PEM 与私钥。
#[derive(Debug)]
struct ApplyCert;

#[async_trait]
impl PipelineStep for ApplyCert {
    fn type_id(&self) -> &'static str {
        "cert.apply"
    }

    async fn execute(&self, _ctx: &mut StepContext<'_>) -> Result<StepOutput> {
        Ok(StepOutput::empty()
            .with_artifact("cert_pem", serde_json::json!(CERT_PEM))
            .with_artifact("key_pem", serde_json::json!(KEY_PEM)))
    }
}

/// 部署证书：**声明**需要前序步骤产出的 PEM 与私钥，并读取它们。
#[derive(Debug)]
struct DeployCert(Arc<AtomicUsize>);

#[async_trait]
impl PipelineStep for DeployCert {
    fn type_id(&self) -> &'static str {
        "cert.deploy"
    }

    fn required_artifacts(&self) -> &'static [&'static str] {
        &["cert_pem", "key_pem"]
    }

    async fn execute(&self, ctx: &mut StepContext<'_>) -> Result<StepOutput> {
        self.0.fetch_add(1, Ordering::SeqCst);

        let pem = ctx.artifact("cert_pem")?;
        let key = ctx.artifact("key_pem")?;
        ctx.log_info(format!(
            "部署 {} 字节的证书与 {} 字节的私钥",
            pem.as_str().map(str::len).unwrap_or_default(),
            key.as_str().map(str::len).unwrap_or_default()
        ));
        Ok(StepOutput::empty())
    }
}

/// 需要一份谁都不产出的产物。
#[derive(Debug)]
struct NeedsConfig;

#[async_trait]
impl PipelineStep for NeedsConfig {
    fn type_id(&self) -> &'static str {
        "test.needs_config"
    }

    fn required_artifacts(&self) -> &'static [&'static str] {
        &["config_json"]
    }

    async fn execute(&self, _ctx: &mut StepContext<'_>) -> Result<StepOutput> {
        unreachable!("执行器应当在执行前就拦下缺产物的步骤")
    }
}

/// 一份结构化的产物。
#[derive(Debug, Serialize, Deserialize, PartialEq)]
struct DomainList {
    domains: Vec<String>,
}

/// 产出结构化的域名列表。
#[derive(Debug)]
struct ProduceDomains;

#[async_trait]
impl PipelineStep for ProduceDomains {
    fn type_id(&self) -> &'static str {
        "test.produce_domains"
    }

    async fn execute(&self, _ctx: &mut StepContext<'_>) -> Result<StepOutput> {
        let list = DomainList {
            domains: vec!["a.example.com".to_owned(), "b.example.com".to_owned()],
        };
        Ok(StepOutput::empty().with_artifact("domains", serde_json::to_value(list).unwrap()))
    }
}

/// 产出同名但形状不对的产物。
#[derive(Debug)]
struct ProduceWrongShape;

#[async_trait]
impl PipelineStep for ProduceWrongShape {
    fn type_id(&self) -> &'static str {
        "test.wrong_shape"
    }

    async fn execute(&self, _ctx: &mut StepContext<'_>) -> Result<StepOutput> {
        Ok(StepOutput::empty().with_artifact("domains", serde_json::json!("我不是数组")))
    }
}

/// 把产物读成结构化的域名列表。
#[derive(Debug)]
struct ConsumeDomains;

#[async_trait]
impl PipelineStep for ConsumeDomains {
    fn type_id(&self) -> &'static str {
        "test.consume_domains"
    }

    fn required_artifacts(&self) -> &'static [&'static str] {
        &["domains"]
    }

    async fn execute(&self, ctx: &mut StepContext<'_>) -> Result<StepOutput> {
        let list: DomainList = ctx.artifact_as("domains")?;
        ctx.log_info(format!("拿到 {} 个域名", list.domains.len()));
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

/// 构造一个步骤定义。
fn step(order_index: i32, type_id: &str) -> StepDefinition {
    StepDefinition {
        order_index,
        type_id: type_id.to_owned(),
        input: serde_json::json!({}),
        enabled: true,
    }
}

/// 全部内置测试步骤的注册表。
fn registry(counted: Arc<AtomicUsize>) -> StepRegistry {
    let mut registry = StepRegistry::new();
    registry.register(Counted(counted)).unwrap();
    registry.register(Produce).unwrap();
    registry.register(Consume).unwrap();
    registry.register(Fails).unwrap();
    registry.register(NeedsDomains).unwrap();
    registry
}

fn pipeline(steps: Vec<StepDefinition>) -> PipelineDefinition {
    PipelineDefinition { id: 1, steps }
}

// ---- Scenario: 全部步骤成功 ----

#[tokio::test]
async fn all_steps_succeed_and_artifacts_flow_in_order() {
    let db = database().await;
    let credentials = credential_store(&db);
    let state = NoState;
    let counting = Arc::new(AtomicUsize::new(0));
    let registry = registry(Arc::clone(&counting));
    let runner = PipelineRunner::new(&registry, &credentials, &state);

    let outcome = runner
        .run(
            &pipeline(vec![step(0, "test.produce"), step(1, "test.consume")]),
            1,
        )
        .await
        .expect("执行器自身不应出错");

    assert_eq!(outcome.status, RunStatus::Succeeded);
    assert!(outcome.is_success());
    assert!(outcome.failure.is_none());
    assert_eq!(outcome.succeeded_steps(), 2);
    assert_eq!(counting.load(Ordering::SeqCst), 0, "本用例没注册计数步骤");

    // 各步产物依次可用：消费步骤确实读到了产出步骤的东西。
    let consumed = outcome
        .steps
        .iter()
        .find(|run| run.type_id == "test.consume")
        .expect("应有消费步骤的记录");
    assert!(
        consumed
            .logs
            .iter()
            .any(|log| log.message.contains("产出的值")),
        "后序步骤应能读到前序产物: {:?}",
        consumed.logs
    );
    assert_eq!(
        outcome.artifacts.get("value").unwrap(),
        &serde_json::json!("产出的值")
    );
}

// ---- Scenario: 中间步骤失败 ----

#[tokio::test]
async fn a_failing_step_stops_everything_after_it() {
    let db = database().await;
    let credentials = credential_store(&db);
    let state = NoState;
    let counting = Arc::new(AtomicUsize::new(0));
    let registry = registry(Arc::clone(&counting));
    let runner = PipelineRunner::new(&registry, &credentials, &state);

    // 第二个步骤失败，第三个不该被执行。
    let outcome = runner
        .run(
            &pipeline(vec![
                step(0, "test.counted"),
                step(1, "test.fails"),
                step(2, "test.counted"),
            ]),
            1,
        )
        .await
        .unwrap();

    assert_eq!(outcome.status, RunStatus::Failed);
    assert!(!outcome.is_success());

    // 核心断言：第三个步骤一次也没跑过。
    assert_eq!(
        counting.load(Ordering::SeqCst),
        1,
        "失败之后的步骤不应被执行"
    );

    // 记录只到失败那一步——把没跑的步骤也列出来会让人以为跑过了。
    assert_eq!(outcome.steps.len(), 2, "只应记录到失败那一步");
    match &outcome.steps[1].status {
        StepStatus::Failed { reason } => {
            assert!(reason.contains("故意失败"), "{reason}");
        }
        other => panic!("期望 Failed，实际 {other:?}"),
    }
}

#[tokio::test]
async fn the_failure_names_the_step_and_the_reason() {
    let db = database().await;
    let credentials = credential_store(&db);
    let state = NoState;
    let registry = registry(Arc::new(AtomicUsize::new(0)));
    let runner = PipelineRunner::new(&registry, &credentials, &state);

    let outcome = runner
        .run(
            &pipeline(vec![step(0, "test.fails"), step(1, "test.produce")]),
            1,
        )
        .await
        .unwrap();

    let failure = outcome.failure.expect("失败时应带详情");
    assert_eq!(failure.step_order, 0);
    assert_eq!(failure.type_id, "test.fails");
    assert!(failure.reason.contains("故意失败"), "{failure:?}");
}

#[tokio::test]
async fn logs_from_the_failing_step_are_kept() {
    // 失败现场的上下文多半就在步骤自己记的日志里，不能因为失败就丢掉。
    let db = database().await;
    let credentials = credential_store(&db);
    let state = NoState;
    let registry = registry(Arc::new(AtomicUsize::new(0)));
    let runner = PipelineRunner::new(&registry, &credentials, &state);

    let outcome = runner
        .run(&pipeline(vec![step(0, "test.fails")]), 1)
        .await
        .unwrap();

    let failed = &outcome.steps[0];
    assert!(
        failed
            .logs
            .iter()
            .any(|log| log.message.contains("失败现场")),
        "失败步骤的日志应被保留: {:?}",
        failed.logs
    );
}

// ---- 中止的其它入口 ----

#[tokio::test]
async fn an_unknown_type_aborts_before_anything_runs() {
    let db = database().await;
    let credentials = credential_store(&db);
    let state = NoState;
    let counting = Arc::new(AtomicUsize::new(0));
    let registry = registry(Arc::clone(&counting));
    let runner = PipelineRunner::new(&registry, &credentials, &state);

    let outcome = runner
        .run(
            &pipeline(vec![step(0, "test.nonexistent"), step(1, "test.counted")]),
            1,
        )
        .await
        .unwrap();

    let failure = outcome.failure.expect("未知类型应导致失败");
    assert!(failure.reason.contains("未知的任务类型"), "{failure:?}");
    // 错误里还该列出可用的类型，否则用户只能去翻代码。
    assert!(failure.reason.contains("test.produce"), "{failure:?}");
    assert_eq!(counting.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn an_invalid_input_aborts_before_the_step_runs() {
    let db = database().await;
    let credentials = credential_store(&db);
    let state = NoState;
    let counting = Arc::new(AtomicUsize::new(0));
    let registry = registry(Arc::clone(&counting));
    let runner = PipelineRunner::new(&registry, &credentials, &state);

    // NeedsDomains 声明了必填的 domains，这里给空输入。
    let outcome = runner
        .run(
            &pipeline(vec![step(0, "test.needs_domains"), step(1, "test.counted")]),
            1,
        )
        .await
        .unwrap();

    let failure = outcome.failure.expect("输入不合法应导致失败");
    assert_eq!(failure.type_id, "test.needs_domains");
    assert!(
        failure.reason.contains("domains"),
        "应指明字段名: {failure:?}"
    );
    // 若真的执行了，NeedsDomains 会 panic —— 走到这里说明没执行。
    assert_eq!(counting.load(Ordering::SeqCst), 0);
}

// ---- 顺序与停用 ----

#[tokio::test]
async fn order_comes_from_order_index_not_the_array() {
    let db = database().await;
    let credentials = credential_store(&db);
    let state = NoState;
    let registry = registry(Arc::new(AtomicUsize::new(0)));
    let runner = PipelineRunner::new(&registry, &credentials, &state);

    // 传进去的顺序是反的：produce 的 order_index 更小，应当先跑。
    let outcome = runner
        .run(
            &pipeline(vec![step(1, "test.consume"), step(0, "test.produce")]),
            1,
        )
        .await
        .unwrap();

    assert!(
        outcome.is_success(),
        "按 order_index 排序后应能跑通: {:?}",
        outcome.failure
    );
    let order: Vec<i32> = outcome.steps.iter().map(|run| run.order_index).collect();
    assert_eq!(order, vec![0, 1], "执行顺序应由 order_index 决定");
}

#[tokio::test]
async fn disabled_steps_are_skipped_not_executed() {
    let db = database().await;
    let credentials = credential_store(&db);
    let state = NoState;
    let counting = Arc::new(AtomicUsize::new(0));
    let registry = registry(Arc::clone(&counting));
    let runner = PipelineRunner::new(&registry, &credentials, &state);

    let mut skipped = step(0, "test.counted");
    skipped.enabled = false;

    let outcome = runner
        .run(&pipeline(vec![skipped, step(1, "test.produce")]), 1)
        .await
        .unwrap();

    assert!(outcome.is_success(), "停用不是失败");
    assert_eq!(counting.load(Ordering::SeqCst), 0, "停用的步骤不应被执行");
    assert_eq!(outcome.steps[0].status, StepStatus::Skipped);
    assert!(
        matches!(outcome.steps[0].logs.as_slice(), [log] if log.message.contains("停用")),
        "跳过应留下说明: {:?}",
        outcome.steps[0].logs
    );
    assert_eq!(outcome.succeeded_steps(), 1);
}

#[tokio::test]
async fn a_step_log_level_round_trips() {
    // 顺带确认日志级别不会在传递中丢失——运行历史要靠它区分信息与告警。
    let db = database().await;
    let credentials = credential_store(&db);
    let state = NoState;
    let registry = registry(Arc::new(AtomicUsize::new(0)));
    let runner = PipelineRunner::new(&registry, &credentials, &state);

    let outcome = runner
        .run(&pipeline(vec![step(0, "test.fails")]), 1)
        .await
        .unwrap();

    assert!(
        outcome.steps[0]
            .logs
            .iter()
            .any(|log| log.level == acmecast_pipeline::StepLogLevel::Warn),
        "{:?}",
        outcome.steps[0].logs
    );
}

// ---- 产物传递 ----

#[tokio::test]
async fn a_deploy_step_reads_the_certificate_produced_earlier() {
    // spec 场景：部署步骤在上下文中读到 PEM 与私钥，无需重新读取磁盘。
    let db = database().await;
    let credentials = credential_store(&db);
    let state = NoState;
    let calls = Arc::new(AtomicUsize::new(0));

    let mut registry = StepRegistry::new();
    registry.register(ApplyCert).unwrap();
    registry.register(DeployCert(Arc::clone(&calls))).unwrap();
    let runner = PipelineRunner::new(&registry, &credentials, &state);

    let outcome = runner
        .run(
            &pipeline(vec![step(0, "cert.apply"), step(1, "cert.deploy")]),
            1,
        )
        .await
        .unwrap();

    assert!(outcome.is_success(), "{:?}", outcome.failure);
    assert_eq!(calls.load(Ordering::SeqCst), 1);

    // 部署步骤拿到的是 PEM 的实际内容，而不是一个路径或引用。
    let deploy = &outcome.steps[1];
    assert!(
        deploy.logs.iter().any(|log| log
            .message
            .contains(&format!("{} 字节的证书", CERT_PEM.len()))),
        "应读到 PEM 的实际内容: {:?}",
        deploy.logs
    );
    assert_eq!(
        outcome.artifacts.get("key_pem").unwrap().as_str(),
        Some(KEY_PEM)
    );
}

#[tokio::test]
async fn a_missing_declared_artifact_stops_before_execution() {
    let db = database().await;
    let credentials = credential_store(&db);
    let state = NoState;
    let calls = Arc::new(AtomicUsize::new(0));

    let mut registry = StepRegistry::new();
    registry.register(DeployCert(Arc::clone(&calls))).unwrap();
    let runner = PipelineRunner::new(&registry, &credentials, &state);

    // 直接从部署开始：它声明需要的两个产物都还不存在。
    let outcome = runner
        .run(&pipeline(vec![step(0, "cert.deploy")]), 1)
        .await
        .unwrap();

    let failure = outcome.failure.expect("缺产物应导致失败");
    assert!(
        failure.reason.contains("cert_pem"),
        "应指出缺的是哪一个: {failure:?}"
    );
    assert_eq!(
        calls.load(Ordering::SeqCst),
        0,
        "缺产物属于执行前的检查，步骤不该被调用"
    );
}

#[tokio::test]
async fn a_missing_artifact_error_lists_what_is_available() {
    // 「你少了什么」不如「现在有什么」有用——后者直接指出步骤顺序写反了。
    let db = database().await;
    let credentials = credential_store(&db);
    let state = NoState;

    let mut registry = StepRegistry::new();
    registry.register(ApplyCert).unwrap();
    registry.register(NeedsConfig).unwrap();
    let runner = PipelineRunner::new(&registry, &credentials, &state);

    let outcome = runner
        .run(
            &pipeline(vec![step(0, "cert.apply"), step(1, "test.needs_config")]),
            1,
        )
        .await
        .unwrap();

    let failure = outcome.failure.expect("应失败");
    assert!(failure.reason.contains("config_json"), "{failure:?}");
    assert!(
        failure.reason.contains("cert_pem"),
        "错误里应列出目前可用的产物: {failure:?}"
    );
}

#[tokio::test]
async fn a_structured_artifact_can_be_read_into_a_typed_struct() {
    let db = database().await;
    let credentials = credential_store(&db);
    let state = NoState;

    let mut registry = StepRegistry::new();
    registry.register(ProduceDomains).unwrap();
    registry.register(ConsumeDomains).unwrap();
    let runner = PipelineRunner::new(&registry, &credentials, &state);

    let outcome = runner
        .run(
            &pipeline(vec![
                step(0, "test.produce_domains"),
                step(1, "test.consume_domains"),
            ]),
            1,
        )
        .await
        .unwrap();

    assert!(outcome.is_success(), "{:?}", outcome.failure);
    assert!(
        outcome.steps[1]
            .logs
            .iter()
            .any(|log| log.message.contains("2 个域名")),
        "{:?}",
        outcome.steps[1].logs
    );
}

#[tokio::test]
async fn an_artifact_of_the_wrong_shape_reports_a_parse_error() {
    let db = database().await;
    let credentials = credential_store(&db);
    let state = NoState;

    let mut registry = StepRegistry::new();
    registry.register(ProduceWrongShape).unwrap();
    registry.register(ConsumeDomains).unwrap();
    let runner = PipelineRunner::new(&registry, &credentials, &state);

    let outcome = runner
        .run(
            &pipeline(vec![
                step(0, "test.wrong_shape"),
                step(1, "test.consume_domains"),
            ]),
            1,
        )
        .await
        .unwrap();

    // 名字在、形状不对：这与「缺产物」是不同的排查方向，错误信息也不该混同。
    let failure = outcome.failure.expect("应失败");
    assert!(
        failure.reason.contains("无法解析"),
        "应报解析错误而非缺少产物: {failure:?}"
    );
    assert_eq!(failure.step_order, 1, "失败应发生在消费方");
}
