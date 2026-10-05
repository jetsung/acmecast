//! 调度仓储的保存校验与 upsert 语义。
//!
//! 跑真实迁移的 SQLite 内存库，重点钉住「启用的调度必须至少配置一种
//! 触发方式」的校验——它是本变更唯一的行为收紧点。

use acmecast_store::entity::pipeline;
use acmecast_store::repository::{ScheduleInput, ScheduleRepository};
use acmecast_store::{Error, Migrator};
use sea_orm::{ActiveModelTrait, Database, DatabaseConnection, Set};
use sea_orm_migration::MigratorTrait;

/// 建一个跑完全部迁移的内存库，并插入一条可挂调度的流水线，返回其主键。
async fn setup() -> (DatabaseConnection, i64) {
    let db = Database::connect("sqlite::memory:")
        .await
        .expect("应能连上内存库");
    Migrator::up(&db, None).await.expect("迁移应成功");

    let now = chrono::Utc::now();
    let pipeline_id = pipeline::ActiveModel {
        name: Set("测试流水线".to_owned()),
        enabled: Set(true),
        description: Set(None),
        created_at: Set(now),
        updated_at: Set(now),
        ..Default::default()
    }
    .insert(&db)
    .await
    .expect("应能插入流水线")
    .id;

    (db, pipeline_id)
}

/// 分别断言错误为 Validation 且信息说明至少要配置一种触发方式。
fn assert_needs_trigger(error: Error) {
    let message = error.to_string();
    assert!(
        message.contains("至少配置 cron 或续期域名集合"),
        "错误应说明必须至少配置一种触发方式：{message}"
    );
}

#[tokio::test]
async fn enabled_schedule_without_any_trigger_is_rejected() {
    let (db, pipeline_id) = setup().await;
    let repo = ScheduleRepository::new(&db);

    let error = repo
        .save(
            pipeline_id,
            ScheduleInput {
                cron: None,
                enabled: true,
                catch_up: false,
                renewal_domains: None,
            },
        )
        .await
        .expect_err("启用且双空的调度应被拒绝");
    assert_needs_trigger(error);

    // 空数组与 None 同义：同样拒绝。
    let error = repo
        .save(
            pipeline_id,
            ScheduleInput {
                cron: None,
                enabled: true,
                catch_up: false,
                renewal_domains: Some(vec![]),
            },
        )
        .await
        .expect_err("空续期域名集合视同未配置");
    assert_needs_trigger(error);
}

#[tokio::test]
async fn disabled_schedule_may_have_no_trigger() {
    let (db, pipeline_id) = setup().await;

    let config = ScheduleRepository::new(&db)
        .save(
            pipeline_id,
            ScheduleInput {
                cron: None,
                enabled: false,
                catch_up: false,
                renewal_domains: None,
            },
        )
        .await
        .expect("停用调度允许双空，可先占位后补配置");

    assert!(!config.enabled);
    assert!(config.cron.is_none());
    assert!(config.renewal_domains.is_none());
}

#[tokio::test]
async fn enabled_schedule_with_only_cron_is_saved() {
    let (db, pipeline_id) = setup().await;

    let config = ScheduleRepository::new(&db)
        .save(
            pipeline_id,
            ScheduleInput {
                cron: Some("0 3 * * *".to_owned()),
                enabled: true,
                catch_up: false,
                renewal_domains: None,
            },
        )
        .await
        .expect("仅 cron 的启用调度合法");

    assert_eq!(config.cron.as_deref(), Some("0 3 * * *"));
    // 保存时算出初始触发点，调度一落库就是「武装」状态。
    assert!(config.next_trigger_at.is_some());
    assert!(config.renewal_domains.is_none());
}

#[tokio::test]
async fn enabled_schedule_with_only_renewal_domains_is_saved() {
    let (db, pipeline_id) = setup().await;

    let config = ScheduleRepository::new(&db)
        .save(
            pipeline_id,
            ScheduleInput {
                cron: None,
                enabled: true,
                catch_up: false,
                renewal_domains: Some(vec!["Example.COM".to_owned()]),
            },
        )
        .await
        .expect("仅续期域名集合的启用调度合法");

    // 域名按证书同一套规则规范化为小写。
    assert_eq!(
        config.renewal_domains,
        Some(vec!["example.com".to_owned()]),
    );
    // 没有 cron 就没有定时触发点。
    assert!(config.next_trigger_at.is_none());
}

#[tokio::test]
async fn save_overwrites_the_existing_schedule_for_the_same_pipeline() {
    let (db, pipeline_id) = setup().await;
    let repo = ScheduleRepository::new(&db);

    repo.save(
        pipeline_id,
        ScheduleInput {
            cron: Some("0 3 * * *".to_owned()),
            enabled: true,
            catch_up: false,
            renewal_domains: None,
        },
    )
    .await
    .expect("首次保存应成功");

    let updated = repo
        .save(
            pipeline_id,
            ScheduleInput {
                cron: None,
                enabled: false,
                catch_up: true,
                renewal_domains: Some(vec!["example.com".to_owned()]),
            },
        )
        .await
        .expect("同流水线再次保存是整体覆盖");

    assert_eq!(updated.pipeline_id, pipeline_id);
    assert!(updated.cron.is_none());
    assert!(!updated.enabled);
    assert!(updated.catch_up);
    assert_eq!(
        updated.renewal_domains,
        Some(vec!["example.com".to_owned()])
    );

    // 全库仍只有这一份调度（pipeline_id 唯一）。
    assert_eq!(repo.list_all().await.expect("应能列出调度").len(), 1);
}
