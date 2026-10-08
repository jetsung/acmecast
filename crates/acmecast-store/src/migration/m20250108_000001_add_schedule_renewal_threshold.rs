//! 给调度表加 `renewal_threshold_days` 列（调度级续期阈值，1-90 可空）。

use sea_orm_migration::prelude::*;

use crate::entity::schedule;

/// 调度级续期阈值迁移。
#[derive(Debug, DeriveMigrationName)]
pub struct Migration;

#[allow(elided_lifetimes_in_paths)]
#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(schedule::Entity)
                    .add_column(ColumnDef::new(schedule::Column::RenewalThresholdDays).small_integer())
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(schedule::Entity)
                    .drop_column(schedule::Column::RenewalThresholdDays)
                    .to_owned(),
            )
            .await
    }
}
