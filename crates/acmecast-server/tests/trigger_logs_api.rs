//! 触发审计查询端点：`GET /api/schedules/trigger-logs`。
//!
//! 覆盖三点：未鉴权 401、全量与按流水线过滤的分页查询（时间倒序）、
//! 来源字段以字符串形式暴露（`cron` / `renewal`）。

mod support;

use acmecast_store::entity::pipeline::TriggerSource;
use acmecast_store::migrate;
use acmecast_store::repository::{TriggerLogInput, TriggerLogRepository};
use axum::{Router, body::Body, http::Request};
use sea_orm::{Database, DatabaseConnection};
use support::*;

/// 自持数据库的强制鉴权路由：触发记录要先落库，再用路由暴露出来。
async fn app_with_seeded_logs(counts: &[(i64, TriggerSource)]) -> (Router, DatabaseConnection) {
    let db = Database::connect("sqlite::memory:")
        .await
        .expect("应能连上内存库");
    migrate(&db).await.expect("迁移应成功");

    // 触发记录的 pipeline_id 是外键：按用例用到的 id 先建好流水线
    // （自增主键从 1 起，与用例里的 1/2 对齐）。
    for pipeline_id in [1_i64, 2] {
        acmecast_store::repository::PipelineRepository::new(&db)
            .save(
                None,
                acmecast_store::repository::PipelineInput {
                    name: format!("流水线 {pipeline_id}"),
                    enabled: true,
                    description: None,
                    steps: vec![acmecast_store::repository::PipelineStepInput {
                        type_id: "test.placeholder".to_owned(),
                        input: serde_json::json!({}),
                        enabled: true,
                    }],
                },
            )
            .await
            .expect("流水线应能保存");
    }

    let repo = TriggerLogRepository::new(&db);
    for (pipeline_id, source) in counts {
        repo.record(TriggerLogInput {
            pipeline_id: *pipeline_id,
            source: *source,
            detail: Some(format!("流水线 {pipeline_id} 的 {source:?} 触发")),
            triggered_at: chrono::Utc::now(),
        })
        .await
        .expect("触发记录应能落库");
    }

    let app = router_with_db(
        db.clone(),
        acmecast_server::config::ServerConfig::default(),
        acmecast_server::auth::AuthMode::enforced(auth_config()),
        None,
        acmecast_server::dependencies::credential_registry(),
        acmecast_server::dependencies::step_registry(),
    )
    .await;
    (app, db)
}

async fn authorized_token(app: &Router) -> String {
    let (_, body) = login(app, ADMIN_PASSWORD).await;
    body["data"]["token"]
        .as_str()
        .expect("登录应返回令牌")
        .to_owned()
}

#[tokio::test]
async fn unauthenticated_request_is_rejected() {
    let (app, _db) = app_with_seeded_logs(&[]).await;

    let response = send(
        &app,
        Request::get("/api/schedules/trigger-logs")
            .body(Body::empty())
            .unwrap(),
    )
    .await;

    assert_eq!(response.status(), axum::http::StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn lists_all_logs_in_reverse_order() {
    let (app, _db) = app_with_seeded_logs(&[
        (1, TriggerSource::Cron),
        (1, TriggerSource::Renewal),
        (2, TriggerSource::Renewal),
    ])
    .await;
    let token = authorized_token(&app).await;

    let response = send(
        &app,
        with_bearer(
            Request::get("/api/schedules/trigger-logs?page_size=10")
                .body(Body::empty())
                .unwrap(),
            &token,
        ),
    )
    .await;

    assert_eq!(response.status(), axum::http::StatusCode::OK);
    let body = json(response).await;
    assert_eq!(body["data"]["total"].as_u64(), Some(3));
    let items = body["data"]["items"].as_array().expect("应有 items");
    assert_eq!(items.len(), 3);
    // 倒序：最后落库的在最前。
    assert_eq!(items[0]["pipeline_id"].as_i64(), Some(2));
    assert_eq!(items[0]["source"].as_str(), Some("renewal"));
    assert_eq!(items[2]["source"].as_str(), Some("cron"));
    assert!(items[0]["detail"].as_str().is_some());
}

#[tokio::test]
async fn filters_by_pipeline_and_paginates() {
    let (app, _db) = app_with_seeded_logs(&[
        (1, TriggerSource::Cron),
        (2, TriggerSource::Cron),
        (1, TriggerSource::Renewal),
    ])
    .await;
    let token = authorized_token(&app).await;

    let response = send(
        &app,
        with_bearer(
            Request::get("/api/schedules/trigger-logs?pipeline_id=1&page=2&page_size=1")
                .body(Body::empty())
                .unwrap(),
            &token,
        ),
    )
    .await;

    assert_eq!(response.status(), axum::http::StatusCode::OK);
    let body = json(response).await;
    // 流水线 1 有两条；第二页是其中较早的那条（renewal 在 cron 之后落库）。
    assert_eq!(body["data"]["total"].as_u64(), Some(2));
    assert_eq!(body["data"]["page"].as_u64(), Some(2));
    assert_eq!(body["data"]["page_size"].as_u64(), Some(1));
    let items = body["data"]["items"].as_array().expect("应有 items");
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["pipeline_id"].as_i64(), Some(1));
    assert_eq!(items[0]["source"].as_str(), Some("cron"));
}
