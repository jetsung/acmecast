//! 运行并发控制。
//!
//! 两道约束，处置方式不同：
//!
//! - **全局并发上限**：名额满了就**排队等待**（spec 要求「排队等待直至许可」）；
//! - **同一流水线不并行**：已在运行时**直接跳过**这次触发，既不排队也不占名额——
//!   第二次触发多半是定时器到了而上次还没跑完，排队只会让它紧接着再跑一遍。

use std::collections::HashSet;
use std::sync::Mutex;

use tokio::sync::{Semaphore, SemaphorePermit};

/// 运行许可的发放者。
#[derive(Debug)]
pub struct RunScheduler {
    /// 全局并发名额。
    slots: Semaphore,
    /// 正在运行的流水线，用于防重入。
    running: Mutex<HashSet<i64>>,
}

impl RunScheduler {
    /// 按上限建一个调度器。上限至少为 1。
    #[must_use]
    pub fn new(max_concurrent: usize) -> Self {
        Self {
            slots: Semaphore::new(max_concurrent.max(1)),
            running: Mutex::new(HashSet::new()),
        }
    }

    /// 申请一次运行的名额。
    ///
    /// 返回 `None` 表示**同一条流水线已在运行**，本次触发应被跳过。
    /// 全局名额不足时不会返回 `None`，而是在这里挂起等待。
    pub async fn acquire(&self, pipeline_id: i64) -> Option<RunPermit<'_>> {
        // 先认领占用。这一步不涉及等待，因此重复触发能立刻拿到结果。
        let occupancy = Occupancy::claim(&self.running, pipeline_id)?;

        // `acquire` 只在信号量被 close 之后才失败，而本类型从不 close。
        let Ok(slot) = self.slots.acquire().await else {
            // occupancy 在这里被 drop，占用会自动解除。
            return None;
        };

        Some(RunPermit {
            occupancy,
            _slot: slot,
        })
    }

    /// 当前正在运行的流水线数。
    #[must_use]
    pub fn running_count(&self) -> usize {
        self.running
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .len()
    }

    /// 还剩多少并发名额。
    #[must_use]
    pub fn available_slots(&self) -> usize {
        self.slots.available_permits()
    }

    /// 并发上限。
    #[must_use]
    pub fn max_concurrent(&self) -> usize {
        // 名额只减不增，因此「当前可用 + 已借出」恒等于上限。
        self.slots.available_permits() + self.running_count()
    }
}

/// 一次运行的许可；drop 时归还名额并解除占用。
#[derive(Debug)]
pub struct RunPermit<'a> {
    /// 占用标记：代表这条流水线「正在运行」。
    occupancy: Occupancy<'a>,
    /// 全局名额，drop 时自动归还。
    _slot: SemaphorePermit<'a>,
}

impl RunPermit<'_> {
    /// 本次运行属于哪条流水线。
    #[must_use]
    pub fn pipeline_id(&self) -> i64 {
        self.occupancy.pipeline_id
    }
}

/// 「某条流水线正在运行」这个事实的标记。
///
/// RAII 是必需的，不是为了省事：`acquire` 是一个会在**排队途中被取消**的异步函数
/// （调用方超时、上层 future 被 drop 都算）。若把「从集合里移除」写成 acquire
/// 成功路径上的显式调用，取消发生时那条记录就留在了集合里——该流水线此后
/// 再也拿不到名额，而现象只是「它一直不跑」，极难排查。
#[derive(Debug)]
struct Occupancy<'a> {
    pipeline_id: i64,
    running: &'a Mutex<HashSet<i64>>,
}

impl<'a> Occupancy<'a> {
    /// 认领占用；已在运行时返回 `None`。
    fn claim(running: &'a Mutex<HashSet<i64>>, pipeline_id: i64) -> Option<Self> {
        {
            let mut set = running
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if !set.insert(pipeline_id) {
                return None;
            }
        }
        Some(Self {
            pipeline_id,
            running,
        })
    }
}

impl Drop for Occupancy<'_> {
    fn drop(&mut self) {
        self.running
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(&self.pipeline_id);
    }
}
