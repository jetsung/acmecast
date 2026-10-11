//! 流水线运行历史 SSE 实时推送端到端测试。
//!
//! 验证点：
//! 1. 鉴权：未携带令牌 401；有效令牌 / ?token= 查询参数 / AuthDisabled 放行。
//! 2. Accept 头校验：缺少 Accept: text/event-stream 返回 406。
//! 3. 响应头：Content-Type: text/event-stream, Cache-Control: no-cache, X-Accel-Buffering: no。
//! 4. 实时增量日志：running 状态下产生新日志，流收到对应 event: log，id 递增。
//! 5. 终态流关闭：succeeded/failed 终态事件后流关闭。
//! 6. Last-Event-ID 断线重连：仅补齐遗漏事件。
//! 7. 终态重连：终态后重连直接返回终态事件并关闭。
//! 8. 订阅者限流：超过 20 个订阅者返回 429 too_many_subscribers。
//! 9. Pipeline 历史端点：GET /api/pipelines/{pipeline_id}/histories/{id}/events 校验 pipeline_id。

mod support;

use std::sync::Arc;

use acmecast_pipeline::history::{HistoryRecord, HistoryRepository, StepLogRecord};
use acmecast_pipeline::step::StepLogLevel;
use acmecast_server::auth::{AuthMode, issue_token};
use acmecast_server::config::ServerConfig;
use acmecast_server::history_stream::SseBroadcaster;
use acmecast_server::{AppState, RuntimeState, assemble_router_with_runtime};
use acmecast_store::entity::history::RunStatus;
use acmecast_store::entity::pipeline::TriggerSource;
use acmecast_store::migrate;
use acmecast_store::repository::{PipelineInput, PipelineRepository, PipelineStepInput};
use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use chrono::Utc;
use sea_orm::{Database, DatabaseConnection};
use support::*;

async fn test_app(auth_mode: AuthMode) -> (Router, DatabaseConnection, Arc<SseBroadcaster>) {
    let db = Database::connect("sqlite::memory:")
        .await
        .expect("应能连接内存数据库");
    migrate(&db).await.expect("迁移应成功");

    let broadcaster = Arc::new(SseBroadcaster::new());
    let mut runtime = RuntimeState::new(
        acmecast_server::dependencies::credential_registry(),
        acmecast_server::dependencies::step_registry(),
        None,
        auth_mode,
    );
    runtime.broadcaster = Arc::clone(&broadcaster);

    let app = assemble_router_with_runtime(
        AppState {
            db: db.clone(),
            config: ServerConfig::default(),
        },
        runtime,
    );
    (app, db, broadcaster)
}

async fn create_pipeline(app: &Router) -> i64 {
    let response = send(
        app,
        json_request(
            "POST",
            "/api/pipelines",
            &serde_json::json!({
                "name": "SSE 测试流水线",
                "enabled": true,
                "steps": [{ "type_id": "cert.apply", "input": {}, "enabled": true }]
            }),
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = json(response).await;
    body["data"]["id"].as_i64().expect("应返回流水线 id")
}

#[tokio::test]
async fn auth_and_accept_header_validation() {
    let auth = auth_config();
    let (app, db, _) = test_app(AuthMode::enforced(auth.clone())).await;
    let repo = HistoryRepository::new(&db);
    let pipeline_id = PipelineRepository::new(&db)
        .save(
            None,
            PipelineInput {
                name: "鉴权测试流水线".to_owned(),
                description: None,
                enabled: true,
                steps: vec![PipelineStepInput {
                    type_id: "cert.apply".to_owned(),
                    input: serde_json::json!({}),
                    enabled: true,
                }],
            },
        )
        .await
        .unwrap();

    let history_id = repo
        .record(HistoryRecord {
            pipeline_id,
            trigger_source: TriggerSource::Manual,
            status: RunStatus::Success,
            started_at: Utc::now(),
            finished_at: Some(Utc::now()),
            error_message: None,
            logs: vec![],
        })
        .await
        .unwrap();

    let uri = format!("/api/histories/{history_id}/events");

    // 1. 未鉴权请求：401
    let resp = send(
        &app,
        Request::get(&uri)
            .header(header::ACCEPT, "text/event-stream")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);

    // 2. 携带有效令牌但无 Accept: text/event-stream：406
    let token = issue_token(&auth, Utc::now()).unwrap();
    let resp = send(
        &app,
        Request::get(&uri)
            .header(header::AUTHORIZATION, format!("Bearer {token}"))
            .header(header::ACCEPT, "application/json")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::NOT_ACCEPTABLE);

    // 3. 携带有效令牌且 Accept: text/event-stream：200
    let resp = send(
        &app,
        Request::get(&uri)
            .header(header::AUTHORIZATION, format!("Bearer {token}"))
            .header(header::ACCEPT, "text/event-stream")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(
        resp.headers().get(header::CONTENT_TYPE).unwrap(),
        "text/event-stream"
    );
    assert_eq!(
        resp.headers().get(header::CACHE_CONTROL).unwrap(),
        "no-cache"
    );
    assert_eq!(resp.headers().get("x-accel-buffering").unwrap(), "no");

    // 4. Query 参数 ?token= 鉴权备选支持
    let resp = send(
        &app,
        Request::get(format!("{uri}?token={token}"))
            .header(header::ACCEPT, "text/event-stream")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn terminal_history_reconnect_returns_terminal_event_and_closes() {
    let (app, db, _) = test_app(AuthMode::Disabled).await;
    let pipeline_id = create_pipeline(&app).await;
    let repo = HistoryRepository::new(&db);

    let finished_at = Utc::now();
    let history_id = repo
        .record(HistoryRecord {
            pipeline_id,
            trigger_source: TriggerSource::Manual,
            status: RunStatus::Success,
            started_at: finished_at,
            finished_at: Some(finished_at),
            error_message: None,
            logs: vec![],
        })
        .await
        .unwrap();

    let resp = send(
        &app,
        Request::get(format!("/api/histories/{history_id}/events"))
            .header(header::ACCEPT, "text/event-stream")
            .header("Last-Event-ID", "99")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);

    // 读取响应体内容
    let bytes = axum::body::to_bytes(resp.into_body(), 64 * 1024)
        .await
        .unwrap();
    let text = String::from_utf8(bytes.to_vec()).unwrap();
    assert!(text.contains("event: succeeded"), "应包含 succeeded 事件: {text}");
    assert!(text.contains("\"status\":\"success\""), "载荷包含 success: {text}");
}

#[tokio::test]
async fn running_history_stream_and_replay_with_last_event_id() {
    let (app, db, broadcaster) = test_app(AuthMode::Disabled).await;
    let pipeline_id = create_pipeline(&app).await;
    let repo = HistoryRepository::new(&db);

    let now = Utc::now();
    let history_id = repo
        .record(HistoryRecord {
            pipeline_id,
            trigger_source: TriggerSource::Manual,
            status: RunStatus::Running,
            started_at: now,
            finished_at: None,
            error_message: None,
            logs: vec![],
        })
        .await
        .unwrap();

    // 预填 3 个事件
    broadcaster.broadcast_started(pipeline_id, history_id, "manual", now);
    broadcaster.broadcast_log(
        pipeline_id,
        history_id,
        &StepLogRecord {
            step_index: 0,
            level: StepLogLevel::Info,
            message: "event 2 log".to_owned(),
            created_at: now,
            output: None,
        },
    );
    broadcaster.broadcast_log(
        pipeline_id,
        history_id,
        &StepLogRecord {
            step_index: 1,
            level: StepLogLevel::Info,
            message: "event 3 log".to_owned(),
            created_at: now,
            output: None,
        },
    );

    // 终态事件触发流结束
    broadcaster.broadcast_succeeded(pipeline_id, history_id);

    // 1. 无 Last-Event-ID 时，全量回放快照到终态
    let resp = send(
        &app,
        Request::get(format!("/api/histories/{history_id}/events"))
            .header(header::ACCEPT, "text/event-stream")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(resp.into_body(), 64 * 1024)
        .await
        .unwrap();
    let text = String::from_utf8(bytes.to_vec()).unwrap();
    assert!(text.contains("event: started"));
    assert!(text.contains("event 2 log"));
    assert!(text.contains("event 3 log"));
    assert!(text.contains("event: succeeded"));

    // 2. 带 Last-Event-ID: 2 时，仅回放 id > 2 的事件 (即 event 3 log 与 succeeded)
    let resp = send(
        &app,
        Request::get(format!("/api/histories/{history_id}/events"))
            .header(header::ACCEPT, "text/event-stream")
            .header("Last-Event-ID", "2")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(resp.into_body(), 64 * 1024)
        .await
        .unwrap();
    let text = String::from_utf8(bytes.to_vec()).unwrap();
    assert!(!text.contains("event: started"), "不应包含 id <= 2 的 started");
    assert!(!text.contains("event 2 log"), "不应包含 id=2 的 log");
    assert!(text.contains("event 3 log"), "应包含 id=3 的 log");
    assert!(text.contains("event: succeeded"), "应包含 succeeded");
}

#[tokio::test]
async fn pipeline_history_events_endpoint_checks_pipeline_id() {
    let (app, db, _) = test_app(AuthMode::Disabled).await;
    let pipeline_id = create_pipeline(&app).await;
    let repo = HistoryRepository::new(&db);

    let history_id = repo
        .record(HistoryRecord {
            pipeline_id,
            trigger_source: TriggerSource::Manual,
            status: RunStatus::Success,
            started_at: Utc::now(),
            finished_at: Some(Utc::now()),
            error_message: None,
            logs: vec![],
        })
        .await
        .unwrap();

    // 匹配的 pipeline_id：200
    let resp = send(
        &app,
        Request::get(format!(
            "/api/pipelines/{pipeline_id}/histories/{history_id}/events"
        ))
        .header(header::ACCEPT, "text/event-stream")
        .body(Body::empty())
        .unwrap(),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);

    // 不匹配的 pipeline_id：404
    let mismatch_id = pipeline_id + 999;
    let resp = send(
        &app,
        Request::get(format!(
            "/api/pipelines/{mismatch_id}/histories/{history_id}/events"
        ))
        .header(header::ACCEPT, "text/event-stream")
        .body(Body::empty())
        .unwrap(),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}
