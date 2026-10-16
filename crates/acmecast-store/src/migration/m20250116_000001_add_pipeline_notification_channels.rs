//! 给流水线表加 `notification_channels`（通知渠道凭据 ID 列表）列。
//!
//! 通知渠道从 `config.toml` 的 `[[notifications]]` 迁移到凭据体系后，
//! 流水线需要记住「运行结束把简讯投给哪些渠道」。该列以 JSON 数组文本存
//! 凭据 ID 列表，存量行默认空数组（不投递），保证升级后旧流水线行为不变。
//!
//! 加新迁移而非回头改初始建表：已应用 `m20250101` 的环境不会重放它，
//! 改它会让那些环境与新环境 schema 分叉。

use sea_orm_migration::prelude::*;

use crate::entity::pipeline;

/// 给流水线记录补 `notification_channels` 列的迁移。
#[derive(Debug, DeriveMigrationName)]
pub struct Migration;

/// 存量与新建的默认通知渠道集合：空数组。
pub const DEFAULT_NOTIFICATION_CHANNELS: &str = "[]";

// 与其它迁移同理：`SchemaManager` 的生命周期只能按 SeaORM 的要求省略。
#[allow(elided_lifetimes_in_paths)]
#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // 三方言 `ALTER TABLE ... ADD COLUMN ... NOT NULL DEFAULT '...'` 均原生支持，
        // 存量行自动得空数组，读取端类型统一为 `String`（JSON 数组文本）。
        manager
            .alter_table(
                Table::alter()
                    .table(pipeline::Entity)
                    .add_column(
                        ColumnDef::new(pipeline::Column::NotificationChannels)
                            .text()
                            .not_null()
                            .default(DEFAULT_NOTIFICATION_CHANNELS),
                    )
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(pipeline::Entity)
                    .drop_column(pipeline::Column::NotificationChannels)
                    .to_owned(),
            )
            .await
    }
}
