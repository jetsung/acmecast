//! 迁移的实际执行验证：全新库初始化、幂等重跑、回滚。
//!
//! SQLite 无需外部依赖即可本地运行；MySQL / PostgreSQL 由 CI 的
//! service container 提供连接串，通过 `ACMECAST__DATABASE__URL` 切换。

use sea_orm::{ConnectionTrait, Database, DatabaseBackend, Statement};
use sea_orm_migration::MigratorTrait;

use acmecast_store::Migrator;

/// 全部核心表名。
const TABLES: [&str; 8] = [
    "acmecast_pipeline",
    "acmecast_pipeline_step",
    "acmecast_history",
    "acmecast_history_log",
    "acmecast_storage",
    "acmecast_credential",
    "acmecast_cert",
    "acmecast_schedule",
];

async fn fresh_sqlite() -> (sea_orm::DatabaseConnection, temp_dir::TempDirGuard) {
    let guard = temp_dir::TempDirGuard::new("acmecast-migration");
    let url = format!("sqlite://{}/test.db?mode=rwc", guard.path().display());
    let db = Database::connect(&url).await.expect("应能连上 SQLite");
    (db, guard)
}

/// 列出当前库中存在的表。
async fn existing_tables(db: &sea_orm::DatabaseConnection) -> Vec<String> {
    let backend = db.get_database_backend();
    let rows = db
        .query_all(Statement::from_string(
            backend,
            match backend {
                DatabaseBackend::Sqlite => {
                    "SELECT name FROM sqlite_master WHERE type='table'".to_owned()
                }
                DatabaseBackend::MySql => "SHOW TABLES".to_owned(),
                DatabaseBackend::Postgres => {
                    "SELECT tablename AS name FROM pg_tables WHERE schemaname='public'".to_owned()
                }
            },
        ))
        .await
        .expect("应能列出表");

    rows.iter()
        .map(|row| row.try_get::<String>("", "name").unwrap_or_default())
        .collect()
}

#[tokio::test]
async fn migration_creates_all_core_tables() {
    let (db, _guard) = fresh_sqlite().await;
    Migrator::up(&db, None).await.expect("迁移应成功");

    let tables = existing_tables(&db).await;
    for table in TABLES {
        assert!(
            tables.contains(&table.to_string()),
            "缺少表 {table}：{tables:?}"
        );
    }
}

#[tokio::test]
async fn rerunning_migration_is_idempotent() {
    let (db, _guard) = fresh_sqlite().await;

    Migrator::up(&db, None).await.expect("首次迁移应成功");
    Migrator::up(&db, None).await.expect("重跑迁移不应报错");
    Migrator::up(&db, None)
        .await
        .expect("第三次重跑同样不应报错");

    let tables = existing_tables(&db).await;
    for table in TABLES {
        assert!(
            tables.contains(&table.to_string()),
            "重复迁移后仍应有表 {table}"
        );
    }
}

#[tokio::test]
async fn migration_status_reports_applied_versions() {
    let (db, _guard) = fresh_sqlite().await;
    Migrator::up(&db, None).await.unwrap();

    let applied = Migrator::get_applied_migrations(&db)
        .await
        .expect("应能读取已应用迁移");
    assert!(!applied.is_empty(), "应有已应用的迁移记录");

    let pending = Migrator::get_pending_migrations(&db)
        .await
        .expect("应能读取待应用迁移");
    assert!(pending.is_empty(), "全部迁移已应用后不应有待办");
}

#[tokio::test]
async fn migration_down_removes_tables() {
    let (db, _guard) = fresh_sqlite().await;
    Migrator::up(&db, None).await.unwrap();
    Migrator::down(&db, None).await.expect("回滚应成功");

    let tables = existing_tables(&db).await;
    for table in TABLES {
        assert!(
            !tables.contains(&table.to_string()),
            "回滚后不应有表 {table}"
        );
    }
}

#[tokio::test]
async fn migration_then_down_then_up_is_recoverable() {
    let (db, _guard) = fresh_sqlite().await;
    Migrator::up(&db, None).await.unwrap();
    Migrator::down(&db, None).await.unwrap();
    Migrator::up(&db, None).await.expect("回滚后重建应成功");

    let tables = existing_tables(&db).await;
    assert!(
        tables.contains(&"acmecast_cert".to_string()),
        "重建后表应存在"
    );
}

#[tokio::test]
async fn cert_table_after_migration_has_account_column() {
    let (db, _guard) = fresh_sqlite().await;
    Migrator::up(&db, None).await.unwrap();

    // design 决策 6：证书表必须带 acme_account_access_id，
    // 这样吊销时直接读表，无需像 certd 那样反查流水线。
    let rows = db
        .query_all(Statement::from_string(
            db.get_database_backend(),
            "SELECT acme_account_access_id FROM acmecast_cert LIMIT 1".to_owned(),
        ))
        .await
        .expect("该列必须存在");
    assert!(rows.is_empty(), "仅验证列存在，不限定行数");
}

/// 一个极简的临时目录守卫，替代引入额外 dev-dependency。
mod temp_dir {
    use std::path::{Path, PathBuf};

    /// 持有并在 Drop 时清理的临时目录。
    pub(crate) struct TempDirGuard {
        path: PathBuf,
    }

    impl TempDirGuard {
        /// 创建临时目录。
        pub(crate) fn new(prefix: &str) -> Self {
            let path = std::env::temp_dir().join(format!("{prefix}-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&path).expect("应能创建临时目录");
            Self { path }
        }

        /// 临时目录路径。
        pub(crate) fn path(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for TempDirGuard {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.path).ok();
        }
    }
}
