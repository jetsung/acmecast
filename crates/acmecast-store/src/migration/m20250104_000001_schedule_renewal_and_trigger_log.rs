//! 给调度表补「续期域名」列，并新建触发记录表。
//!
//! `renewal_domains` 把「证书关联的流水线」从隐式推断变成显式配置：到期扫描
//! 命中证书后按域名交集找到目标流水线，而不是去解析流水线步骤输入的结构。
//! 触发记录（`acmecast_trigger_log`）承载 spec 的「触发记录」要求——来源、
//! 时间、目标流水线，同时是续期触发去重的依据。
//!
//! 这是一条**新增迁移**而不是去改 `create_core_tables`：已经应用过的迁移
//! 不该再被修改，否则两边 schema 会分叉。

use sea_orm_migration::prelude::*;

use crate::entity::{pipeline, schedule, trigger_log};

/// 给调度表补列并新建触发记录表的迁移。
#[derive(Debug, DeriveMigrationName)]
pub struct Migration;

/// 文本列长度。
const TEXT_LEN: u32 = 512;

// 与首个迁移同理：`SchemaManager` 的生命周期只能按 SeaORM 的要求省略。
#[allow(elided_lifetimes_in_paths)]
#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // SQLite 的一条 ALTER TABLE 只能做一个动作。
        manager
            .alter_table(
                Table::alter()
                    .table(schedule::Entity)
                    .add_column(ColumnDef::new(schedule::Column::RenewalDomains).text())
                    .to_owned(),
            )
            .await?;

        manager.create_table(create_trigger_log_table()).await?;

        // 触发历史按「流水线 + 时间」倒序查询，建复合索引。
        manager
            .create_index(
                Index::create()
                    .name("idx_trigger_log_pipeline_time")
                    .table(trigger_log::Entity)
                    .col(trigger_log::Column::PipelineId)
                    .col(trigger_log::Column::TriggeredAt)
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(trigger_log::Entity).to_owned())
            .await?;

        manager
            .alter_table(
                Table::alter()
                    .table(schedule::Entity)
                    .drop_column(schedule::Column::RenewalDomains)
                    .to_owned(),
            )
            .await
    }
}

fn create_trigger_log_table() -> TableCreateStatement {
    Table::create()
        .table(trigger_log::Entity)
        .if_not_exists()
        .col(
            ColumnDef::new(trigger_log::Column::Id)
                .big_integer()
                .not_null()
                .auto_increment()
                .primary_key(),
        )
        .col(
            ColumnDef::new(trigger_log::Column::PipelineId)
                .big_integer()
                .not_null(),
        )
        .col(
            ColumnDef::new(trigger_log::Column::Source)
                .string_len(32)
                .not_null(),
        )
        .col(ColumnDef::new(trigger_log::Column::Detail).string_len(TEXT_LEN))
        .col(
            ColumnDef::new(trigger_log::Column::TriggeredAt)
                .date_time()
                .not_null(),
        )
        .foreign_key(
            ForeignKey::create()
                .name("fk_trigger_log_pipeline")
                .from(trigger_log::Entity, trigger_log::Column::PipelineId)
                .to(pipeline::Entity, pipeline::Column::Id)
                .on_delete(ForeignKeyAction::Cascade)
                .on_update(ForeignKeyAction::Cascade),
        )
        .to_owned()
}
