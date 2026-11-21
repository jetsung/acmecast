//! 新建全局调度表。
//!
//! 全局调度（`global-schedule` spec）：一份 cron 配置到点后按 `pipeline_id`
//! 升序串行运行全部启用的流水线。单例行，与流水线级调度表（`acmecast_schedule`，
//! 以 `pipeline_id` 外键唯一挂靠）分开建模——全局调度没有归属的流水线，
//! 共表会破坏外键与唯一约束的语义。
//!
//! 这是一条**新增迁移**而不是去改 `create_core_tables`：已经应用过的迁移
//! 不该再被修改，否则两边 schema 会分叉。

use sea_orm_migration::prelude::*;

use crate::entity::global_schedule;

/// 新建全局调度表的迁移。
#[derive(Debug, DeriveMigrationName)]
pub struct Migration;

// 与首个迁移同理：`SchemaManager` 的生命周期只能按 SeaORM 的要求省略。
#[allow(elided_lifetimes_in_paths)]
#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.create_table(create_global_schedule_table()).await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(global_schedule::Entity).to_owned())
            .await
    }
}

fn create_global_schedule_table() -> TableCreateStatement {
    Table::create()
        .table(global_schedule::Entity)
        .if_not_exists()
        .col(
            ColumnDef::new(global_schedule::Column::Id)
                .big_integer()
                .not_null()
                .auto_increment()
                .primary_key(),
        )
        .col(ColumnDef::new(global_schedule::Column::Cron).text())
        .col(
            ColumnDef::new(global_schedule::Column::Enabled)
                .boolean()
                .not_null()
                .default(false),
        )
        .col(
            ColumnDef::new(global_schedule::Column::CatchUp)
                .boolean()
                .not_null()
                .default(false),
        )
        .col(ColumnDef::new(global_schedule::Column::LastTriggeredAt).date_time())
        .col(ColumnDef::new(global_schedule::Column::NextTriggerAt).date_time())
        .col(
            ColumnDef::new(global_schedule::Column::UpdatedAt)
                .date_time()
                .not_null(),
        )
        .to_owned()
}
