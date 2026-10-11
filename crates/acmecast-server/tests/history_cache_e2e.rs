//! 运行历史与 Redis / Valkey 缓存集成的端到端测试。
//!
//! 验证点：
//! 1. `get_history` / `history_logs` / `list_histories_for` 优先读缓存。
//! 2. 缓存 miss 时回源数据库，并在 `running` 状态时回填缓存。
//! 3. 缓存命中时直接返回缓存数据。
//! 4. 当 Redis 未配置或不可用时，优雅回退到数据库直读直写，所有端点正常工作。

mod support;

use std::sync::Arc;

use acmecast_pipeline::history::{HistoryRecord, HistoryRepository, StepLogRecord};
use acmecast_pipeline::step::StepLogLevel;
use acmecast_server::auth::AuthMode;
use acmecast_server::config::ServerConfig;
use acmecast_server::history_cache::{HistoryCache, HistoryMeta};
use acmecast_server::{AppState, RuntimeState, assemble_router_with_runtime};
use acmecast_store::entity::history::RunStatus;
use acmecast_store::entity::pipeline::TriggerSource;
use acmecast_store::migrate;
use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use chrono::Utc;
use sea_orm::{Database, DatabaseConnection};
use support::*;

/// 装配一个带可选 Redis 连接的测试路由。
async fn app_with_redis(redis_manager: Option<Arc<redis::aio::ConnectionManager>>) -> (Router, DatabaseConnection) {
    let db = Database::connect("sqlite::memory:")
        .await
        .expect("应能连接内存数据库");
    migrate(&db).await.expect("迁移应成功");

    let mut runtime = RuntimeState::new(
        acmecast_server::dependencies::credential_registry(),
        acmecast_server::dependencies::step_registry(),
        None,
        AuthMode::Disabled,
    );
    runtime.redis = redis_manager;

    let app = assemble_router_with_runtime(
        AppState {
            db: db.clone(),
            config: ServerConfig::default(),
        },
        runtime,
    );
    (app, db)
}

async fn create_pipeline(app: &Router) -> i64 {
    let response = send(
        app,
        json_request(
            "POST",
            "/api/pipelines",
            &serde_json::json!({
                "name": "缓存测试流水线",
                "enabled": true,
                "steps": [{ "type_id": "cert.apply", "input": {}, "enabled": true }]
            }),
        ),
    )
    .await;
    let status = response.status();
    let body = json(response).await;
    assert_eq!(status, StatusCode::OK, "status: {status}, body: {body}");
    body["data"]["id"].as_i64().expect("应返回流水线 id")
}

#[tokio::test]
async fn fallback_when_redis_is_none() {
    let (app, db) = app_with_redis(None).await;
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
            logs: vec![StepLogRecord {
                step_index: 0,
                level: StepLogLevel::Info,
                message: "fallback startup log".to_owned(),
                created_at: now,
                output: None,
            }],
        })
        .await
        .expect("记录历史应成功");

    // 1. get_history 直读 DB 成功
    let resp = send(
        &app,
        Request::get(format!("/api/histories/{history_id}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = json(resp).await;
    assert_eq!(body["data"]["status"], "running");

    // 2. history_logs 直读 DB 成功
    let resp = send(
        &app,
        Request::get(format!("/api/histories/{history_id}/logs"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = json(resp).await;
    let logs = body["data"].as_array().expect("应为数组");
    assert_eq!(logs.len(), 1);
    assert_eq!(logs[0]["message"], "fallback startup log");

    // 3. list_histories 直读 DB 成功
    let resp = send(
        &app,
        Request::get("/api/histories").body(Body::empty()).unwrap(),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = json(resp).await;
    assert_eq!(body["data"]["total"], 1);
}

#[tokio::test]
async fn cache_hit_miss_and_backfill_with_live_redis() {
    let redis_manager = match acmecast_server::try_connect_redis("redis://127.0.0.1:6379").await {
        Some(m) => m,
        None => {
            eprintln!("Redis 不可用，跳过 live cache 集成测试");
            return;
        }
    };

    let (app, db) = app_with_redis(Some(redis_manager.clone())).await;
    let pipeline_id = create_pipeline(&app).await;
    let cache = HistoryCache::new(Some(redis_manager));
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
            logs: vec![StepLogRecord {
                step_index: 0,
                level: StepLogLevel::Info,
                message: "initial db log".to_owned(),
                created_at: now,
                output: None,
            }],
        })
        .await
        .expect("记录历史应成功");

    // 确保测试前缓存中不存在该 history
    cache.del(history_id).await;

    // 1. 初次调用 get_history：cache miss，回源 DB 并回填缓存
    let resp = send(
        &app,
        Request::get(format!("/api/histories/{history_id}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = json(resp).await;
    assert_eq!(body["data"]["status"], "running");

    // 验证缓存已被回填
    let cached_meta = cache.get_meta(history_id).await;
    assert!(cached_meta.is_some(), "Running 状态的 get_history 应当回填缓存");
    let cached_meta = cached_meta.unwrap();
    assert_eq!(cached_meta.id, history_id);
    assert_eq!(cached_meta.status, RunStatus::Running);

    // 2. 模拟缓存被外部更新（例如修改 error_message），再次 get_history 应 hit 缓存返回更新后的数据
    let updated_meta = HistoryMeta {
        error_message: Some("cache updated message".to_owned()),
        ..cached_meta
    };
    cache.set_meta(&updated_meta).await;

    let resp = send(
        &app,
        Request::get(format!("/api/histories/{history_id}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = json(resp).await;
    assert_eq!(
        body["data"]["error_message"], "cache updated message",
        "应直接命中缓存返回更新后的数据"
    );

    // 3. 初次调用 history_logs：cache miss，回源 DB 并回填日志到缓存
    let resp = send(
        &app,
        Request::get(format!("/api/histories/{history_id}/logs"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = json(resp).await;
    let logs = body["data"].as_array().expect("应为数组");
    assert_eq!(logs.len(), 1);
    assert_eq!(logs[0]["message"], "initial db log");

    // 验证日志缓存已被回填
    let cached_logs = cache.get_logs(history_id).await.expect("应已回填日志缓存");
    assert_eq!(cached_logs.len(), 1);

    // 往缓存追加一条新日志
    let extra_log = StepLogRecord {
        step_index: 1,
        level: StepLogLevel::Info,
        message: "realtime cached log".to_owned(),
        created_at: Utc::now(),
        output: None,
    };
    cache.push_log(history_id, &extra_log).await;

    // 再次调用 history_logs 应直接从缓存读出 2 条
    let resp = send(
        &app,
        Request::get(format!("/api/histories/{history_id}/logs"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = json(resp).await;
    let logs = body["data"].as_array().expect("应为数组");
    assert_eq!(logs.len(), 2, "命中缓存应返回 2 条日志");
    assert_eq!(logs[1]["message"], "realtime cached log");

    // 4. list_histories_for 叠加 running 缓存元数据
    let resp = send(
        &app,
        Request::get("/api/histories").body(Body::empty()).unwrap(),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = json(resp).await;
    let items = body["data"]["items"].as_array().expect("应为数组");
    let item = items.iter().find(|i| i["id"] == history_id).expect("应找到该历史");
    assert_eq!(item["error_message"], "cache updated message");

    // 清理测试缓存键
    cache.del(history_id).await;
}
