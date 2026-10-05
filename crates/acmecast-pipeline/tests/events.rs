//! 6.9 生命周期事件发布。
//!
//! spec 要求四个节点各发结构化事件：开始、步骤完成、成功、失败。
//! 其中被点名验证的是失败事件——它得包含**流水线标识与失败摘要**，
//! 否则订阅者收到「某处失败了」也没法做通知。

mod common;

use std::sync::Mutex;

use acmecast_pipeline::{
    EventSink, PipelineDefinition, PipelineEvent, PipelineRunner, PipelineStep, Result, RunOutcome,
    StepContext, StepOutput, StepRegistry,
};
use async_trait::async_trait;
use common::{credential_store, database, pipeline, step};

// ---- 步骤 ----

#[derive(Debug)]
struct Fine;

#[async_trait]
impl PipelineStep for Fine {
    fn type_id(&self) -> &'static str {
        "test.fine"
    }

    async fn execute(&self, _ctx: &mut StepContext<'_>) -> Result<StepOutput> {
        Ok(StepOutput::empty())
    }
}

#[derive(Debug)]
struct Broken;

#[async_trait]
impl PipelineStep for Broken {
    fn type_id(&self) -> &'static str {
        "test.broken"
    }

    async fn execute(&self, _ctx: &mut StepContext<'_>) -> Result<StepOutput> {
        Err(acmecast_pipeline::Error::Core(
            acmecast_core::Error::Internal("磁盘写满了".to_owned()),
        ))
    }
}

/// 把事件收进 Vec 的订阅者。
#[derive(Debug, Default)]
struct RecordingSink(Mutex<Vec<PipelineEvent>>);

impl RecordingSink {
    fn events(&self) -> Vec<PipelineEvent> {
        self.0.lock().expect("锁不应中毒").clone()
    }
}

#[async_trait]
impl EventSink for RecordingSink {
    async fn publish(&self, event: PipelineEvent) {
        self.0.lock().expect("锁不应中毒").push(event);
    }
}

fn registry() -> StepRegistry {
    let mut registry = StepRegistry::new();
    registry.register(Fine).unwrap();
    registry.register(Broken).unwrap();
    registry
}

/// 跑一次并返回（结果, 订阅者）。
async fn run_with_events(
    registry: &StepRegistry,
    sink: &RecordingSink,
    definition: &PipelineDefinition,
) -> RunOutcome {
    let db = database().await;
    let credentials = credential_store(&db);
    let state = acmecast_pipeline::DatabaseStateStore::new(&db);
    let runner = PipelineRunner::new(registry, &credentials, &state).with_events(sink);

    runner.run(definition, 7).await.expect("执行器自身不应出错")
}

// ---- Scenario: 流水线失败事件 ----

#[tokio::test]
async fn a_failure_event_carries_the_pipeline_and_the_reason() {
    let sink = RecordingSink::default();
    let outcome = run_with_events(
        &registry(),
        &sink,
        &pipeline(42, vec![step(0, "test.fine"), step(1, "test.broken")]),
    )
    .await;
    assert!(!outcome.is_success());

    let failed = sink
        .events()
        .into_iter()
        .find_map(|event| match event {
            PipelineEvent::Failed {
                pipeline_id,
                run_id,
                step_order,
                type_id,
                reason,
            } => Some((pipeline_id, run_id, step_order, type_id, reason)),
            _ => None,
        })
        .expect("失败时应发出 Failed 事件");

    // spec 点名的两项：流水线标识与失败摘要。
    assert_eq!(failed.0, 42, "事件应带流水线标识");
    assert_eq!(failed.1, 7, "事件应带本次运行标识");
    assert_eq!(failed.2, 1, "应指出是第几步失败");
    assert_eq!(failed.3, "test.broken");
    assert!(
        failed.4.contains("磁盘写满了"),
        "摘要应含失败原因: {}",
        failed.4
    );
}

#[tokio::test]
async fn the_failure_event_is_emitted_right_after_the_failing_step() {
    let sink = RecordingSink::default();
    let outcome = run_with_events(
        &registry(),
        &sink,
        &pipeline(1, vec![step(0, "test.broken"), step(1, "test.fine")]),
    )
    .await;

    let events = sink.events();
    let kinds: Vec<&str> = events
        .iter()
        .map(|event| match event {
            PipelineEvent::Started { .. } => "started",
            PipelineEvent::StepFinished { .. } => "step",
            PipelineEvent::Succeeded { .. } => "succeeded",
            PipelineEvent::Failed { .. } => "failed",
        })
        .collect();

    // 第二个步骤根本没跑，因此没有它的 StepFinished，也没有 Succeeded。
    assert_eq!(kinds, vec!["started", "failed"], "{events:?}");
    assert_eq!(outcome.steps.len(), 1);
}

// ---- 其余三个节点 ----

#[tokio::test]
async fn a_successful_run_emits_start_step_and_success_events_in_order() {
    let sink = RecordingSink::default();
    let outcome = run_with_events(
        &registry(),
        &sink,
        &pipeline(9, vec![step(0, "test.fine"), step(1, "test.fine")]),
    )
    .await;
    assert!(outcome.is_success());

    let events = sink.events();
    assert_eq!(events.len(), 4, "开始 + 两步完成 + 成功: {events:?}");

    assert!(matches!(
        events[0],
        PipelineEvent::Started {
            pipeline_id: 9,
            run_id: 7,
            ..
        }
    ));
    for (index, event) in events[1..3].iter().enumerate() {
        match event {
            PipelineEvent::StepFinished {
                pipeline_id,
                step_order,
                skipped,
                ..
            } => {
                assert_eq!(*pipeline_id, 9);
                assert_eq!(*step_order, index as i32);
                assert!(!skipped);
            }
            other => panic!("期望 StepFinished，实际 {other:?}"),
        }
    }
    match events[3] {
        PipelineEvent::Succeeded {
            pipeline_id,
            run_id,
            succeeded_steps,
        } => {
            assert_eq!(pipeline_id, 9);
            assert_eq!(run_id, 7);
            assert_eq!(succeeded_steps, 2);
        }
        ref other => panic!("期望 Succeeded，实际 {other:?}"),
    }
}

#[tokio::test]
async fn a_skipped_step_still_reports_its_finish() {
    // 停用的步骤也是「走过了一遍」，订阅者做统计时要能看见它。
    let sink = RecordingSink::default();
    let mut disabled = step(0, "test.fine");
    disabled.enabled = false;

    run_with_events(
        &registry(),
        &sink,
        &pipeline(1, vec![disabled, step(1, "test.fine")]),
    )
    .await;

    let skipped: Vec<bool> = sink
        .events()
        .iter()
        .filter_map(|event| match event {
            PipelineEvent::StepFinished { skipped, .. } => Some(*skipped),
            _ => None,
        })
        .collect();
    assert_eq!(skipped, vec![true, false]);
}

#[tokio::test]
async fn the_trigger_source_rides_along_on_the_start_event() {
    let db = database().await;
    let credentials = credential_store(&db);
    let state = acmecast_pipeline::DatabaseStateStore::new(&db);
    let registry = registry();
    let sink = RecordingSink::default();

    let runner = PipelineRunner::new(&registry, &credentials, &state)
        .with_events(&sink)
        .with_trigger(acmecast_store::entity::pipeline::TriggerSource::Cron);
    runner
        .run(&pipeline(1, vec![step(0, "test.fine")]), 1)
        .await
        .unwrap();

    match &sink.events()[0] {
        PipelineEvent::Started { trigger_source, .. } => assert_eq!(trigger_source, "cron"),
        other => panic!("期望 Started，实际 {other:?}"),
    }
}

// ---- 无订阅者 ----

#[tokio::test]
async fn a_run_without_a_subscriber_is_fine() {
    // 事件是可选的：没接订阅者时执行照常，不该因此出岔子。
    let db = database().await;
    let credentials = credential_store(&db);
    let state = acmecast_pipeline::DatabaseStateStore::new(&db);
    let registry = registry();

    let runner = PipelineRunner::new(&registry, &credentials, &state);
    let outcome = runner
        .run(&pipeline(1, vec![step(0, "test.fine")]), 1)
        .await
        .unwrap();

    assert!(outcome.is_success());
}

// ---- 便利访问器 ----

#[test]
fn the_shared_accessors_work_for_every_variant() {
    let started = PipelineEvent::Started {
        pipeline_id: 3,
        run_id: 4,
        trigger_source: "manual".to_owned(),
    };
    assert_eq!(started.pipeline_id(), 3);
    assert_eq!(started.run_id(), 4);

    let failed = PipelineEvent::Failed {
        pipeline_id: 5,
        run_id: 6,
        step_order: 1,
        type_id: "test.x".to_owned(),
        reason: "坏了".to_owned(),
    };
    assert_eq!(failed.pipeline_id(), 5);
    assert_eq!(failed.run_id(), 6);
}
