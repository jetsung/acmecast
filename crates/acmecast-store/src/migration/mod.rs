//! 版本化、幂等的数据库迁移。
//!
//! 迁移由一份代码定义，在需要方言差异的地方按 `DatabaseBackend` 分支，
//! 由 SeaQuery 负责把同一份定义翻译成 SQLite / MySQL / PostgreSQL 各自的 DDL。
//!
//! 这样做的前提是三方言差异是有限的且可枚举：自增主键、布尔表示、
//! 时间类型、`TEXT` 长度上限。把这几点显式列出（见模块内注释），
//! 比维护三份会漂移的 SQL 脚本更可靠。

pub mod m20250101_000001_create_core_tables;
pub mod m20250102_000001_create_deployment;
pub mod m20250103_000001_add_deployment_details;
pub mod m20250104_000001_schedule_renewal_and_trigger_log;
pub mod m20250105_000001_pg_timestamptz;

use sea_orm::DatabaseConnection;
use sea_orm_migration::prelude::*;

use crate::error::{Error, Result};

/// 迁移器：汇集所有 migrate 步骤，供服务端启动时调用。
#[derive(Debug, DeriveMigrationName)]
pub struct Migrator;

pub use m20250101_000001_create_core_tables::Migration as CreateCoreTables;
pub use m20250102_000001_create_deployment::Migration as CreateDeployment;
pub use m20250103_000001_add_deployment_details::Migration as AddDeploymentDetails;
pub use m20250104_000001_schedule_renewal_and_trigger_log::Migration as ScheduleRenewalAndTriggerLog;
pub use m20250105_000001_pg_timestamptz::Migration as PgTimestamptz;

#[async_trait::async_trait]
impl MigratorTrait for Migrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![
            Box::new(CreateCoreTables),
            Box::new(CreateDeployment),
            Box::new(AddDeploymentDetails),
            Box::new(ScheduleRenewalAndTriggerLog),
            Box::new(PgTimestamptz),
        ]
    }
}

/// 执行本 crate 定义的全部迁移，失败时指明失败的版本。
///
/// 服务端启动时应当调用这个函数，而不是直接调 [`MigratorTrait::up`]——
/// 后者抛出的错误里没有版本信息，排障时只能去翻日志。
pub async fn migrate(db: &DatabaseConnection) -> Result<()> {
    migrate_with::<Migrator>(db).await
}

/// 与 [`migrate`] 相同，但迁移链由 `M` 决定。
///
/// 生产代码一律用 [`migrate`]。这个泛型入口存在的意义是让测试能注入一条
/// 故意失败的迁移，从而验证「失败即中止后续」与「错误中指明失败版本」。
pub async fn migrate_with<M>(db: &DatabaseConnection) -> Result<()>
where
    M: MigratorTrait,
{
    // 先记下计划中的全部版本：失败后要靠它与已应用列表比对出是哪一条挂了。
    let planned: Vec<String> = M::migrations()
        .iter()
        .map(|migration| migration.name().to_owned())
        .collect();

    if let Err(source) = M::up(db, None).await {
        return Err(Error::Migration {
            version: failed_version::<M>(db, &planned).await,
            source,
        });
    }
    Ok(())
}

/// 无法从版本表确定失败版本时的占位说明。
///
/// 两种情况会落到这里：查询版本表本身失败（连不上库、建不了元数据表），
/// 或所有计划中的迁移都已应用却仍然失败。两者的共同点是「失败不在某条具体迁移里」，
/// 此时真正的原因由错误携带的底层 `DbErr` 给出。
const UNKNOWN_VERSION: &str = "<未确定>";

/// 推导失败的迁移版本：计划中第一个未被记录为已应用的版本。
///
/// SeaORM 在中止时**不会**给失败的迁移写版本记录（见 `exec_up`），
/// 因此「第一个未应用的」就是失败的那一条。
///
/// 版本表读不出来时返回 [`UNKNOWN_VERSION`]，而不是把它当成空表——
/// 空表会让所有计划中的版本都显得「未应用」，从而把失败错报成第一条迁移。
async fn failed_version<M>(db: &DatabaseConnection, planned: &[String]) -> String
where
    M: MigratorTrait,
{
    let Ok(applied) = M::get_applied_migrations(db).await else {
        return UNKNOWN_VERSION.to_owned();
    };
    let applied: Vec<String> = applied
        .iter()
        .map(|migration| migration.name().to_owned())
        .collect();

    planned
        .iter()
        .find(|name| !applied.contains(name))
        .cloned()
        .unwrap_or_else(|| UNKNOWN_VERSION.to_owned())
}
