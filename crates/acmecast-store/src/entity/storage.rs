//! 流水线级键值存储。
//!
//! 供任务保存跨次运行的状态（如上一次成功的 DNS 记录、上一次的部署路径）。
//! 作用域局限于单条流水线，不同流水线互不干扰。
//!
//! 列名用 `store_key` / `store_value`：`key` 是 MySQL 保留字。

use sea_orm::entity::prelude::*;

/// 键值存储表。
#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq)]
#[sea_orm(table_name = "acmecast_storage")]
pub struct Model {
    /// 自增主键。
    #[sea_orm(primary_key)]
    pub id: i64,
    /// 所属流水线。
    pub pipeline_id: i64,
    /// 键。
    pub store_key: String,
    /// 值，可存放任意文本（通常为 JSON）。
    pub store_value: String,
    /// 更新时间。
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

/// 存储的关系。
#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    /// 存储归属于一条流水线。
    #[sea_orm(
        belongs_to = "super::pipeline::Entity",
        from = "Column::PipelineId",
        to = "super::pipeline::Column::Id",
        on_update = "Cascade",
        on_delete = "Cascade"
    )]
    Pipeline,
}

impl Related<super::pipeline::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Pipeline.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn storage_columns_avoid_reserved_words() {
        // MySQL 保留字检查：`key` / `value` 不能直接用作列名。
        assert_ne!(Column::StoreKey.to_string(), "key");
        assert_ne!(Column::StoreValue.to_string(), "value");
        assert_eq!(Column::StoreKey.to_string(), "store_key");
        assert_eq!(Column::StoreValue.to_string(), "store_value");
    }

    #[test]
    fn table_is_prefixed() {
        assert!(
            <Entity as sea_orm::EntityName>::table_name(&Entity).starts_with("acmecast_"),
            "表名统一加 acmecast_ 前缀以避免与其他应用混库时冲突"
        );
    }
}
