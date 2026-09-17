//! 部署记录。
//!
//! 每次部署写一条，因此它同时承担两件事：
//!
//! - **幂等判断**（8.5）：取某目标最新一条的指纹，与本证书的指纹比对；
//! - **部署历史**（8.7）：按目标、时间、是否跳过写入查询。
//!
//! 不做成「一张状态表 + 一张历史表」是因为二者的数据来源完全相同，分开反而要维护两处写入。

use sea_orm::entity::prelude::*;

/// 部署记录表。
#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq)]
#[sea_orm(table_name = "acmecast_deployment")]
pub struct Model {
    /// 自增主键。
    #[sea_orm(primary_key)]
    pub id: i64,
    /// 部署目标的类型标识，如 `local`／`ssh`。
    pub target_type: String,
    /// 目标的定位键，由部署输入推导；同一键视为同一目标。
    pub target_key: String,
    /// 本次部署的证书指纹。
    pub fingerprint: String,
    /// 本次是否跳过了文件写入。
    pub skipped_write: bool,
    /// 部署时间。
    pub deployed_at: chrono::DateTime<chrono::Utc>,
    /// 本次写入的路径（JSON 数组）。
    ///
    /// 路径是运维回溯时最先要看的东西——「上次到底写到哪儿去了」。
    pub paths: Json,
    /// 重载命令的输出；未配置重载或未执行时为 `None`。
    pub reload_output: Option<String>,
}

/// 部署记录的关系。
#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}

/// 一个部署目标。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TargetRef {
    /// 目标类型标识。
    pub target_type: String,
    /// 目标定位键。
    pub target_key: String,
}
