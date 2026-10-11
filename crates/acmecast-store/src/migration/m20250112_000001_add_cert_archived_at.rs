//! 给证书表加 `archived_at`（归档时间）列。
//!
//! 归档是把活跃目录 `certs/ssl/` 的材料移至 `certs/archived/<指纹>/` 的本地
//! 操作，不触及 CA。列表页需要以持久化状态判定「已归档」来禁用归档按钮，
//! 而不能仅依赖 `cert_pem_path` 前缀（历史手动上传路径不规范，且前端刷新后
//! 需要稳定可查询的事实来源）。`archived_at` 为空表示未归档。

use sea_orm_migration::prelude::*;

use crate::entity::cert;

/// 给证书记录补 `archived_at` 列的迁移。
#[derive(Debug, DeriveMigrationName)]
pub struct Migration;

#[allow(elided_lifetimes_in_paths)]
#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(cert::Entity)
                    .add_column(ColumnDef::new(cert::Column::ArchivedAt).timestamp_with_time_zone())
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(cert::Entity)
                    .drop_column(cert::Column::ArchivedAt)
                    .to_owned(),
            )
            .await
    }
}
