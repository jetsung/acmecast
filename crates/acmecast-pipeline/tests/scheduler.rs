//! 6.10 并发上限与防重入。
//!
//! 两条防线分开验证：名额满了要**排队**（不是失败），同一流水线重复触发要
//! **被跳过**（不排队也不占名额）。

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use acmecast_access::CredentialStore;
use acmecast_pipeline::{
    DatabaseStateStore, PipelineRunner, PipelineStep, Result, RunScheduler, StepContext,
    StepOutput, StepRegistry,
};
use async_trait::async_trait;
use common::{credential_store, database, pipeline, step};
use tokio::time::timeout;

// ---- 名额 ----

#[tokio::test]
async fn acquiring_within_the_limit_succeeds() {
    let scheduler = RunScheduler::new(2);

    let first = scheduler.acquire(1).await.expect("第一个应拿到名额");
    let second = scheduler.acquire(2).await.expect("第二个应拿到名额");

    assert_eq!(first.pipeline_id(), 1);
    assert_eq!(second.pipeline_id(), 2);
    assert_eq!(scheduler.running_count(), 2);
    assert_eq!(scheduler.available_slots(), 0);
}

#[tokio::test]
async fn exceeding_the_limit_waits_rather_than_failing() {
    let scheduler = RunScheduler::new(1);
    let held = scheduler.acquire(1).await.expect("第一个应拿到");

    // 同一个 future 先后被借用两次：先短暂超时证明它在排队，
    // 名额空出后再等它完成——不必把它塞进 spawn（permit 借用着调度器）。
    let mut pending = std::pin::pin!(scheduler.acquire(2));
    let raced = timeout(Duration::from_millis(50), &mut pending).await;
    assert!(raced.is_err(), "超出上限时应排队等待，而不是立即返回");

    drop(held);
    let permit = timeout(Duration::from_secs(1), &mut pending)
        .await
        .expect("名额空出后应当继续，不该一直等下去")
        .expect("应拿到名额");
    assert_eq!(permit.pipeline_id(), 2);
}

#[tokio::test]
async fn permits_return_their_slot_when_dropped() {
    let scheduler = RunScheduler::new(1);
    {
        let permit = scheduler.acquire(1).await.expect("应拿到");
        assert_eq!(permit.pipeline_id(), 1);
        assert_eq!(scheduler.available_slots(), 0);
    }

    assert_eq!(scheduler.available_slots(), 1, "drop 后名额应归还");
    assert_eq!(scheduler.running_count(), 0, "占用也应解除");
}

// ---- 防重入 ----

#[tokio::test]
async fn a_second_trigger_of_a_running_pipeline_is_skipped() {
    let scheduler = RunScheduler::new(4);
    let held = scheduler.acquire(7).await.expect("首次应拿到");

    // 同一条流水线再次触发：立即被跳过，不会排队等自己结束。
    let skipped = timeout(Duration::from_millis(50), scheduler.acquire(7))
        .await
        .expect("重复触发应当立即返回，而不是排队");
    assert!(skipped.is_none(), "重复触发应被跳过");

    // 上一次结束后可以再触发。
    drop(held);
    assert!(
        scheduler.acquire(7).await.is_some(),
        "上一次结束后应能再次运行"
    );
}

#[tokio::test]
async fn a_skipped_trigger_does_not_consume_a_slot() {
    // 跳过的触发若占了名额，并发上限会被静默虚耗——上线后表现为
    // 「明明没几条在跑，新的却一直排队」。
    let scheduler = RunScheduler::new(2);
    let held = scheduler.acquire(1).await.expect("应拿到");
    assert_eq!(scheduler.available_slots(), 1);

    for _ in 0..5 {
        assert!(scheduler.acquire(1).await.is_none(), "重复触发应被跳过");
    }

    assert_eq!(scheduler.available_slots(), 1, "跳过的触发不该占名额");
    assert_eq!(scheduler.running_count(), 1);

    drop(held);
    assert_eq!(scheduler.available_slots(), 2);
}

#[tokio::test]
async fn different_pipelines_do_not_block_each_other() {
    let scheduler = RunScheduler::new(4);

    // permit 是 RAII：不留下它们，占用会随 drop 一并解除。
    let mut permits = Vec::new();
    for id in 1..=4 {
        permits.push(
            scheduler
                .acquire(id)
                .await
                .unwrap_or_else(|| panic!("流水线 {id} 应拿到名额")),
        );
    }

    assert_eq!(scheduler.running_count(), 4);
    assert_eq!(scheduler.available_slots(), 0);
}

#[tokio::test]
async fn a_cancelled_acquire_does_not_wedge_the_pipeline() {
    // 排队途中被取消是常见情形：调用方超时、上层 future 被 drop 都算。
    // 若「正在运行」的标记没被一并清掉，这条流水线此后永远拿不到名额，
    // 而现象只是「它一直不跑」——最难排查的那类故障。
    let scheduler = RunScheduler::new(1);
    let held = scheduler.acquire(1).await.expect("应拿到");

    // 流水线 2 排在队里，然后被取消。
    let cancelled = timeout(Duration::from_millis(30), scheduler.acquire(2)).await;
    assert!(cancelled.is_err(), "名额满时它应当在排队");

    assert_eq!(
        scheduler.running_count(),
        1,
        "被取消的占用应立即解除，不能留在集合里"
    );

    // 名额空出后，流水线 2 应当能正常拿到。
    drop(held);
    let permit = timeout(Duration::from_millis(500), scheduler.acquire(2))
        .await
        .expect("被取消不该让这条流水线永久无法运行")
        .expect("应拿到名额");
    assert_eq!(permit.pipeline_id(), 2);
}

// ---- 与执行器一起用 ----

/// 记录执行期间的并发度。
#[derive(Debug)]
struct TrackConcurrency {
    current: Arc<AtomicUsize>,
    peak: Arc<AtomicUsize>,
}

#[async_trait]
impl PipelineStep for TrackConcurrency {
    fn type_id(&self) -> &'static str {
        "test.track"
    }

    async fn execute(&self, _ctx: &mut StepContext<'_>) -> Result<StepOutput> {
        let now = self.current.fetch_add(1, Ordering::SeqCst) + 1;
        self.peak.fetch_max(now, Ordering::SeqCst);
        tokio::time::sleep(Duration::from_millis(30)).await;
        self.current.fetch_sub(1, Ordering::SeqCst);
        Ok(StepOutput::empty())
    }
}

/// 走一遍「拿名额 → 跑 → 归还」。
///
/// 用借用而非 `'static`：`CredentialStore` / `DatabaseStateStore` 都借用数据库连接，
/// 塞进 `spawn` 会被要求活到 `'static`。同一任务内用 `join!` 并发即可——
/// 步骤里是 `sleep`，轮转调度足以让三次触发真的重叠。
async fn trigger(
    scheduler: &RunScheduler,
    registry: &StepRegistry,
    credentials: &CredentialStore<'_>,
    state: &DatabaseStateStore<'_>,
    id: i64,
) -> bool {
    let permit = scheduler.acquire(id).await.expect("最终都应拿到名额");
    let runner = PipelineRunner::new(registry, credentials, state);
    let outcome = runner
        .run(&pipeline(id, vec![step(0, "test.track")]), 1)
        .await;
    drop(permit);

    outcome.expect("执行器不应出错").is_success()
}

#[tokio::test]
async fn the_scheduler_actually_caps_concurrency() {
    let db = database().await;
    let credentials = credential_store(&db);
    let state = DatabaseStateStore::new(&db);

    let current = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));
    let mut registry = StepRegistry::new();
    registry
        .register(TrackConcurrency {
            current: Arc::clone(&current),
            peak: Arc::clone(&peak),
        })
        .unwrap();

    // 上限 1，同时触发三次——用不同的流水线标识绕开防重入，只测并发上限。
    let scheduler = RunScheduler::new(1);
    let (first, second, third) = tokio::join!(
        trigger(&scheduler, &registry, &credentials, &state, 1),
        trigger(&scheduler, &registry, &credentials, &state, 2),
        trigger(&scheduler, &registry, &credentials, &state, 3),
    );

    assert!(first && second && third, "三次触发都应成功（先后执行）");
    assert_eq!(
        peak.load(Ordering::SeqCst),
        1,
        "上限为 1 时，同时执行的流水线不该超过 1 条"
    );
    assert_eq!(scheduler.available_slots(), 1, "结束后名额应全部归还");
    assert_eq!(scheduler.running_count(), 0, "结束后不该有残留占用");
}
