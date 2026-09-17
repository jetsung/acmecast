//! 给部署记录补上「写入路径」与「重载输出」两列。
//!
//! spec 要求每次部署记录目标、时间、**写入路径**、指纹、是否跳过与**重载命令执行结果**，
//! 而 `m20250102` 建表时只考虑了幂等判断所需的最小集合。
//!
//! 加新迁移而不是回头改那张表的定义：已经应用过 `m20250102` 的环境不会重放它，
//! 改它会让那些环境与新环境 schema 分叉。

use sea_orm_migration::prelude::*;

use crate::entity::deployment;

/// 给部署记录补列的迁移。
#[derive(Debug, DeriveMigrationName)]
pub struct Migration;

// 与其它迁移同理：`SchemaManager` 的生命周期只能按 SeaORM 的要求省略。
#[allow(elided_lifetimes_in_paths)]
#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        use sea_orm::DatabaseBackend;

        // SQLite 的一条 ALTER TABLE 只能做一个动作，两列拆成两条语句。
        //
        // 实体侧是 `Json`：PG 的解码严格要求 jsonb，SQLite/MySQL 的 JSON
        // 列也能承载字符串内容，因此统一用 JSON 列类型而不是 TEXT（TEXT
        // 在 PG 上无法按 Json 解码）。默认值 `[]` 是给「加列时已存在的行」
        // 兜底——但 MySQL/MariaDB 禁止 JSON 列带 DEFAULT，只能分支处理：
        // MySQL 上靠 NOT NULL 的隐式默认与实体读取的 `unwrap_or_default`
        // 容错（数据库升级窗口内不会写入部署记录）。
        let backend = manager.get_database_backend();
        let mut paths_column = ColumnDef::new(deployment::Column::Paths)
            .json()
            .not_null()
            .to_owned();
        if backend != DatabaseBackend::MySql {
            paths_column.default("[]");
        }

        manager
            .alter_table(
                Table::alter()
                    .table(deployment::Entity)
                    .add_column(paths_column)
                    .to_owned(),
            )
            .await?;

        manager
            .alter_table(
                Table::alter()
                    .table(deployment::Entity)
                    .add_column(ColumnDef::new(deployment::Column::ReloadOutput).text())
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(deployment::Entity)
                    .drop_column(deployment::Column::ReloadOutput)
                    .to_owned(),
            )
            .await?;

        manager
            .alter_table(
                Table::alter()
                    .table(deployment::Entity)
                    .drop_column(deployment::Column::Paths)
                    .to_owned(),
            )
            .await
    }
}
