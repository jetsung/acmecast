//! 全局调度仓储的单例语义与保存校验。
//!
//! 跑真实迁移的 SQLite 内存库，钉住三件事：未创建时 `get_or_default`
//! 返回默认形态、save upsert 至多一行、启用但无 cron 被拒绝（global-schedule
//! spec 的「单例存储」需求）。

use acmecast_store::Migrator;
use acmecast_store::repository::{GlobalScheduleInput, GlobalScheduleRepository};
use sea_orm::{Database, DatabaseConnection, EntityTrait};
use sea_orm_migration::MigratorTrait;

/// 建一个跑完全部迁移的内存库。
async fn setup() -> DatabaseConnection {
    let db = Database::connect("sqlite::memory:")
        .await
        .expect("应能连上内存库");
    Migrator::up(&db, None).await.expect("迁移应成功");
    db
}

#[tokio::test]
async fn get_or_default_returns_the_disabled_form_before_first_save() {
    let db = setup().await;
    let repo = GlobalScheduleRepository::new(&db);

    let config = repo.get_or_default().await.expect("未创建应返回默认形态");
    assert!(!config.enabled, "默认形态应停用");
    assert_eq!(config.cron, None, "默认形态应无 cron");
    assert_eq!(config.next_trigger_at, None);
}

#[tokio::test]
async fn save_upserts_a_single_row_and_computes_the_next_trigger() {
    let db = setup().await;
    let repo = GlobalScheduleRepository::new(&db);

    let first = repo
        .save(
            GlobalScheduleInput {
                cron: Some("0 3 * * *".to_owned()),
                enabled: true,
                catch_up: false,
            },
            chrono_tz::UTC,
        )
        .await
        .expect("合法保存应成功");
    assert!(first.enabled);
    assert!(first.next_trigger_at.is_some(), "保存时应算出下次触发点");

    // 整体更新：换 cron、停用——还是同一行（id=1），不产生第二行。
    let second = repo
        .save(
            GlobalScheduleInput {
                cron: Some("0 4 * * *".to_owned()),
                enabled: false,
                catch_up: true,
            },
            chrono_tz::UTC,
        )
        .await
        .expect("再次保存应成功");
    assert!(!second.enabled);
    assert!(second.catch_up);

    let rows = acmecast_store::entity::global_schedule::Entity::find()
        .all(&db)
        .await
        .expect("应能读表");
    assert_eq!(rows.len(), 1, "单例：至多一行，实际 {} 行", rows.len());
    assert_eq!(rows[0].cron.as_deref(), Some("0 4 * * *"));
}

#[tokio::test]
async fn enabled_without_cron_is_rejected() {
    let db = setup().await;
    let repo = GlobalScheduleRepository::new(&db);

    let error = repo
        .save(
            GlobalScheduleInput {
                cron: None,
                enabled: true,
                catch_up: false,
            },
            chrono_tz::UTC,
        )
        .await
        .expect_err("启用但无 cron 应被拒绝");
    let message = error.to_string();
    assert!(
        message.contains("启用的全局调度必须配置 cron"),
        "错误应说明原因：{message}"
    );
}

#[tokio::test]
async fn invalid_cron_is_rejected() {
    let db = setup().await;
    let repo = GlobalScheduleRepository::new(&db);

    let error = repo
        .save(
            GlobalScheduleInput {
                cron: Some("0 99 * * *".to_owned()),
                enabled: true,
                catch_up: false,
            },
            chrono_tz::UTC,
        )
        .await
        .expect_err("非法 cron 应被拒绝");
    let message = error.to_string();
    assert!(
        message.contains("0 99 * * *"),
        "错误应带上 cron 原文：{message}"
    );
}

#[tokio::test]
async fn mark_triggered_advances_the_trigger_point() {
    let db = setup().await;
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
    .expect("保存应成功");

    let now = chrono::Utc::now();
    let next = chrono::Utc::now() + chrono::Duration::days(1);
    repo.mark_triggered(Some(Some(now)), Some(next))
        .await
        .expect("推进应成功");

    let config = repo.get_or_default().await.expect("应能读回");
    assert_eq!(config.last_triggered_at, Some(now));
    assert_eq!(config.next_trigger_at, Some(next));
}
