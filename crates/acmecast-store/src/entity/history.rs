//! 运行历史。
//!
//! 每次流水线运行产生一条历史；分步输出记录在 [`super::history_log`] 中。

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

/// 运行历史表。
#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq)]
#[sea_orm(table_name = "acmecast_history")]
pub struct Model {
    /// 自增主键。
    #[sea_orm(primary_key)]
    pub id: i64,
    /// 所属流水线。
    pub pipeline_id: i64,
    /// 触发来源，取值来自 [`super::pipeline::TriggerSource::as_str`]。
    pub trigger_source: String,
    /// 运行状态，取值来自 [`RunStatus::as_str`]。
    pub status: String,
    /// 开始时间。
    pub started_at: chrono::DateTime<chrono::Utc>,
    /// 结束时间；运行中为空。
    pub finished_at: Option<chrono::DateTime<chrono::Utc>>,
    /// 失败原因摘要。
    pub error_message: Option<String>,
}

/// 运行结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RunStatus {
    /// 正在执行。
    Running,
    /// 全部步骤成功。
    Success,
    /// 因某步骤失败而中止。
    Failed,
}

impl RunStatus {
    /// 与库中存储形态互转的字符串表示。
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Success => "success",
            Self::Failed => "failed",
        }
    }

    /// 解析字符串表示；未知状态返回 `None`。
    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "running" => Some(Self::Running),
            "success" => Some(Self::Success),
            "failed" => Some(Self::Failed),
            _ => None,
        }
    }

    /// 是否已到达终态。
    #[must_use]
    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Success | Self::Failed)
    }
}

/// 运行历史的关系。
#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    /// 一次运行有若干条步骤日志。
    #[sea_orm(has_many = "super::history_log::Entity")]
    Logs,
    /// 历史归属于一条流水线。
    #[sea_orm(
        belongs_to = "super::pipeline::Entity",
        from = "Column::PipelineId",
        to = "super::pipeline::Column::Id",
        on_update = "Cascade",
        on_delete = "Cascade"
    )]
    Pipeline,
}

impl Related<super::history_log::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Logs.def()
    }
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
    fn run_status_roundtrips() {
        for status in [RunStatus::Running, RunStatus::Success, RunStatus::Failed] {
            assert_eq!(RunStatus::parse(status.as_str()), Some(status));
        }
    }

    #[test]
    fn only_success_and_failed_are_terminal() {
        assert!(!RunStatus::Running.is_terminal());
        assert!(RunStatus::Success.is_terminal());
        assert!(RunStatus::Failed.is_terminal());
    }

    #[test]
    fn unknown_status_is_rejected() {
        assert_eq!(RunStatus::parse("paused"), None);
        assert_eq!(RunStatus::parse(""), None);
    }
}
