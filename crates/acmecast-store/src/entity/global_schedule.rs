//! 全局调度配置（单例）。
//!
//! 与流水线级调度（`acmecast_schedule`）并存的一份 cron 配置：到点后按
//! `pipeline_id` 升序串行运行全部启用的流水线（见 `global-schedule` spec）。
//! 不占用流水线级表的外键与唯一键——它没有归属的流水线，塞 `pipeline_id`
//! 占位值会破坏两边的参照完整性。
//!
//! 单例语义由仓储层保证（`get_or_default` / upsert，至多一行）；本表不含
//! 续期相关字段——全局调度的职责只有「定时串行跑全量」，按域名找流水线的
//! 续期扫描仍归流水线级调度。

use sea_orm::entity::prelude::*;

/// 全局调度表。
#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq)]
#[sea_orm(table_name = "acmecast_global_schedule")]
pub struct Model {
    /// 自增主键；单例语义下只应有一行。
    #[sea_orm(primary_key)]
    pub id: i64,
    /// cron 表达式；启用时必填。
    pub cron: Option<String>,
    /// 是否启用。停用后不再有任何全局触发。
    pub enabled: bool,
    /// 停机补跑策略：`true` 表示恢复后追赶最近一次（启动一次序列），`false`
    /// 表示跳过错过的触发点。
    pub catch_up: bool,
    /// 上次触发时间。
    pub last_triggered_at: Option<chrono::DateTime<chrono::Utc>>,
    /// 下次预计触发时间。
    pub next_trigger_at: Option<chrono::DateTime<chrono::Utc>>,
    /// 记录更新时间。
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

/// 单例表无关联关系。
#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
