//! 流水线运行历史的 Redis / Valkey 缓存层。
//!
//! 运行中将运行概要（`history:{id}` Hash）与日志序列（`history:{id}:logs` List）
//! 增量写入缓存，查询接口优先读缓存以减少数据库负载；终态以整条 JSON
//! 原子落库后回收或失效缓存。
//! 所有缓存操作带有 200ms 超时和降级兜底，缓存服务异常不阻断主业务流程。

use std::sync::Arc;
use std::time::Duration;

use acmecast_pipeline::history::StepLogRecord;
use acmecast_store::entity::{history::RunStatus, pipeline::TriggerSource};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// 运行中历史元数据的缓存默认 TTL：24 小时（86400 秒）。
pub const RUNNING_TTL_SECS: i64 = 86400;

/// 终态后历史元数据保留 TTL：5 分钟（300 秒）。
pub const FINALIZED_TTL_SECS: i64 = 300;

/// 单个历史的最大日志条数上限，防止异常刷屏撑爆内存。
pub const MAX_LOGS_PER_HISTORY: isize = 5000;

/// 操作超时阈值：200 毫秒。
pub const CACHE_TIMEOUT: Duration = Duration::from_millis(200);

/// 运行历史元数据（对应 `history:{id}` 结构）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryMeta {
    /// 运行历史主键 ID。
    pub id: i64,
    /// 所属流水线 ID。
    pub pipeline_id: i64,
    /// 运行触发来源。
    pub trigger_source: TriggerSource,
    /// 运行当前状态。
    pub status: RunStatus,
    /// 开始运行时间。
    pub started_at: DateTime<Utc>,
    /// 结束运行时间（未结束为 None）。
    pub finished_at: Option<DateTime<Utc>>,
    /// 失败错误消息（成功为 None）。
    pub error_message: Option<String>,
}

impl From<HistoryMeta> for acmecast_pipeline::history::HistoryEntry {
    fn from(m: HistoryMeta) -> Self {
        Self {
            id: m.id,
            pipeline_id: m.pipeline_id,
            trigger_source: m.trigger_source,
            status: m.status,
            started_at: m.started_at,
            finished_at: m.finished_at,
            error_message: m.error_message,
        }
    }
}

impl From<&acmecast_pipeline::history::HistoryEntry> for HistoryMeta {
    fn from(e: &acmecast_pipeline::history::HistoryEntry) -> Self {
        Self {
            id: e.id,
            pipeline_id: e.pipeline_id,
            trigger_source: e.trigger_source,
            status: e.status,
            started_at: e.started_at,
            finished_at: e.finished_at,
            error_message: e.error_message.clone(),
        }
    }
}

/// 历史缓存客户端封装。
#[derive(Clone)]
pub struct HistoryCache {
    manager: Option<Arc<redis::aio::ConnectionManager>>,
}

impl std::fmt::Debug for HistoryCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HistoryCache")
            .field("available", &self.is_available())
            .finish()
    }
}

impl HistoryCache {
    /// 构造缓存客户端，若传入 `None` 则所有方法安全回退为无缓存模式。
    #[must_use]
    pub fn new(manager: Option<Arc<redis::aio::ConnectionManager>>) -> Self {
        Self { manager }
    }

    /// 缓存服务是否已配置并可用。
    #[must_use]
    pub fn is_available(&self) -> bool {
        self.manager.is_some()
    }

    /// 元数据缓存键名：`history:{id}`。
    pub fn meta_key(history_id: i64) -> String {
        format!("history:{history_id}")
    }

    /// 日志列表缓存键名：`history:{id}:logs`。
    pub fn logs_key(history_id: i64) -> String {
        format!("history:{history_id}:logs")
    }

    /// 写入或更新运行历史元数据。
    pub async fn set_meta(&self, meta: &HistoryMeta) -> bool {
        let Some(manager) = &self.manager else {
            return false;
        };
        let mut conn = manager.as_ref().clone();
        let key = Self::meta_key(meta.id);
        let ttl = if meta.status == RunStatus::Running {
            RUNNING_TTL_SECS
        } else {
            FINALIZED_TTL_SECS
        };

        let op = async {
            let mut pipe = redis::pipe();
            pipe.hset(&key, "id", meta.id)
                .hset(&key, "pipeline_id", meta.pipeline_id)
                .hset(&key, "trigger_source", meta.trigger_source.as_str())
                .hset(&key, "status", meta.status.as_str())
                .hset(&key, "started_at", meta.started_at.to_rfc3339())
                .hset(
                    &key,
                    "finished_at",
                    meta.finished_at.map(|t| t.to_rfc3339()).unwrap_or_default(),
                )
                .hset(
                    &key,
                    "error_message",
                    meta.error_message.as_deref().unwrap_or_default(),
                )
                .expire(&key, ttl);
            let () = pipe.query_async(&mut conn).await?;
            Ok::<_, redis::RedisError>(())
        };

        match tokio::time::timeout(CACHE_TIMEOUT, op).await {
            Ok(Ok(())) => true,
            Ok(Err(err)) => {
                tracing::warn!(history_id = meta.id, %err, "写入历史元数据缓存失败");
                false
            }
            Err(_) => {
                tracing::warn!(history_id = meta.id, "写入历史元数据缓存超时 (200ms)");
                false
            }
        }
    }

    /// 获取运行历史元数据。未命中或出错返回 `None`。
    pub async fn get_meta(&self, history_id: i64) -> Option<HistoryMeta> {
        let manager = self.manager.as_ref()?;
        let mut conn = manager.as_ref().clone();
        let key = Self::meta_key(history_id);

        let op = async {
            let map: std::collections::HashMap<String, String> = redis::cmd("HGETALL")
                .arg(&key)
                .query_async(&mut conn)
                .await?;
            Ok::<_, redis::RedisError>(map)
        };

        let map = match tokio::time::timeout(CACHE_TIMEOUT, op).await {
            Ok(Ok(m)) if !m.is_empty() => m,
            Ok(Ok(_)) => return None,
            Ok(Err(err)) => {
                tracing::warn!(history_id, %err, "读取历史元数据缓存失败");
                return None;
            }
            Err(_) => {
                tracing::warn!(history_id, "读取历史元数据缓存超时 (200ms)");
                return None;
            }
        };

        let pipeline_id = map.get("pipeline_id")?.parse::<i64>().ok()?;
        let trigger_source = match map.get("trigger_source")?.as_str() {
            "manual" => TriggerSource::Manual,
            "cron" => TriggerSource::Cron,
            "renewal" => TriggerSource::Renewal,
            _ => return None,
        };
        let status = match map.get("status")?.as_str() {
            "running" => RunStatus::Running,
            "success" => RunStatus::Success,
            "failed" => RunStatus::Failed,
            _ => return None,
        };
        let started_at = DateTime::parse_from_rfc3339(map.get("started_at")?)
            .ok()?
            .with_timezone(&Utc);
        let finished_at = map
            .get("finished_at")
            .filter(|s| !s.is_empty())
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .map(|dt| dt.with_timezone(&Utc));
        let error_message = map.get("error_message").filter(|s| !s.is_empty()).cloned();

        Some(HistoryMeta {
            id: history_id,
            pipeline_id,
            trigger_source,
            status,
            started_at,
            finished_at,
            error_message,
        })
    }

    /// 向列表追加一条日志并设置 TTL。
    pub async fn push_log(&self, history_id: i64, log: &StepLogRecord) -> bool {
        let Some(manager) = &self.manager else {
            return false;
        };
        let mut conn = manager.as_ref().clone();
        let key = Self::logs_key(history_id);
        let payload = match serde_json::to_string(log) {
            Ok(s) => s,
            Err(err) => {
                tracing::warn!(history_id, %err, "序列化日志条目失败");
                return false;
            }
        };

        let op = async {
            let mut pipe = redis::pipe();
            pipe.rpush(&key, payload)
                .ltrim(&key, -MAX_LOGS_PER_HISTORY, -1)
                .expire(&key, RUNNING_TTL_SECS);
            let () = pipe.query_async(&mut conn).await?;
            Ok::<_, redis::RedisError>(())
        };

        match tokio::time::timeout(CACHE_TIMEOUT, op).await {
            Ok(Ok(())) => true,
            Ok(Err(err)) => {
                tracing::warn!(history_id, %err, "写入日志缓存失败");
                false
            }
            Err(_) => {
                tracing::warn!(history_id, "写入日志缓存超时 (200ms)");
                false
            }
        }
    }

    /// 获取全量日志列表。未命中或超时返回 `None`。
    pub async fn get_logs(&self, history_id: i64) -> Option<Vec<StepLogRecord>> {
        let manager = self.manager.as_ref()?;
        let mut conn = manager.as_ref().clone();
        let key = Self::logs_key(history_id);

        let op = async {
            let exists: bool = redis::cmd("EXISTS")
                .arg(&key)
                .query_async(&mut conn)
                .await?;
            if !exists {
                return Ok::<_, redis::RedisError>(None);
            }
            let raw_logs: Vec<String> = redis::cmd("LRANGE")
                .arg(&key)
                .arg(0)
                .arg(-1)
                .query_async(&mut conn)
                .await?;
            Ok(Some(raw_logs))
        };

        let raw_opt = match tokio::time::timeout(CACHE_TIMEOUT, op).await {
            Ok(Ok(opt)) => opt?,
            Ok(Err(err)) => {
                tracing::warn!(history_id, %err, "读取日志缓存失败");
                return None;
            }
            Err(_) => {
                tracing::warn!(history_id, "读取日志缓存超时 (200ms)");
                return None;
            }
        };

        let mut logs = Vec::with_capacity(raw_opt.len());
        for item in raw_opt {
            if let Ok(record) = serde_json::from_str::<StepLogRecord>(&item) {
                logs.push(record);
            }
        }
        Some(logs)
    }

    /// 终态清理：更新元数据 TTL 并删除日志列表缓存。
    pub async fn finalize(&self, history_id: i64, final_meta: Option<&HistoryMeta>) -> bool {
        let Some(manager) = &self.manager else {
            return false;
        };
        let mut conn = manager.as_ref().clone();
        let meta_key = Self::meta_key(history_id);
        let logs_key = Self::logs_key(history_id);

        let op = async {
            let mut pipe = redis::pipe();
            if let Some(meta) = final_meta {
                pipe.hset(&meta_key, "status", meta.status.as_str())
                    .hset(
                        &meta_key,
                        "finished_at",
                        meta.finished_at.map(|t| t.to_rfc3339()).unwrap_or_default(),
                    )
                    .hset(
                        &meta_key,
                        "error_message",
                        meta.error_message.as_deref().unwrap_or_default(),
                    )
                    .expire(&meta_key, FINALIZED_TTL_SECS);
            } else {
                pipe.del(&meta_key);
            }
            pipe.del(&logs_key);
            let () = pipe.query_async(&mut conn).await?;
            Ok::<_, redis::RedisError>(())
        };

        match tokio::time::timeout(CACHE_TIMEOUT, op).await {
            Ok(Ok(())) => true,
            Ok(Err(err)) => {
                tracing::warn!(history_id, %err, "终态清理缓存失败");
                false
            }
            Err(_) => {
                tracing::warn!(history_id, "终态清理缓存超时 (200ms)");
                false
            }
        }
    }

    /// 删除指定运行的历史键。
    pub async fn del(&self, history_id: i64) -> bool {
        let Some(manager) = &self.manager else {
            return false;
        };
        let mut conn = manager.as_ref().clone();
        let meta_key = Self::meta_key(history_id);
        let logs_key = Self::logs_key(history_id);

        let op = async {
            let mut pipe = redis::pipe();
            pipe.del(&meta_key).del(&logs_key);
            let () = pipe.query_async(&mut conn).await?;
            Ok::<_, redis::RedisError>(())
        };

        match tokio::time::timeout(CACHE_TIMEOUT, op).await {
            Ok(Ok(())) => true,
            Ok(Err(err)) => {
                tracing::warn!(history_id, %err, "删除历史缓存键失败");
                false
            }
            Err(_) => {
                tracing::warn!(history_id, "删除历史缓存键超时");
                false
            }
        }
    }
}

/// 组装落库与归档用的整条 JSON。
pub fn assemble_final_json(meta: &HistoryMeta, logs: &[StepLogRecord]) -> serde_json::Value {
    serde_json::json!({
        "id": meta.id,
        "pipeline_id": meta.pipeline_id,
        "trigger_source": meta.trigger_source.as_str(),
        "status": meta.status.as_str(),
        "started_at": meta.started_at.to_rfc3339(),
        "finished_at": meta.finished_at.map(|t| t.to_rfc3339()),
        "error_message": meta.error_message,
        "logs": logs,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use acmecast_pipeline::step::StepLogLevel;

    #[tokio::test]
    async fn cache_unavailable_fallback_behavior() {
        let cache = HistoryCache::new(None);
        assert!(!cache.is_available());

        let meta = HistoryMeta {
            id: 100,
            pipeline_id: 1,
            trigger_source: TriggerSource::Manual,
            status: RunStatus::Running,
            started_at: Utc::now(),
            finished_at: None,
            error_message: None,
        };

        assert!(!cache.set_meta(&meta).await);
        assert_eq!(cache.get_meta(100).await, None);

        let log = StepLogRecord {
            step_index: 0,
            level: StepLogLevel::Info,
            message: "test log".to_owned(),
            created_at: Utc::now(),
            output: None,
        };
        assert!(!cache.push_log(100, &log).await);
        assert_eq!(cache.get_logs(100).await, None);
        assert!(!cache.finalize(100, Some(&meta)).await);
        assert!(!cache.del(100).await);
    }

    #[tokio::test]
    async fn assemble_final_json_format() {
        let now = Utc::now();
        let meta = HistoryMeta {
            id: 42,
            pipeline_id: 10,
            trigger_source: TriggerSource::Cron,
            status: RunStatus::Success,
            started_at: now,
            finished_at: Some(now),
            error_message: None,
        };
        let log = StepLogRecord {
            step_index: 1,
            level: StepLogLevel::Info,
            message: "done".to_owned(),
            created_at: now,
            output: Some(serde_json::json!({"ok": true})),
        };
        let val = assemble_final_json(&meta, &[log]);
        assert_eq!(val["id"], 42);
        assert_eq!(val["pipeline_id"], 10);
        assert_eq!(val["trigger_source"], "cron");
        assert_eq!(val["status"], "success");
        assert_eq!(val["logs"][0]["message"], "done");
    }

    #[tokio::test]
    async fn live_cache_roundtrip_if_redis_available() {
        let manager = match crate::try_connect_redis("redis://127.0.0.1:6379").await {
            Some(m) => m,
            None => {
                eprintln!("Redis 不可用，跳过 live roundtrip 测试");
                return;
            }
        };

        let cache = HistoryCache::new(Some(manager));
        assert!(cache.is_available());

        let test_id = 999_888_777;
        let now = Utc::now();
        let meta = HistoryMeta {
            id: test_id,
            pipeline_id: 5,
            trigger_source: TriggerSource::Renewal,
            status: RunStatus::Running,
            started_at: now,
            finished_at: None,
            error_message: None,
        };

        // 1. set_meta & get_meta
        assert!(cache.set_meta(&meta).await);
        let fetched = cache.get_meta(test_id).await.expect("应能读回元数据");
        assert_eq!(fetched.id, test_id);
        assert_eq!(fetched.pipeline_id, 5);
        assert_eq!(fetched.trigger_source, TriggerSource::Renewal);
        assert_eq!(fetched.status, RunStatus::Running);

        // 2. push_log & get_logs
        let log1 = StepLogRecord {
            step_index: 0,
            level: StepLogLevel::Info,
            message: "step 0 started".to_owned(),
            created_at: now,
            output: None,
        };
        let log2 = StepLogRecord {
            step_index: 1,
            level: StepLogLevel::Warn,
            message: "step 1 warned".to_owned(),
            created_at: now,
            output: Some(serde_json::json!({"warning": "dns propagation slow"})),
        };
        assert!(cache.push_log(test_id, &log1).await);
        assert!(cache.push_log(test_id, &log2).await);

        let logs = cache.get_logs(test_id).await.expect("应能读回日志");
        assert_eq!(logs.len(), 2);
        assert_eq!(logs[0].message, "step 0 started");
        assert_eq!(logs[1].message, "step 1 warned");

        // 3. finalize
        let finished_meta = HistoryMeta {
            status: RunStatus::Success,
            finished_at: Some(Utc::now()),
            ..meta
        };
        assert!(cache.finalize(test_id, Some(&finished_meta)).await);

        // logs 应当已被 DEL
        let logs_after = cache.get_logs(test_id).await;
        assert_eq!(logs_after, None);

        // meta 仍保留
        let meta_after = cache.get_meta(test_id).await.expect("终态元数据仍保留");
        assert_eq!(meta_after.status, RunStatus::Success);

        // 4. 清理
        assert!(cache.del(test_id).await);
        assert_eq!(cache.get_meta(test_id).await, None);
    }
}
