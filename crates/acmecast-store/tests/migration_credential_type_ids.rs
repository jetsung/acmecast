//! `m20250115` 凭据 type_id 重写迁移的验证：空库、旧值重写、新值保持、重复执行不变。
//!
//! SQLite 无需外部依赖即可本地运行；MySQL / PostgreSQL 由 CI 的
//! service container 提供连接串，通过 `ACMECAST__DATABASE__URL` 切换。

use sea_orm::{ConnectionTrait, Database, DatabaseBackend, Statement};
use sea_orm_migration::{MigrationTrait, MigratorTrait, SchemaManager};

use acmecast_store::migration::MigrateCredentialTypeIds;
use acmecast_store::Migrator;

/// 旧短名 → 新分域名，与迁移定义一一对应。
const REWRITES: [(&str, &str); 7] = [
    ("cloudflare", "cloud.cloudflare"),
    ("aliyun", "cloud.aliyun"),
    ("tencent", "cloud.tencent"),
    ("tencent-eo", "dns.tencent-eo"),
    ("ssh", "deploy.ssh"),
    ("s3", "deploy.s3"),
    ("webdav", "deploy.webdav"),
];

async fn connect() -> (sea_orm::DatabaseConnection, temp_dir::TempDirGuard) {
    // CI 通过环境变量提供 MySQL / PostgreSQL 连接串；未设置时落回本地 SQLite。
    let env_url = std::env::var("ACMECAST__DATABASE__URL").ok();
    if let Some(url) = env_url.filter(|url| !url.starts_with("sqlite")) {
        let db = Database::connect(&url).await.expect("应能连上外部数据库");
        return (db, temp_dir::TempDirGuard::noop());
    }
    let guard = temp_dir::TempDirGuard::new("acmecast-typeid-migration");
    let url = format!("sqlite://{}/test.db?mode=rwc", guard.path().display());
    let db = Database::connect(&url).await.expect("应能连上 SQLite");
    (db, guard)
}

/// 应用前 14 条迁移（建表），不含 type_id 重写迁移。
async fn migrate_before_rewrite(db: &sea_orm::DatabaseConnection) {
    Migrator::up(db, Some(14))
        .await
        .expect("前 14 条迁移应成功");
}

/// 直接重放 type_id 重写迁移本身（不经过 Migrator 的已应用去重）。
async fn run_rewrite(db: &sea_orm::DatabaseConnection) {
    let manager = SchemaManager::new(db);
    MigrateCredentialTypeIds
        .up(&manager)
        .await
        .expect("重写迁移应成功");
}

/// 插入一条指定 type_id 的凭据（其余列给最小占位值）。
async fn insert_credential(db: &sea_orm::DatabaseConnection, type_id: &str) {
    let backend = db.get_database_backend();
    let sql = match backend {
        DatabaseBackend::Postgres => {
            "INSERT INTO acmecast_credential (name, type_id, encrypted_fields, created_at, updated_at, capabilities) \
             VALUES ('c', $1, '{}', NOW(), NOW(), '[\"dns\",\"deploy\"]')"
                .to_owned()
        }
        DatabaseBackend::MySql => {
            "INSERT INTO acmecast_credential (name, type_id, encrypted_fields, created_at, updated_at, capabilities) \
             VALUES ('c', ?, '{}', NOW(), NOW(), '[\"dns\",\"deploy\"]')"
                .to_owned()
        }
        DatabaseBackend::Sqlite => {
            "INSERT INTO acmecast_credential (name, type_id, encrypted_fields, created_at, updated_at, capabilities) \
             VALUES ('c', ?, '{}', CURRENT_TIMESTAMP, CURRENT_TIMESTAMP, '[\"dns\",\"deploy\"]')"
                .to_owned()
        }
    };
    db.execute(Statement::from_sql_and_values(backend, sql, [type_id.into()]))
        .await
        .expect("应能插入凭据行");
}

/// 清空凭据表。
async fn clear_credentials(db: &sea_orm::DatabaseConnection) {
    db.execute(Statement::from_string(
        db.get_database_backend(),
        "DELETE FROM acmecast_credential".to_owned(),
    ))
    .await
    .expect("清表应成功");
}

/// 查询指定 type_id 的凭据行是否存在。
async fn exists_type_id(db: &sea_orm::DatabaseConnection, type_id: &str) -> bool {
    let backend = db.get_database_backend();
    let sql = "SELECT type_id FROM acmecast_credential WHERE type_id = ?";
    db.query_one(Statement::from_sql_and_values(backend, sql, [type_id.into()]))
        .await
        .expect("查询应成功")
        .is_some()
}

#[tokio::test]
async fn migration_leaves_empty_database_untouched() {
    let (db, _guard) = connect().await;
    migrate_before_rewrite(&db).await;
    run_rewrite(&db).await;

    let rows = db
        .query_all(Statement::from_string(
            db.get_database_backend(),
            "SELECT type_id FROM acmecast_credential".to_owned(),
        ))
        .await
        .expect("查询应成功");
    assert!(rows.is_empty(), "空库重写后凭据表应仍为空");
}

#[tokio::test]
async fn migration_rewrites_old_type_ids_and_keeps_new_ones() {
    let (db, _guard) = connect().await;
    migrate_before_rewrite(&db).await;

    // 逐个验证旧值重写：每轮单独插入待重写行，重放迁移后断言。
    for (old, new) in REWRITES {
        clear_credentials(&db).await;
        insert_credential(&db, old).await;

        run_rewrite(&db).await;

        assert!(
            exists_type_id(&db, new).await,
            "旧值 {old} 应被重写为 {new}"
        );
        assert!(
            !exists_type_id(&db, old).await,
            "重写后不应残留旧值 {old}"
        );
    }

    // 新值（acme.account）不被任何映射命中，重写后保持不变。
    clear_credentials(&db).await;
    insert_credential(&db, "acme.account").await;
    run_rewrite(&db).await;
    assert!(
        exists_type_id(&db, "acme.account").await,
        "acme.account 不应被重写"
    );
}

#[tokio::test]
async fn rerunning_migration_does_not_change_rewritten_rows() {
    let (db, _guard) = connect().await;
    migrate_before_rewrite(&db).await;
    insert_credential(&db, "cloudflare").await;

    run_rewrite(&db).await;
    // 重复执行：幂等，不报错也不改变结果。
    run_rewrite(&db).await;

    assert!(
        exists_type_id(&db, "cloud.cloudflare").await,
        "重写结果应保持稳定"
    );
    assert!(
        !exists_type_id(&db, "cloudflare").await,
        "重复执行不应把行改回旧值或产生新旧行"
    );
}

/// 极简临时目录守卫，替代引入额外 dev-dependency。
mod temp_dir {
    use std::path::{Path, PathBuf};

    /// 持有并在 Drop 时清理的临时目录；`noop` 模式不创建也不清理（外部数据库）。
    pub(crate) struct TempDirGuard {
        path: Option<PathBuf>,
    }

    impl TempDirGuard {
        pub(crate) fn new(prefix: &str) -> Self {
            let path = std::env::temp_dir().join(format!("{prefix}-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&path).expect("应能创建临时目录");
            Self { path: Some(path) }
        }

        pub(crate) fn noop() -> Self {
            Self { path: None }
        }

        pub(crate) fn path(&self) -> &Path {
            self.path.as_deref().unwrap_or(Path::new("."))
        }
    }

    impl Drop for TempDirGuard {
        fn drop(&mut self) {
            if let Some(path) = &self.path {
                std::fs::remove_dir_all(path).ok();
            }
        }
    }
}
