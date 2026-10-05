//! 定时调度：cron 触发与证书到期扫描触发的执行中枢。
//!
//! 对应 `specs/scheduled-renewal/spec.md`。引擎本身不跑流水线——它判定
//! 「何时该触发、是否允许触发」，把启动动作交给注入的
//! [`PipelineLauncher`]；服务端组装时把启动器接到流水线执行器上。

pub mod config;
pub mod engine;
pub mod error;
pub mod launcher;

pub use config::SchedulerConfig;
pub use engine::SchedulerEngine;
pub use error::{Error, Result};
pub use launcher::{LaunchRequest, PipelineLauncher};
