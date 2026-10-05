//! 流水线步骤。
//!
//! 每个步骤记录一个任务类型标识（`type_id`）与一份 JSON 输入，
//! 按 `order_index` 顺序执行。

use sea_orm::entity::prelude::*;

/// 流水线步骤表。
#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq)]
#[sea_orm(table_name = "acmecast_pipeline_step")]
pub struct Model {
    /// 自增主键。
    #[sea_orm(primary_key)]
    pub id: i64,
    /// 所属流水线。
    pub pipeline_id: i64,
    /// 步骤在同一条流水线内的执行顺序，从 0 起。
    pub order_index: i32,
    /// 任务类型标识，需在任务注册表中已登记。
    pub type_id: String,
    /// JSON 格式的输入配置。
    pub input: Json,
    /// 单步开关，停用后执行时跳过该步骤。
    pub enabled: bool,
}

/// 步骤的关系。
#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    /// 步骤归属于一条流水线。
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
