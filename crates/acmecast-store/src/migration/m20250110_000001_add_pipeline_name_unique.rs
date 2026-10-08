//! 为流水线名称添加唯一索引。
//!
//! 流水线名称需全局唯一，便于在列表、调度与运维沟通中以名称定位对象。
//! 此前 `acmecast_pipeline.name` 无唯一约束，并发与复制路径可产生重名。
//! 新增唯一索引 `idx_pipeline_name_unique`，存量重名数据需先行修复。

use sea_orm_migration::prelude::*;

use crate::entity::pipeline;

/// 流水线名称唯一索引迁移。
#[derive(Debug, DeriveMigrationName)]
pub struct Migration;

#[allow(elided_lifetimes_in_paths)]
#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_index(
                Index::create()
                    .name("idx_pipeline_name_unique")
                    .table(pipeline::Entity)
                    .col(pipeline::Column::Name)
                    .unique()
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_index(
                Index::drop()
                    .name("idx_pipeline_name_unique")
                    .table(pipeline::Entity)
                    .to_owned(),
            )
            .await
    }
}
