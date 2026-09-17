//! 调度引擎：定时触发（cron）与到期扫描触发（续期）的执行中枢。
//!
//! 对应 `specs/scheduled-renewal/spec.md`。引擎只回答三个问题：
//!
//! 1. **何时触发**——cron 调度按 `next_trigger_at` 到点判定；续期按扫描
//!    周期检查证书仓库、用阈值圈出续期窗口。
//! 2. **是否该触发**——运行中的流水线不重复触发；同一去重窗口内不重复
//!    续期触发（9.4）。
//! 3. **怎么触发**——把 [`LaunchRequest`] 交给注入的 [`PipelineLauncher`]，
//!    并把触发记录落库（9.7）。启动失败不推进触发时间，下一轮再试。
//!
//! 所有判定方法都显式接收 `now` 而不是内部取当前时间：时钟是测试的
//! 最大不确定性来源，把它交到调用方手里，行为才可复现。

use std::sync::Mutex;

use chrono::{DateTime, Utc};
use sea_orm::DatabaseConnection;
use tokio::sync::watch;

use acmecast_store::entity::CertModel;
use acmecast_store::entity::pipeline::TriggerSource;
use acmecast_store::repository::{
    CertRepository, ScheduleRepository, TriggerLogInput, TriggerLogRepository,
};

use crate::config::SchedulerConfig;
use crate::error::Result;
use crate::launcher::{LaunchRequest, PipelineLauncher};

/// 调度引擎。
#[derive(Debug)]
pub struct SchedulerEngine<'a> {
    db: &'a DatabaseConnection,
    launcher: &'a dyn PipelineLauncher,
    config: SchedulerConfig,
    /// 上次到期扫描的时间；`None` 表示启动后还没扫过。
    last_scan_at: Mutex<Option<DateTime<Utc>>>,
}

impl<'a> SchedulerEngine<'a> {
    /// 组装引擎。
    #[must_use]
    pub fn new(
        db: &'a DatabaseConnection,
        launcher: &'a dyn PipelineLauncher,
        config: SchedulerConfig,
    ) -> Self {
        Self {
            db,
            launcher,
            config,
            last_scan_at: Mutex::new(None),
        }
    }

    /// 重启装载（9.5/9.6）。
    ///
    /// 服务启动时调用：把库里全部启用的 cron 调度重新算出 `next_trigger_at`。
    /// 对「触发点已过」的调度按补跑策略处理——`catch_up = true` 时补跑
    /// **最近一次**错过的触发点（且仅此一次，随后的触发点作废）；
    /// `false`（默认）时直接跳过错过的点，等待下一次正常触发。
    ///
    /// 装载不校验 cron——保存时已校验过；解析失败的调度记 error 并跳过，
    /// 不让一条坏配置拖垮整个装载。
    pub async fn load(&self, now: DateTime<Utc>) -> Result<()> {
        let schedules = ScheduleRepository::new(self.db).list_all().await?;

        for schedule in schedules {
            if !schedule.enabled {
                continue;
            }
            let Some(cron) = schedule.cron.as_deref() else {
                // 没有 cron 的调度只参与续期扫描，无需装载触发点。
                continue;
            };

            // 已有未来的触发点（例如刚保存完还没停机）就原样保留。
            if schedule.next_trigger_at.is_some_and(|next| next > now) {
                continue;
            }

            let next = acmecast_store::repository::next_cron_trigger(cron, now);
            if schedule.catch_up {
                // 追赶最近一次：把「错过的最后一个点」补上——实际执行一次。
                // 运行中检查拦下时不记 last（没有真触发），只推进触发点。
                match self
                    .trigger_pipeline(
                        schedule.pipeline_id,
                        TriggerSource::Cron,
                        Some("停机补跑".to_owned()),
                        now,
                    )
                    .await
                {
                    Ok(true) => {
                        // 补跑是一次真实的定时触发，审计里要能看到它。
                        self.record_trigger(
                            schedule.pipeline_id,
                            TriggerSource::Cron,
                            Some("停机补跑".to_owned()),
                            now,
                        )
                        .await?;
                        ScheduleRepository::new(self.db)
                            .mark_triggered(schedule.pipeline_id, Some(Some(now)), next)
                            .await?;
                    }
                    Ok(false) => {
                        ScheduleRepository::new(self.db)
                            .mark_triggered(schedule.pipeline_id, Some(None), next)
                            .await?;
                    }
                    Err(err) => {
                        // 推进失败一并落在这里：下一轮 tick 会看到同样的过期点再试。
                        tracing::error!(
                            pipeline_id = schedule.pipeline_id,
                            error = %err,
                            "补跑流水线失败，保留原触发点"
                        );
                        continue;
                    }
                }
            } else if next.is_some() {
                // 跳过包场：错过的点作废，直接排到未来。
                ScheduleRepository::new(self.db)
                    .mark_triggered(schedule.pipeline_id, Some(None), next)
                    .await?;
            }
        }

        Ok(())
    }

    /// 一轮完整的检查（cron 触发 + 按周期节流的到期扫描）。
    ///
    /// 服务运行期间由 [`SchedulerEngine::serve`] 周期调用；测试可以直接
    /// 传入确定的 `now` 逐轮驱动。
    pub async fn tick(&self, now: DateTime<Utc>) -> Result<()> {
        self.run_cron_due(now).await?;

        let scan_due = {
            let last = self.locked_last_scan();
            match *last {
                Some(previous) => now - previous >= self.config.scan_interval,
                None => true,
            }
        };
        if scan_due {
            self.scan_expiring(now).await?;
            *self.locked_last_scan() = Some(now);
        }

        Ok(())
    }

    /// cron 触发（9.1）：所有到达触发点的调度各触发一次。
    ///
    /// 触发成功后推进触发时间——这一步同时就是 cron 的去重机制：
    /// `next_trigger_at` 被推到未来，同一触发点不会被消费两次。
    async fn run_cron_due(&self, now: DateTime<Utc>) -> Result<()> {
        let schedules = ScheduleRepository::new(self.db).list_all().await?;

        for schedule in schedules {
            if !schedule.enabled {
                continue;
            }
            let Some(cron) = schedule.cron.as_deref() else {
                continue;
            };
            // 未装载触发点（None）或尚未到期都不触发。
            if !schedule.next_trigger_at.is_some_and(|next| next <= now) {
                continue;
            }

            match self
                .trigger_pipeline(schedule.pipeline_id, TriggerSource::Cron, None, now)
                .await
            {
                Ok(true) => {
                    // 定时触发也是一次真实触发，落审计记录（9.7）。
                    self.record_trigger(schedule.pipeline_id, TriggerSource::Cron, None, now)
                        .await?;
                    let next = acmecast_store::repository::next_cron_trigger(cron, now);
                    if next.is_none() {
                        tracing::warn!(
                            pipeline_id = schedule.pipeline_id,
                            cron = %cron,
                            "触发点已消费，但该 cron 表达式推算不出下一次触发时刻，调度将停摆"
                        );
                    }
                    ScheduleRepository::new(self.db)
                        .mark_triggered(schedule.pipeline_id, Some(Some(now)), next)
                        .await?;
                }
                Ok(false) => {
                    // 被运行中检查拦下：本次触发点已消费，推进以避免每轮重复尝试。
                    ScheduleRepository::new(self.db)
                        .mark_triggered(
                            schedule.pipeline_id,
                            Some(None),
                            acmecast_store::repository::next_cron_trigger(cron, now),
                        )
                        .await?;
                }
                Err(err) => {
                    // 启动失败：不推进触发点，下一轮 tick 到点再试。
                    tracing::error!(
                        pipeline_id = schedule.pipeline_id,
                        error = %err,
                        "定时触发流水线失败，保留原触发点待重试"
                    );
                }
            }
        }

        Ok(())
    }

    /// 到期扫描触发（9.3）：把剩余有效期不足阈值的证书找出来，
    /// 触发负责续期它们的流水线。
    ///
    /// 扫描与节流解耦：节流在 [`SchedulerEngine::tick`] 里做，这里每次
    /// 都完整扫一遍——测试与手动触发都不必等扫描周期。
    pub async fn scan_expiring(&self, now: DateTime<Utc>) -> Result<()> {
        let due = CertRepository::new(self.db)
            .list_due(now + self.config.renewal_threshold)
            .await?;
        if due.is_empty() {
            return Ok(());
        }

        let schedules = ScheduleRepository::new(self.db).list_all().await?;
        for cert in due {
            self.renewal_for_cert(&cert, &schedules, now).await?;
        }

        Ok(())
    }

    /// 为一张进入续期窗口的证书找到目标流水线并触发。
    ///
    /// 「证书关联的流水线」按域名交集匹配：证书覆盖的任一域名出现在某条
    /// 调度的 `renewal_domains` 里即命中，取匹配的第一条——同一张证书
    /// 一次扫描只产生一次触发（9.4）。
    async fn renewal_for_cert(
        &self,
        cert: &CertModel,
        schedules: &[acmecast_store::repository::ScheduleConfig],
        now: DateTime<Utc>,
    ) -> Result<()> {
        let domains = cert.domain_set();

        let matched = schedules.iter().find(|schedule| {
            schedule.enabled
                && schedule
                    .renewal_domains
                    .as_ref()
                    .is_some_and(|renewal| domains.iter().any(|domain| renewal.contains(domain)))
        });
        let Some(schedule) = matched else {
            tracing::warn!(
                domains = %cert.domains,
                "证书已进入续期窗口，但没有调度声明负责它的域名"
            );
            return Ok(());
        };

        let detail = format!(
            "证书 {} 距到期不足 {} 天",
            cert.domains,
            self.config.renewal_threshold.num_days()
        );

        match self
            .trigger_pipeline(
                schedule.pipeline_id,
                TriggerSource::Renewal,
                Some(detail.clone()),
                now,
            )
            .await
        {
            // 真正发出了启动请求：落触发记录。launch 失败也落——「试过但
            // 失败」是审计需要知道的事，且失败若是永久性的（比如流水线
            // 配置坏了），靠记录驱动去重窗口节流，才不会每轮扫描都重试
            // 成触发风暴。
            Ok(true) => {
                self.record_trigger(
                    schedule.pipeline_id,
                    TriggerSource::Renewal,
                    Some(detail),
                    now,
                )
                .await?;
            }
            Err(err) => {
                tracing::error!(pipeline_id = schedule.pipeline_id, error = %err, "续期触发流水线失败");
                self.record_trigger(
                    schedule.pipeline_id,
                    TriggerSource::Renewal,
                    Some(format!("{detail}；启动失败：{err}")),
                    now,
                )
                .await?;
            }
            // 被去重或运行中拦下：这不是一次触发，不落记录——否则每次
            // 抑制都会刷新去重窗口的基准点，证书长期在窗口内时审计里
            // 全是「从未发生过的触发」。
            Ok(false) => {}
        }

        // 启动失败不向上抛：一张证书的续期问题不该中断整个扫描，
        // 重试由去重窗口天然节流。
        Ok(())
    }

    /// 落一条触发记录（9.7）。
    ///
    /// 记录的语义是「对这条流水线发出了一次启动」：只在实际 launch 后调用，
    /// 被去重或运行中检查拦下的尝试不算。
    async fn record_trigger(
        &self,
        pipeline_id: i64,
        source: TriggerSource,
        detail: Option<String>,
        now: DateTime<Utc>,
    ) -> Result<()> {
        TriggerLogRepository::new(self.db)
            .record(TriggerLogInput {
                pipeline_id,
                source,
                detail,
                triggered_at: now,
            })
            .await?;
        Ok(())
    }

    /// 触发一条流水线：先过两道去重（9.4），再把启动交给 launcher。
    ///
    /// 返回是否**真正发出**了启动请求；被去重拦下返回 `false`。
    /// launcher 自身失败以 `Err` 上抛，由调用方决定重试策略。
    async fn trigger_pipeline(
        &self,
        pipeline_id: i64,
        source: TriggerSource,
        detail: Option<String>,
        now: DateTime<Utc>,
    ) -> Result<bool> {
        // 运行中的流水线不再触发：并发运行同一流水线没有任何好处，
        // 反而会让证书请求、文件写入互相踩踏。
        if acmecast_pipeline::HistoryRepository::new(self.db)
            .has_running(pipeline_id)
            .await?
        {
            tracing::debug!(pipeline_id, "流水线仍在运行中，跳过本次触发");
            return Ok(false);
        }

        // 续期触发的窗口去重。cron 触发不查窗口——它的去重靠
        // next_trigger_at 推进，重复到点即意味着时间真的到了。
        if source == TriggerSource::Renewal {
            let last = TriggerLogRepository::new(self.db)
                .last_triggered_at(pipeline_id, TriggerSource::Renewal)
                .await?;
            if last.is_some_and(|previous| now - previous <= self.config.dedup_window) {
                tracing::debug!(pipeline_id, "距离上次续期触发不足去重窗口，跳过本次触发");
                return Ok(false);
            }
        }

        self.launcher
            .launch(LaunchRequest {
                pipeline_id,
                source,
                detail,
            })
            .await?;

        Ok(true)
    }

    /// 持续运行：按配置周期调用 [`SchedulerEngine::tick`]，直到收到停机信号。
    ///
    /// 单轮检查的失败只记日志不终止循环——调度是常驻服务，一次抖动
    /// 不该让它退出。
    ///
    /// # 参数
    ///
    /// `shutdown` 由服务端持有发送端；`send(true)` 即停机。
    pub async fn serve(&self, mut shutdown: watch::Receiver<bool>) -> Result<()> {
        let interval = self.config.tick_interval.to_std().map_err(|_| {
            crate::error::Error::InvalidConfig(format!(
                "tick_interval 必须为正，实际为 {:?}",
                self.config.tick_interval
            ))
        })?;

        let mut ticker = tokio::time::interval(interval);
        // 错过的 tick 直接跳过而不是连发补齐：调度按绝对时间判定到点，
        // 补齐的空转 tick 没有意义。
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        loop {
            tokio::select! {
                _ = ticker.tick() => {
                    if let Err(err) = self.tick(Utc::now()).await {
                        tracing::error!(error = %err, "调度 tick 失败");
                    }
                }
                _ = shutdown.changed() => {
                    // 发送端被丢弃或发出停机信号都终止循环。
                    return Ok(());
                }
            }
        }
    }

    /// 取锁。中毒只说明别的线程在持锁时 panic 过——内层数据仍然可用。
    fn locked_last_scan(&self) -> std::sync::MutexGuard<'_, Option<DateTime<Utc>>> {
        self.last_scan_at
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}
