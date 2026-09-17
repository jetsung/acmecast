//! SeaORM 实体定义。
//!
//! 八张核心表：**证书、流水线、流水线步骤、运行历史、步骤日志、
//! 键值存储、凭据、调度配置**；部署记录表由后续迁移单独建立。
//!
//! 所有实体平铺在 `entity/` 下（这是 SeaORM 关系 derive 的路径约定），
//! 表名统一加 `acmecast_` 前缀，列名避开各数据库方言的保留字
//! （例如 `store_key` 而非 `key`——`key` 是 MySQL 保留字）。

pub mod cert;
pub mod credential;
pub mod deployment;
pub mod history;
pub mod history_log;
pub mod pipeline;
pub mod pipeline_step;
pub mod schedule;
pub mod storage;
pub mod trigger_log;

pub use cert::{Entity as Cert, Model as CertModel};
pub use credential::{Entity as Credential, Model as CredentialModel};
pub use deployment::{Entity as Deployment, Model as DeploymentModel, TargetRef};
pub use history::{Entity as History, Model as HistoryModel, RunStatus};
pub use history_log::{Entity as HistoryLog, Level as LogLevel, Model as LogModel};
pub use pipeline::{Entity as Pipeline, Model as PipelineModel, TriggerSource};
pub use pipeline_step::{Entity as PipelineStep, Model as StepModel};
pub use schedule::{Entity as Schedule, Model as ScheduleModel};
pub use storage::{Entity as Storage, Model as StorageModel};
pub use trigger_log::{Entity as TriggerLog, Model as TriggerLogModel};
