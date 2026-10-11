//! 给证书表加 `key_algorithm`（私钥算法）列。
//!
//! 列表页需要展示每本证书的加密算法（RSA / ECDSA P-256 / Ed25519 等）；
//! 算法在证书记录入库时从私钥检测得出，历史数据保持 NULL，
//! 由服务启动时的回填任务读取 `key_pem_path` 补齐。

use sea_orm_migration::prelude::*;

use crate::entity::cert;

/// 给证书记录补 `key_algorithm` 列的迁移。
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
                    .add_column(ColumnDef::new(cert::Column::KeyAlgorithm).text())
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(cert::Entity)
                    .drop_column(cert::Column::KeyAlgorithm)
                    .to_owned(),
            )
            .await
    }
}
