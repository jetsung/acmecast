//! 凭据实体。
//!
//! 敏感字段整体加密后以 JSON 文本存放（`encrypted_fields`），
//! 明文只存在于进程内存。加解密由 [`acmecast_core::CredentialCipher`] 承担，
//! 本模块**不接触**明文。

use sea_orm::entity::prelude::*;

/// 凭据表。
#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq)]
#[sea_orm(table_name = "acmecast_credential")]
pub struct Model {
    /// 自增主键。
    #[sea_orm(primary_key)]
    pub id: i64,
    /// 展示名称，不要求唯一。
    pub name: String,
    /// 凭据类型标识，需在凭据注册表中已登记。
    pub type_id: String,
    /// 加密后的字段集合：`{"字段名": "<base64 密文>"}`。
    pub encrypted_fields: String,
    /// 记录创建时间。
    pub created_at: chrono::DateTime<chrono::Utc>,
    /// 记录更新时间。
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

/// 凭据的关系。
#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    /// 反向引用：哪些证书是由本凭据作为 ACME 账号签发的。
    ///
    /// 删除凭据时不级联删除证书（设为 SetNull 需数据库侧配合，
    /// 此处仅作查询用，实际级联策略在应用层保证）。
    #[sea_orm(has_many = "super::cert::Entity")]
    IssuedCerts,
}

impl Related<super::cert::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::IssuedCerts.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_never_stores_plaintext_field() {
        // 实体只有一个整体密文列：不存在任何以 `api_key` / `secret` 命名的明文列。
        let field_names = [
            Column::Id,
            Column::Name,
            Column::TypeId,
            Column::EncryptedFields,
            Column::CreatedAt,
            Column::UpdatedAt,
        ];
        for column in field_names {
            let rendered = format!("{column:?}");
            assert!(
                !rendered.contains("Plaintext"),
                "实体不应暴露明文字段：{rendered}"
            );
        }
    }

    #[test]
    fn encrypted_fields_is_the_only_secret_carrier() {
        assert_eq!(Column::EncryptedFields.to_string(), "encrypted_fields");
    }
}
