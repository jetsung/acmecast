//! 6.7 流水线级键值存储与跨流水线隔离。
//!
//! spec 的两个场景：同一流水线跨运行可读到、不同流水线之间互不影响。

use acmecast_pipeline::{DatabaseStateStore, PipelineStateStore};
use acmecast_store::entity::{pipeline, storage};
use acmecast_store::migrate;
use chrono::Utc;
use sea_orm::ColumnTrait;
use sea_orm::{ActiveModelTrait, Database, DatabaseConnection, EntityTrait, QueryFilter, Set};

async fn setup() -> DatabaseConnection {
    let db = Database::connect("sqlite::memory:")
        .await
        .expect("应能连上内存库");
    migrate(&db).await.expect("迁移应成功");
    db
}

/// 插入一条流水线（状态表有外键指向它），返回主键。
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

// ---- Scenario: 跨运行保存状态 ----

#[tokio::test]
async fn a_value_written_in_one_run_is_readable_in_the_next() {
    let db = setup().await;
    let pipeline_id = insert_pipeline(&db, "甲流水线").await;

    // 这一次运行写下状态……
    DatabaseStateStore::new(&db)
        .set(
            pipeline_id,
            "last_deployed_path",
            serde_json::json!("/etc/ssl/site.pem"),
        )
        .await
        .expect("应能写入");

    // ……下一次运行换个实例来读（相当于进程重启后重新装配），值仍在。
    let later_run = DatabaseStateStore::new(&db);
    assert_eq!(
        later_run
            .get(pipeline_id, "last_deployed_path")
            .await
            .expect("应能读取"),
        Some(serde_json::json!("/etc/ssl/site.pem"))
    );
}

#[tokio::test]
async fn an_unwritten_key_reads_as_none() {
    let db = setup().await;
    let pipeline_id = insert_pipeline(&db, "甲流水线").await;

    assert_eq!(
        DatabaseStateStore::new(&db)
            .get(pipeline_id, "从未写过的键")
            .await
            .expect("读取不存在不是错误"),
        None
    );
}

#[tokio::test]
async fn a_non_string_value_round_trips() {
    // 值不限于字符串：域名列表、时间戳之类都要能存。
    let db = setup().await;
    let store = DatabaseStateStore::new(&db);
    let pipeline_id = insert_pipeline(&db, "甲流水线").await;

    let value = serde_json::json!({
        "domains": ["a.example.com", "b.example.com"],
        "attempt": 3,
        "ok": true,
    });
    store
        .set(pipeline_id, "last_attempt", value.clone())
        .await
        .unwrap();

    assert_eq!(
        store.get(pipeline_id, "last_attempt").await.unwrap(),
        Some(value)
    );
}

// ---- Scenario: 跨流水线隔离 ----

#[tokio::test]
async fn another_pipeline_cannot_see_the_same_key() {
    let db = setup().await;
    let store = DatabaseStateStore::new(&db);
    let first = insert_pipeline(&db, "甲流水线").await;
    let second = insert_pipeline(&db, "乙流水线").await;

    store
        .set(first, "last_path", serde_json::json!("甲的值"))
        .await
        .unwrap();

    // 键名相同，但另一条流水线读到的是空。
    assert_eq!(store.get(second, "last_path").await.unwrap(), None);
    assert_eq!(
        store.get(first, "last_path").await.unwrap(),
        Some(serde_json::json!("甲的值"))
    );

    // 另一条写自己的同名键，也不会盖掉第一条的。
    store
        .set(second, "last_path", serde_json::json!("乙的值"))
        .await
        .unwrap();
    assert_eq!(
        store.get(first, "last_path").await.unwrap(),
        Some(serde_json::json!("甲的值")),
        "两条流水线的同名键应是各自独立的两行"
    );
    assert_eq!(
        store.get(second, "last_path").await.unwrap(),
        Some(serde_json::json!("乙的值"))
    );
}

// ---- 写入语义 ----

#[tokio::test]
async fn writing_the_same_key_again_replaces_the_value() {
    // 任务是反复写同一个键的（每次运行更新「上次成功的记录」），
    // 因此必须是 upsert，而不是攒出一堆重复行。
    let db = setup().await;
    let store = DatabaseStateStore::new(&db);
    let pipeline_id = insert_pipeline(&db, "甲流水线").await;

    for attempt in 1..=3 {
        store
            .set(pipeline_id, "last_attempt", serde_json::json!(attempt))
            .await
            .unwrap();
    }

    assert_eq!(
        store.get(pipeline_id, "last_attempt").await.unwrap(),
        Some(serde_json::json!(3))
    );

    let rows = storage::Entity::find()
        .filter(storage::Column::PipelineId.eq(pipeline_id))
        .all(&db)
        .await
        .unwrap();
    assert_eq!(rows.len(), 1, "同一个键只应有一行");
}

#[tokio::test]
async fn deleting_the_pipeline_takes_its_state_with_it() {
    // 外键级联：流水线没了，它的状态不该留在库里变成孤儿。
    let db = setup().await;
    let store = DatabaseStateStore::new(&db);
    let pipeline_id = insert_pipeline(&db, "甲流水线").await;

    store
        .set(pipeline_id, "last_path", serde_json::json!("/x"))
        .await
        .unwrap();

    pipeline::Entity::delete_by_id(pipeline_id)
        .exec(&db)
        .await
        .expect("应能删除流水线");

    assert!(
        storage::Entity::find().all(&db).await.unwrap().is_empty(),
        "流水线被删后其状态应一并清掉"
    );
}
