//! 给证书表加 `description`（描述）列。
//!
//! 列表页需要在 `/certificates` 展示可扫视的说明；`description` 是
//! 管理员对该证书记录的备注，非敏感、可为空，列表与详情 API 都返回。

use sea_orm_migration::prelude::*;

use crate::entity::cert;

/// 给证书记录补 `description` 列的迁移。
#[derive(Debug, DeriveMigrationName)]
pub struct Migration;

#[allow(elided_lifetimes_in_paths)]
#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // 可空 TEXT：历史数据保持 NULL，无需回填；新增行由仓储归一空白为 None。
        manager
            .alter_table(
                Table::alter()
                    .table(cert::Entity)
                    .add_column(ColumnDef::new(cert::Column::Description).text())
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(cert::Entity)
                    .drop_column(cert::Column::Description)
                    .to_owned(),
            )
            .await
    }
}
