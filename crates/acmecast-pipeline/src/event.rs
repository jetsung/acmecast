//! 流水线运行的生命周期事件。
//!
//! 引擎在四个节点发出结构化事件：开始、步骤完成、成功、失败。上层订阅后
//! 据此做通知与统计——引擎自己不认识任何通知渠道，那是订阅者的事。

use async_trait::async_trait;

/// 运行生命周期中的一个事件。
///
/// 失败与成功各成一体，而不是「统一事件 + 状态字段」：订阅者多为按事件类型
/// 分派（失败要告警、成功只记账），分类清晰比字段通用更省事。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PipelineEvent {
    /// 一次运行开始。
    Started {
        /// 流水线主键。
        pipeline_id: i64,
        /// 本次运行的标识。
        run_id: i64,
        /// 触发来源，取值与运行历史一致。
        trigger_source: String,
    },

    /// 某个步骤执行完成。
    ///
    /// 只覆盖成功与被跳过的步骤：失败的那一步由 [`PipelineEvent::Failed`]
    /// 承载，同一件事不需要两种事件各说一遍。
    StepFinished {
        /// 流水线主键。
        pipeline_id: i64,
        /// 本次运行的标识。
        run_id: i64,
        /// 步骤序号。
        step_order: i32,
        /// 任务类型。
        type_id: String,
        /// 这一步是被跳过（已停用）还是真的执行了。
        skipped: bool,
    },

    /// 一次运行成功结束。
    Succeeded {
        /// 流水线主键。
        pipeline_id: i64,
        /// 本次运行的标识。
        run_id: i64,
        /// 成功走完的步骤数。
        succeeded_steps: usize,
    },

    /// 一次运行因某步骤失败而中止。
    Failed {
        /// 流水线主键。
        pipeline_id: i64,
        /// 本次运行的标识。
        run_id: i64,
        /// 失败步骤的序号。
        step_order: i32,
        /// 失败步骤的任务类型。
        type_id: String,
        /// 失败摘要。
        reason: String,
    },
}

impl PipelineEvent {
    /// 事件所属的流水线。
    ///
    /// 订阅者常要按流水线分流（比如只关心某几条的告警），单独提出来省得每处 match。
    #[must_use]
    pub fn pipeline_id(&self) -> i64 {
        match self {
            Self::Started { pipeline_id, .. }
            | Self::StepFinished { pipeline_id, .. }
            | Self::Succeeded { pipeline_id, .. }
            | Self::Failed { pipeline_id, .. } => *pipeline_id,
        }
    }

    /// 本次运行的标识。
    #[must_use]
    pub fn run_id(&self) -> i64 {
        match self {
            Self::Started { run_id, .. }
            | Self::StepFinished { run_id, .. }
            | Self::Succeeded { run_id, .. }
            | Self::Failed { run_id, .. } => *run_id,
        }
    }
}

/// 事件订阅者。
///
/// 引擎在 [`publish`](EventSink::publish) 上 await，因此实现者若要做慢活
/// （发 webhook、写外部系统），应当自行 `spawn` 出去——引擎只保证
/// 「事件已交给订阅者」，不保证订阅者的下游已经处理完。
#[async_trait]
pub trait EventSink: Send + Sync + std::fmt::Debug {
    /// 接收一个事件。
    async fn publish(&self, event: PipelineEvent);
}
