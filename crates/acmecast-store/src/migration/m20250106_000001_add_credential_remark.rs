//! 给凭据表加 `remark`（备注）列。
//!
//! 凭据记录此前只有名称与类型，同一平台多对密钥在流水线下拉里难以区分。
//! `remark` 是人类可读的用途/归属说明，非敏感、可为空，列表与详情 API 都返回。
//!
//! 加新迁移而非回头改初始建表：已应用 `m20250101` 的环境不会重放它，
//! 改它会让那些环境与新环境 schema 分叉。

use sea_orm_migration::prelude::*;

use crate::entity::credential;

/// 给凭据记录补 `remark` 列的迁移。
#[derive(Debug, DeriveMigrationName)]
pub struct Migration;

// 与其它迁移同理：`SchemaManager` 的生命周期只能按 SeaORM 的要求省略。
#[allow(elided_lifetimes_in_paths)]
#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // 三方言 `ALTER TABLE ... ADD COLUMN ... NOT NULL DEFAULT ''` 均原生支持，
        // 存量行自动填空串，读取端类型统一为 `String`，无需 `Option` 解包。
        manager
            .alter_table(
                Table::alter()
                    .table(credential::Entity)
                    .add_column(
                        ColumnDef::new(credential::Column::Remark)
                            .string_len(255)
                            .not_null()
                            .default(""),
                    )
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(credential::Entity)
                    .drop_column(credential::Column::Remark)
                    .to_owned(),
            )
            .await
    }
}
