//! 给运行历史日志补上「结构化输出」列。
//!
//! 腾讯云 SSL 部署成功后，步骤日志需要结构化地携带云端证书 ID，
//! 而 `acmecast_history_log` 建表时只有纯文本 `message` 列，
//! 无从承载结构化产物。新增 `output` JSON 列（可空），让步骤日志
//! 可附带程序化可读的输出摘要。
//!
//! 加新迁移而不是回头改 `create_core_tables`：已经应用过那条迁移的
//! 环境不会重放它，改它会让那些环境与新环境 schema 分叉。

use sea_orm_migration::prelude::*;

use crate::entity::history_log;

/// 给运行历史日志补列的迁移。
#[derive(Debug, DeriveMigrationName)]
pub struct Migration;

// 与其它迁移同理：`SchemaManager` 的生命周期只能按 SeaORM 的要求省略
// （写成 `&SchemaManager<'_>` 会触发 E0195）。
#[allow(elided_lifetimes_in_paths)]
#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // 实体侧是 `Option<Json>`：PG 的解码严格要求 jsonb，SQLite/MySQL
        // 的 JSON 列也能承载字符串内容，因此统一用 JSON 列类型而不是
        // TEXT（TEXT 在 PG 上无法按 Json 解码）。列可空，存量行取 NULL，
        // 对未产生结构化输出的步骤日志自然缺省为空。
        //
        // MySQL/MariaDB 禁止 JSON 列带 DEFAULT，但本列可空，不需要默认值；
        // 加列时存量行自动得到 NULL，无需分支处理。
        manager
            .alter_table(
                Table::alter()
                    .table(history_log::Entity)
                    .add_column(ColumnDef::new(history_log::Column::Output).json().null())
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(history_log::Entity)
                    .drop_column(history_log::Column::Output)
                    .to_owned(),
            )
            .await
    }
}
