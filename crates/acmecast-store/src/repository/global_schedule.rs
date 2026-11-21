//! 全局调度仓储。
//!
//! 单例语义在这一层收敛：`get_or_default` 未创建时返回默认形态（HTTP 层
//! 不必区分「没配过」与「停用」），`save` upsert 保证至多一行。校验口径与
//! 流水线级调度一致——cron 在保存时校验（`parse_cron` 同一套实现），启用
//! 必有 cron；差异是全局调度没有续期语义，不需要域名/阈值字段。

use chrono::{DateTime, Utc};
use sea_orm::{ActiveModelTrait, DatabaseConnection, EntityTrait, Set};

use crate::entity::global_schedule;
use crate::error::{Error, Result};
use crate::repository::schedule::next_cron_trigger;

/// 一份全局调度配置的输入。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GlobalScheduleInput {
    /// cron 表达式；启用时必填，空/None 视为不按 cron 触发（停用态）。
    pub cron: Option<String>,
    /// 是否启用。
    pub enabled: bool,
    /// 停机补跑策略。
    pub catch_up: bool,
}

/// 读出的一份全局调度配置。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GlobalScheduleConfig {
    /// cron 表达式原文；`None` 表示未配置定时触发。
    pub cron: Option<String>,
    /// 是否启用。
    pub enabled: bool,
    /// 停机补跑策略。
    pub catch_up: bool,
    /// 上次触发时间。
    pub last_triggered_at: Option<DateTime<Utc>>,
    /// 下次预计触发时间。
    pub next_trigger_at: Option<DateTime<Utc>>,
    /// 记录更新时间。
    pub updated_at: DateTime<Utc>,
}

impl GlobalScheduleConfig {
    /// 未创建时的默认形态：停用、无 cron、无触发时间。
    #[must_use]
    pub fn default_form() -> Self {
        Self {
            cron: None,
            enabled: false,
            catch_up: false,
            last_triggered_at: None,
            next_trigger_at: None,
            updated_at: Utc::now(),
        }
    }
}

/// 全局调度仓储（单例）。
#[derive(Debug)]
pub struct GlobalScheduleRepository<'db> {
    db: &'db DatabaseConnection,
}

impl<'db> GlobalScheduleRepository<'db> {
    /// 绑定数据库连接。
    #[must_use]
    pub fn new(db: &'db DatabaseConnection) -> Self {
        Self { db }
    }

    /// 读全局调度；未创建时返回默认形态（单例语义，调用方不必判空）。
    pub async fn get_or_default(&self) -> Result<GlobalScheduleConfig> {
        let row = global_schedule::Entity::find().one(self.db).await?;
        Ok(row
            .map(config_from_row)
            .unwrap_or_else(GlobalScheduleConfig::default_form))
    }

    /// 保存（新建或整体更新）全局调度，返回更新后的配置。
    ///
    /// cron 非法或「启用但无 cron」时拒绝保存；`next_trigger_at` 随保存算出
    /// 初始值，引擎装载时会重算兜底（与流水线级调度同一理由：让「刚保存、
    /// 引擎还没装载」的窗口里也有合理的下一次触发点）。
    pub async fn save(&self, input: GlobalScheduleInput) -> Result<GlobalScheduleConfig> {
        let cron = input
            .cron
            .map(|expr| expr.trim().to_owned())
            .filter(|expr| !expr.is_empty());
        if let Some(expr) = &cron {
            crate::repository::schedule::parse_cron(expr)?;
        }
        // 全局调度没有续期语义，「启用但没有触发途径」是一条永不自动触发的
        // 死配置，与流水线级调度的口径一致：保存时硬拒绝。
        if input.enabled && cron.is_none() {
            return Err(Error::Validation("启用的全局调度必须配置 cron".to_owned()));
        }

        let now = Utc::now();
        let next_trigger_at = cron
            .as_deref()
            .and_then(|expr| next_cron_trigger(expr, now));

        let existing = global_schedule::Entity::find().one(self.db).await?;
        let row = match existing {
            Some(row) => {
                global_schedule::ActiveModel {
                    id: Set(row.id),
                    cron: Set(cron.clone()),
                    enabled: Set(input.enabled),
                    catch_up: Set(input.catch_up),
                    next_trigger_at: Set(next_trigger_at),
                    updated_at: Set(now),
                    ..Default::default()
                }
                .update(self.db)
                .await?
            }
            None => {
                global_schedule::ActiveModel {
                    id: Set(1),
                    cron: Set(cron.clone()),
                    enabled: Set(input.enabled),
                    catch_up: Set(input.catch_up),
                    next_trigger_at: Set(next_trigger_at),
                    updated_at: Set(now),
                    ..Default::default()
                }
                .insert(self.db)
                .await?
            }
        };

        Ok(config_from_row(row))
    }

    /// 触发后推进触发时间。
    ///
    /// `last_triggered_at` 记本次触发时刻（`None` 表示不改写）；全局调度的
    /// 触发点**到点即推进**——成员失败不整序列重跑（见 design D3）。
    pub async fn mark_triggered(
        &self,
        last_triggered_at: Option<Option<DateTime<Utc>>>,
        next_trigger_at: Option<DateTime<Utc>>,
    ) -> Result<()> {
        let row = global_schedule::Entity::find()
            .one(self.db)
            .await?
            .ok_or_else(|| Error::Validation("全局调度尚未配置".to_owned()))?;

        let mut update = global_schedule::ActiveModel {
            id: Set(row.id),
            next_trigger_at: Set(next_trigger_at),
            updated_at: Set(Utc::now()),
            ..Default::default()
        };
        if let Some(last) = last_triggered_at {
            update.last_triggered_at = Set(last);
        }

        update.update(self.db).await?;
        Ok(())
    }
}

/// 把一行全局调度记录转成领域类型。
fn config_from_row(row: global_schedule::Model) -> GlobalScheduleConfig {
    GlobalScheduleConfig {
        cron: row.cron,
        enabled: row.enabled,
        catch_up: row.catch_up,
        last_triggered_at: row.last_triggered_at,
        next_trigger_at: row.next_trigger_at,
        updated_at: row.updated_at,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_form_is_disabled_without_cron() {
        let config = GlobalScheduleConfig::default_form();
        assert!(!config.enabled);
        assert_eq!(config.cron, None);
        assert_eq!(config.next_trigger_at, None);
    }
}
