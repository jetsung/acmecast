//! 把 PostgreSQL 上所有「无时区」时间戳列转换为 `timestamptz`。
//!
//! 实体里 `chrono::DateTime<Utc>` 在 PG 侧要求 `timestamptz`，而
//! sea-query 的 `date_time()` 在 PG 生成 `timestamp without time zone`
//! （MySQL 生成 DATETIME、SQLite 生成 TIMESTAMP，均无碍）。这条迁移
//! 只对 PG 生效：把库里全部无时区时间戳列就地转换，动态发现、逐列
//! ALTER，对 MySQL / SQLite 是空操作。
//!
//! `timestamptz` 存的仍是 UTC 时刻，且转换不移动时间点——把既有数据
//! 从「无时区约定为 UTC」升级为「显式带时区」，不丢信息。

use sea_orm::DatabaseBackend;
use sea_orm_migration::prelude::*;

/// PostgreSQL 时间戳列的时区修正迁移。
#[derive(Debug, DeriveMigrationName)]
pub struct Migration;

// 与其它迁移同理：`SchemaManager` 的生命周期只能按 SeaORM 的要求省略。
#[allow(elided_lifetimes_in_paths)]
#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if manager.get_database_backend() != DatabaseBackend::Postgres {
            return Ok(());
        }

        let connection = manager.get_connection();
        let rows = connection
            .query_all(sea_orm::Statement::from_string(
                DatabaseBackend::Postgres,
                "SELECT table_name, column_name FROM information_schema.columns \
                 WHERE table_schema = current_schema() \
                   AND data_type = 'timestamp without time zone'",
            ))
            .await?;

        for row in rows {
            let table: String = row.try_get("", "table_name")?;
            let column: String = row.try_get("", "column_name")?;
            // 列名来自 information_schema，不存在外部输入；仍用带引号的
            // 标识符拼接，避免任何大小写折叠或保留字意外。
            let alter =
                format!("ALTER TABLE \"{table}\" ALTER COLUMN \"{column}\" TYPE timestamptz");
            connection
                .execute_unprepared(&alter)
                .await
                .map_err(|error| DbErr::Custom(format!("转换 {table}.{column} 失败: {error}")))?;
        }

        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        // 前向修正没有回退意义：回到「无时区」只会重新引入歧义。
        Ok(())
    }
}
