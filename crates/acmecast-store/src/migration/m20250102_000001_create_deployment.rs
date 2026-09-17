//! 新建部署记录表。
//!
//! 幂等部署（8.5）需要记住「目标上已经部署的是哪一份证书」，部署历史（8.7）也需要按目标查询——两者的数据来源相同，因此共用这一张表。
//!
//! 这是一条**新增迁移**而不是去改 `create_core_tables`：已经应用过的迁移不该再被修改，否则已经跑过迁移的环境不会重放它，新环境却会，两边 schema 就此分叉。

use sea_orm_migration::prelude::*;

use crate::entity::deployment;

/// 新建部署记录表的迁移。
#[derive(Debug, DeriveMigrationName)]
pub struct Migration;

/// 文本列长度。
const TEXT_LEN: u32 = 512;

// 与首个迁移同理：`SchemaManager` 的生命周期只能按 SeaORM 的要求省略
// （写成 `&SchemaManager<'_>` 会触发 E0195）。
#[allow(elided_lifetimes_in_paths)]
#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.create_table(create_deployment_table()).await?;

        // 幂等判断与历史查询都按「目标 + 时间」定位，因此建这个复合索引。
        manager
            .create_index(
                Index::create()
                    .name("idx_deployment_target_time")
                    .table(deployment::Entity)
                    .col(deployment::Column::TargetType)
                    .col(deployment::Column::TargetKey)
                    .col(deployment::Column::DeployedAt)
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(deployment::Entity).to_owned())
            .await
    }
}

fn create_deployment_table() -> TableCreateStatement {
    Table::create()
        .table(deployment::Entity)
        .if_not_exists()
        .col(
            ColumnDef::new(deployment::Column::Id)
                .big_integer()
                .not_null()
                .auto_increment()
                .primary_key(),
        )
        // 复合索引（target_type + target_key + deployed_at）在 utf8mb4 下
        // 每字符 4 字节，两列各 512 会超过 MySQL 3072 字节的索引上限；
        // 实际内容最长是 "sha256:" + 44 字符的 base64url，256 绰绰有余。
        .col(
            ColumnDef::new(deployment::Column::TargetType)
                .string_len(256)
                .not_null(),
        )
        .col(
            ColumnDef::new(deployment::Column::TargetKey)
                .string_len(256)
                .not_null(),
        )
        .col(
            ColumnDef::new(deployment::Column::Fingerprint)
                .string_len(TEXT_LEN)
                .not_null(),
        )
        .col(
            ColumnDef::new(deployment::Column::SkippedWrite)
                .boolean()
                .not_null()
                .default(false),
        )
        .col(
            ColumnDef::new(deployment::Column::DeployedAt)
                .date_time()
                .not_null(),
        )
        .to_owned()
}
