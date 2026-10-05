//! 初始 schema：建立八张核心表。
//!
//! 三方言差异集中处理说明：
//!
//! | 关注点 | SQLite | MySQL | PostgreSQL |
//! | --- | --- | --- | --- |
//! | 自增主键 | `INTEGER PRIMARY KEY AUTOINCREMENT` | `BIGINT AUTO_INCREMENT` | `BIGSERIAL` |
//! | 布尔 | `INTEGER` 0/1 | `TINYINT(1)` | `BOOLEAN` |
//! | 时间 | `TEXT`（RFC 3339） | `DATETIME` | `TIMESTAMP` |
//! | JSON | `TEXT` | `JSON` | `JSON` |
//! | 默认值 | 由 SeaQuery 内联到 DDL | 同 | 同 |
//!
//! 上述差异均由 SeaQuery 按 backend 自动翻译，因此迁移代码只有一份。
//! 时间统一按 UTC 存储，读取端使用 `chrono::DateTime<Utc>` 解读。
use sea_orm::sea_query::{Alias, ColumnDef, ForeignKey, ForeignKeyAction, Index, Table};
use sea_orm_migration::prelude::*;

use crate::entity::{
    cert, credential, history, history_log, pipeline, pipeline_step, schedule, storage,
};

/// 初始建表迁移。
#[derive(Debug, DeriveMigrationName)]
pub struct Migration;

// SeaORM 的 `MigrationTrait` 签名在 impl 侧不接受显式生命周期参数
// （`&SchemaManager<'_>` 会触发 E0195 early/late-bound 不匹配），
// 因此此处只能按库的要求省略生命周期。
#[allow(elided_lifetimes_in_paths)]
#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.create_table(create_pipeline_table()).await?;
        manager.create_table(create_pipeline_step_table()).await?;
        manager.create_table(create_history_table()).await?;
        manager.create_table(create_history_log_table()).await?;
        manager.create_table(create_storage_table()).await?;
        manager.create_table(create_credential_table()).await?;
        manager.create_table(create_cert_table()).await?;
        manager.create_table(create_schedule_table()).await?;

        // 所有外键都在各自的 `CREATE TABLE` 语句里内联声明。
        // 不能在这里事后调 `create_foreign_key` 补一个 ALTER TABLE 外键：
        // **SQLite 不支持对已存在的表添加外键约束**，那条路径会直接 panic。
        create_indexes(manager).await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        for table in [
            "acmecast_pipeline_step",
            "acmecast_history_log",
            "acmecast_history",
            "acmecast_storage",
            "acmecast_schedule",
            "acmecast_cert",
            "acmecast_credential",
            "acmecast_pipeline",
        ] {
            manager
                .drop_table(Table::drop().table(Alias::new(table)).to_owned())
                .await?;
        }
        Ok(())
    }
}

/// 字符串列的默认长度。MySQL 的 InnoDB 单列索引有长度上限，
/// 统一用 255 可避免出现索引超长的方言差异。
const TEXT_LEN: u32 = 255;

fn create_pipeline_table() -> TableCreateStatement {
    Table::create()
        .table(pipeline::Entity)
        .if_not_exists()
        .col(
            ColumnDef::new(pipeline::Column::Id)
                .big_integer()
                .not_null()
                .auto_increment()
                .primary_key(),
        )
        .col(
            ColumnDef::new(pipeline::Column::Name)
                .string_len(TEXT_LEN)
                .not_null(),
        )
        .col(
            ColumnDef::new(pipeline::Column::Enabled)
                .boolean()
                .not_null()
                .default(true),
        )
        .col(ColumnDef::new(pipeline::Column::Description).text())
        .col(
            ColumnDef::new(pipeline::Column::CreatedAt)
                .date_time()
                .not_null(),
        )
        .col(
            ColumnDef::new(pipeline::Column::UpdatedAt)
                .date_time()
                .not_null(),
        )
        .to_owned()
}

fn create_pipeline_step_table() -> TableCreateStatement {
    Table::create()
        .table(pipeline_step::Entity)
        .if_not_exists()
        .col(
            ColumnDef::new(pipeline_step::Column::Id)
                .big_integer()
                .not_null()
                .auto_increment()
                .primary_key(),
        )
        .col(
            ColumnDef::new(pipeline_step::Column::PipelineId)
                .big_integer()
                .not_null(),
        )
        .col(
            ColumnDef::new(pipeline_step::Column::OrderIndex)
                .integer()
                .not_null(),
        )
        .col(
            ColumnDef::new(pipeline_step::Column::TypeId)
                .string_len(TEXT_LEN)
                .not_null(),
        )
        .col(
            ColumnDef::new(pipeline_step::Column::Input)
                .json()
                .not_null(),
        )
        .col(
            ColumnDef::new(pipeline_step::Column::Enabled)
                .boolean()
                .not_null()
                .default(true),
        )
        .foreign_key(
            ForeignKey::create()
                .name("fk_step_pipeline")
                .from(pipeline_step::Entity, pipeline_step::Column::PipelineId)
                .to(pipeline::Entity, pipeline::Column::Id)
                .on_delete(ForeignKeyAction::Cascade)
                .on_update(ForeignKeyAction::Cascade),
        )
        .to_owned()
}

fn create_history_table() -> TableCreateStatement {
    Table::create()
        .table(history::Entity)
        .if_not_exists()
        .col(
            ColumnDef::new(history::Column::Id)
                .big_integer()
                .not_null()
                .auto_increment()
                .primary_key(),
        )
        .col(
            ColumnDef::new(history::Column::PipelineId)
                .big_integer()
                .not_null(),
        )
        .col(
            ColumnDef::new(history::Column::TriggerSource)
                .string_len(32)
                .not_null(),
        )
        .col(
            ColumnDef::new(history::Column::Status)
                .string_len(32)
                .not_null(),
        )
        .col(
            ColumnDef::new(history::Column::StartedAt)
                .date_time()
                .not_null(),
        )
        .col(ColumnDef::new(history::Column::FinishedAt).date_time())
        .col(ColumnDef::new(history::Column::ErrorMessage).text())
        .foreign_key(
            ForeignKey::create()
                .name("fk_history_pipeline")
                .from(history::Entity, history::Column::PipelineId)
                .to(pipeline::Entity, pipeline::Column::Id)
                .on_delete(ForeignKeyAction::Cascade)
                .on_update(ForeignKeyAction::Cascade),
        )
        .to_owned()
}

fn create_history_log_table() -> TableCreateStatement {
    Table::create()
        .table(history_log::Entity)
        .if_not_exists()
        .col(
            ColumnDef::new(history_log::Column::Id)
                .big_integer()
                .not_null()
                .auto_increment()
                .primary_key(),
        )
        .col(
            ColumnDef::new(history_log::Column::HistoryId)
                .big_integer()
                .not_null(),
        )
        .col(
            ColumnDef::new(history_log::Column::StepIndex)
                .integer()
                .not_null(),
        )
        .col(
            ColumnDef::new(history_log::Column::Level)
                .string_len(16)
                .not_null(),
        )
        .col(
            ColumnDef::new(history_log::Column::Message)
                .text()
                .not_null(),
        )
        .col(
            ColumnDef::new(history_log::Column::CreatedAt)
                .date_time()
                .not_null(),
        )
        .foreign_key(
            ForeignKey::create()
                .name("fk_log_history")
                .from(history_log::Entity, history_log::Column::HistoryId)
                .to(history::Entity, history::Column::Id)
                .on_delete(ForeignKeyAction::Cascade)
                .on_update(ForeignKeyAction::Cascade),
        )
        .to_owned()
}

fn create_storage_table() -> TableCreateStatement {
    Table::create()
        .table(storage::Entity)
        .if_not_exists()
        .col(
            ColumnDef::new(storage::Column::Id)
                .big_integer()
                .not_null()
                .auto_increment()
                .primary_key(),
        )
        .col(
            ColumnDef::new(storage::Column::PipelineId)
                .big_integer()
                .not_null(),
        )
        // `store_key` 而非 `key`：`key` 是 MySQL 保留字。
        .col(
            ColumnDef::new(storage::Column::StoreKey)
                .string_len(TEXT_LEN)
                .not_null(),
        )
        .col(
            ColumnDef::new(storage::Column::StoreValue)
                .text()
                .not_null(),
        )
        .col(
            ColumnDef::new(storage::Column::UpdatedAt)
                .date_time()
                .not_null(),
        )
        .foreign_key(
            ForeignKey::create()
                .name("fk_storage_pipeline")
                .from(storage::Entity, storage::Column::PipelineId)
                .to(pipeline::Entity, pipeline::Column::Id)
                .on_delete(ForeignKeyAction::Cascade)
                .on_update(ForeignKeyAction::Cascade),
        )
        .to_owned()
}

fn create_credential_table() -> TableCreateStatement {
    Table::create()
        .table(credential::Entity)
        .if_not_exists()
        .col(
            ColumnDef::new(credential::Column::Id)
                .big_integer()
                .not_null()
                .auto_increment()
                .primary_key(),
        )
        .col(
            ColumnDef::new(credential::Column::Name)
                .string_len(TEXT_LEN)
                .not_null(),
        )
        .col(
            ColumnDef::new(credential::Column::TypeId)
                .string_len(TEXT_LEN)
                .not_null(),
        )
        // 唯一的密文载体：整包加密后的 JSON 文本，永不明文。
        .col(
            ColumnDef::new(credential::Column::EncryptedFields)
                .text()
                .not_null(),
        )
        .col(
            ColumnDef::new(credential::Column::CreatedAt)
                .date_time()
                .not_null(),
        )
        .col(
            ColumnDef::new(credential::Column::UpdatedAt)
                .date_time()
                .not_null(),
        )
        .to_owned()
}

fn create_cert_table() -> TableCreateStatement {
    Table::create()
        .table(cert::Entity)
        .if_not_exists()
        .col(
            ColumnDef::new(cert::Column::Id)
                .big_integer()
                .not_null()
                .auto_increment()
                .primary_key(),
        )
        // 域名集合列上有普通索引；MySQL 的 TEXT 列无法直接建索引，用
        // VARCHAR(768)（utf8mb4 索引长度上限内）。域名集合经规范化去重，
        // 768 字符足以覆盖常规的多 SAN 证书。
        .col(
            ColumnDef::new(cert::Column::Domains)
                .string_len(768)
                .not_null(),
        )
        .col(ColumnDef::new(cert::Column::CertPemPath).text().not_null())
        .col(ColumnDef::new(cert::Column::KeyPemPath).text().not_null())
        .col(
            ColumnDef::new(cert::Column::Fingerprint)
                .string_len(TEXT_LEN)
                .not_null()
                .unique_key(),
        )
        .col(ColumnDef::new(cert::Column::Issuer).string_len(TEXT_LEN))
        .col(
            ColumnDef::new(cert::Column::NotBefore)
                .date_time()
                .not_null(),
        )
        .col(
            ColumnDef::new(cert::Column::NotAfter)
                .date_time()
                .not_null(),
        )
        // 关键列：签发账号直接落在证书行上。
        //
        // 这是本实现相对 certd 的数据模型修正（见 design 决策 6）：
        // certd 在吊销时要通过 `resolveRevokeParams` 反查流水线才能拿到 ACME 账号，
        // 本实现从建表起就把它持久化在此，吊销时直接读表。
        .col(ColumnDef::new(cert::Column::AcmeAccountAccessId).big_integer())
        .col(ColumnDef::new(cert::Column::RevokedAt).date_time())
        .col(
            ColumnDef::new(cert::Column::CreatedAt)
                .date_time()
                .not_null(),
        )
        .col(
            ColumnDef::new(cert::Column::UpdatedAt)
                .date_time()
                .not_null(),
        )
        // 外键必须内联在建表语句里：**SQLite 不支持对已存在的表 ALTER 添加外键**，
        // 事后调用 `create_foreign_key` 在 SQLite 上会直接 panic。
        //
        // 用 `SetNull` 而非 `Cascade`：删除凭据时不清掉证书，
        // 由应用层在删除前做引用检查（spec 5.7 要求拒绝删除并列出引用者）。
        .foreign_key(
            ForeignKey::create()
                .name("fk_cert_acme_account")
                .from(cert::Entity, cert::Column::AcmeAccountAccessId)
                .to(credential::Entity, credential::Column::Id)
                .on_delete(ForeignKeyAction::SetNull)
                .on_update(ForeignKeyAction::Cascade),
        )
        .to_owned()
}

fn create_schedule_table() -> TableCreateStatement {
    Table::create()
        .table(schedule::Entity)
        .if_not_exists()
        .col(
            ColumnDef::new(schedule::Column::Id)
                .big_integer()
                .not_null()
                .auto_increment()
                .primary_key(),
        )
        .col(
            ColumnDef::new(schedule::Column::PipelineId)
                .big_integer()
                .not_null()
                .unique_key(),
        )
        .col(ColumnDef::new(schedule::Column::Cron).string_len(128))
        .col(
            ColumnDef::new(schedule::Column::Enabled)
                .boolean()
                .not_null()
                .default(true),
        )
        // spec 9.6：默认跳过补跑，仅在显式配置时才追赶最近一次。
        .col(
            ColumnDef::new(schedule::Column::CatchUp)
                .boolean()
                .not_null()
                .default(false),
        )
        .col(ColumnDef::new(schedule::Column::LastTriggeredAt).date_time())
        .col(ColumnDef::new(schedule::Column::NextTriggerAt).date_time())
        .col(
            ColumnDef::new(schedule::Column::UpdatedAt)
                .date_time()
                .not_null(),
        )
        .foreign_key(
            ForeignKey::create()
                .name("fk_schedule_pipeline")
                .from(schedule::Entity, schedule::Column::PipelineId)
                .to(pipeline::Entity, pipeline::Column::Id)
                .on_delete(ForeignKeyAction::Cascade)
                .on_update(ForeignKeyAction::Cascade),
        )
        .to_owned()
}

async fn create_indexes(manager: &SchemaManager<'_>) -> Result<(), DbErr> {
    // 步骤表：按流水线 + 顺序检索是最常见路径。
    manager
        .create_index(
            Index::create()
                .name("idx_step_pipeline_order")
                .table(pipeline_step::Entity)
                .col(pipeline_step::Column::PipelineId)
                .col(pipeline_step::Column::OrderIndex)
                .to_owned(),
        )
        .await?;

    // 键值存储：同一流水线内键唯一，也是查询主键。
    manager
        .create_index(
            Index::create()
                .name("idx_storage_pipeline_key")
                .table(storage::Entity)
                .col(storage::Column::PipelineId)
                .col(storage::Column::StoreKey)
                .unique()
                .to_owned(),
        )
        .await?;

    // 历史：按流水线倒序分页。
    manager
        .create_index(
            Index::create()
                .name("idx_history_pipeline_started")
                .table(history::Entity)
                .col(history::Column::PipelineId)
                .col(history::Column::StartedAt)
                .to_owned(),
        )
        .await?;

    // 日志：按历史分页。
    manager
        .create_index(
            Index::create()
                .name("idx_log_history")
                .table(history_log::Entity)
                .col(history_log::Column::HistoryId)
                .col(history_log::Column::StepIndex)
                .to_owned(),
        )
        .await?;

    // 证书：按域名检索、按到期排序。
    manager
        .create_index(
            Index::create()
                .name("idx_cert_domains")
                .table(cert::Entity)
                .col(cert::Column::Domains)
                .to_owned(),
        )
        .await?;
    manager
        .create_index(
            Index::create()
                .name("idx_cert_not_after")
                .table(cert::Entity)
                .col(cert::Column::NotAfter)
                .to_owned(),
        )
        .await
}
#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::sea_query::{MysqlQueryBuilder, PostgresQueryBuilder, SqliteQueryBuilder};

    /// 把建表语句渲染成指定方言的 DDL 文本，用于断言方言差异。
    fn ddl_sqlite(stmt: &TableCreateStatement) -> String {
        stmt.to_string(SqliteQueryBuilder)
    }
    fn ddl_mysql(stmt: &TableCreateStatement) -> String {
        stmt.to_string(MysqlQueryBuilder)
    }
    fn ddl_postgres(stmt: &TableCreateStatement) -> String {
        stmt.to_string(PostgresQueryBuilder)
    }

    #[test]
    fn cert_table_carries_account_column() {
        // design 决策 6 的核心：证书行必须能直接找到签发账号，
        // 吊销时无需反查流水线。
        let rendered = ddl_sqlite(&create_cert_table());
        assert!(
            rendered.contains("acme_account_access_id"),
            "证书表缺少 acme_account_access_id: {rendered}"
        );
    }

    #[test]
    fn three_dialects_use_distinct_autoincrement_forms() {
        let stmt = create_pipeline_table();
        assert!(
            ddl_sqlite(&stmt).contains("AUTOINCREMENT"),
            "SQLite: {}",
            ddl_sqlite(&stmt)
        );
        assert!(
            ddl_mysql(&stmt).contains("AUTO_INCREMENT"),
            "MySQL: {}",
            ddl_mysql(&stmt)
        );
        assert!(
            ddl_postgres(&stmt).to_uppercase().contains("SERIAL"),
            "PostgreSQL: {}",
            ddl_postgres(&stmt)
        );
    }

    #[test]
    fn three_dialects_render_boolean_compatibly() {
        let stmt = create_pipeline_table();
        assert!(ddl_sqlite(&stmt).contains("\"enabled\""));
        assert!(ddl_mysql(&stmt).contains("`enabled`"), "MySQL 用反引号");
        assert!(ddl_postgres(&stmt).contains("\"enabled\""));

        // 布尔在三方言均为非空且都有默认值。
        for rendered in [ddl_sqlite(&stmt), ddl_mysql(&stmt), ddl_postgres(&stmt)] {
            assert!(
                rendered.to_uppercase().contains("DEFAULT"),
                "enabled 应有默认值: {rendered}"
            );
        }
    }

    #[test]
    fn catch_up_defaults_to_false() {
        let stmt = create_schedule_table();
        let sqlite = ddl_sqlite(&stmt).to_lowercase();
        let pg = ddl_postgres(&stmt).to_lowercase();
        let mysql = ddl_mysql(&stmt).to_lowercase();

        // spec 9.6：默认跳过补跑。
        assert!(!sqlite.contains("default 1"), "SQLite: {sqlite}");
        assert!(pg.contains("default false"), "PostgreSQL: {pg}");
        assert!(!mysql.contains("default 1"), "MySQL: {mysql}");
    }

    #[test]
    fn storage_uses_non_reserved_column_names() {
        let rendered = ddl_mysql(&create_storage_table());
        assert!(rendered.contains("store_key"), "MySQL: {rendered}");
        assert!(
            !rendered.contains("`key`"),
            "不得使用 MySQL 保留字 key 作列名: {rendered}"
        );
    }

    #[test]
    fn all_tables_are_prefixed() {
        for stmt in [
            create_pipeline_table(),
            create_cert_table(),
            create_schedule_table(),
            create_credential_table(),
            create_storage_table(),
            create_history_table(),
            create_history_log_table(),
            create_pipeline_step_table(),
        ] {
            let rendered = ddl_sqlite(&stmt);
            assert!(rendered.contains("acmecast_"), "表名应有前缀: {rendered}");
        }
    }

    #[test]
    fn cert_fingerprint_is_unique() {
        let rendered = ddl_sqlite(&create_cert_table()).to_lowercase();
        assert!(rendered.contains("unique"), "指纹应唯一: {rendered}");
    }

    #[test]
    fn json_column_renders_on_every_dialect() {
        let stmt = create_pipeline_step_table();
        for rendered in [ddl_sqlite(&stmt), ddl_mysql(&stmt), ddl_postgres(&stmt)] {
            assert!(
                rendered.to_lowercase().contains("json")
                    || rendered.to_lowercase().contains("text"),
                "JSON 列应在三方言上都有对应类型: {rendered}"
            );
        }
    }
}
