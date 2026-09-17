//! 执行器：按顺序跑完一条流水线的启用步骤，中途失败则中止。
//!
//! 失败是**正常返回**而不是 `Err`：那是业务结果（会随运行历史落库），
//! `Err` 留给执行器自身出问题的情况。

use acmecast_access::CredentialStore;
use acmecast_store::entity::pipeline::TriggerSource;
use tracing::{info, warn};

use crate::artifacts::Artifacts;
use crate::error::Result;
use crate::event::{EventSink, PipelineEvent};
use crate::registry::StepRegistry;
use crate::state::PipelineStateStore;
use crate::step::{StepContext, StepLog};

/// 执行器看到的步骤定义。
#[derive(Debug, Clone, PartialEq)]
pub struct StepDefinition {
    /// 执行顺序，从 0 起。
    pub order_index: i32,
    /// 任务类型标识。
    pub type_id: String,
    /// JSON 输入。
    pub input: serde_json::Value,
    /// 是否启用；停用的步骤会被跳过而不是执行。
    pub enabled: bool,
}

/// 执行器看到的流水线定义。
///
/// 与持久层的实体分开：引擎不该绑定某种存储，这里也只需要它用得上的字段。
#[derive(Debug, Clone, PartialEq)]
pub struct PipelineDefinition {
    /// 流水线主键。
    pub id: i64,
    /// 步骤定义；执行顺序由各步的 `order_index` 决定，与数组顺序无关。
    pub steps: Vec<StepDefinition>,
}

/// 单步的执行状态。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StepStatus {
    /// 执行成功。
    Succeeded,
    /// 执行失败，附原因。
    Failed {
        /// 失败原因。
        reason: String,
    },
    /// 已停用，跳过。
    Skipped,
}

/// 单个步骤的运行记录。
#[derive(Debug, Clone, PartialEq)]
pub struct StepRun {
    /// 步骤序号。
    pub order_index: i32,
    /// 任务类型。
    pub type_id: String,
    /// 执行状态。
    pub status: StepStatus,
    /// 本步产生的日志，含框架补的说明性条目。
    pub logs: Vec<StepLog>,
}

/// 流水线为何失败。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunFailure {
    /// 失败步骤的序号。
    pub step_order: i32,
    /// 失败步骤的任务类型。
    pub type_id: String,
    /// 失败原因。
    pub reason: String,
}

/// 一次运行的总体状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunStatus {
    /// 全部启用步骤都成功。
    Succeeded,
    /// 中止于某一步。
    Failed,
}

/// 一次运行的结果。
#[derive(Debug, Clone, PartialEq)]
pub struct RunOutcome {
    /// 总体状态。
    pub status: RunStatus,
    /// 已走过的步骤记录，按执行顺序。
    ///
    /// 失败时**只到失败那一步为止**：后续步骤根本不曾执行，
    /// 把它们也列出来会让人以为跑过了。
    pub steps: Vec<StepRun>,
    /// 失败详情；成功时为 `None`。
    pub failure: Option<RunFailure>,
    /// 这次运行结束时手头可用的产物。
    pub artifacts: Artifacts,
}

impl RunOutcome {
    /// 是否成功。
    #[must_use]
    pub fn is_success(&self) -> bool {
        self.status == RunStatus::Succeeded
    }

    /// 成功走完的步骤数（不含跳过与失败的）。
    #[must_use]
    pub fn succeeded_steps(&self) -> usize {
        self.steps
            .iter()
            .filter(|run| run.status == StepStatus::Succeeded)
            .count()
    }
}

/// 一次运行的身份。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RunScope {
    pipeline_id: i64,
    run_id: i64,
}

/// 运行过程中累积下来的状态。
///
/// 打包成一个结构是为了让「收尾」这类需要完整现场的操作有个明确的入参，
/// 而不是拖着一长串参数。
#[derive(Debug, Default)]
struct RunState {
    steps: Vec<StepRun>,
    artifacts: Artifacts,
}

/// 流水线执行器。
#[derive(Debug)]
pub struct PipelineRunner<'a> {
    registry: &'a StepRegistry,
    credentials: &'a CredentialStore<'a>,
    state: &'a dyn PipelineStateStore,
    events: Option<&'a dyn EventSink>,
    trigger_source: TriggerSource,
}

impl<'a> PipelineRunner<'a> {
    /// 组装执行器。
    #[must_use]
    pub fn new(
        registry: &'a StepRegistry,
        credentials: &'a CredentialStore<'a>,
        state: &'a dyn PipelineStateStore,
    ) -> Self {
        Self {
            registry,
            credentials,
            state,
            events: None,
            trigger_source: TriggerSource::Manual,
        }
    }

    /// 接上事件订阅者。
    ///
    /// 单独一步而不是塞进 [`PipelineRunner::new`]：多数调用点（尤其是测试）
    /// 不关心事件，没必要为了可选能力去改所有构造处。
    #[must_use]
    pub fn with_events(mut self, sink: &'a dyn EventSink) -> Self {
        self.events = Some(sink);
        self
    }

    /// 设置本次运行的触发来源，缺省为手动触发。
    ///
    /// 它只影响 [`PipelineEvent::Started`] 的内容——运行历史里的触发来源
    /// 由调用方在落库时给出（见 `HistoryRecord`）。
    #[must_use]
    pub fn with_trigger(mut self, trigger_source: TriggerSource) -> Self {
        self.trigger_source = trigger_source;
        self
    }

    /// 发布一个事件；没有订阅者时什么也不做。
    async fn emit(&self, event: PipelineEvent) {
        if let Some(sink) = self.events {
            sink.publish(event).await;
        }
    }

    /// 按顺序执行一条流水线的全部启用步骤。
    pub async fn run(&self, pipeline: &PipelineDefinition, run_id: i64) -> Result<RunOutcome> {
        let scope = RunScope {
            pipeline_id: pipeline.id,
            run_id,
        };
        let mut artifacts = Artifacts::new();
        let mut steps = Vec::with_capacity(pipeline.steps.len());

        // 顺序由 order_index 决定，不依赖传入数组的排列。
        let mut ordered: Vec<&StepDefinition> = pipeline.steps.iter().collect();
        ordered.sort_by_key(|step| step.order_index);

        info!(
            pipeline_id = pipeline.id,
            run_id,
            steps = ordered.len(),
            "开始执行流水线"
        );

        self.emit(PipelineEvent::Started {
            pipeline_id: pipeline.id,
            run_id,
            trigger_source: self.trigger_source.as_str().to_owned(),
        })
        .await;

        for step in ordered {
            if !step.enabled {
                steps.push(StepRun {
                    order_index: step.order_index,
                    type_id: step.type_id.clone(),
                    status: StepStatus::Skipped,
                    logs: vec![StepLog::info("步骤已停用，跳过")],
                });
                self.emit(PipelineEvent::StepFinished {
                    pipeline_id: pipeline.id,
                    run_id,
                    step_order: step.order_index,
                    type_id: step.type_id.clone(),
                    skipped: true,
                })
                .await;
                continue;
            }

            // 1) 类型必须已注册——未知类型直接中止，后续步骤不再执行。
            let implementation = match self.registry.require(&step.type_id) {
                Ok(found) => found,
                Err(err) => {
                    return Ok(self
                        .abort(
                            RunState { steps, artifacts },
                            scope,
                            step,
                            err.to_string(),
                            Vec::new(),
                        )
                        .await);
                }
            };

            // 2) 输入校验：spec 要求「缺失必填输入时不进入任务执行逻辑」。
            if let Err(err) = implementation.validate_input(&step.input) {
                return Ok(self
                    .abort(
                        RunState { steps, artifacts },
                        scope,
                        step,
                        err.to_string(),
                        Vec::new(),
                    )
                    .await);
            }

            // 3) 声明的产物必须齐备。与输入校验同样是执行前的事：让步骤
            //    「跑到一半才发现缺东西」既浪费前面的工作，也把失败现场搅浑。
            for name in implementation.required_artifacts() {
                if let Err(err) = artifacts.get(name) {
                    return Ok(self
                        .abort(
                            RunState { steps, artifacts },
                            scope,
                            step,
                            err.to_string(),
                            Vec::new(),
                        )
                        .await);
                }
            }

            // 4) 执行。上下文只借用产物集合，作用域结束后才轮到写回。
            let (executed, mut logs) = {
                let mut ctx = StepContext::new(
                    pipeline.id,
                    run_id,
                    step.order_index,
                    &step.input,
                    &artifacts,
                    self.credentials,
                    self.state,
                );
                let result = implementation.execute(&mut ctx).await;
                (result, ctx.take_logs())
            };

            match executed {
                Ok(output) => {
                    artifacts.merge(step.order_index, &step.type_id, output.artifacts.clone());
                    logs.extend(output.logs);
                    info!(
                        step_order = step.order_index,
                        type_id = %step.type_id,
                        "步骤执行成功"
                    );
                    steps.push(StepRun {
                        order_index: step.order_index,
                        type_id: step.type_id.clone(),
                        status: StepStatus::Succeeded,
                        logs,
                    });
                    self.emit(PipelineEvent::StepFinished {
                        pipeline_id: pipeline.id,
                        run_id,
                        step_order: step.order_index,
                        type_id: step.type_id.clone(),
                        skipped: false,
                    })
                    .await;
                }
                Err(err) => {
                    // 步骤自己记的日志要留住——失败现场的上下文多半就在里面。
                    return Ok(self
                        .abort(
                            RunState { steps, artifacts },
                            scope,
                            step,
                            err.to_string(),
                            logs,
                        )
                        .await);
                }
            }
        }

        info!(pipeline_id = pipeline.id, run_id, "流水线执行成功");

        let outcome = RunOutcome {
            status: RunStatus::Succeeded,
            steps,
            failure: None,
            artifacts,
        };
        self.emit(PipelineEvent::Succeeded {
            pipeline_id: pipeline.id,
            run_id,
            succeeded_steps: outcome.succeeded_steps(),
        })
        .await;

        Ok(outcome)
    }

    /// 记录失败的那一步并收尾——后续步骤不再执行。
    async fn abort(
        &self,
        mut state: RunState,
        scope: RunScope,
        step: &StepDefinition,
        reason: String,
        logs: Vec<StepLog>,
    ) -> RunOutcome {
        warn!(
            step_order = step.order_index,
            type_id = %step.type_id,
            reason = %reason,
            "步骤失败，中止后续步骤"
        );

        self.emit(PipelineEvent::Failed {
            pipeline_id: scope.pipeline_id,
            run_id: scope.run_id,
            step_order: step.order_index,
            type_id: step.type_id.clone(),
            reason: reason.clone(),
        })
        .await;

        state.steps.push(StepRun {
            order_index: step.order_index,
            type_id: step.type_id.clone(),
            status: StepStatus::Failed {
                reason: reason.clone(),
            },
            logs,
        });

        RunOutcome {
            status: RunStatus::Failed,
            steps: state.steps,
            failure: Some(RunFailure {
                step_order: step.order_index,
                type_id: step.type_id.clone(),
                reason,
            }),
            artifacts: state.artifacts,
        }
    }
}
