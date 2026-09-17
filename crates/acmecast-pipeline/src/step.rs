//! 步骤契约与执行上下文。

use std::collections::BTreeMap;
use std::sync::Mutex;

use acmecast_access::CredentialStore;
use schemars::schema::RootSchema;

use crate::artifacts::Artifacts;
use crate::error::{Error, Result};
use crate::state::PipelineStateStore;

/// 一条步骤日志。
///
/// 日志会被落库并在运行历史里展示，因此**不要写入凭据明文**——
/// 需要说明「用了哪份凭据」时用凭据标识，不要用它的内容。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepLog {
    /// 级别。
    pub level: StepLogLevel,
    /// 内容。
    pub message: String,
    /// 产生时间。
    ///
    /// 逐条记录而不是落库时统一填运行结束时间：后者会让同一次运行的日志
    /// 时间戳完全相同，「这一步花了多久」就再也看不出来了。
    pub created_at: chrono::DateTime<chrono::Utc>,
}

/// 步骤日志级别。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepLogLevel {
    /// 常规信息。
    Info,
    /// 警告：不影响本次成功，但值得看一眼。
    Warn,
    /// 失败前的说明。
    Error,
}

impl StepLogLevel {
    /// 与库中存储形态互转的字符串表示。
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Info => "info",
            Self::Warn => "warn",
            Self::Error => "error",
        }
    }

    /// 从库中存储形态解析。
    ///
    /// `debug` 也归到 [`StepLogLevel::Info`]：本 crate 不产生调试级日志，
    /// 但库里可能有别处写入的，遇到时降级显示好过报错。
    #[must_use]
    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "info" | "debug" => Some(Self::Info),
            "warn" => Some(Self::Warn),
            "error" => Some(Self::Error),
            _ => None,
        }
    }
}

impl StepLog {
    /// 一条信息级日志。
    #[must_use]
    pub fn info(message: impl Into<String>) -> Self {
        Self::new(StepLogLevel::Info, message)
    }

    /// 一条警告级日志。
    #[must_use]
    pub fn warn(message: impl Into<String>) -> Self {
        Self::new(StepLogLevel::Warn, message)
    }

    /// 一条错误级日志。
    #[must_use]
    pub fn error(message: impl Into<String>) -> Self {
        Self::new(StepLogLevel::Error, message)
    }

    /// 按级别建一条日志，时间戳取当下。
    #[must_use]
    pub fn new(level: StepLogLevel, message: impl Into<String>) -> Self {
        Self {
            level,
            message: message.into(),
            created_at: chrono::Utc::now(),
        }
    }
}

/// 步骤执行成功后的结果。
///
/// 步骤对运行状态的贡献全部集中在这里：产出什么、记了什么日志。
/// 执行器负责把产物并入 [`Artifacts`]（含同名覆盖的告警）、把日志成批落库。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct StepOutput {
    /// 本次产出的具名产物，供后序步骤读取。
    pub artifacts: BTreeMap<String, serde_json::Value>,
    /// 本次记录的日志。
    pub logs: Vec<StepLog>,
}

impl StepOutput {
    /// 什么也没产出。
    #[must_use]
    pub fn empty() -> Self {
        Self::default()
    }

    /// 追加一件产物。
    #[must_use]
    pub fn with_artifact(mut self, name: impl Into<String>, value: serde_json::Value) -> Self {
        self.artifacts.insert(name.into(), value);
        self
    }

    /// 追加一条日志。
    #[must_use]
    pub fn with_log(mut self, log: StepLog) -> Self {
        self.logs.push(log);
        self
    }
}

/// 一种流水线步骤。
///
/// 实现者描述「这一步做什么」，不持有输入——注册表在启动时把实现一次性装好，
/// 因此每个实现都是无状态单例，输入从 [`StepContext`] 取。
#[async_trait::async_trait]
pub trait PipelineStep: Send + Sync + std::fmt::Debug + 'static {
    /// 类型标识，如 `cert.apply`。在同一个注册表内必须唯一。
    fn type_id(&self) -> &'static str;

    /// 输入字段定义的 JSON Schema，供前端据此渲染表单。
    ///
    /// 用 `schemars` 从输入结构体直接推导，避免「结构体改了、Schema 没改」的漂移。
    /// 默认 `None` 表示该步骤不声明输入结构。
    fn input_schema(&self) -> Option<RootSchema> {
        None
    }

    /// 本步骤需要的前序产物名。
    ///
    /// 执行器会在调用 [`PipelineStep::execute`] **之前**确认它们都在——缺产物与
    /// 缺必填输入一样，属于「不该进入执行逻辑」的前置条件。
    ///
    /// 默认空：只在确实依赖某个具名产物时才声明。产物名依赖输入的场景
    /// （「部署到用户填的那个产物」）声明不了，仍由 [`StepContext::artifact`]
    /// 在运行时把关。
    fn required_artifacts(&self) -> &'static [&'static str] {
        &[]
    }

    /// 按本步骤声明的输入定义校验一份输入。
    ///
    /// 执行器应当在调用 [`PipelineStep::execute`] **之前**调用它——spec 要求
    /// 「缺失必填输入时返回指明字段名的校验错误，不进入任务执行逻辑」。
    ///
    /// 默认实现按 [`PipelineStep::input_schema`] 做通用校验；未声明 Schema 的步骤
    /// 直接放行，由它自己在反序列化时把关。
    fn validate_input(&self, input: &serde_json::Value) -> Result<()> {
        let Some(schema) = self.input_schema() else {
            return Ok(());
        };
        let schema = serde_json::to_value(schema)
            .map_err(|e| Error::Core(acmecast_core::Error::Serialization(e.to_string())))?;
        crate::input::validate(&schema, input)
    }

    /// 按一份**当前输入**导出定义，并把此刻不可见的字段标记出来。
    ///
    /// 与 [`PipelineStep::input_schema`] 的区别：后者是静态定义（字段带
    /// `x-visible-when` 条件，供前端动态判断），这里则是对给定输入求值后的结果——
    /// spec 要求「条件不满足时，导出定义中该字段被标记为隐藏」。
    ///
    /// 未声明输入结构的步骤返回 `Ok(None)`。
    fn export_input_schema(&self, input: &serde_json::Value) -> Result<Option<serde_json::Value>> {
        let Some(mut schema) = self.input_schema() else {
            return Ok(None);
        };
        crate::visibility::mark_hidden_fields(&mut schema, input);

        serde_json::to_value(schema)
            .map(Some)
            .map_err(|e| Error::Core(acmecast_core::Error::Serialization(e.to_string())))
    }

    /// 执行本步骤。
    ///
    /// 失败直接返回 `Err`，由执行器决定中止还是继续——步骤自己不该关心这个。
    async fn execute(&self, ctx: &mut StepContext<'_>) -> Result<StepOutput>;
}

/// 一次运行中传给步骤的执行上下文。
///
/// **只读**：步骤对世界的改动通过 [`StepOutput`] 返回，由执行器统一并入运行状态。
/// 唯一的例外是 [`StepContext::log`]——它在内部缓冲，因为「随手记一行日志」
/// 不该被写成一次异步落库，那既拖慢步骤也污染它的签名。
pub struct StepContext<'a> {
    pipeline_id: i64,
    run_id: i64,
    step_order: i32,
    input: &'a serde_json::Value,
    artifacts: &'a Artifacts,
    credentials: &'a CredentialStore<'a>,
    state: &'a dyn PipelineStateStore,
    /// 日志缓冲。用 `Mutex` 而非 `RefCell`，是为了让上下文仍是 `Sync`——
    /// 将来若有并行步骤或步骤内部起子任务，共享它才不会出问题。
    logs: Mutex<Vec<StepLog>>,
}

impl std::fmt::Debug for StepContext<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // 不打印日志内容：它可能含用户数据，且长度不可控。
        f.debug_struct("StepContext")
            .field("pipeline_id", &self.pipeline_id)
            .field("run_id", &self.run_id)
            .field("step_order", &self.step_order)
            .field("input", &self.input)
            .field("artifacts", &self.artifacts)
            .finish_non_exhaustive()
    }
}

impl<'a> StepContext<'a> {
    /// 由执行器构造。
    #[must_use]
    pub fn new(
        pipeline_id: i64,
        run_id: i64,
        step_order: i32,
        input: &'a serde_json::Value,
        artifacts: &'a Artifacts,
        credentials: &'a CredentialStore<'a>,
        state: &'a dyn PipelineStateStore,
    ) -> Self {
        Self {
            pipeline_id,
            run_id,
            step_order,
            input,
            artifacts,
            credentials,
            state,
            logs: Mutex::new(Vec::new()),
        }
    }

    /// 本步骤的输入。
    #[must_use]
    pub fn input(&self) -> &serde_json::Value {
        self.input
    }

    /// 把输入反序列化成步骤自己的结构体。
    ///
    /// 输入在执行前已按 [`PipelineStep::input_schema`] 校验过，
    /// 这里再失败通常是「结构体改了、Schema 没改」。
    pub fn input_as<T>(&self) -> Result<T>
    where
        T: serde::de::DeserializeOwned,
    {
        serde_json::from_value(self.input.clone())
            .map_err(|e| Error::Core(acmecast_core::Error::Serialization(e.to_string())))
    }

    /// 按名称读取前序步骤产出的产物。
    pub fn artifact(&self, name: &str) -> Result<&serde_json::Value> {
        self.artifacts.get(name)
    }

    /// 按名称读取产物并反序列化成步骤自己的类型。
    ///
    /// 名字不存在时报 [`Error::MissingArtifact`]；名字在、形状不对则报序列化错误——
    /// 两者的排查方向完全不同，不该混成一种。
    pub fn artifact_as<T>(&self, name: &str) -> Result<T>
    where
        T: serde::de::DeserializeOwned,
    {
        let value = self.artifact(name)?;
        serde_json::from_value(value.clone()).map_err(|e| {
            Error::Core(acmecast_core::Error::Serialization(format!(
                "产物 `{name}` 无法解析为期望的结构: {e}"
            )))
        })
    }

    /// 目前已有的全部产物。
    #[must_use]
    pub fn artifacts(&self) -> &Artifacts {
        self.artifacts
    }

    /// 凭据体系的入口：按标识取一份解密后的凭据。
    ///
    /// 凭据在**执行时**按标识现取，既不驻留在步骤配置里，也不跨运行缓存——
    /// 因此轮换或删除凭据后，下一次运行立刻按新状态行事。
    #[must_use]
    pub fn credentials(&self) -> &CredentialStore<'_> {
        self.credentials
    }

    /// 流水线级键值存储。
    #[must_use]
    pub fn state(&self) -> &dyn PipelineStateStore {
        self.state
    }

    /// 记一条日志；执行器会在本步结束后成批落库。
    pub fn log(&self, level: StepLogLevel, message: impl Into<String>) {
        // 锁中毒只说明别的线程在持锁时 panic 过；日志不值得为此中断执行。
        let mut logs = self
            .logs
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        logs.push(StepLog::new(level, message));
    }

    /// 记一条信息级日志。
    pub fn log_info(&self, message: impl Into<String>) {
        self.log(StepLogLevel::Info, message);
    }

    /// 记一条警告级日志。
    pub fn log_warn(&self, message: impl Into<String>) {
        self.log(StepLogLevel::Warn, message);
    }

    /// 取走已缓冲的日志（执行器用）。
    #[must_use]
    pub fn take_logs(&self) -> Vec<StepLog> {
        let mut logs = self
            .logs
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        std::mem::take(&mut *logs)
    }

    /// 流水线主键。
    #[must_use]
    pub fn pipeline_id(&self) -> i64 {
        self.pipeline_id
    }

    /// 本次运行的标识。
    #[must_use]
    pub fn run_id(&self) -> i64 {
        self.run_id
    }

    /// 本步骤在流水线中的序号，从 0 起。
    #[must_use]
    pub fn step_order(&self) -> i32 {
        self.step_order
    }
}
