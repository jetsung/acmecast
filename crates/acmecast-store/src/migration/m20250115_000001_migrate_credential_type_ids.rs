//! 凭据 type_id 统一重写：短名 → 分域前缀名。
//!
//! 凭据类型 id 从裸短名（`cloudflare` / `aliyun` / `tencent` / `tencent-eo`
//! / `ssh` / `s3` / `webdav`）改为带命名空间的分域形式（`cloud.*` / `dns.*`
//! / `deploy.*`），`acme.account` 本就带前缀，保持不动。
//!
//! 只改存量行的 `type_id` 列文本，不动表结构；`WHERE type_id = 旧值`
//! 保证幂等——重复执行不命中任何行，已是新值的行也不受影响。

use sea_orm::Statement;
use sea_orm_migration::prelude::*;

/// 旧短名 → 新分域名。顺序无关，映射间不存在前缀冲突。
const TYPE_ID_REWRITES: [(&str, &str); 7] = [
    ("cloudflare", "cloud.cloudflare"),
    ("aliyun", "cloud.aliyun"),
    ("tencent", "cloud.tencent"),
    ("tencent-eo", "dns.tencent-eo"),
    ("ssh", "deploy.ssh"),
    ("s3", "deploy.s3"),
    ("webdav", "deploy.webdav"),
];

/// 凭据 type_id 重写迁移。
#[derive(Debug, DeriveMigrationName)]
pub struct Migration;

// 与其它迁移同理：`SchemaManager` 的生命周期只能按 SeaORM 的要求省略。
#[allow(elided_lifetimes_in_paths)]
#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        for (old, new) in TYPE_ID_REWRITES {
            // SeaQuery 把同一份 UPDATE 翻译到 SQLite / MySQL / PostgreSQL；
            // 参数绑定避免旧值里的 `-` 之外还可能引入的引号问题。
            db.execute(
                Statement::from_sql_and_values(
                    db.get_database_backend(),
                    "UPDATE acmecast_credential SET type_id = ? WHERE type_id = ?",
                    [new.into(), old.into()],
                ),
            )
            .await?;
        }
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        for (old, new) in TYPE_ID_REWRITES {
            db.execute(
                Statement::from_sql_and_values(
                    db.get_database_backend(),
                    "UPDATE acmecast_credential SET type_id = ? WHERE type_id = ?",
                    [old.into(), new.into()],
                ),
            )
            .await?;
        }
        Ok(())
    }
}
