//! 流水线运行历史的 SSE 实时推送与广播。
//!
//! 提供 `GET /api/histories/{id}/events` 与 `GET /api/pipelines/{pipeline_id}/histories/{id}/events`
//! 端点，向订阅客户端推送运行生命周期事件（`started/log/step_finished/succeeded/failed`）。
//! 支持 `Last-Event-ID` 断线重连补齐、20 订阅者背压上限、64 KiB 消息截断与 15 秒空闲保活。

use std::collections::HashMap;
use std::convert::Infallible;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::Duration;

use acmecast_pipeline::event::PipelineEvent;
use acmecast_pipeline::history::{HistoryRepository, StepLogRecord};
use acmecast_store::entity::history::RunStatus;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::http::header::{ACCEPT, HeaderMap, HeaderName, HeaderValue};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use chrono::Utc;
use sea_orm::EntityTrait;
use serde::{Deserialize, Serialize};

use crate::HttpState;
use crate::history_cache::HistoryCache;
use crate::response::ApiError;

/// 每个历史最大 SSE 订阅者数量上限。
pub const MAX_SUBSCRIBERS_PER_HISTORY: usize = 20;

/// 广播通道容量。
pub const BROADCAST_CHANNEL_CAPACITY: usize = 256;

/// 单条事件 payload 最大字节数 (64 KiB)。
pub const MAX_DATA_BYTES: usize = 64 * 1024;

/// SSE 事件消息结构。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SseMessage {
    /// 递增事件序列号。
    pub id: u64,
    /// 事件类型名 (`started`, `log`, `step_finished`, `succeeded`, `failed`)。
    pub event: String,
    /// JSON 格式的数据载荷。
    pub data: serde_json::Value,
}

/// 限制单条事件 data 大小不超过 64 KiB。
/// 若超限则截断 `message` 字段并追加 `…[truncated]`。
pub fn enforce_max_data_size(data: &serde_json::Value) -> String {
    let serialized = serde_json::to_string(data).unwrap_or_default();
    if serialized.len() <= MAX_DATA_BYTES {
        return serialized;
    }

    let mut cloned = data.clone();
    if let Some(msg_val) = cloned.get_mut("message") {
        let msg_str_opt = msg_val.as_str().map(|s| s.to_owned());
        if let Some(msg_str) = msg_str_opt {
            let overflow = serialized.len().saturating_sub(MAX_DATA_BYTES);
            let suffix = "…[truncated]";
            let target_len = msg_str.len().saturating_sub(overflow + suffix.len() + 16);
            let truncated = if target_len > 0 && target_len < msg_str.len() {
                let mut indices = msg_str.char_indices();
                let split = indices
                    .nth(target_len)
                    .map(|(i, _)| i)
                    .unwrap_or(msg_str.len());
                format!("{}{suffix}", &msg_str[..split])
            } else {
                suffix.to_owned()
            };
            *msg_val = serde_json::Value::String(truncated);
        }
    }

    let mut res = serde_json::to_string(&cloned).unwrap_or_default();
    if res.len() > MAX_DATA_BYTES {
        res.truncate(MAX_DATA_BYTES - 15);
        res.push_str("…[truncated]");
    }
    res
}

/// 单个历史运行的流分发通道与状态。
pub struct HistoryStreamChannel {
    /// 广播通道发送端。
    pub sender: tokio::sync::broadcast::Sender<Arc<SseMessage>>,
    /// 活跃订阅者计数。
    pub subscriber_count: Arc<AtomicUsize>,
    /// 下一个分配的事件 ID。
    pub next_id: AtomicU64,
    /// 缓冲的事件历史，供重放与快照。
    pub events: std::sync::RwLock<Vec<Arc<SseMessage>>>,
    /// 终态事件（如果已结束）。
    pub terminal: std::sync::RwLock<Option<Arc<SseMessage>>>,
}

impl std::fmt::Debug for HistoryStreamChannel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HistoryStreamChannel")
            .field("subscribers", &self.subscriber_count.load(Ordering::SeqCst))
            .field("next_id", &self.next_id.load(Ordering::SeqCst))
            .finish()
    }
}

impl HistoryStreamChannel {
    fn new() -> Self {
        let (sender, _) = tokio::sync::broadcast::channel(BROADCAST_CHANNEL_CAPACITY);
        Self {
            sender,
            subscriber_count: Arc::new(AtomicUsize::new(0)),
            next_id: AtomicU64::new(1),
            events: std::sync::RwLock::new(Vec::new()),
            terminal: std::sync::RwLock::new(None),
        }
    }
}

/// 订阅者计数递减守护，在流关闭或客户端断开时自动释放资源。
#[derive(Debug)]
pub struct SubscriberGuard {
    count: Arc<AtomicUsize>,
}

impl Drop for SubscriberGuard {
    fn drop(&mut self) {
        self.count.fetch_sub(1, Ordering::SeqCst);
    }
}

/// 集中管理的 SSE 广播器。
#[derive(Clone, Default)]
pub struct SseBroadcaster {
    channels: Arc<std::sync::RwLock<HashMap<i64, Arc<HistoryStreamChannel>>>>,
}

impl std::fmt::Debug for SseBroadcaster {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SseBroadcaster").finish()
    }
}

impl SseBroadcaster {
    /// 创建一个新的广播管理器。
    #[must_use]
    pub fn new() -> Self {
        Self {
            channels: Arc::new(std::sync::RwLock::new(HashMap::new())),
        }
    }

    /// 获取或创建指定 history 的通道。
    pub fn get_or_create_channel(&self, history_id: i64) -> Arc<HistoryStreamChannel> {
        let mut map = self.channels.write().unwrap();
        map.entry(history_id)
            .or_insert_with(|| Arc::new(HistoryStreamChannel::new()))
            .clone()
    }

    /// 获取指定 history 的通道（若存在）。
    pub fn get_channel(&self, history_id: i64) -> Option<Arc<HistoryStreamChannel>> {
        let map = self.channels.read().unwrap();
        map.get(&history_id).cloned()
    }

    /// 发送并缓冲一个事件。
    pub fn publish_event(&self, history_id: i64, event_type: &str, data: serde_json::Value) -> u64 {
        let channel = self.get_or_create_channel(history_id);
        let id = channel.next_id.fetch_add(1, Ordering::SeqCst);
        let msg = Arc::new(SseMessage {
            id,
            event: event_type.to_owned(),
            data,
        });

        // 终态事件特殊记录
        if event_type == "succeeded" || event_type == "failed" {
            let mut term = channel.terminal.write().unwrap();
            *term = Some(Arc::clone(&msg));
        }

        {
            let mut events = channel.events.write().unwrap();
            events.push(Arc::clone(&msg));
        }

        // 向活跃订阅者发送广播
        let _ = channel.sender.send(msg);
        id
    }

    /// 广播流水线启动事件。
    pub fn broadcast_started(
        &self,
        pipeline_id: i64,
        history_id: i64,
        trigger_source: &str,
        started_at: chrono::DateTime<Utc>,
    ) -> u64 {
        let channel = self.get_or_create_channel(history_id);
        // 若已存在 started 事件，不重复发布
        {
            let events = channel.events.read().unwrap();
            if let Some(first) = events.first().filter(|e| e.event == "started") {
                return first.id;
            }
        }
        self.publish_event(
            history_id,
            "started",
            serde_json::json!({
                "pipeline_id": pipeline_id,
                "trigger_source": trigger_source,
                "started_at": started_at,
            }),
        )
    }

    /// 广播单条增量日志。
    pub fn broadcast_log(&self, _pipeline_id: i64, history_id: i64, log: &StepLogRecord) -> u64 {
        self.publish_event(
            history_id,
            "log",
            serde_json::json!({
                "step_index": log.step_index,
                "level": log.level.as_str(),
                "message": log.message,
                "output": log.output,
                "created_at": log.created_at,
            }),
        )
    }

    /// 广播步骤完成事件。
    pub fn broadcast_step_finished(
        &self,
        _pipeline_id: i64,
        history_id: i64,
        step_order: i32,
        type_id: &str,
        skipped: bool,
    ) -> u64 {
        self.publish_event(
            history_id,
            "step_finished",
            serde_json::json!({
                "step_index": step_order,
                "type_id": type_id,
                "status": if skipped { "skipped" } else { "success" },
            }),
        )
    }

    /// 广播运行成功终态事件。
    pub fn broadcast_succeeded(&self, _pipeline_id: i64, history_id: i64) -> u64 {
        self.publish_event(
            history_id,
            "succeeded",
            serde_json::json!({
                "status": "success",
                "finished_at": Utc::now(),
                "error_message": null,
            }),
        )
    }

    /// 广播运行失败终态事件。
    pub fn broadcast_failed(
        &self,
        _pipeline_id: i64,
        history_id: i64,
        _step_order: i32,
        _type_id: &str,
        reason: &str,
    ) -> u64 {
        self.publish_event(
            history_id,
            "failed",
            serde_json::json!({
                "status": "failed",
                "finished_at": Utc::now(),
                "error_message": reason,
            }),
        )
    }

    /// 订阅指定 history 的事件流。
    /// 检查 20 订阅者上限，若超限返回 429。
    #[allow(clippy::type_complexity)]
    pub fn subscribe(
        &self,
        history_id: i64,
    ) -> Result<
        (
            tokio::sync::broadcast::Receiver<Arc<SseMessage>>,
            Vec<Arc<SseMessage>>,
            Option<Arc<SseMessage>>,
            SubscriberGuard,
        ),
        ApiError,
    > {
        let channel = self.get_or_create_channel(history_id);
        let cur = channel.subscriber_count.fetch_add(1, Ordering::SeqCst);
        if cur >= MAX_SUBSCRIBERS_PER_HISTORY {
            channel.subscriber_count.fetch_sub(1, Ordering::SeqCst);
            return Err(ApiError::new(
                StatusCode::TOO_MANY_REQUESTS,
                "too_many_subscribers",
                "历史运行 SSE 订阅者数量已达上限 (20)",
            ));
        }

        let guard = SubscriberGuard {
            count: Arc::clone(&channel.subscriber_count),
        };
        let rx = channel.sender.subscribe();
        let events = channel.events.read().unwrap().clone();
        let terminal = channel.terminal.read().unwrap().clone();

        Ok((rx, events, terminal, guard))
    }
}

#[async_trait::async_trait]
impl acmecast_pipeline::EventSink for SseBroadcaster {
    async fn publish(&self, event: PipelineEvent) {
        match event {
            PipelineEvent::Started {
                pipeline_id,
                run_id,
                trigger_source,
            } => {
                self.broadcast_started(pipeline_id, run_id, &trigger_source, Utc::now());
            }
            PipelineEvent::Log {
                pipeline_id,
                run_id,
                log,
            } => {
                self.broadcast_log(pipeline_id, run_id, &log);
            }
            PipelineEvent::StepFinished {
                pipeline_id,
                run_id,
                step_order,
                type_id,
                skipped,
            } => {
                self.broadcast_step_finished(pipeline_id, run_id, step_order, &type_id, skipped);
            }
            PipelineEvent::Succeeded {
                pipeline_id,
                run_id,
                ..
            } => {
                self.broadcast_succeeded(pipeline_id, run_id);
            }
            PipelineEvent::Failed {
                pipeline_id,
                run_id,
                step_order,
                type_id,
                reason,
            } => {
                self.broadcast_failed(pipeline_id, run_id, step_order, &type_id, &reason);
            }
        }
    }
}

/// 格式化单个 SSE Event。
fn format_sse_event(msg: &SseMessage) -> Event {
    Event::default()
        .id(msg.id.to_string())
        .event(&msg.event)
        .data(enforce_max_data_size(&msg.data))
}

/// 构造标准的 SSE 响应，带 15s ping 保活与无缓冲头。
fn create_sse_response<S>(stream: S) -> Response
where
    S: futures_util::Stream<Item = Result<Event, Infallible>> + Send + 'static,
{
    let mut resp = Sse::new(stream)
        .keep_alive(
            KeepAlive::new()
                .interval(Duration::from_secs(15))
                .text("ping"),
        )
        .into_response();

    let headers = resp.headers_mut();
    headers.insert(
        HeaderName::from_static("x-accel-buffering"),
        HeaderValue::from_static("no"),
    );
    headers.insert(
        HeaderName::from_static("cache-control"),
        HeaderValue::from_static("no-cache"),
    );
    headers.insert(
        HeaderName::from_static("content-type"),
        HeaderValue::from_static("text/event-stream"),
    );
    resp
}

/// 核心事件流处理逻辑。
async fn build_history_event_stream(
    state: &HttpState,
    headers: &HeaderMap,
    id: i64,
) -> Result<Response, ApiError> {
    // 1. 校验 Accept 请求头必须包含 text/event-stream
    let accept = headers
        .get(ACCEPT)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    if !accept.contains("text/event-stream") && !accept.contains("*/*") {
        return Err(ApiError::new(
            StatusCode::NOT_ACCEPTABLE,
            "not_acceptable",
            "请求必须包含 Accept: text/event-stream",
        ));
    }

    // 2. 检查运行历史是否存在及当前状态
    let cache = HistoryCache::new(state.redis());
    let (status, pipeline_id, trigger_source, started_at, finished_at, error_msg) =
        if let Some(meta) = cache.get_meta(id).await {
            (
                meta.status,
                meta.pipeline_id,
                meta.trigger_source.as_str().to_owned(),
                meta.started_at,
                meta.finished_at,
                meta.error_message,
            )
        } else {
            // 回源数据库
            let row = acmecast_store::entity::history::Entity::find_by_id(id)
                .one(&state.app.db)
                .await
                .map_err(|e| ApiError::from(acmecast_store::Error::from(e)))?
                .ok_or_else(|| {
                    ApiError::new(
                        StatusCode::NOT_FOUND,
                        "not_found",
                        format!("运行历史不存在: {id}"),
                    )
                })?;
            let status = RunStatus::parse(&row.status).unwrap_or(RunStatus::Failed);
            (
                status,
                row.pipeline_id,
                row.trigger_source,
                row.started_at,
                row.finished_at,
                row.error_message,
            )
        };

    // 3. 终态已达成时直接推送终态事件并关闭
    if status != RunStatus::Running {
        let event_name = if status == RunStatus::Success {
            "succeeded"
        } else {
            "failed"
        };
        let data = serde_json::json!({
            "status": status.as_str(),
            "finished_at": finished_at.unwrap_or_else(Utc::now),
            "error_message": error_msg,
        });
        let msg = SseMessage {
            id: 1,
            event: event_name.to_owned(),
            data,
        };
        let stream = futures_util::stream::once(async move { Ok(format_sse_event(&msg)) });
        return Ok(create_sse_response(stream));
    }

    // 4. 处于 Running 状态，订阅广播
    let (mut rx, mut buffered_events, terminal_event, guard) = state.broadcaster().subscribe(id)?;

    // 若 buffered_events 为空（例如服务重启后首个订阅），从缓存/DB 组装快照
    if buffered_events.is_empty() {
        let mut snapshot = Vec::new();
        // 1) started
        snapshot.push(Arc::new(SseMessage {
            id: 1,
            event: "started".to_owned(),
            data: serde_json::json!({
                "pipeline_id": pipeline_id,
                "trigger_source": trigger_source,
                "started_at": started_at,
            }),
        }));
        // 2) logs
        let logs = if let Some(logs) = cache.get_logs(id).await {
            logs
        } else {
            HistoryRepository::new(&state.app.db)
                .logs_of(id)
                .await
                .unwrap_or_default()
        };
        for (idx, log) in logs.into_iter().enumerate() {
            snapshot.push(Arc::new(SseMessage {
                id: 2 + idx as u64,
                event: "log".to_owned(),
                data: serde_json::json!({
                    "step_index": log.step_index,
                    "level": log.level.as_str(),
                    "message": log.message,
                    "output": log.output,
                    "created_at": log.created_at,
                }),
            }));
        }
        buffered_events = snapshot;
    }

    // 5. 解析 Last-Event-ID
    let last_event_id: Option<u64> = headers
        .get("Last-Event-ID")
        .or_else(|| headers.get("last-event-id"))
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse().ok());

    // 过滤需要重放的事件
    let replayed: Vec<Arc<SseMessage>> = if let Some(last_id) = last_event_id {
        buffered_events
            .into_iter()
            .filter(|e| e.id > last_id)
            .collect()
    } else {
        buffered_events
    };

    let mut last_seen_id = last_event_id.unwrap_or(0);
    for e in &replayed {
        if e.id > last_seen_id {
            last_seen_id = e.id;
        }
    }

    let stream = async_stream::stream! {
        let _guard = guard; // 离开作用域时自动递减订阅者计数

        // 先重放历史/快照事件
        for msg in replayed {
            let is_terminal = msg.event == "succeeded" || msg.event == "failed";
            yield Ok::<_, Infallible>(format_sse_event(&msg));
            if is_terminal {
                return;
            }
        }

        // 若重放中已有终态，直接退出
        if terminal_event.as_ref().is_some_and(|term| term.id <= last_seen_id) {
            return;
        }

        // 持续消费广播事件
        loop {
            match rx.recv().await {
                Ok(msg) => {
                    if msg.id <= last_seen_id {
                        continue;
                    }
                    last_seen_id = msg.id;
                    let is_terminal = msg.event == "succeeded" || msg.event == "failed";
                    yield Ok(format_sse_event(&msg));
                    if is_terminal {
                        return;
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                    tracing::warn!(history_id = id, skipped, "SSE 订阅者落后广播通道");
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                    return;
                }
            }
        }
    };

    Ok(create_sse_response(stream))
}

/// `GET /api/histories/{id}/events` 端点处理函数。
pub(crate) async fn handle_history_events(
    State(state): State<HttpState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> Result<Response, ApiError> {
    build_history_event_stream(&state, &headers, id).await
}

/// `GET /api/pipelines/{pipeline_id}/histories/{id}/events` 端点处理函数。
pub(crate) async fn handle_pipeline_history_events(
    State(state): State<HttpState>,
    headers: HeaderMap,
    Path((pipeline_id, id)): Path<(i64, i64)>,
) -> Result<Response, ApiError> {
    // 校验流水线与历史匹配
    let cache = HistoryCache::new(state.redis());
    let actual_pipeline_id = if let Some(meta) = cache.get_meta(id).await {
        Some(meta.pipeline_id)
    } else {
        acmecast_store::entity::history::Entity::find_by_id(id)
            .one(&state.app.db)
            .await
            .map_err(|e| ApiError::from(acmecast_store::Error::from(e)))?
            .map(|r| r.pipeline_id)
    };

    match actual_pipeline_id {
        Some(actual) if actual == pipeline_id => {
            build_history_event_stream(&state, &headers, id).await
        }
        _ => Err(ApiError::new(
            StatusCode::NOT_FOUND,
            "not_found",
            format!("流水线 {pipeline_id} 下不存在运行历史 {id}"),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use acmecast_pipeline::step::StepLogLevel;

    #[test]
    fn enforce_max_data_size_truncates_large_message() {
        let small_data = serde_json::json!({
            "message": "normal log message",
            "level": "info",
        });
        let res = enforce_max_data_size(&small_data);
        assert_eq!(res, serde_json::to_string(&small_data).unwrap());

        let huge_msg = "a".repeat(100 * 1024);
        let large_data = serde_json::json!({
            "message": huge_msg,
            "level": "info",
        });
        let res = enforce_max_data_size(&large_data);
        assert!(res.len() <= MAX_DATA_BYTES);
        assert!(res.contains("…[truncated]"));
    }

    #[tokio::test]
    async fn broadcaster_event_sequence_and_subscriber_limit() {
        let broadcaster = SseBroadcaster::new();
        let history_id = 12345;

        // 1. broadcast started and log
        let id1 = broadcaster.broadcast_started(1, history_id, "manual", Utc::now());
        assert_eq!(id1, 1);

        let log = StepLogRecord {
            step_index: 0,
            level: StepLogLevel::Info,
            message: "first log".to_owned(),
            created_at: Utc::now(),
            output: None,
        };
        let id2 = broadcaster.broadcast_log(1, history_id, &log);
        assert_eq!(id2, 2);

        // 2. subscribe
        let (mut rx, events, terminal, _guard) =
            broadcaster.subscribe(history_id).expect("应能订阅");
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].event, "started");
        assert_eq!(events[1].event, "log");
        assert!(terminal.is_none());

        // 3. send new log
        let log2 = StepLogRecord {
            step_index: 1,
            level: StepLogLevel::Info,
            message: "second log".to_owned(),
            created_at: Utc::now(),
            output: None,
        };
        broadcaster.broadcast_log(1, history_id, &log2);

        let received = rx.recv().await.expect("应收到广播");
        assert_eq!(received.id, 3);
        assert_eq!(received.event, "log");

        // 4. 超过 20 个订阅者限流
        let mut guards = Vec::new();
        for _ in 0..19 {
            let (_, _, _, g) = broadcaster
                .subscribe(history_id)
                .expect("应能继续订阅至 20");
            guards.push(g);
        }
        let overflow = broadcaster.subscribe(history_id);
        assert!(overflow.is_err(), "第 21 个订阅应返回 429");
        let err = overflow.unwrap_err();
        assert_eq!(err.status(), StatusCode::TOO_MANY_REQUESTS);

        // 释放一个后可再次订阅
        drop(guards.pop());
        assert!(broadcaster.subscribe(history_id).is_ok());
    }
}
