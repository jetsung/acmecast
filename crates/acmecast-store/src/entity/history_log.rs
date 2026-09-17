//! 步骤日志。
//!
//! 每个步骤的执行输出按条记入，与运行历史通过 `history_id` 关联，
//! 支持分页回溯。

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

/// 日志级别。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Level {
    /// 调试信息。
    Debug,
    /// 常规输出。
    Info,
    /// 需要留意的情形。
    Warn,
    /// 错误。
    Error,
}

impl Level {
    /// 字符串表示。
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Debug => "debug",
            Self::Info => "info",
            Self::Warn => "warn",
            Self::Error => "error",
        }
    }

    /// 解析字符串表示；未知级别返回 `None` 而非回落到某一默认值，
    /// 避免把 `trace` 这类级别静默记成 info。
    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "debug" => Some(Self::Debug),
            "info" => Some(Self::Info),
            "warn" => Some(Self::Warn),
            "error" => Some(Self::Error),
            _ => None,
        }
    }
}

/// 日志表。
#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq)]
#[sea_orm(table_name = "acmecast_history_log")]
pub struct Model {
    /// 自增主键。
    #[sea_orm(primary_key)]
    pub id: i64,
    /// 所属运行历史。
    pub history_id: i64,
    /// 步骤序号，与流水线步骤的 `order_index` 对应。
    pub step_index: i32,
    /// 日志级别，取值来自 [`Level::as_str`]。
    pub level: String,
    /// 日志正文。
    pub message: String,
    /// 产生时间。
    pub created_at: chrono::DateTime<chrono::Utc>,
}

/// 日志的关系。
#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    /// 日志归属于一次运行历史。
    #[sea_orm(
        belongs_to = "super::history::Entity",
        from = "Column::HistoryId",
        to = "super::history::Column::Id",
        on_update = "Cascade",
        on_delete = "Cascade"
    )]
    History,
}

impl Related<super::history::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::History.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_level_roundtrips() {
        for level in [Level::Debug, Level::Info, Level::Warn, Level::Error] {
            assert_eq!(Level::parse(level.as_str()), Some(level));
        }
    }

    #[test]
    fn unknown_log_level_is_rejected() {
        assert_eq!(Level::parse("trace"), None);
        assert_eq!(Level::parse(""), None);
    }

    #[test]
    fn levels_have_distinct_encoding() {
        let all = ["debug", "info", "warn", "error"];
        let unique: std::collections::BTreeSet<_> = all.iter().collect();
        assert_eq!(unique.len(), all.len());
    }
}
