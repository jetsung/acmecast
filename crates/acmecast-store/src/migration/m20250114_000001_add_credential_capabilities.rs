//! 给凭据表加 `capabilities`（能力标记）列。
//!
//! 同一云平台（腾讯云 / Cloudflare / 阿里云）的密钥可按使用场景拆分为
//! DNS 与部署两类权限，部分账号仅授予其中一项。该列以 JSON 数组文本存
//! `dns` / `deploy` 子集，存量行默认双能力，保证升级后旧下拉不因空能力被过滤。
//!
//! 加新迁移而非回头改初始建表：已应用 `m20250101` 的环境不会重放它，
//! 改它会让那些环境与新环境 schema 分叉。

use sea_orm_migration::prelude::*;

use crate::entity::credential;

/// 给凭据记录补 `capabilities` 列的迁移。
#[derive(Debug, DeriveMigrationName)]
pub struct Migration;

/// 存量与新建的默认能力集合：DNS 与部署均可用。
pub const DEFAULT_CAPABILITIES: &str = r#"["dns","deploy"]"#;

// 与其它迁移同理：`SchemaManager` 的生命周期只能按 SeaORM 的要求省略。
#[allow(elided_lifetimes_in_paths)]
#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // 三方言 `ALTER TABLE ... ADD COLUMN ... NOT NULL DEFAULT '...'` 均原生支持，
        // 存量行自动得默认双能力，读取端类型统一为 `String`（JSON 数组文本）。
        manager
            .alter_table(
                Table::alter()
                    .table(credential::Entity)
                    .add_column(
                        ColumnDef::new(credential::Column::Capabilities)
                            .text()
                            .not_null()
                            .default(DEFAULT_CAPABILITIES),
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
                    .drop_column(credential::Column::Capabilities)
                    .to_owned(),
            )
            .await
    }
}
