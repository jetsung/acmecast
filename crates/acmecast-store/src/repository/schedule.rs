//! 调度配置仓储。
//!
//! 一条流水线最多挂一份调度（`pipeline_id` 唯一）。调度配置本身就是 spec
//! 「调度持久化与恢复」的载体：cron、启用开关、补跑策略与续期域名集合都
//! 落库，服务重启后由调度引擎从这里重新装载。
//!
//! cron 表达式**在保存时校验**（spec：无效的 cron 在保存时被拒绝），而不是
//! 等到引擎装载时才发现——那时用户早已离开编辑页面，错误无人认领。

use chrono::{DateTime, Utc};
use cron::Schedule as CronSchedule;
use sea_orm::{ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter, Set};
use std::str::FromStr;

use crate::entity::schedule;
use crate::error::{Error, Result};

/// 一份调度配置的输入。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScheduleInput {
    /// cron 表达式；`None` 或空表示不按 cron 触发（可仅参与续期扫描）。
    pub cron: Option<String>,
    /// 是否启用。停用后不再有任何自动触发。
    pub enabled: bool,
    /// 停机补跑策略：`true` 追赶最近一次，`false` 跳过错过的触发点。
    pub catch_up: bool,
    /// 该流水线负责续期的域名集合；`None` 或空表示不参与到期扫描触发。
    pub renewal_domains: Option<Vec<String>>,
}

/// 读出的一份调度配置。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScheduleConfig {
    /// 所属流水线。
    pub pipeline_id: i64,
    /// cron 表达式原文；`None` 表示不按 cron 触发。
    pub cron: Option<String>,
    /// 是否启用。
    pub enabled: bool,
    /// 停机补跑策略。
    pub catch_up: bool,
    /// 负责续期的域名集合（已规范化）；`None` 表示不参与到期扫描触发。
    pub renewal_domains: Option<Vec<String>>,
    /// 上次触发时间。
    pub last_triggered_at: Option<DateTime<Utc>>,
    /// 下次预计触发时间。
    pub next_trigger_at: Option<DateTime<Utc>>,
    /// 记录更新时间。
    pub updated_at: DateTime<Utc>,
}

/// 解析并规范化 cron 表达式。
///
/// 用户习惯写 5 段的 unix cron（`0 3 * * *`），而 `cron` crate 要求
/// `秒 分 时 日 月 周 [年]` 的 6/7 段格式——5 段输入在前面补 `0 0`，
/// 即「整分零秒」触发。6/7 段原样解析。
///
/// 解析失败返回 [`Error::Validation`]，错误信息带原文——保存路径（9.2）
/// 靠它拒绝非法表达式；调度引擎的装载与触发推算也复用这里，保证
/// 「保存时认为合法的」与「运行时能解析的」是同一套判断。
pub fn parse_cron(expr: &str) -> Result<CronSchedule> {
    let expr = expr.trim();
    let normalized = match expr.split_whitespace().count() {
        5 => format!("0 {expr}"),
        6 | 7 => expr.to_owned(),
        _ => {
            return Err(Error::Validation(format!(
                "cron 表达式应为 5 段（分 时 日 月 周）或 6 段（秒 分 时 日 月 周）：`{expr}`"
            )));
        }
    };

    CronSchedule::from_str(&normalized)
        .map_err(|source| Error::Validation(format!("cron 表达式 `{expr}` 无法解析：{source}")))
}

/// cron 表达式的下一次触发时刻；解析失败返回 `None`。
///
/// 供调度引擎推算 `next_trigger_at` 使用。与 [`parse_cron`] 不同，这里
/// 把解析失败静默归为 `None`：配置在保存时已校验过，运行时再失败属于
/// 理论上不可达的防御分支，不该让整个调度循环因此崩溃。
#[must_use]
pub fn next_cron_trigger(expr: &str, after: DateTime<Utc>) -> Option<DateTime<Utc>> {
    parse_cron(expr).ok()?.after(&after).next()
}

/// 调度配置仓储。
#[derive(Debug)]
pub struct ScheduleRepository<'db> {
    db: &'db DatabaseConnection,
}

impl<'db> ScheduleRepository<'db> {
    /// 绑定数据库连接。
    #[must_use]
    pub fn new(db: &'db DatabaseConnection) -> Self {
        Self { db }
    }

    /// 为流水线保存（新建或整体更新）一份调度配置，返回更新后的配置。
    ///
    /// cron 非法时拒绝保存（9.2）。`next_trigger_at` 在这里随保存算出初始值；
    /// 服务重启后引擎装载时会重算兜底——它本质是运行时缓存，持久化它只是
    /// 让「配置刚保存、引擎还没装载」的窗口里也能看到合理的下一次触发点。
    pub async fn save(&self, pipeline_id: i64, input: ScheduleInput) -> Result<ScheduleConfig> {
        // 空字符串与 None 同义：清空 cron 就是取消定时触发。
        let cron = input
            .cron
            .map(|expr| expr.trim().to_owned())
            .filter(|expr| !expr.is_empty());
        if let Some(expr) = &cron {
            parse_cron(expr)?;
        }

        // 域名集合与 cron 同理：空集合视作「不参与续期触发」，存 `None`。
        // 规范化（小写、去重、排序）复用证书域名的同一套规则，两边的
        // 「同一域名」判定才不会因书写差异错过匹配。
        let renewal_domains = input
            .renewal_domains
            .map(|domains| crate::entity::cert::normalize_domains(&domains))
            .filter(|domains| !domains.is_empty());
        let renewal_json = renewal_domains
            .as_ref()
            .map(|domains| serde_json::to_string(domains).expect("域名数组的序列化不会失败"));

        // 启用的调度没有任何触发途径就是一条永不自动触发的死配置——
        // 「配了却不触发」的困惑多半源于此，宁可保存时硬拒绝。
        if input.enabled && cron.is_none() && renewal_domains.is_none() {
            return Err(Error::Validation(
                "启用的调度必须至少配置 cron 或续期域名集合之一".to_owned(),
            ));
        }

        let now = Utc::now();
        let next_trigger_at = cron
            .as_deref()
            .and_then(|expr| next_cron_trigger(expr, now));

        let existing = schedule::Entity::find()
            .filter(schedule::Column::PipelineId.eq(pipeline_id))
            .one(self.db)
            .await?;

        let row = match existing {
            Some(row) => {
                schedule::ActiveModel {
                    id: Set(row.id),
                    cron: Set(cron.clone()),
                    enabled: Set(input.enabled),
                    catch_up: Set(input.catch_up),
                    renewal_domains: Set(renewal_json),
                    next_trigger_at: Set(next_trigger_at),
                    updated_at: Set(now),
                    ..Default::default()
                }
                .update(self.db)
                .await?
            }
            None => {
                schedule::ActiveModel {
                    pipeline_id: Set(pipeline_id),
                    cron: Set(cron.clone()),
                    enabled: Set(input.enabled),
                    catch_up: Set(input.catch_up),
                    renewal_domains: Set(renewal_json),
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

    /// 读某条流水线的调度配置。
    pub async fn find(&self, pipeline_id: i64) -> Result<Option<ScheduleConfig>> {
        let row = schedule::Entity::find()
            .filter(schedule::Column::PipelineId.eq(pipeline_id))
            .one(self.db)
            .await?;

        Ok(row.map(config_from_row))
    }

    /// 读全部调度配置，供引擎装载。
    pub async fn list_all(&self) -> Result<Vec<ScheduleConfig>> {
        let rows = schedule::Entity::find().all(self.db).await?;
        Ok(rows.into_iter().map(config_from_row).collect())
    }

    /// 触发后推进触发时间。
    ///
    /// `last_triggered_at` 记本次触发时刻（`None` 表示不改写——用于「该触发
    /// 被运行中检查拦下、只把 `next_trigger_at` 推过去」的场景）；
    /// `next_trigger_at` 推到之后的下一个触发点。
    ///
    /// 推进由**引擎在触发完成后**调用而不是保存时预填：触发可能失败（流水线
    /// 启动报错），失败时引擎不调这里，原 `next_trigger_at` 保留，下一轮
    /// tick 还能再试。
    pub async fn mark_triggered(
        &self,
        pipeline_id: i64,
        last_triggered_at: Option<Option<DateTime<Utc>>>,
        next_trigger_at: Option<DateTime<Utc>>,
    ) -> Result<()> {
        let row = schedule::Entity::find()
            .filter(schedule::Column::PipelineId.eq(pipeline_id))
            .one(self.db)
            .await?
            .ok_or_else(|| Error::Validation(format!("流水线 {pipeline_id} 没有调度配置")))?;

        let mut update = schedule::ActiveModel {
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

/// 把一行调度记录转成领域类型；域名集合解析失败视为「不参与续期」。
fn config_from_row(row: schedule::Model) -> ScheduleConfig {
    ScheduleConfig {
        pipeline_id: row.pipeline_id,
        cron: row.cron,
        enabled: row.enabled,
        catch_up: row.catch_up,
        renewal_domains: row
            .renewal_domains
            .and_then(|json| serde_json::from_str(&json).ok()),
        last_triggered_at: row.last_triggered_at,
        next_trigger_at: row.next_trigger_at,
        updated_at: row.updated_at,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn five_field_cron_is_accepted() {
        assert!(parse_cron("0 3 * * *").is_ok());
    }

    #[test]
    fn six_field_cron_is_accepted() {
        assert!(parse_cron("0 0 3 * * *").is_ok());
    }

    #[test]
    fn invalid_cron_is_rejected_with_the_original_text() {
        let error = parse_cron("0 99 * * *").unwrap_err();
        let message = error.to_string();
        assert!(message.contains("0 99 * * *"), "错误应带原文：{message}");
    }

    #[test]
    fn wrong_field_count_is_rejected() {
        assert!(parse_cron("* * * *").is_err());
        assert!(parse_cron("* * * * * * * *").is_err());
    }

    #[test]
    fn next_trigger_advances_into_the_future() {
        let after = chrono::DateTime::parse_from_rfc3339("2026-01-01T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);

        let next = next_cron_trigger("0 3 * * *", after).unwrap();
        assert!(next > after);
        // 5 段 `0 3 * * *` 归一为「每天 03:00:00」——同日 03:00 还没到就落在今天。
        assert_eq!(
            next,
            chrono::DateTime::parse_from_rfc3339("2026-01-01T03:00:00Z")
                .unwrap()
                .with_timezone(&Utc)
        );
    }

    #[test]
    fn garbage_cron_yields_no_trigger() {
        let now = Utc::now();
        assert_eq!(next_cron_trigger("definitely not cron", now), None);
    }
}
