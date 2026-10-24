//! `m20250117` 凭据 `proxy_mode` 明文列迁移与回填的验证：
//! 空库建列、存量旧 `proxy_url` 回填为 `custom`、无代理行保持 `direct`、
//! 非默认值不被覆盖、重复执行幂等、无密钥跳过。
//!
//! SQLite 无需外部依赖即可本地运行；MySQL / PostgreSQL 由 CI 的
//! service container 提供连接串，通过 `ACMECAST__DATABASE__URL` 切换。

use sea_orm::{ConnectionTrait, Database, DatabaseBackend, Statement};
use sea_orm_migration::{MigrationTrait, MigratorTrait, SchemaManager};

use acmecast_store::migration::AddCredentialProxyMode;
use acmecast_store::Migrator;

/// 与测试密钥一致的加密器：构造存量 `encrypted_fields` 用。
fn cipher() -> acmecast_core::CredentialCipher {
    acmecast_core::CredentialCipher::from_bytes(&[7u8; 32]).expect("密钥应合法")
}

async fn connect() -> (sea_orm::DatabaseConnection, temp_dir::TempDirGuard) {
    // CI 通过环境变量提供 MySQL / PostgreSQL 连接串；未设置时落回本地 SQLite。
    let env_url = std::env::var("ACMECAST__DATABASE__URL").ok();
    if let Some(url) = env_url.filter(|url| !url.starts_with("sqlite")) {
        let db = Database::connect(&url).await.expect("应能连上外部数据库");
        return (db, temp_dir::TempDirGuard::noop());
    }
    let guard = temp_dir::TempDirGuard::new("acmecast-proxy-mode-migration");
    let url = format!("sqlite://{}/test.db?mode=rwc", guard.path().display());
    let db = Database::connect(&url).await.expect("应能连上 SQLite");
    (db, guard)
}

/// 应用前 16 条迁移（建表），不含 proxy_mode 列迁移。
async fn migrate_before_proxy_mode(db: &sea_orm::DatabaseConnection) {
    Migrator::up(db, Some(16))
        .await
        .expect("前 16 条迁移应成功");
}

/// 应用 proxy_mode 列迁移（建列，存量行得默认 direct）。
async fn run_proxy_mode_migration(db: &sea_orm::DatabaseConnection) {
    let manager = SchemaManager::new(db);
    AddCredentialProxyMode
        .up(&manager)
        .await
        .expect("proxy_mode 迁移应成功");
}

/// 插入一条凭据：`fields` 加密后作为 `encrypted_fields`（proxy_mode 列不指定，取默认值）。
async fn insert_credential(
    db: &sea_orm::DatabaseConnection,
    name: &str,
    fields: &serde_json::Value,
) {
    let encrypted = cipher()
        .encrypt_string(&fields.to_string())
        .expect("应能加密字段");
    let backend = db.get_database_backend();
    let sql = match backend {
        DatabaseBackend::Postgres => {
            "INSERT INTO acmecast_credential (name, type_id, encrypted_fields, created_at, updated_at, capabilities) \
             VALUES ($1, 'cloud.cloudflare', $2, NOW(), NOW(), '[]')"
                .to_owned()
        }
        DatabaseBackend::MySql => {
            "INSERT INTO acmecast_credential (name, type_id, encrypted_fields, created_at, updated_at, capabilities) \
             VALUES (?, 'cloud.cloudflare', ?, NOW(), NOW(), '[]')"
                .to_owned()
        }
        DatabaseBackend::Sqlite => {
            "INSERT INTO acmecast_credential (name, type_id, encrypted_fields, created_at, updated_at, capabilities) \
             VALUES (?, 'cloud.cloudflare', ?, CURRENT_TIMESTAMP, CURRENT_TIMESTAMP, '[]')"
                .to_owned()
        }
    };
    db.execute(Statement::from_sql_and_values(
        backend,
        sql,
        [name.into(), encrypted.into()],
    ))
    .await
    .expect("应能插入凭据行");
}

/// 查询指定名称凭据的 proxy_mode 列值。
async fn proxy_mode_of(db: &sea_orm::DatabaseConnection, name: &str) -> String {
    let row = db
        .query_one(Statement::from_sql_and_values(
            db.get_database_backend(),
            "SELECT proxy_mode FROM acmecast_credential WHERE name = ?",
            [name.into()],
        ))
        .await
        .expect("查询应成功")
        .expect("行应存在");
    row.try_get::<String>("", "proxy_mode")
        .expect("proxy_mode 应为文本")
}

#[tokio::test]
async fn migration_adds_column_without_touching_empty_database() {
    let (db, _guard) = connect().await;
    migrate_before_proxy_mode(&db).await;
    run_proxy_mode_migration(&db).await;

    // 空表查询 proxy_mode 不报错，即列已存在；无行需回填。
    let rows = db
        .query_all(Statement::from_string(
            db.get_database_backend(),
            "SELECT proxy_mode FROM acmecast_credential".to_owned(),
        ))
        .await
        .expect("查询 proxy_mode 列应成功");
    assert!(rows.is_empty(), "空库迁移后凭据表应仍为空");
}

#[tokio::test]
async fn backfill_marks_legacy_proxy_url_rows_as_custom_and_others_direct() {
    let (db, _guard) = connect().await;
    migrate_before_proxy_mode(&db).await;
    // 存量凭据：一行带旧 proxy_url（无 proxy_mode），一行完全无代理字段。
    insert_credential(
        &db,
        "legacy",
        &serde_json::json!({ "api_token": "tok", "proxy_url": "socks5://127.0.0.1:7890" }),
    )
    .await;
    insert_credential(&db, "plain", &serde_json::json!({ "api_token": "tok" })).await;
    run_proxy_mode_migration(&db).await;
    assert_eq!(proxy_mode_of(&db, "legacy").await, "direct", "建列后存量行得默认 direct");
    assert_eq!(proxy_mode_of(&db, "plain").await, "direct");

    // 密钥就绪后回填：仅「默认 direct 但实际为 custom」的行被改写。
    acmecast_store::migration::backfill_credential_proxy_mode(&db, Some(&cipher()))
        .await
        .expect("回填应成功");
    assert_eq!(
        proxy_mode_of(&db, "legacy").await,
        "custom",
        "旧 proxy_url 有值的存量行应回填为 custom"
    );
    assert_eq!(proxy_mode_of(&db, "plain").await, "direct", "无代理字段行保持 direct");
}

#[tokio::test]
async fn backfill_is_idempotent_and_preserves_non_default_values() {
    let (db, _guard) = connect().await;
    migrate_before_proxy_mode(&db).await;
    insert_credential(
        &db,
        "legacy",
        &serde_json::json!({ "api_token": "tok", "proxy_url": "http://127.0.0.1:8080" }),
    )
    .await;
    insert_credential(
        &db,
        "inherited",
        &serde_json::json!({ "api_token": "tok", "proxy_mode": "inherit" }),
    )
    .await;
    run_proxy_mode_migration(&db).await;

    acmecast_store::migration::backfill_credential_proxy_mode(&db, Some(&cipher()))
        .await
        .expect("首次回填应成功");
    assert_eq!(proxy_mode_of(&db, "legacy").await, "custom");
    assert_eq!(proxy_mode_of(&db, "inherited").await, "inherit", "inherit 行不应被回填改动");

    // 重复执行：幂等，值保持稳定。
    acmecast_store::migration::backfill_credential_proxy_mode(&db, Some(&cipher()))
        .await
        .expect("重复回填应成功");
    assert_eq!(proxy_mode_of(&db, "legacy").await, "custom");
    assert_eq!(proxy_mode_of(&db, "inherited").await, "inherit");
}

#[tokio::test]
async fn backfill_without_cipher_keeps_default_direct() {
    let (db, _guard) = connect().await;
    migrate_before_proxy_mode(&db).await;
    insert_credential(
        &db,
        "legacy",
        &serde_json::json!({ "api_token": "tok", "proxy_url": "socks5://127.0.0.1:7890" }),
    )
    .await;
    run_proxy_mode_migration(&db).await;

    // 密钥未配置：跳过回填不报错，行保持默认 direct，待密钥配置后补回填。
    acmecast_store::migration::backfill_credential_proxy_mode(&db, None)
        .await
        .expect("无密钥跳过回填不应报错");
    assert_eq!(proxy_mode_of(&db, "legacy").await, "direct");
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
