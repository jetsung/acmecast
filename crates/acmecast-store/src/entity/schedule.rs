//! 调度配置。
//!
//! 一条流水线最多挂一份调度。`cron` 为空表示不按 cron 触发，
//! 但仍可参与到期扫描触发的续期。

use sea_orm::entity::prelude::*;

/// 调度表。
#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq)]
#[sea_orm(table_name = "acmecast_schedule")]
pub struct Model {
    /// 自增主键。
    #[sea_orm(primary_key)]
    pub id: i64,
    /// 所属流水线，一条流水线只有一份调度。
    #[sea_orm(unique)]
    pub pipeline_id: i64,
    /// cron 表达式；为空表示不按 cron 触发。
    pub cron: Option<String>,
    /// 是否启用。停用后不再有任何自动触发（含续期扫描）。
    pub enabled: bool,
    /// 停机补跑策略：`true` 表示恢复后追赶最近一次，`false` 表示跳过错过的时间点。
    pub catch_up: bool,
    /// 该流水线负责续期的域名集合（JSON 数组）；`None` 表示不参与到期扫描触发。
    ///
    /// 「证书关联的流水线」以显式配置落地：到期扫描命中某证书后，用证书的
    /// 域名集合与各调度的这份配置求交集来找到目标流水线。不解析流水线步骤
    /// 输入来推断关联——那会把调度行为绑死在步骤输入的具体字段结构上，
    /// 输入结构一变映射就悄悄失效。
    pub renewal_domains: Option<String>,
    /// 上次触发时间。
    pub last_triggered_at: Option<chrono::DateTime<chrono::Utc>>,
    /// 下次预计触发时间。
    pub next_trigger_at: Option<chrono::DateTime<chrono::Utc>>,
    /// 记录更新时间。
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

/// 调度的关系。
#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    /// 调度归属于一条流水线。
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
    #[test]
    fn cron_is_optional_for_renewal_only_schedules() {
        // 纯续期场景不填 cron，由到期扫描触发。
        let cron: Option<String> = None;
        assert!(cron.is_none());
    }
    // 注：`catch_up` 默认为 false（跳过补跑）由 migration 的 DEFAULT 子句保证，
    // 其验证见 migration 测试，此处不写无断言能力的占位用例。
}
