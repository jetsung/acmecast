//! 证书四文件布局（对齐 acme.sh 三文件 + key）。
//!
//! 存量表中 `cert_pem_path` / `key_pem_path` 仍保留作兼容读取，新增 `cert_path` / `ca_path` / `fullchain_path` / `key_path` 四列
//! 对应 `acme.sh $CERT_PATH` / `$CA_CERT_PATH` / `$CERT_FULLCHAIN_PATH` / `$CERT_KEY_PATH`。

use sea_orm_migration::prelude::*;

use crate::entity::cert;

/// 证书四文件布局迁移。
#[derive(Debug, DeriveMigrationName)]
pub struct Migration;

#[allow(elided_lifetimes_in_paths)]
#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        for col in [
            cert::Column::CertPath,
            cert::Column::CaPath,
            cert::Column::FullchainPath,
            cert::Column::KeyPath,
        ] {
            manager
                .alter_table(
                    Table::alter()
                        .table(cert::Entity)
                        .add_column(ColumnDef::new(col).text())
                        .to_owned(),
                )
                .await?;
        }
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        for col in [
            cert::Column::CertPath,
            cert::Column::CaPath,
            cert::Column::FullchainPath,
            cert::Column::KeyPath,
        ] {
            manager
                .alter_table(
                    Table::alter()
                        .table(cert::Entity)
                        .drop_column(col)
                        .to_owned(),
                )
                .await?;
        }
        Ok(())
    }
}
