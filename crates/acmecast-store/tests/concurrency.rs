//! 2.6 连接池与并发安全。
//!
//! 两条断言：
//! - **并发更新同一记录不出现字段混杂**：最终态整体来自某一次写入，
//!   而不是「A 改的 name + B 改的 description」这种拼接结果；
//! - **连接池耗尽时请求排队而非报错**，且池上限确实被施加了。
//!
//! 用**文件库**而非内存库：`sqlite::memory:` 的每条连接都是一个独立的库，
//! 多连接下根本测不到并发语义。

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use acmecast_core::config::DatabaseConfig;
use acmecast_store::entity::pipeline;
use acmecast_store::{connect, migrate};
use chrono::Utc;
use sea_orm::{ActiveModelTrait, DatabaseConnection, EntityTrait, Set, TransactionTrait};

/// 临时目录守卫，Drop 时清理。
struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        let path =
            std::env::temp_dir().join(format!("acmecast-concurrency-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&path).expect("应能创建临时目录");
        Self(path)
    }

    fn db_url(&self) -> String {
        format!("sqlite://{}/test.db?mode=rwc", self.0.display())
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).ok();
    }
}

/// 建一个跑完迁移的文件库。
async fn setup(max_connections: u32) -> (DatabaseConnection, TempDir) {
    let temp = TempDir::new();
    let config = DatabaseConfig {
        url: temp.db_url(),
        max_connections,
        ..Default::default()
    };
    let db = connect(&config).await.expect("应能建立连接");
    migrate(&db).await.expect("迁移应成功");
    (db, temp)
}

/// 插入一条流水线，返回其主键。
async fn insert_pipeline(db: &DatabaseConnection, name: &str) -> i64 {
    let now = Utc::now();
    pipeline::ActiveModel {
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

/// 把流水线整体改写成「自洽的一组值」：三个字段都带同一个 `tag`。
///
/// 关键在于这是一次**完整**写入——若并发下出现字段混杂，
/// 最终态里就会看到不同 tag 的字段被拼在一起。
async fn rewrite_pipeline(db: DatabaseConnection, id: i64, tag: usize) {
    pipeline::ActiveModel {
        id: Set(id),
        name: Set(format!("名称-{tag}")),
        enabled: Set(tag.is_multiple_of(2)),
        description: Set(Some(format!("来自-{tag}"))),
        updated_at: Set(Utc::now()),
        ..Default::default()
    }
    .update(&db)
    .await
    .expect("更新不应失败");
}

/// 从「名称-<tag>」里取出 tag。
fn tag_of(name: &str) -> usize {
    name.rsplit('-')
        .next()
        .and_then(|suffix| suffix.parse().ok())
        .unwrap_or_else(|| panic!("名称应带 tag 后缀，实际为 {name:?}"))
}

// ---- Scenario: 并发更新同一流水线 ----

#[tokio::test]
async fn concurrent_updates_never_interleave_fields() {
    let (db, _temp) = setup(5).await;
    let id = insert_pipeline(&db, "初始").await;

    let writers = 16;
    let mut handles = Vec::with_capacity(writers);
    for tag in 0..writers {
        let db = db.clone();
        handles.push(tokio::spawn(rewrite_pipeline(db, id, tag)));
    }
    for handle in handles {
        handle.await.expect("任务不应 panic");
    }

    let final_state = pipeline::Entity::find_by_id(id)
        .one(&db)
        .await
        .expect("应能读回")
        .expect("记录应仍在");

    // 核心断言：三个字段必须来自**同一次**写入。
    let name_tag = tag_of(&final_state.name);
    assert!(name_tag < writers, "最终态应来自某一次写入: {name_tag}");
    assert_eq!(
        final_state.description,
        Some(format!("来自-{name_tag}")),
        "描述与名称应来自同一次写入，而不是被拼接成混杂态"
    );
    assert_eq!(
        final_state.enabled,
        name_tag.is_multiple_of(2),
        "启用状态应与同一次写入的 tag 相符"
    );

    // 并发更新不应凭空多出记录。
    let total = pipeline::Entity::find()
        .all(&db)
        .await
        .expect("应能统计")
        .len();
    assert_eq!(total, 1, "并发更新不应产生新记录");
}

#[tokio::test]
async fn concurrent_updates_keep_the_row_readable() {
    // 混杂之外的另一类事故是「更新到一半留下不可读的数据」。
    let (db, _temp) = setup(5).await;
    let id = insert_pipeline(&db, "初始").await;

    let mut handles = Vec::new();
    for tag in 0..12 {
        let db = db.clone();
        handles.push(tokio::spawn(rewrite_pipeline(db, id, tag)));
    }
    for handle in handles {
        handle.await.expect("任务不应 panic");
    }

    // 读回来的每一列都必须能按定义的类型解出。
    let rows = pipeline::Entity::find()
        .all(&db)
        .await
        .expect("并发写入后记录仍应可读");
    assert_eq!(rows.len(), 1);
    assert!(!rows[0].name.is_empty(), "名称不应为空");
    assert!(
        rows[0]
            .description
            .as_deref()
            .is_some_and(|d| d.starts_with("来自-")),
        "描述应是某次写入留下的完整值"
    );
}

// ---- Scenario: 连接池耗尽 ----

#[tokio::test]
async fn the_pool_caps_concurrency_and_queues_the_rest() {
    let (db, _temp) = setup(1).await;

    let in_flight = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));

    let requests = 8;
    let mut handles = Vec::with_capacity(requests);
    for _ in 0..requests {
        let db = db.clone();
        let in_flight = Arc::clone(&in_flight);
        let peak = Arc::clone(&peak);

        handles.push(tokio::spawn(async move {
            // 开启事务即从池中取走一条连接，并在事务存续期间一直占用它。
            let transaction = db.begin().await.expect("连接池应排队等待而非报错");

            let now = in_flight.fetch_add(1, Ordering::SeqCst) + 1;
            peak.fetch_max(now, Ordering::SeqCst);
            tokio::time::sleep(Duration::from_millis(10)).await;
            in_flight.fetch_sub(1, Ordering::SeqCst);

            transaction.rollback().await.expect("应能回滚");
        }));
    }

    // 每个请求都必须成功——池满时应当是排队，而不是返回错误。
    for handle in handles {
        handle.await.expect("任务不应 panic");
    }

    assert_eq!(
        peak.load(Ordering::SeqCst),
        1,
        "池上限为 1 时，同时持有连接的任务不应超过 1 个——超过即说明上限没被施加"
    );
    assert!(db.ping().await.is_ok(), "排队结束后连接池应恢复可用");
}

#[tokio::test]
async fn a_generous_pool_still_serves_requests_in_parallel() {
    // 与上一条对照：池上限放宽后，并发度应当随之上升——
    // 否则「排队」可能只是碰巧串行，而不是池上限在起作用。
    let (db, _temp) = setup(4).await;

    let in_flight = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));

    let mut handles = Vec::new();
    for _ in 0..4 {
        let db = db.clone();
        let in_flight = Arc::clone(&in_flight);
        let peak = Arc::clone(&peak);

        handles.push(tokio::spawn(async move {
            let transaction = db.begin().await.expect("应能取得连接");
            let now = in_flight.fetch_add(1, Ordering::SeqCst) + 1;
            peak.fetch_max(now, Ordering::SeqCst);
            tokio::time::sleep(Duration::from_millis(20)).await;
            in_flight.fetch_sub(1, Ordering::SeqCst);
            transaction.rollback().await.expect("应能回滚");
        }));
    }

    for handle in handles {
        handle.await.expect("任务不应 panic");
    }

    assert!(
        peak.load(Ordering::SeqCst) > 1,
        "池上限放宽到 4 时应当真的并发执行，实际峰值为 1"
    );
}
