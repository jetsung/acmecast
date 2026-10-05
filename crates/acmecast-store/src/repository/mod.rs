//! 仓储层：把实体操作收敛为带领域语义的方法。
//!
//! 上层（流水线引擎、HTTP API）不应自行拼 SeaORM 查询，而应经由仓储方法调用，
//! 这样「同一域名集合去重更新」之类的规则只有一处实现，不会在多个调用点漂移。

pub mod cert;
pub mod pipeline;
pub mod schedule;
pub mod trigger_log;

pub use cert::{
    CertInput, CertPage, CertQuery, CertRepository, CertSort, DEFAULT_PAGE_SIZE, MAX_PAGE_SIZE,
    SaveOutcome,
};
pub use pipeline::{
    Pipeline, PipelineInput, PipelineRepository, PipelineStep, PipelineStepInput, PipelineSummary,
};
pub use schedule::{
    ScheduleConfig, ScheduleInput, ScheduleRepository, next_cron_trigger, parse_cron,
};
pub use trigger_log::{TriggerLogEntry, TriggerLogInput, TriggerLogPage, TriggerLogRepository};
