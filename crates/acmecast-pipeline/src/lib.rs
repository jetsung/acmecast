//! 流水线引擎：步骤契约、执行上下文与运行编排。
//!
//! 对应 `specs/pipeline-engine/spec.md`。本 crate 定义「步骤长什么样、能拿到什么、
//! 返回什么」；具体步骤由 `acmecast-cert` / `acmecast-dns` / `acmecast-deploy`
//! 等 crate 实现，启动时在 `acmecast-server` 里一次性注册。

pub mod artifacts;
pub mod error;
pub mod event;
pub mod history;
pub mod input;
pub mod registry;
pub mod runner;
pub mod scheduler;
pub mod state;
pub mod step;
pub mod visibility;

pub use artifacts::{Artifact, Artifacts};
pub use error::{Error, Result};
pub use event::{EventSink, PipelineEvent};
pub use history::{
    HistoryEntry, HistoryPage, HistoryQuery, HistoryRecord, HistoryRepository, StepLogRecord,
};
pub use registry::StepRegistry;
pub use runner::{
    PipelineDefinition, PipelineRunner, RunFailure, RunOutcome, RunStatus, StepDefinition, StepRun,
    StepStatus,
};
pub use scheduler::{RunPermit, RunScheduler};
pub use state::{DatabaseStateStore, PipelineStateStore};
pub use step::{PipelineStep, StepContext, StepLog, StepLogLevel, StepOutput};
pub use visibility::{HIDDEN_KEY, VISIBLE_WHEN_KEY, VisibleWhen};
