//! 全局调度的串行序列行为（`global-schedule` spec）。
//!
//! 引擎判定显式接收 `now`，测试把时钟拨到想验证的位置直接调用 `tick`：
//! 到点启动序列、按 ID 升序串行、运行中/停用成员跳过、成员失败继续、
//! 触发点推进防重复，以及防重叠标志下的「到点只推进不启动」。

use std::sync::{Arc, Mutex};

use acmecast_pipeline::{HistoryRecord, HistoryRepository};
use acmecast_scheduler::{LaunchRequest, PipelineLauncher, SchedulerConfig, SchedulerEngine};
use acmecast_store::entity::history::RunStatus;
use acmecast_store::entity::pipeline::TriggerSource;
use acmecast_store::migrate;
use acmecast_store::repository::{
    GlobalScheduleInput, GlobalScheduleRepository, PipelineInput, PipelineRepository,
    TriggerLogRepository,
};
use async_trait::async_trait;
use chrono::{DateTime, Duration, Utc};
use sea_orm::{Database, DatabaseConnection};

// ---- 脚手架（与 scheduled_renewal.rs 同构） ----

/// 记录启动请求的假启动器；`fail` 为真时模拟启动失败。
#[derive(Debug, Default)]
struct RecordingLauncher {
    requests: Mutex<Vec<LaunchRequest>>,
    fail: bool,
}

impl RecordingLauncher {
    fn requested(&self) -> Vec<LaunchRequest> {
        self.requests.lock().expect("测试中的锁不会中毒").clone()
    }
}

#[async_trait]
impl PipelineLauncher for RecordingLauncher {
    async fn launch(&self, request: LaunchRequest) -> acmecast_scheduler::Result<()> {
        // 失败的尝试也记录：全局序列的测试要断言「失败成员仍被尝试、
        // 后续成员照常启动」，尝试本身即可观测事实。
        self.requests
            .lock()
            .expect("测试中的锁不会中毒")
            .push(request);
        if self.fail {
            return Err(acmecast_scheduler::Error::InvalidConfig(
                "启动被配置为失败".to_owned(),
            ));
        }
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

/// 组装引擎；launcher 泛型化以覆盖即时/慢速两种测试替身。
fn make_engine<'a, L: PipelineLauncher + 'static>(
    db: &'a DatabaseConnection,
    launcher: &'a Arc<L>,
) -> SchedulerEngine<'a> {
    SchedulerEngine::new(db, Arc::clone(launcher) as _, Default::default())
}

/// 落一条流水线并返回主键（id 随插入顺序递增，即全局序列的执行顺序）。
async fn a_pipeline(db: &DatabaseConnection, name: &str, enabled: bool) -> i64 {
    PipelineRepository::new(db)
        .save(
            None,
            PipelineInput {
                name: name.to_owned(),
                description: None,
                enabled,
                notification_channels: Vec::new(),
                steps: vec![acmecast_store::repository::PipelineStepInput {
                    type_id: "test.noop".to_owned(),
                    input: serde_json::json!({}),
                    enabled: true,
                }],
            },
        )
        .await
        .expect("流水线应能保存")
}

/// 把某条流水线写成一条「正在运行」的历史。
async fn a_running_run(db: &DatabaseConnection, pipeline_id: i64) {
    HistoryRepository::new(db)
        .record(HistoryRecord {
            pipeline_id,
            trigger_source: TriggerSource::Manual,
            status: RunStatus::Running,
            started_at: Utc::now(),
            finished_at: None,
            error_message: None,
            logs: Vec::new(),
        })
        .await
        .expect("运行历史应能写入");
}

/// 保存启用的全局调度，并把触发点拨到过去（下一次 tick 即到点）。
async fn a_due_global_schedule(db: &DatabaseConnection, now: DateTime<Utc>) {
    let repo = GlobalScheduleRepository::new(db);
    repo.save(
        GlobalScheduleInput {
            cron: Some("0 3 * * *".to_owned()),
            enabled: true,
            catch_up: false,
        },
        chrono_tz::UTC,
    )
    .await
    .expect("全局调度应能保存");
    // 触发点拨到过去：下一次 tick 即视为到点。
    repo.mark_triggered(Some(None), Some(now - Duration::seconds(1)))
        .await
        .expect("触发点应能拨到过去");
}

/// 等待 spawn 出去的序列任务把请求记录到假启动器（轮询让出调度权）。
async fn wait_for_requests(launcher: &RecordingLauncher, count: usize) {
    for _ in 0..500 {
        if launcher.requested().len() >= count {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    panic!(
        "序列未在预期时间内产生 {} 个启动请求，实际 {}",
        count,
        launcher.requested().len()
    );
}

async fn trigger_logs_of(db: &DatabaseConnection) -> Vec<(i64, TriggerSource, Option<String>)> {
    TriggerLogRepository::new(db)
        .list(None, 1, 100)
        .await
        .expect("触发记录应能查询")
        .items
        .into_iter()
        .map(|entry| (entry.pipeline_id, entry.source, entry.detail))
        .collect()
}

// ---- 序列行为 ----

#[tokio::test]
async fn the_sequence_runs_enabled_pipelines_in_id_order_and_records_each() {
    let db = database().await;
    let launcher = Arc::new(RecordingLauncher::default());
    let engine = make_engine(&db, &launcher);
    let now = Utc::now();

    // 插入顺序即 id 升序：b、a、c 的命名故意与 id 序相反，钉住「按 ID 而非名称」。
    let id_b = a_pipeline(&db, "b", true).await;
    let id_a = a_pipeline(&db, "a", true).await;
    let id_c = a_pipeline(&db, "c", true).await;
    a_due_global_schedule(&db, now).await;

    engine.tick(now).await.expect("tick 应成功");
    wait_for_requests(&launcher, 3).await;

    let requested = launcher.requested();
    assert_eq!(
        requested.iter().map(|r| r.pipeline_id).collect::<Vec<_>>(),
        vec![id_b, id_a, id_c],
        "序列应按流水线 ID 升序串行触发"
    );
    assert!(
        requested.iter().all(|r| r.source == TriggerSource::Global),
        "序列成员的触发来源应为 global：{requested:?}"
    );

    let logs = trigger_logs_of(&db).await;
    assert_eq!(logs.len(), 3, "每条真正启动的成员各落一条审计：{logs:?}");
    assert!(
        logs.iter()
            .all(|(_, source, _)| *source == TriggerSource::Global)
    );
}

#[tokio::test]
async fn running_members_are_skipped_without_blocking_the_rest() {
    let db = database().await;
    let launcher = Arc::new(RecordingLauncher::default());
    let engine = make_engine(&db, &launcher);
    let now = Utc::now();

    let first = a_pipeline(&db, "first", true).await;
    let middle = a_pipeline(&db, "middle", true).await;
    let last = a_pipeline(&db, "last", true).await;
    a_running_run(&db, middle).await;
    a_due_global_schedule(&db, now).await;

    engine.tick(now).await.expect("tick 应成功");
    wait_for_requests(&launcher, 2).await;

    let requested = launcher.requested();
    assert_eq!(
        requested.iter().map(|r| r.pipeline_id).collect::<Vec<_>>(),
        vec![first, last],
        "运行中的成员应被跳过，且不阻塞后续成员"
    );
}

#[tokio::test]
async fn disabled_pipelines_are_not_part_of_the_sequence() {
    let db = database().await;
    let launcher = Arc::new(RecordingLauncher::default());
    let engine = make_engine(&db, &launcher);
    let now = Utc::now();

    let active = a_pipeline(&db, "active", true).await;
    a_pipeline(&db, "disabled", false).await;
    a_due_global_schedule(&db, now).await;

    engine.tick(now).await.expect("tick 应成功");
    wait_for_requests(&launcher, 1).await;

    let requested = launcher.requested();
    assert_eq!(
        requested.iter().map(|r| r.pipeline_id).collect::<Vec<_>>(),
        vec![active],
        "停用的流水线不应出现在序列中"
    );
}

#[tokio::test]
async fn a_failed_member_does_not_abort_the_sequence() {
    let db = database().await;
    let launcher = Arc::new(RecordingLauncher {
        fail: true,
        ..Default::default()
    });
    let engine = make_engine(&db, &launcher);
    let now = Utc::now();

    let _first = a_pipeline(&db, "first", true).await;
    let _second = a_pipeline(&db, "second", true).await;
    a_due_global_schedule(&db, now).await;

    engine.tick(now).await.expect("tick 应成功");
    wait_for_requests(&launcher, 2).await;

    // 两个成员都被尝试（第一个失败后第二个照常启动）。
    assert_eq!(launcher.requested().len(), 2, "成员失败不应中断序列");
    let logs = trigger_logs_of(&db).await;
    assert_eq!(logs.len(), 2, "失败的成员也落审计");
    assert!(
        logs.iter()
            .all(|(_, _, detail)| detail.as_deref().is_some_and(|text| text.contains("失败"))),
        "失败成员的审计应标注失败原因：{logs:?}"
    );
}

#[tokio::test]
async fn the_trigger_point_advances_so_a_repeated_tick_does_not_rerun() {
    let db = database().await;
    let launcher = Arc::new(RecordingLauncher::default());
    let engine = make_engine(&db, &launcher);
    let now = Utc::now();

    a_pipeline(&db, "only", true).await;
    a_due_global_schedule(&db, now).await;

    engine.tick(now).await.expect("tick 应成功");
    wait_for_requests(&launcher, 1).await;
    engine.tick(now).await.expect("第二次 tick 应成功");

    assert_eq!(
        launcher.requested().len(),
        1,
        "触发点已推进：同一触发点不应被消费两次"
    );

    // 全局调度的触发点照常推进到未来。
    let global = GlobalScheduleRepository::new(&db)
        .get_or_default()
        .await
        .expect("全局调度应能读回");
    assert!(
        global.next_trigger_at.is_some_and(|next| next > now),
        "到点即推进（成员失败不整轮重跑）：{global:?}"
    );
}

/// 每条成员运行一小段真实时间的慢启动器：让序列任务在第二次 tick 时
/// 仍未结束，验证防重叠标志（第二次到点只推进触发点，不启动第二个序列）。
struct SlowLauncher {
    duration: Duration,
    requests: Mutex<Vec<LaunchRequest>>,
}

// 引擎要求 launcher 可调试（trait bound：Debug），Mutex 字段不参与输出。
impl std::fmt::Debug for SlowLauncher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SlowLauncher")
            .field("duration", &self.duration)
            .finish_non_exhaustive()
    }
}

#[async_trait]
impl PipelineLauncher for SlowLauncher {
    async fn launch(&self, request: LaunchRequest) -> acmecast_scheduler::Result<()> {
        tokio::time::sleep(self.duration.to_std().expect("正值")).await;
        self.requests
            .lock()
            .expect("测试中的锁不会中毒")
            .push(request);
        Ok(())
    }
}

#[tokio::test]
async fn an_overlapping_due_point_does_not_start_a_second_sequence() {
    let db = database().await;
    // 每条成员「运行」300ms：两条共约 600ms，第二次 tick 在序列结束前发起。
    let launcher = Arc::new(SlowLauncher {
        duration: Duration::milliseconds(300),
        requests: Mutex::new(Vec::new()),
    });
    let engine = make_engine(&db, &launcher);
    let now = Utc::now();

    let first = a_pipeline(&db, "one", true).await;
    let second = a_pipeline(&db, "two", true).await;
    a_due_global_schedule(&db, now).await;

    // 第一次 tick：spawn 序列（成员各自 sleep 300ms，序列约 600ms）。
    engine.tick(now).await.expect("第一次 tick 应成功");

    // 序列还在运行时把触发点拨回过去并再次 tick：防重叠应只推进触发点、
    // 不启动第二个序列。
    let repo = GlobalScheduleRepository::new(&db);
    repo.mark_triggered(Some(None), Some(now - Duration::seconds(1)))
        .await
        .expect("触发点应能拨回过去");
    engine
        .tick(now + Duration::seconds(2))
        .await
        .expect("第二次 tick 应成功");

    // 等第一轮序列（至多两条）跑完。
    for _ in 0..200 {
        if launcher.requests.lock().expect("锁不会中毒").len() >= 2 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }

    let requested = launcher.requests.lock().expect("锁不会中毒").clone();
    assert_eq!(
        requested.iter().map(|r| r.pipeline_id).collect::<Vec<_>>(),
        vec![first, second],
        "防重叠：第二轮到点不应启动第二个序列（只有一轮的成员）"
    );
    // 触发点仍被推进（防重叠不堆积补跑）。
    let global = repo.get_or_default().await.expect("全局调度应能读回");
    assert!(
        global
            .next_trigger_at
            .is_some_and(|next| next > now + Duration::seconds(2)),
        "第二次到点照常推进触发点：{global:?}"
    );
}

#[tokio::test]
async fn catch_up_starts_the_sequence_after_a_restart_gap() {
    let db = database().await;
    let launcher = Arc::new(RecordingLauncher::default());
    let engine = make_engine(&db, &launcher);
    let now = Utc::now();

    a_pipeline(&db, "member", true).await;
    let repo = GlobalScheduleRepository::new(&db);
    repo.save(
        GlobalScheduleInput {
            cron: Some("0 3 * * *".to_owned()),
            enabled: true,
            catch_up: true,
        },
        chrono_tz::UTC,
    )
    .await
    .expect("全局调度应能保存");
    // 触发点拨到过去：load 视为「停机跨过了最近一次触发点」。
    repo.mark_triggered(Some(None), Some(now - Duration::seconds(1)))
        .await
        .expect("触发点应能拨到过去");

    engine.load(now).await.expect("装载应成功");
    wait_for_requests(&launcher, 1).await;

    assert_eq!(
        launcher.requested().len(),
        1,
        "catch_up 应补跑最近一次错过的触发点（启动一次序列）"
    );
    let global = repo.get_or_default().await.expect("全局调度应能读回");
    assert!(
        global.next_trigger_at.is_some_and(|next| next > now),
        "补跑后触发点推进到未来：{global:?}"
    );
}

#[tokio::test]
async fn the_global_schedule_fires_at_the_configured_local_time() {
    // 东八区本地 03:00 == UTC 前一日 19:00：引擎按配置时区判定到点。
    let db = database().await;
    let launcher = Arc::new(RecordingLauncher::default());
    a_pipeline(&db, "only", true).await;
    let repo = GlobalScheduleRepository::new(&db);
    repo.save(
        GlobalScheduleInput {
            cron: Some("0 3 * * *".to_owned()),
            enabled: true,
            catch_up: false,
        },
        chrono_tz::UTC,
    )
    .await
    .expect("全局调度应能保存");

    let due = DateTime::parse_from_rfc3339("2026-01-01T19:00:00Z")
        .unwrap()
        .with_timezone(&Utc);
    repo.mark_triggered(Some(None), Some(due))
        .await
        .expect("触发点应能拨到本地 03:00");

    let engine = SchedulerEngine::new(
        &db,
        Arc::clone(&launcher) as _,
        SchedulerConfig {
            tz: chrono_tz::Asia::Shanghai,
            ..Default::default()
        },
    );

    // 本地 02:59：尚未到点，不启动序列。
    engine
        .tick(due - Duration::minutes(1))
        .await
        .expect("tick 应成功");
    wait_for_requests(&launcher, 0).await;
    assert!(launcher.requested().is_empty(), "本地 02:59 不应触发");

    // 本地 03:00 过后：启动一次全局串行序列。
    engine
        .tick(due + Duration::minutes(1))
        .await
        .expect("tick 应成功");
    wait_for_requests(&launcher, 1).await;
    assert_eq!(
        launcher.requested().len(),
        1,
        "本地 03:00 应启动一次全局序列"
    );
}

#[tokio::test]
async fn global_catch_up_runs_the_missed_local_trigger_once() {
    // 停机补跑按配置时区判定：重启装载时补跑最近一次错过的本地触发点。
    let db = database().await;
    let launcher = Arc::new(RecordingLauncher::default());
    a_pipeline(&db, "only", true).await;
    let repo = GlobalScheduleRepository::new(&db);
    repo.save(
        GlobalScheduleInput {
            cron: Some("0 3 * * *".to_owned()),
            enabled: true,
            catch_up: true,
        },
        chrono_tz::UTC,
    )
    .await
    .expect("全局调度应能保存");

    let missed = DateTime::parse_from_rfc3339("2026-01-01T19:00:00Z")
        .unwrap()
        .with_timezone(&Utc);
    repo.mark_triggered(Some(None), Some(missed))
        .await
        .expect("触发点应能拨到错过的本地时刻");

    let engine = SchedulerEngine::new(
        &db,
        Arc::clone(&launcher) as _,
        SchedulerConfig {
            tz: chrono_tz::Asia::Shanghai,
            ..Default::default()
        },
    );
    engine
        .load(missed + Duration::hours(1))
        .await
        .expect("装载应成功");

    wait_for_requests(&launcher, 1).await;
    assert_eq!(
        launcher.requested().len(),
        1,
        "装载时应补跑错过的本地 03:00 触发点"
    );
    let global = repo.get_or_default().await.expect("查询应成功");
    assert!(
        global.next_trigger_at.is_some_and(|next| next > missed),
        "补跑后触发点推进到未来：{global:?}"
    );
}
