//! 流水线实体。
//!
//! 流水线本身只持有元信息；具体的步骤列表见 [`super::pipeline_step`]。

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

/// 流水线表。
#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq)]
#[sea_orm(table_name = "acmecast_pipeline")]
pub struct Model {
    /// 自增主键。
    #[sea_orm(primary_key)]
    pub id: i64,
    /// 展示名称。
    pub name: String,
    /// 是否启用。停用后不再被任何触发方式启动。
    pub enabled: bool,
    /// 额外说明。
    pub description: Option<String>,
    /// 记录创建时间。
    pub created_at: chrono::DateTime<chrono::Utc>,
    /// 记录更新时间。
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

/// 流水线的关系。
#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    /// 一条流水线包含多个步骤。
    #[sea_orm(has_many = "super::pipeline_step::Entity")]
    Steps,
    /// 一条流水线有多条运行历史。
    #[sea_orm(has_many = "super::history::Entity")]
    Histories,
    /// 一条流水线有自己的键值存储。
    #[sea_orm(has_many = "super::storage::Entity")]
    Storage,
    /// 一条流水线可以挂一份调度配置。
    #[sea_orm(has_one = "super::schedule::Entity")]
    Schedule,
}

impl Related<super::pipeline_step::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Steps.def()
    }
}
impl Related<super::history::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Histories.def()
    }
}
impl Related<super::storage::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Storage.def()
    }
}
impl Related<super::schedule::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Schedule.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}

/// 运行触发来源。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TriggerSource {
    /// 用户手动触发。
    Manual,
    /// 由 cron 表达式触发。
    Cron,
    /// 由到期扫描触发的续期。
    Renewal,
}

impl TriggerSource {
    /// 与库中存储形态互转的字符串表示。
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Manual => "manual",
            Self::Cron => "cron",
            Self::Renewal => "renewal",
        }
    }

    /// 解析字符串表示；未知来源返回 `None`。
    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "manual" => Some(Self::Manual),
            "cron" => Some(Self::Cron),
            "renewal" => Some(Self::Renewal),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trigger_source_roundtrips() {
        for source in [
            TriggerSource::Manual,
            TriggerSource::Cron,
            TriggerSource::Renewal,
        ] {
            assert_eq!(TriggerSource::parse(source.as_str()), Some(source));
        }
    }

    #[test]
    fn unknown_trigger_source_is_rejected() {
        assert_eq!(TriggerSource::parse("nightly"), None);
    }

    #[test]
    fn trigger_sources_are_distinct_strings() {
        assert_ne!(
            TriggerSource::Cron.as_str(),
            TriggerSource::Renewal.as_str()
        );
        assert_ne!(TriggerSource::Manual.as_str(), TriggerSource::Cron.as_str());
    }
}
