//! 9.x 定时调度：cron 触发、保存校验、到期扫描、触发去重、重启装载、
//! 停机补跑与触发记录。
//!
//! 引擎的所有判定方法都显式接收 `now`，测试因此不需要等待真实时间——
//! 每个场景都把时钟拨到想验证的位置，直接调用 `tick`／`scan_expiring`／`load`。

use std::sync::Mutex;

use acmecast_pipeline::{HistoryRecord, HistoryRepository};
use acmecast_scheduler::{
    Error, LaunchRequest, PipelineLauncher, SchedulerConfig, SchedulerEngine,
};
use acmecast_store::entity::history::RunStatus;
use acmecast_store::entity::pipeline::TriggerSource;
use acmecast_store::migrate;
use acmecast_store::repository::{
    CertInput, CertRepository, PipelineInput, PipelineRepository, PipelineStepInput, ScheduleInput,
    ScheduleRepository, TriggerLogInput, TriggerLogRepository,
};
use async_trait::async_trait;
use chrono::{DateTime, Duration, Utc};
use sea_orm::{Database, DatabaseConnection};

// ---- 脚手架 ----

/// 记录启动请求的假启动器；`fail` 为真时模拟启动失败。
#[derive(Debug, Default)]
struct RecordingLauncher {
    requests: Mutex<Vec<LaunchRequest>>,
    fail: bool,
}

impl RecordingLauncher {
    fn failing() -> Self {
        Self {
            fail: true,
            ..Default::default()
        }
    }

    fn requested(&self) -> Vec<LaunchRequest> {
        self.requests.lock().expect("测试中的锁不会中毒").clone()
    }
}

#[async_trait]
impl PipelineLauncher for RecordingLauncher {
    async fn launch(&self, request: LaunchRequest) -> acmecast_scheduler::Result<()> {
        if self.fail {
            return Err(Error::InvalidConfig("启动被配置为失败".to_owned()));
        }
        self.requests
            .lock()
            .expect("测试中的锁不会中毒")
            .push(request);
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

fn make_engine<'a>(
    db: &'a DatabaseConnection,
    launcher: &'a RecordingLauncher,
) -> SchedulerEngine<'a> {
    SchedulerEngine::new(db, launcher, SchedulerConfig::default())
}

/// 落一条流水线并返回主键——调度与触发记录都以它为外键。
async fn a_pipeline(db: &DatabaseConnection, name: &str) -> i64 {
    PipelineRepository::new(db)
        .save(
            None,
            PipelineInput {
                name: name.to_owned(),
                description: None,
                enabled: true,
                steps: vec![PipelineStepInput {
                    type_id: "test.noop".to_owned(),
                    input: serde_json::json!({}),
                    enabled: true,
                }],
            },
        )
        .await
        .expect("流水线应能保存")
}

/// 落一条证书；到期时间决定它是否进入续期窗口。
async fn a_cert(db: &DatabaseConnection, domains: &[&str], not_after: DateTime<Utc>, id: &str) {
    CertRepository::new(db)
        .save(CertInput {
            domains: domains.iter().map(|domain| domain.to_string()).collect(),
            cert_pem_path: format!("certs/{id}.pem"),
            key_pem_path: format!("keys/{id}.pem"),
            fingerprint: format!("sha256:{id}"),
            issuer: Some("test-ca".to_owned()),
            not_before: not_after - Duration::days(90),
            not_after,
            acme_account_access_id: None,
        })
        .await
        .expect("证书应能写入");
}

/// 把某条调度写成一条「正在运行」的历史，制造运行中状态。
async fn a_running_run(db: &DatabaseConnection, pipeline_id: i64) {
    HistoryRepository::new(db)
        .record(HistoryRecord {
            pipeline_id,
            trigger_source: TriggerSource::Renewal,
            status: RunStatus::Running,
            started_at: Utc::now(),
            finished_at: None,
            error_message: None,
            logs: Vec::new(),
        })
        .await
        .expect("运行记录应能写入");
}

/// 保存一条 cron 调度（其余字段取默认）。
async fn a_cron_schedule(db: &DatabaseConnection, pipeline_id: i64, cron: &str, catch_up: bool) {
    ScheduleRepository::new(db)
        .save(
            pipeline_id,
            ScheduleInput {
                cron: Some(cron.to_owned()),
                enabled: true,
                catch_up,
                renewal_domains: None,
            },
        )
        .await
        .expect("调度应能保存");
}

/// 把触发点拨到过去，模拟「到达触发点」或「停机错过」。
async fn make_trigger_due(db: &DatabaseConnection, pipeline_id: i64, due_at: DateTime<Utc>) {
    ScheduleRepository::new(db)
        .mark_triggered(pipeline_id, Some(None), Some(due_at))
        .await
        .expect("触发点应能改写");
}

// ---- 9.1 cron 触发 ----

#[tokio::test]
async fn a_due_cron_schedule_triggers_its_pipeline_as_scheduled() {
    let db = database().await;
    let launcher = RecordingLauncher::default();
    let engine = make_engine(&db, &launcher);

    let pipeline_id = a_pipeline(&db, "nightly").await;
    a_cron_schedule(&db, pipeline_id, "0 3 * * *", false).await;
    let now = Utc::now();
    make_trigger_due(&db, pipeline_id, now - Duration::minutes(1)).await;

    engine.tick(now).await.expect("tick 应成功");

    let requests = launcher.requested();
    assert_eq!(requests.len(), 1, "到点应触发一次");
    assert_eq!(requests[0].pipeline_id, pipeline_id);
    assert_eq!(requests[0].source, TriggerSource::Cron, "来源应标记为定时");

    // 触发点已推进到未来：同一触发点不会被消费两次。
    let schedule = ScheduleRepository::new(&db)
        .find(pipeline_id)
        .await
        .expect("查询应成功")
        .expect("调度应存在");
    assert_eq!(schedule.last_triggered_at, Some(now));
    assert!(
        schedule.next_trigger_at.is_some_and(|next| next > now),
        "下一次触发应排到未来，实际为 {:?}",
        schedule.next_trigger_at
    );

    // 再走一轮：不重复触发。
    engine.tick(now + Duration::seconds(30)).await.unwrap();
    assert_eq!(launcher.requested().len(), 1, "触发点推进后不应再触发");

    // 定时触发落了审计记录（9.7）。
    let logs = TriggerLogRepository::new(&db)
        .list(Some(pipeline_id), 1, 20)
        .await
        .expect("查询应成功");
    assert_eq!(logs.total, 1, "cron 触发应落一条记录");
    assert_eq!(logs.items[0].source, TriggerSource::Cron);
}

// ---- 9.2 无效 cron 保存时拒绝 ----

#[tokio::test]
async fn an_invalid_cron_is_rejected_on_save() {
    let db = database().await;

    let error = ScheduleRepository::new(&db)
        .save(
            1,
            ScheduleInput {
                cron: Some("0 99 * * *".to_owned()),
                enabled: true,
                catch_up: false,
                renewal_domains: None,
            },
        )
        .await
        .expect_err("非法 cron 应被拒绝");

    assert!(
        error.to_string().contains("0 99 * * *"),
        "错误应能定位到表达式本身：{error}"
    );
    assert!(
        ScheduleRepository::new(&db)
            .find(1)
            .await
            .expect("查询应成功")
            .is_none(),
        "被拒绝的调度不应留下半条记录"
    );
}

// ---- 9.3 到期扫描触发 ----

#[tokio::test]
async fn a_certificate_within_the_renewal_window_triggers_its_pipeline() {
    let db = database().await;
    let launcher = RecordingLauncher::default();
    let engine = make_engine(&db, &launcher);

    let now = Utc::now();
    a_cert(&db, &["a.example.com"], now + Duration::days(10), "near").await;
    let pipeline_id = a_pipeline(&db, "renew-a").await;
    ScheduleRepository::new(&db)
        .save(
            pipeline_id,
            ScheduleInput {
                cron: None,
                enabled: true,
                catch_up: false,
                renewal_domains: Some(vec!["a.example.com".to_owned()]),
            },
        )
        .await
        .expect("调度应能保存");

    engine.scan_expiring(now).await.expect("扫描应成功");

    let requests = launcher.requested();
    assert_eq!(requests.len(), 1, "窗口内的证书应触发续期");
    assert_eq!(
        requests[0].source,
        TriggerSource::Renewal,
        "来源应标记为续期"
    );
    let detail = requests[0].detail.as_deref().unwrap_or_default();
    assert!(
        detail.contains("a.example.com"),
        "说明里应能看出是哪张证书：{detail}"
    );
}

#[tokio::test]
async fn a_certificate_outside_the_window_does_not_trigger() {
    let db = database().await;
    let launcher = RecordingLauncher::default();
    let engine = make_engine(&db, &launcher);

    let now = Utc::now();
    // 默认阈值 30 天：60 天后到期，尚在安全期。
    a_cert(&db, &["safe.example.com"], now + Duration::days(60), "safe").await;
    let pipeline_id = a_pipeline(&db, "renew-safe").await;
    ScheduleRepository::new(&db)
        .save(
            pipeline_id,
            ScheduleInput {
                cron: None,
                enabled: true,
                catch_up: false,
                renewal_domains: Some(vec!["safe.example.com".to_owned()]),
            },
        )
        .await
        .expect("调度应能保存");

    engine.scan_expiring(now).await.expect("扫描应成功");

    assert!(
        launcher.requested().is_empty(),
        "安全期内的证书不应触发任何流水线"
    );
}

// ---- 9.4 触发去重 ----

#[tokio::test]
async fn a_running_pipeline_is_not_triggered_again() {
    let db = database().await;
    let launcher = RecordingLauncher::default();
    let engine = make_engine(&db, &launcher);

    let now = Utc::now();
    a_cert(&db, &["busy.example.com"], now + Duration::days(5), "busy").await;
    let pipeline_id = a_pipeline(&db, "renew-busy").await;
    ScheduleRepository::new(&db)
        .save(
            pipeline_id,
            ScheduleInput {
                cron: None,
                enabled: true,
                catch_up: false,
                renewal_domains: Some(vec!["busy.example.com".to_owned()]),
            },
        )
        .await
        .expect("调度应能保存");
    a_running_run(&db, pipeline_id).await;

    engine.scan_expiring(now).await.expect("扫描应成功");

    assert!(
        launcher.requested().is_empty(),
        "运行中的流水线不应被重复触发"
    );
    // 被拦下的尝试不是触发：审计里不该出现「从未发生过的触发」。
    let logs = TriggerLogRepository::new(&db)
        .list(Some(pipeline_id), 1, 20)
        .await
        .expect("查询应成功");
    assert_eq!(logs.total, 0, "被运行中拦下的尝试不应落触发记录");
}

#[tokio::test]
async fn repeated_hits_within_the_dedup_window_trigger_once() {
    let db = database().await;
    let launcher = RecordingLauncher::default();
    let engine = make_engine(&db, &launcher);

    let now = Utc::now();
    a_cert(&db, &["due.example.com"], now + Duration::days(2), "due").await;
    let pipeline_id = a_pipeline(&db, "renew-due").await;
    ScheduleRepository::new(&db)
        .save(
            pipeline_id,
            ScheduleInput {
                cron: None,
                enabled: true,
                catch_up: false,
                renewal_domains: Some(vec!["due.example.com".to_owned()]),
            },
        )
        .await
        .expect("调度应能保存");

    // 默认去重窗口 1 小时：窗口内的重复扫描只产生一次触发。
    engine.scan_expiring(now).await.unwrap();
    engine
        .scan_expiring(now + Duration::minutes(5))
        .await
        .unwrap();
    assert_eq!(
        launcher.requested().len(),
        1,
        "去重窗口内的重复命中应被抑制"
    );
    // 被抑制的尝试不落记录——否则每次抑制都会刷新窗口基准点，
    // 审计里也会堆满假触发。
    let logs = TriggerLogRepository::new(&db)
        .list(Some(pipeline_id), 1, 20)
        .await
        .expect("查询应成功");
    assert_eq!(logs.total, 1, "窗口内的重复命中不应追加触发记录");

    // 窗口过后（把窗口后移的时钟）允许再次触发。
    engine
        .scan_expiring(now + Duration::hours(2))
        .await
        .unwrap();
    assert_eq!(launcher.requested().len(), 2, "窗口外的扫描应重新触发");
}

// ---- 9.5 调度持久化与重启装载 ----

#[tokio::test]
async fn schedules_survive_a_restart_and_are_reloaded() {
    let db = database().await;
    let launcher = RecordingLauncher::default();

    let pipeline_id = a_pipeline(&db, "reload").await;
    a_cron_schedule(&db, pipeline_id, "0 3 * * *", false).await;

    // 「重启」：同一个库上新建引擎并装载。
    let now = Utc::now();
    make_engine(&db, &launcher)
        .load(now)
        .await
        .expect("装载应成功");

    let schedule = ScheduleRepository::new(&db)
        .find(pipeline_id)
        .await
        .expect("查询应成功")
        .expect("调度不应因重启丢失");
    assert_eq!(
        schedule.cron.as_deref(),
        Some("0 3 * * *"),
        "配置应原样保留"
    );
    assert!(
        schedule.next_trigger_at.is_some_and(|next| next > now),
        "装载后应重算出未来的触发点，实际为 {:?}",
        schedule.next_trigger_at
    );
}

// ---- 9.6 停机包场策略 ----

#[tokio::test]
async fn a_stopped_service_skips_missed_triggers_by_default() {
    let db = database().await;
    let launcher = RecordingLauncher::default();

    // 停机一晚：触发点落在 8 小时前。
    let pipeline_id = a_pipeline(&db, "nightly").await;
    a_cron_schedule(&db, pipeline_id, "0 3 * * *", false).await;
    let now = Utc::now();
    make_trigger_due(&db, pipeline_id, now - Duration::hours(8)).await;

    make_engine(&db, &launcher)
        .load(now)
        .await
        .expect("装载应成功");

    assert!(
        launcher.requested().is_empty(),
        "默认应跳过包场，不为错过的点补跑"
    );
    let schedule = ScheduleRepository::new(&db)
        .find(pipeline_id)
        .await
        .expect("查询应成功")
        .expect("调度应存在");
    assert!(
        schedule.next_trigger_at.is_some_and(|next| next > now),
        "错过的点作废后应等待下一次正常触发"
    );
    assert_eq!(schedule.last_triggered_at, None, "没触发就不该记时间");
}

#[tokio::test]
async fn catch_up_runs_the_last_missed_trigger_once() {
    let db = database().await;
    let launcher = RecordingLauncher::default();

    let pipeline_id = a_pipeline(&db, "catch-up").await;
    a_cron_schedule(&db, pipeline_id, "0 3 * * *", true).await;
    let now = Utc::now();
    make_trigger_due(&db, pipeline_id, now - Duration::hours(8)).await;

    make_engine(&db, &launcher)
        .load(now)
        .await
        .expect("装载应成功");

    let requests = launcher.requested();
    assert_eq!(requests.len(), 1, "追赶模式应补跑错过的触发点");
    assert_eq!(requests[0].source, TriggerSource::Cron);

    // 补跑是一次真实的定时触发，审计里要能看到它。
    let logs = TriggerLogRepository::new(&db)
        .list(Some(pipeline_id), 1, 20)
        .await
        .expect("查询应成功");
    assert_eq!(logs.total, 1, "补跑应落一条触发记录");
    assert_eq!(logs.items[0].source, TriggerSource::Cron);
    let detail = logs.items[0].detail.as_deref().unwrap_or_default();
    assert!(detail.contains("补跑"), "说明应标明这是补跑：{detail}");

    // 再装载一次：触发点已被推进，不再补跑——补跑有且只有一次。
    make_engine(&db, &launcher)
        .load(now + Duration::minutes(1))
        .await
        .expect("二次装载应成功");
    assert_eq!(launcher.requested().len(), 1, "补跑仅此一次");
}

// ---- 9.7 触发记录 ----

#[tokio::test]
async fn trigger_history_is_listed_newest_first_with_its_source() {
    let db = database().await;
    let pipeline_id = a_pipeline(&db, "tracked").await;
    let logs = TriggerLogRepository::new(&db);
    let base = Utc::now();

    for (source, offset, detail) in [
        (
            TriggerSource::Renewal,
            -60,
            Some("证书 a.example.com 距到期不足 30 天".to_owned()),
        ),
        (TriggerSource::Cron, -30, None),
        (TriggerSource::Manual, -10, None),
    ] {
        logs.record(TriggerLogInput {
            pipeline_id,
            source,
            detail,
            triggered_at: base + Duration::minutes(offset),
        })
        .await
        .expect("触发记录应能写入");
    }

    let page = logs
        .list(Some(pipeline_id), 1, 20)
        .await
        .expect("查询应成功");

    assert_eq!(page.total, 3);
    let sources: Vec<TriggerSource> = page.items.iter().map(|entry| entry.source).collect();
    assert_eq!(
        sources,
        vec![
            TriggerSource::Manual,
            TriggerSource::Cron,
            TriggerSource::Renewal
        ],
        "应按时间倒序且标明来源"
    );
    assert!(
        page.items[2]
            .detail
            .as_deref()
            .is_some_and(|detail| detail.contains("a.example.com")),
        "续期触发应带证书说明"
    );
}

// ---- 启动失败的重试语义 ----

#[tokio::test]
async fn a_failed_launch_keeps_the_trigger_due_for_retry() {
    let db = database().await;
    let launcher = RecordingLauncher::failing();
    let engine = make_engine(&db, &launcher);

    let pipeline_id = a_pipeline(&db, "retry").await;
    a_cron_schedule(&db, pipeline_id, "0 3 * * *", false).await;
    let now = Utc::now();
    make_trigger_due(&db, pipeline_id, now - Duration::minutes(1)).await;

    engine.tick(now).await.expect("tick 不应因启动失败而中断");

    let schedule = ScheduleRepository::new(&db)
        .find(pipeline_id)
        .await
        .expect("查询应成功")
        .expect("调度应存在");
    assert!(
        schedule.next_trigger_at.is_some_and(|next| next <= now),
        "启动失败时触发点应保留待重试"
    );

    // 换成正常启动器后，同一触发点可以再次尝试。
    let working = RecordingLauncher::default();
    make_engine(&db, &working)
        .tick(now + Duration::minutes(1))
        .await
        .unwrap();
    assert_eq!(working.requested().len(), 1, "重试应当成功");
}

// ---- 续期启动失败的审计与节流 ----

#[tokio::test]
async fn a_failed_renewal_launch_is_recorded_and_throttled() {
    let db = database().await;
    let launcher = RecordingLauncher::failing();
    let engine = make_engine(&db, &launcher);

    let now = Utc::now();
    a_cert(
        &db,
        &["broken.example.com"],
        now + Duration::days(3),
        "broken",
    )
    .await;
    let pipeline_id = a_pipeline(&db, "renew-broken").await;
    ScheduleRepository::new(&db)
        .save(
            pipeline_id,
            ScheduleInput {
                cron: None,
                enabled: true,
                catch_up: false,
                renewal_domains: Some(vec!["broken.example.com".to_owned()]),
            },
        )
        .await
        .expect("调度应能保存");

    // 失败的触发也要落审计记录——否则「为什么这张证书一直没续上」无从查起。
    engine
        .scan_expiring(now)
        .await
        .expect("扫描不应因启动失败而中断");
    let logs = TriggerLogRepository::new(&db)
        .list(Some(pipeline_id), 1, 20)
        .await
        .expect("查询应成功");
    assert_eq!(logs.total, 1, "失败的触发应落一条记录");
    let detail = logs.items[0].detail.as_deref().unwrap_or_default();
    assert!(detail.contains("启动失败"), "说明应标明启动失败：{detail}");

    // 记录驱动去重窗口：失败后的下一轮扫描不会立刻重试成风暴。
    engine
        .scan_expiring(now + Duration::minutes(10))
        .await
        .expect("二次扫描应成功");
    let logs = TriggerLogRepository::new(&db)
        .list(Some(pipeline_id), 1, 20)
        .await
        .expect("查询应成功");
    assert_eq!(logs.total, 1, "窗口内不应重试，也不应追加记录");
}

// ---- 一条流水线服务多张证书 ----

#[tokio::test]
async fn multiple_certs_matching_one_pipeline_trigger_it_once_per_scan() {
    let db = database().await;
    let launcher = RecordingLauncher::default();
    let engine = make_engine(&db, &launcher);

    let now = Utc::now();
    // 两张不同域名的证书都由同一条流水线续期。
    a_cert(&db, &["one.example.com"], now + Duration::days(4), "one").await;
    a_cert(&db, &["two.example.com"], now + Duration::days(6), "two").await;
    let pipeline_id = a_pipeline(&db, "renew-both").await;
    ScheduleRepository::new(&db)
        .save(
            pipeline_id,
            ScheduleInput {
                cron: None,
                enabled: true,
                catch_up: false,
                renewal_domains: Some(vec![
                    "one.example.com".to_owned(),
                    "two.example.com".to_owned(),
                ]),
            },
        )
        .await
        .expect("调度应能保存");

    engine.scan_expiring(now).await.expect("扫描应成功");

    assert_eq!(
        launcher.requested().len(),
        1,
        "同一次扫描内多张证书命中同一条流水线只应触发一次"
    );
    let logs = TriggerLogRepository::new(&db)
        .list(Some(pipeline_id), 1, 20)
        .await
        .expect("查询应成功");
    assert_eq!(logs.total, 1, "第二次命中被窗口去重拦下，不落记录");
}
