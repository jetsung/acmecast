//! 调度器配置。
//!
//! 全部带默认值：定时触发依赖 cron 表达式本身，扫描类参数用保守的
//! 缺省即可上线，精细调整留给部署方。

use chrono::Duration;

/// 调度器的运行参数。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchedulerConfig {
    /// cron 检查周期。
    ///
    /// 引擎每隔这么久醒来检查一次「有没有调度到达触发点」。它决定 cron
    /// 触发的**精度上界**——间隔 30 秒意味着触发最多迟到 30 秒；不影响
    /// 去重与推进逻辑（那些以 `next_trigger_at` 为准，与检查频率无关）。
    pub tick_interval: Duration,

    /// 证书到期扫描周期。
    ///
    /// 续期触发由扫描驱动，扫描本身按这个间隔节流——tick 再频繁，
    /// 扫描也不会更勤。与续期阈值配合：阈值 30 天、扫描 1 小时，
    /// 意味着证书进入续期窗口后最迟 1 小时内被触发。
    pub scan_interval: Duration,

    /// 续期阈值：剩余有效期小于等于它时证书进入续期窗口。
    ///
    /// 与 spec 一致取 30 天——足够完成一次无人值守的重签与部署，
    /// 又不至于把证书「白签」太多天。
    pub renewal_threshold: Duration,

    /// 续期触发的去重窗口。
    ///
    /// 同一流水线两次续期触发之间的最小间隔（9.4：同一时间窗口不重复
    /// 触发）。窗口取值应当覆盖「一次重签流水线跑完」的合理时长。
    pub dedup_window: Duration,
}

impl Default for SchedulerConfig {
    fn default() -> Self {
        Self {
            tick_interval: Duration::seconds(30),
            scan_interval: Duration::hours(1),
            renewal_threshold: Duration::days(30),
            dedup_window: Duration::hours(1),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_the_spec() {
        let config = SchedulerConfig::default();
        // tick 只决定触发精度；阈值 30 天与 spec 的续期窗口一致。
        assert_eq!(config.tick_interval, Duration::seconds(30));
        assert_eq!(config.renewal_threshold, Duration::days(30));
    }
}
