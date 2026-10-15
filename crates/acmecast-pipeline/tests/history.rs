//! 6.8 运行历史与步骤日志的落库与查询。
//!
//! spec 的两个要点：历史**按 ID 倒序**、日志**归属正确**（属于哪一步）。
//! 前者决定列表好不好用，后者决定排查时能不能顺着步骤读下去。

use std::sync::Arc;

use acmecast_access::{CredentialRegistry, CredentialStore};
use acmecast_core::CredentialCipher;
use acmecast_pipeline::{
    DatabaseStateStore, HistoryQuery, HistoryRecord, HistoryRepository, PipelineDefinition,
    PipelineRunner, PipelineStep, Result, StepContext, StepDefinition, StepLogLevel, StepOutput,
    StepRegistry,
};
use acmecast_store::entity::{history, pipeline::TriggerSource};
use acmecast_store::migrate;
use async_trait::async_trait;
use chrono::{Duration, Utc};
use sea_orm::{ActiveModelTrait, Database, DatabaseConnection, Set};

// ---- 步骤 ----

/// 各记一条日志的两个步骤，用于验证日志归属。
#[derive(Debug)]
struct First;

#[async_trait]
impl PipelineStep for First {
    fn type_id(&self) -> &'static str {
        "test.first"
    }

    async fn execute(&self, ctx: &mut StepContext<'_>) -> Result<StepOutput> {
        ctx.log_info("第一步记下的日志");
        Ok(StepOutput::empty())
    }
}

#[derive(Debug)]
struct Second;

#[async_trait]
impl PipelineStep for Second {
    fn type_id(&self) -> &'static str {
        "test.second"
    }

    async fn execute(&self, ctx: &mut StepContext<'_>) -> Result<StepOutput> {
        ctx.log_warn("第二步记下的日志");
        // 随返回值带出的日志也应该落库，且归到本步。
        Ok(StepOutput::empty().with_log(acmecast_pipeline::StepLog::info("第二步的另一条")))
    }
}

/// 失败，并留下一条失败前的说明。
#[derive(Debug)]
struct Broken;

#[async_trait]
impl PipelineStep for Broken {
    fn type_id(&self) -> &'static str {
        "test.broken"
    }

    async fn execute(&self, ctx: &mut StepContext<'_>) -> Result<StepOutput> {
        ctx.log_warn("失败之前");
        Err(acmecast_pipeline::Error::Core(
            acmecast_core::Error::Internal("故意失败".to_owned()),
        ))
    }
}

// ---- 脚手架 ----

async fn database() -> DatabaseConnection {
    let db = Database::connect("sqlite::memory:")
        .await
        .expect("应能连上内存库");
    migrate(&db).await.expect("迁移应成功");
    db
}

fn credential_store(db: &DatabaseConnection) -> CredentialStore<'_> {
    let cipher = CredentialCipher::from_base64(&CredentialCipher::generate_key_base64())
        .expect("密钥应可用");
    CredentialStore::new(db, Arc::new(CredentialRegistry::new()), Arc::new(cipher))
}

fn registry() -> StepRegistry {
    let mut registry = StepRegistry::new();
    registry.register(First).unwrap();
    registry.register(Second).unwrap();
    registry.register(Broken).unwrap();
    registry
}

fn step(order_index: i32, type_id: &str) -> StepDefinition {
    StepDefinition {
        order_index,
        type_id: type_id.to_owned(),
        input: serde_json::json!({}),
        enabled: true,
    }
}

fn pipeline(id: i64, steps: Vec<StepDefinition>) -> PipelineDefinition {
    PipelineDefinition { id, steps }
}

/// 插入一条流水线（历史表有外键指向它）。
async fn insert_pipeline(db: &DatabaseConnection, name: &str) -> i64 {
    let now = Utc::now();
    acmecast_store::entity::pipeline::ActiveModel {
        name: Set(name.to_owned()),
        enabled: Set(true),
        description: Set(None),
        created_at: Set(now),
        updated_at: Set(now),
        ..Default::default()
    }
    .insert(db)
    .await
    .expect("应能插入流水线")
    .id
}

/// 直接落一条历史（用于控制开始时间，验证排序与分页）。
async fn record_at(
    repo: &HistoryRepository<'_>,
    pipeline_id: i64,
    started_at: chrono::DateTime<Utc>,
) -> i64 {
    repo.record(HistoryRecord {
        pipeline_id,
        trigger_source: TriggerSource::Manual,
        status: history::RunStatus::Success,
        started_at,
        finished_at: Some(started_at + Duration::seconds(3)),
        error_message: None,
        logs: Vec::new(),
    })
    .await
    .expect("应能落库")
}

// ---- 端到端：跑一次，落库，再读出来 ----

#[tokio::test]
async fn a_finished_run_is_recorded_with_its_logs_attributed_to_steps() {
    let db = database().await;
    let credentials = credential_store(&db);
    let state = DatabaseStateStore::new(&db);
    let registry = registry();
    let runner = PipelineRunner::new(&registry, &credentials, &state);
    let history_repo = HistoryRepository::new(&db);

    let pipeline_id = insert_pipeline(&db, "甲流水线").await;
    let started_at = Utc::now();

    let outcome = runner
        .run(
            &pipeline(
                pipeline_id,
                vec![step(0, "test.first"), step(1, "test.second")],
            ),
            1,
        )
        .await
        .unwrap();

    let history_id = history_repo
        .record(HistoryRecord::from_outcome(
            pipeline_id,
            TriggerSource::Manual,
            &outcome,
            started_at,
        ))
        .await
        .expect("应能落库");

    // 历史本身。
    let page = history_repo
        .list(HistoryQuery {
            pipeline_id: Some(pipeline_id),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(page.total, 1);
    let entry = &page.items[0];
    assert_eq!(entry.id, history_id);
    assert_eq!(entry.status, history::RunStatus::Success);
    assert!(entry.error_message.is_none());

    // 日志归属：三条日志分别属于第 0 步与第 1 步。
    let logs = history_repo.logs_of(history_id).await.unwrap();
    assert_eq!(logs.len(), 3, "三条日志都应落库: {logs:?}");

    let first_step: Vec<&str> = logs
        .iter()
        .filter(|log| log.step_index == 0)
        .map(|log| log.message.as_str())
        .collect();
    let second_step: Vec<&str> = logs
        .iter()
        .filter(|log| log.step_index == 1)
        .map(|log| log.message.as_str())
        .collect();

    assert_eq!(first_step, vec!["第一步记下的日志"]);
    assert_eq!(
        second_step,
        vec!["第二步记下的日志", "第二步的另一条"],
        "随返回值带出的日志也该归到本步"
    );

    // 级别也一并落库。
    assert!(
        logs.iter()
            .any(|log| log.level == StepLogLevel::Warn && log.step_index == 1),
        "{logs:?}"
    );
}

#[tokio::test]
async fn a_failed_run_records_a_summary_that_names_the_step() {
    let db = database().await;
    let credentials = credential_store(&db);
    let state = DatabaseStateStore::new(&db);
    let registry = registry();
    let runner = PipelineRunner::new(&registry, &credentials, &state);
    let history_repo = HistoryRepository::new(&db);

    let pipeline_id = insert_pipeline(&db, "甲流水线").await;
    let outcome = runner
        .run(
            &pipeline(
                pipeline_id,
                vec![step(0, "test.first"), step(1, "test.broken")],
            ),
            1,
        )
        .await
        .unwrap();

    history_repo
        .record(HistoryRecord::from_outcome(
            pipeline_id,
            TriggerSource::Cron,
            &outcome,
            Utc::now(),
        ))
        .await
        .unwrap();

    let page = history_repo.list(HistoryQuery::default()).await.unwrap();
    let entry = &page.items[0];

    assert_eq!(entry.status, history::RunStatus::Failed);
    assert_eq!(entry.trigger_source, TriggerSource::Cron);

    // 摘要里要能看出是第几步、什么类型失败的——只写「内部错误」等于没记。
    let message = entry.error_message.as_deref().expect("失败应有摘要");
    assert!(message.contains("第 2 步"), "{message}");
    assert!(message.contains("test.broken"), "{message}");
    assert!(message.contains("故意失败"), "{message}");
}

// ---- 排序与分页 ----

#[tokio::test]
async fn history_is_listed_newest_first() {
    let db = database().await;
    let repo = HistoryRepository::new(&db);
    let pipeline_id = insert_pipeline(&db, "甲流水线").await;

    // 刻意乱序落库：middle(ID=1) → oldest(ID=2) → newest(ID=3)。
    // ID 倒序为 newest, oldest, middle；时间倒序为 newest, middle, oldest——
    // 两种排序在 oldest 与 middle 的先后上不同，据此区分。
    let now = Utc::now();
    let middle = record_at(&repo, pipeline_id, now - Duration::minutes(10)).await;
    let oldest = record_at(&repo, pipeline_id, now - Duration::minutes(30)).await;
    let newest = record_at(&repo, pipeline_id, now).await;

    let page = repo.list(HistoryQuery::default()).await.unwrap();
    let ids: Vec<i64> = page.items.iter().map(|item| item.id).collect();

    assert_eq!(ids, vec![newest, oldest, middle], "应按 ID 倒序");
}

#[tokio::test]
async fn paging_walks_the_whole_history_without_repeats() {
    let db = database().await;
    let repo = HistoryRepository::new(&db);
    let pipeline_id = insert_pipeline(&db, "甲流水线").await;

    let now = Utc::now();
    let mut expected = Vec::new();
    for index in 0..5 {
        // 前三条时间戳完全相同：靠主键定序才不会翻页错乱。
        let started_at = if index < 3 {
            now
        } else {
            now - Duration::minutes(index as i64)
        };
        expected.push(record_at(&repo, pipeline_id, started_at).await);
    }

    let mut seen = Vec::new();
    for page_number in 1..=3 {
        let page = repo
            .list(HistoryQuery {
                page: page_number,
                page_size: 2,
                ..Default::default()
            })
            .await
            .unwrap();

        assert_eq!(page.total, 5, "总数与页码无关");
        assert_eq!(page.page, page_number);
        seen.extend(page.items.into_iter().map(|item| item.id));
    }

    assert_eq!(seen.len(), 5, "三页应覆盖全部 5 条");
    let mut unique = seen.clone();
    unique.sort_unstable();
    unique.dedup();
    assert_eq!(unique.len(), 5, "同一条不该出现在两页里");
}

#[tokio::test]
async fn history_can_be_filtered_by_pipeline() {
    let db = database().await;
    let repo = HistoryRepository::new(&db);
    let first = insert_pipeline(&db, "甲流水线").await;
    let second = insert_pipeline(&db, "乙流水线").await;

    let now = Utc::now();
    record_at(&repo, first, now - Duration::minutes(5)).await;
    let target = record_at(&repo, second, now).await;

    let all = repo.list(HistoryQuery::default()).await.unwrap();
    assert_eq!(all.total, 2, "不传过滤条件时应看到全部");

    let filtered = repo
        .list(HistoryQuery {
            pipeline_id: Some(second),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(filtered.total, 1);
    assert_eq!(filtered.items[0].id, target);
    assert_eq!(filtered.items[0].pipeline_id, second);
}

#[tokio::test]
async fn paging_parameters_are_normalized() {
    let db = database().await;
    let repo = HistoryRepository::new(&db);
    let pipeline_id = insert_pipeline(&db, "甲流水线").await;
    record_at(&repo, pipeline_id, Utc::now()).await;

    // 页码从 0 起算没有意义，归一成 1；每页条数归到上限之内。
    let page = repo
        .list(HistoryQuery {
            page: 0,
            page_size: u64::MAX,
            ..Default::default()
        })
        .await
        .unwrap();

    assert_eq!(page.page, 1);
    assert!(
        page.page_size <= acmecast_store::repository::MAX_PAGE_SIZE,
        "每页条数应被归一到上限内"
    );
    assert_eq!(page.items.len(), 1);
}

#[tokio::test]
async fn a_run_without_logs_reads_back_empty() {
    let db = database().await;
    let repo = HistoryRepository::new(&db);
    let pipeline_id = insert_pipeline(&db, "甲流水线").await;

    let history_id = record_at(&repo, pipeline_id, Utc::now()).await;
    assert!(repo.logs_of(history_id).await.unwrap().is_empty());
}

// ---- 启动时清理跨进程遗留的 running 历史 ----

/// 落一条仍在运行中的历史（模拟进程中断后遗留的记录）。
async fn record_running(
    repo: &HistoryRepository<'_>,
    pipeline_id: i64,
    trigger: TriggerSource,
) -> i64 {
    repo.record(HistoryRecord {
        pipeline_id,
        trigger_source: trigger,
        status: history::RunStatus::Running,
        started_at: Utc::now(),
        finished_at: None,
        error_message: None,
        logs: Vec::new(),
    })
    .await
    .expect("应能落库")
}

/// 遗留的 running 历史被标记为失败：has_running 恢复 false（解除触发阻塞），
/// 已完成的历史不受影响，重复清理幂等。
#[tokio::test]
async fn stale_running_histories_are_marked_failed_and_unblock_their_pipelines() {
    let db = database().await;
    let repo = HistoryRepository::new(&db);
    let pipeline_a = insert_pipeline(&db, "中断的流水线").await;
    let pipeline_b = insert_pipeline(&db, "另一条中断的流水线").await;

    let stale_a = record_running(&repo, pipeline_a, TriggerSource::Cron).await;
    let stale_b = record_running(&repo, pipeline_b, TriggerSource::Manual).await;
    assert!(repo.has_running(pipeline_a).await.unwrap());

    // 一条正常完成的历史混在其中：清理不应碰它。
    let finished = record_at(&repo, pipeline_a, Utc::now()).await;

    let marked = repo.fail_stale_running().await.expect("清理应成功");
    assert_eq!(marked, 2, "两条遗留 running 都应被标记");

    // 终态与摘要：failed + 结束时间 + 可读原因。
    let page = repo
        .list(HistoryQuery {
            pipeline_id: Some(pipeline_a),
            ..HistoryQuery::default()
        })
        .await
        .unwrap();
    let stale = page
        .items
        .iter()
        .find(|item| item.id == stale_a)
        .expect("清理后的记录应仍在列表里");
    assert_eq!(stale.status, history::RunStatus::Failed);
    assert!(stale.finished_at.is_some());
    let reason = stale.error_message.as_deref().unwrap_or_default();
    assert!(reason.contains("服务重启"), "摘要应说明中断原因: {reason}");

    // 阻塞解除：两条流水线都恢复可触发。
    assert!(!repo.has_running(pipeline_a).await.unwrap());
    assert!(!repo.has_running(pipeline_b).await.unwrap());

    // 正常完成的历史未被波及。
    let done = page
        .items
        .iter()
        .find(|item| item.id == finished)
        .expect("已完成记录应仍在列表里");
    assert_eq!(done.status, history::RunStatus::Success);
    assert!(done.error_message.is_none());

    // 幂等：再清一次无事可做。
    let marked = repo.fail_stale_running().await.expect("再次清理应成功");
    assert_eq!(marked, 0, "没有 running 时清点应为 0");
}

/// 判定与清理配合覆盖完整生命周期：running 期间拒绝并发，终态后放行。
#[tokio::test]
async fn running_block_last_only_until_a_terminal_state_is_written() {
    let db = database().await;
    let repo = HistoryRepository::new(&db);
    let pipeline_id = insert_pipeline(&db, "会结束的流水线").await;

    let history_id = record_running(&repo, pipeline_id, TriggerSource::Manual).await;
    assert!(
        repo.has_running(pipeline_id).await.unwrap(),
        "运行中应判定为运行中"
    );

    repo.finish(history_id, history::RunStatus::Success, None, Vec::new())
        .await
        .expect("应能写回终态");
    assert!(
        !repo.has_running(pipeline_id).await.unwrap(),
        "终态后应解除并发阻挡"
    );
}
