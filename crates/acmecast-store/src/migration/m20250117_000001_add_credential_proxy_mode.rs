//! 给凭据表加 `proxy_mode`（出网代理模式）明文列。
//!
//! `proxy_mode`（`direct` / `inherit` / `custom`）原本只存在加密字段
//! `encrypted_fields` 内，列表 API 不解密、不返回，用户须逐条打开编辑
//! 弹窗才能确认某凭据的出网代理方式。该列与 `capabilities` 同口径：
//! 从加密字段中提取的非敏感枚举值单独持久化，列表直接读明文列返回；
//! 代理 URL 本体仍在 `encrypted_fields` 加密保存。
//!
//! 加新迁移而非回头改初始建表：已应用 `m20250101` 的环境不会重放它，
//! 改它会让那些环境与新环境 schema 分叉。
//!
//! 回填不放在迁移 `up` 里：启动序列中 `migrate` 先于凭据加密密钥初始化，
//! 而解密存量 `encrypted_fields` 需要 [`CredentialCipher`]。回填由调用方
//! 在密钥就绪后调用 [`backfill_credential_proxy_mode`] 完成；密钥未配置时
//! 跳过（列保持默认 `direct`），待密钥配置后下次启动补回填。

use sea_orm::{ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter, Set};
use sea_orm_migration::prelude::*;

use crate::entity::credential;

/// 给凭据记录补 `proxy_mode` 列的迁移。
#[derive(Debug, DeriveMigrationName)]
pub struct Migration;

/// 明文列的默认值：直连（与 [`acmecast_core::ProxyMode`] 的缺省一致）。
pub const DEFAULT_PROXY_MODE: &str = "direct";

// 与其它迁移同理：`SchemaManager` 的生命周期只能按 SeaORM 的要求省略。
#[allow(elided_lifetimes_in_paths)]
#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // 三方言 `ALTER TABLE ... ADD COLUMN ... NOT NULL DEFAULT '...'` 均原生支持，
        // 存量行自动得默认 `direct`，回填再按加密字段的归一化结果修正。
        manager
            .alter_table(
                Table::alter()
                    .table(credential::Entity)
                    .add_column(
                        ColumnDef::new(credential::Column::ProxyMode)
                            .text()
                            .not_null()
                            .default(DEFAULT_PROXY_MODE),
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
                    .drop_column(credential::Column::ProxyMode)
                    .to_owned(),
            )
            .await
    }
}

/// 为存量凭据回填 `proxy_mode` 明文列（密钥就绪后由调用方延迟执行）。
///
/// 遍历列值为默认 `direct` 的行，解密 `encrypted_fields` 后按
/// [`acmecast_core::ProxyMode::from_credentials`] 归一化（与运行时传输层
/// 选择同口径），把归一化结果非 `direct` 的行（`custom`，或存量已显式
/// 保存的 `inherit`）写回明文列。这保证了幂等：回填后值不再是默认
/// `direct`，重复执行不再命中。
///
/// `cipher` 传 `None`（密钥未配置）时跳过回填——列已有默认 `direct`，
/// 不报错，待密钥配置后下次启动补回填。单行解密失败（如密钥更换后的
/// 孤儿数据）只记日志跳过，不阻断其余行；数据库错误原样返回。
pub async fn backfill_credential_proxy_mode(
    db: &DatabaseConnection,
    cipher: Option<&acmecast_core::CredentialCipher>,
) -> Result<(), DbErr> {
    let Some(cipher) = cipher else {
        tracing::info!("凭据加密密钥未配置，跳过 proxy_mode 回填（下次配置后启动补回填）");
        return Ok(());
    };
    let rows = credential::Entity::find()
        .filter(credential::Column::ProxyMode.eq(DEFAULT_PROXY_MODE))
        .all(db)
        .await?;
    for row in rows {
        let plaintext = match cipher.decrypt_string(&row.encrypted_fields) {
            Ok(plaintext) => plaintext,
            Err(error) => {
                tracing::warn!(credential_id = row.id, %error, "解密凭据失败，跳过该行回填");
                continue;
            }
        };
        let fields: serde_json::Value = match serde_json::from_str(&plaintext) {
            Ok(fields) => fields,
            Err(error) => {
                tracing::warn!(credential_id = row.id, %error, "凭据字段不是合法 JSON，跳过回填");
                continue;
            }
        };
        let mode = acmecast_core::ProxyMode::from_credentials(&fields);
        if mode != acmecast_core::ProxyMode::Direct {
            let mut active: credential::ActiveModel = row.into();
            active.proxy_mode = Set(mode.as_str().to_owned());
            active.update(db).await?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 归一化口径与运行时一致：旧 `proxy_url` 有值视为 `custom`，无值为 `direct`。
    #[test]
    fn proxy_mode_text_matches_runtime_normalization() {
        let custom = serde_json::json!({ "proxy_url": "socks5://127.0.0.1:1080" });
        let direct = serde_json::json!({ "api_token": "t" });
        assert_eq!(
            acmecast_core::ProxyMode::from_credentials(&custom),
            acmecast_core::ProxyMode::Custom
        );
        assert_eq!(
            acmecast_core::ProxyMode::from_credentials(&direct),
            acmecast_core::ProxyMode::Direct
        );
        assert_eq!(acmecast_core::ProxyMode::Custom.as_str(), "custom");
        assert_eq!(acmecast_core::ProxyMode::Direct.as_str(), "direct");
    }
}
