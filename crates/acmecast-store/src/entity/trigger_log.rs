//! 触发记录。
//!
//! 每次自动（或手动）触发流水线都落一条：谁发起的、什么时候、目标是谁。
//! 它服务于两件事——审计（这台证书的续期到底是谁在什么时候启动的）与
//! 调度自身的去重（同一时间窗口内的重复续期触发靠它判定）。

use sea_orm::entity::prelude::*;

/// 触发记录表。
#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq)]
#[sea_orm(table_name = "acmecast_trigger_log")]
pub struct Model {
    /// 自增主键。
    #[sea_orm(primary_key)]
    pub id: i64,
    /// 被触发的流水线。
    pub pipeline_id: i64,
    /// 触发来源：`cron`／`renewal`／`manual`。
    pub source: String,
    /// 触发说明；例如续期触发时记录命中的证书域名集合。
    pub detail: Option<String>,
    /// 触发时间。
    pub triggered_at: chrono::DateTime<chrono::Utc>,
}

/// 触发记录的关系。
#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    /// 记录归属于一条流水线。
    ///
    /// 流水线被删除时其触发记录一并删除：记录的意义随流水线消失。
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
