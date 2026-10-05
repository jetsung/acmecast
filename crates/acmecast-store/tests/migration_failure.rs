//! 2.4 迁移失败处理。
//!
//! 验证四件事：
//!
//! 1. 某条迁移失败时，错误里**点名**失败版本——SeaORM 自己抛的 `DbErr` 不含版本信息，
//!    版本名只出现在日志里；
//! 2. 失败**之后**的迁移不再执行；
//! 3. 失败**之前**的迁移保持已应用，失败的那条**不**被记为已应用；
//! 4. 数据库仍可连接可查询，把失败那条改对之后能从断点续跑。
//!
//! 「故意失败的迁移」由本测试自行定义并经 `migrate_with::<M>` 注入——
//! 生产迁移链里不该、也没有这种东西。

use acmecast_store::Error;
use acmecast_store::migration::{migrate, migrate_with};
use sea_orm::sea_query::{Alias, ColumnDef, Table};
use sea_orm::{ConnectionTrait, Database, DatabaseBackend, DatabaseConnection, Statement};
use sea_orm_migration::prelude::*;

const FIRST_VERSION: &str = "m20990101_000000_probe_first";
const FAILING_VERSION: &str = "m20990101_000001_probe_middle";
const LAST_VERSION: &str = "m20990101_000002_probe_last";

const PROBE_FIRST: &str = "probe_first";
const PROBE_MIDDLE: &str = "probe_middle";
const PROBE_LAST: &str = "probe_last";

// ---- 测试用迁移链 ----

/// 建一张探针表。表在不在，就是「这条迁移有没有跑过」的证据。
fn create_probe(name: &'static str) -> TableCreateStatement {
    Table::create()
        .table(Alias::new(name))
        .if_not_exists()
        .col(
            ColumnDef::new(Alias::new("id"))
                .big_integer()
                .primary_key()
                .auto_increment(),
        )
        .to_owned()
}

fn drop_probe(name: &'static str) -> TableDropStatement {
    Table::drop().table(Alias::new(name)).to_owned()
}

struct CreateFirstProbe;

impl MigrationName for CreateFirstProbe {
    fn name(&self) -> &str {
        FIRST_VERSION
    }
}

#[allow(elided_lifetimes_in_paths)]
#[async_trait::async_trait]
impl MigrationTrait for CreateFirstProbe {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.create_table(create_probe(PROBE_FIRST)).await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.drop_table(drop_probe(PROBE_FIRST)).await
    }
}

struct CreateLastProbe;

impl MigrationName for CreateLastProbe {
    fn name(&self) -> &str {
        LAST_VERSION
    }
}

#[allow(elided_lifetimes_in_paths)]
#[async_trait::async_trait]
impl MigrationTrait for CreateLastProbe {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.create_table(create_probe(PROBE_LAST)).await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.drop_table(drop_probe(PROBE_LAST)).await
    }
}

/// 故意失败的迁移：什么都不建，直接返回错误。
struct DeliberatelyFailing;

impl MigrationName for DeliberatelyFailing {
    fn name(&self) -> &str {
        FAILING_VERSION
    }
}

#[allow(elided_lifetimes_in_paths)]
#[async_trait::async_trait]
impl MigrationTrait for DeliberatelyFailing {
    async fn up(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        Err(DbErr::Migration(
            "故意失败：用于验证迁移链的中止行为".to_owned(),
        ))
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        Ok(())
    }
}

/// 「改对之后」的同名迁移：版本名与 [`DeliberatelyFailing`] 完全相同，
/// 用来模拟把失败的迁移修好之后再跑一次。
struct FixedMiddle;

impl MigrationName for FixedMiddle {
    fn name(&self) -> &str {
        FAILING_VERSION
    }
}

#[allow(elided_lifetimes_in_paths)]
#[async_trait::async_trait]
impl MigrationTrait for FixedMiddle {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.create_table(create_probe(PROBE_MIDDLE)).await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.drop_table(drop_probe(PROBE_MIDDLE)).await
    }
}

/// 含一条故意失败迁移的链：成功 → 失败 → 不该被执行。
struct BrokenMigrator;

#[async_trait::async_trait]
impl MigratorTrait for BrokenMigrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![
            Box::new(CreateFirstProbe),
            Box::new(DeliberatelyFailing),
            Box::new(CreateLastProbe),
        ]
    }
}

/// 把失败那条改对之后的同一条链。
struct RepairedMigrator;

#[async_trait::async_trait]
impl MigratorTrait for RepairedMigrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![
            Box::new(CreateFirstProbe),
            Box::new(FixedMiddle),
            Box::new(CreateLastProbe),
        ]
    }
}

// ---- 辅助 ----

async fn fresh_db() -> DatabaseConnection {
    Database::connect("sqlite::memory:")
        .await
        .expect("应能连上内存库")
}

async fn table_exists(db: &DatabaseConnection, name: &str) -> bool {
    let rows = db
        .query_all(Statement::from_string(
            DatabaseBackend::Sqlite,
            format!("SELECT name FROM sqlite_master WHERE type='table' AND name='{name}'"),
        ))
        .await
        .expect("应能查询 sqlite_master");
    !rows.is_empty()
}

async fn applied_versions(db: &DatabaseConnection) -> Vec<String> {
    let rows = db
        .query_all(Statement::from_string(
            DatabaseBackend::Sqlite,
            "SELECT version FROM seaql_migrations".to_owned(),
        ))
        .await
        .expect("应能读取迁移版本表");
    rows.iter()
        .map(|row| row.try_get::<String>("", "version").unwrap_or_default())
        .collect()
}

// ---- Scenario: 某步迁移出错 ----

#[tokio::test]
async fn a_failing_migration_aborts_the_chain_and_names_itself() {
    let db = fresh_db().await;

    let err = migrate_with::<BrokenMigrator>(&db)
        .await
        .expect_err("迁移链应失败");

    // 1) 错误里点名了失败的版本。
    match &err {
        Error::Migration { version, .. } => {
            assert_eq!(version, FAILING_VERSION, "错误应指出失败的迁移版本");
        }
        other => panic!("期望 Error::Migration，实际 {other:?}"),
    }
    assert!(
        err.to_string().contains(FAILING_VERSION),
        "错误文本里应能看到失败版本: {err}"
    );

    // 2) 失败之前的迁移保持已应用。
    assert!(
        table_exists(&db, PROBE_FIRST).await,
        "失败之前的迁移应已完成"
    );

    // 3) 失败之后的迁移没有被执行。
    assert!(
        !table_exists(&db, PROBE_LAST).await,
        "失败之后的迁移不应被执行"
    );

    // 4) 数据库保持可启动：连得上、查得动，版本表如实反映进度。
    assert!(db.ping().await.is_ok(), "迁移失败后数据库应仍然可用");
    let applied = applied_versions(&db).await;
    assert!(applied.contains(&FIRST_VERSION.to_owned()));
    assert!(
        !applied.contains(&FAILING_VERSION.to_owned()),
        "失败的迁移不应被记为已应用"
    );
    assert!(
        !applied.contains(&LAST_VERSION.to_owned()),
        "未执行的迁移不应被记为已应用"
    );
}

#[tokio::test]
async fn repairing_the_failing_migration_lets_the_chain_finish() {
    let db = fresh_db().await;

    migrate_with::<BrokenMigrator>(&db)
        .await
        .expect_err("首次应失败");

    // 把失败那条改对之后重跑：应从断点继续，把剩下的跑完。
    migrate_with::<RepairedMigrator>(&db)
        .await
        .expect("修复后应能跑完");

    assert!(table_exists(&db, PROBE_FIRST).await);
    assert!(
        table_exists(&db, PROBE_MIDDLE).await,
        "修复后的那条迁移应被执行"
    );
    assert!(
        table_exists(&db, PROBE_LAST).await,
        "其后原本被中止的迁移应继续执行"
    );

    // 再跑一次仍应是幂等的。
    migrate_with::<RepairedMigrator>(&db)
        .await
        .expect("已全部应用后再跑不应报错");
}

#[tokio::test]
async fn the_production_migrator_runs_clean() {
    // 生产入口 `migrate` 走的是同一个包装，这里确认它在正常路径上不出岔子。
    let db = fresh_db().await;

    migrate(&db).await.expect("生产迁移链应成功");
    assert!(table_exists(&db, "acmecast_cert").await, "核心表应已建好");

    migrate(&db).await.expect("重复调用不应报错");
}
