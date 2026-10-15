//! 运行取消令牌。
//!
//! 取消不是杀进程：执行器在步骤边界与每步执行内部感知取消后，走正常的中止
//! 路径——已完成的步骤保留、当前步骤记为失败、终态写回 `failed`。因此取消
//! 是**可观测**的：运行历史里有终态、日志与失败原因，SSE 订阅者也能收到
//! 失败事件。
//!
//! 触发方（HTTP 终止端点、调度器）持令牌的克隆调用 [`CancelToken::cancel`]，
//! 执行侧在下一个检查点停下。取消最多延迟到当前网络操作完成——而每个网络
//! 操作都受 `[network] timeout_secs` 约束，因此最迟超时时长内生效。

/// 一次运行的取消令牌。
///
/// 克隆共享同一份取消状态；三个方法都是同步可从任意线程调用的（`cancelled`
/// 是异步等待，专供执行器在 `select!` 里挂起）。
#[derive(Debug, Clone)]
pub struct CancelToken {
    inner: tokio_util::sync::CancellationToken,
}

impl CancelToken {
    /// 新建一个未取消的令牌。
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: tokio_util::sync::CancellationToken::new(),
        }
    }

    /// 触发取消：此后 [`Self::is_cancelled`] 恒为 `true`，等待中的
    /// [`Self::cancelled`] 立即返回。重复调用是幂等的。
    pub fn cancel(&self) {
        self.inner.cancel();
    }

    /// 是否已被取消。
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.inner.is_cancelled()
    }

    /// 等待取消：已取消则立即返回，否则挂起直到 [`Self::cancel`] 被调用。
    pub async fn cancelled(&self) {
        self.inner.cancelled().await;
    }
}
