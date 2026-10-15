//! 运行控制：正在本进程内执行的流水线运行的取消令牌注册表。
//!
//! 「取消一次运行」的请求经它找到对应运行的任务令牌并触发；执行侧
//! （[`crate::run::execute_run`]）在步骤边界与步骤执行内部感知取消，
//! 走正常的中止路径写回终态。注册表只在内存里：令牌的生命周期与运行
//! 任务一致，进程退出后自然清空（跨进程遗留的 running 记录由启动时
//! 的 `fail_stale_running` 与终止接口的「无令牌直接清理」兜底）。

use std::collections::HashMap;
use std::sync::Mutex;

use acmecast_pipeline::CancelToken;

/// 运行控制注册表：history_id → 取消令牌。
#[derive(Debug, Default)]
pub struct RunControl {
    running: Mutex<HashMap<i64, CancelToken>>,
}

impl RunControl {
    /// 新建空注册表。
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// 登记一次运行的取消令牌。
    ///
    /// 同一运行重复登记返回 `false`（正常运行每个 history_id 只执行一次，
    /// 重复登记意味着装配错误，调用方应保留首个令牌）。
    pub fn register(&self, history_id: i64, token: CancelToken) -> bool {
        let mut guard = self.lock();
        match guard.entry(history_id) {
            std::collections::hash_map::Entry::Vacant(slot) => {
                slot.insert(token);
                true
            }
            std::collections::hash_map::Entry::Occupied(_) => false,
        }
    }

    /// 触发一次运行的取消；该运行不在本进程执行中时返回 `false`。
    pub fn cancel(&self, history_id: i64) -> bool {
        let guard = self.lock();
        match guard.get(&history_id) {
            Some(token) => {
                token.cancel();
                true
            }
            None => false,
        }
    }

    /// 注销一次运行（终态写回完成后调用，防止映射无限增长）。
    pub fn remove(&self, history_id: i64) {
        self.lock().remove(&history_id);
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<i64, CancelToken>> {
        self.running
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 登记后可取消、可注销；未登记的取消返回 false；重复登记被拒。
    #[test]
    fn register_cancel_remove_roundtrip() {
        let control = RunControl::new();
        let token = CancelToken::new();

        assert!(control.register(1, token.clone()));
        assert!(!control.register(1, CancelToken::new()), "重复登记应被拒");

        assert!(!token.is_cancelled());
        assert!(control.cancel(1));
        assert!(token.is_cancelled(), "触发后令牌应处于已取消状态");

        control.remove(1);
        assert!(!control.cancel(1), "注销后取消应找不到令牌");
    }
}
