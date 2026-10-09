//! 删除调度端点：`DELETE /api/schedules/{pipeline_id}`。
//!
//! 覆盖三点：删除已有调度返回成功、删除不存在返回 404、
//! 删除调度后该流水线的触发记录被联动清理。

mod support;

use acmecast_store::entity::pipeline::TriggerSource;
use acmecast_store::migrate;
use acmecast_store::repository::{
    PipelineInput, PipelineRepository, PipelineStepInput, ScheduleInput, ScheduleRepository,
    TriggerLogInput, TriggerLogRepository,
};
use axum::body::Body;
use axum::http::{Request, StatusCode};
use sea_orm::Database;
use support::*;

/// 起一个共享内存库：先 seed 流水线 + 调度 + 两条触发记录，
/// 再用它装配路由（鉴权关闭，方便直接发请求）。
async fn seeded_app() -> axum::Router {
    let db = Database::connect("sqlite::memory:")
        .await
        .expect("应能连接内存数据库");
    migrate(&db).await.expect("迁移应成功");

    let pipeline_id = PipelineRepository::new(&db)
        .save(
            None,
            PipelineInput {
                name: "scheduled".to_owned(),
                enabled: true,
                description: None,
                steps: vec![PipelineStepInput {
                    type_id: "test.placeholder".to_owned(),
                    input: serde_json::json!({}),
                    enabled: true,
                }],
            },
        )
        .await
        .expect("流水线应能保存");

    ScheduleRepository::new(&db)
        .save(
            pipeline_id,
            ScheduleInput {
                cron: Some("0 3 * * *".to_owned()),
                enabled: true,
                catch_up: false,
                renewal_domains: None,
                renewal_threshold_days: None,
            },
        )
        .await
        .expect("调度应能保存");

    let log_repo = TriggerLogRepository::new(&db);
    for source in [TriggerSource::Cron, TriggerSource::Renewal] {
        log_repo
            .record(TriggerLogInput {
                pipeline_id,
                source,
                detail: None,
                triggered_at: chrono::Utc::now(),
            })
            .await
            .expect("触发记录应能落库");
    }

    router_with_db(
        db,
        acmecast_server::config::ServerConfig::default(),
        acmecast_server::auth::AuthMode::Disabled,
        None,
        acmecast_server::dependencies::credential_registry(),
        acmecast_server::dependencies::step_registry(),
    )
    .await
}

#[tokio::test]
async fn deleting_existing_schedule_returns_ok_and_cleans_trigger_logs() {
    let app = seeded_app().await;

    // 删除调度。
    let response = send(
        &app,
        Request::delete("/api/schedules/1")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = json(response).await;
    assert_eq!(body["data"]["deleted"], true);

    // 调度列表不再包含该流水线。
    let list_response = send(
        &app,
        Request::get("/api/schedules")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(list_response.status(), StatusCode::OK);
    let list_body = json(list_response).await;
    let schedules = list_body["data"].as_array().expect("data 应为数组");
    assert!(
        schedules
            .iter()
            .all(|s| s["pipeline_id"].as_i64() != Some(1)),
        "已删除的调度不应出现在列表中"
    );

    // 触发记录列表不再包含该流水线。
    let logs_response = send(
        &app,
        Request::get("/api/schedules/trigger-logs")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(logs_response.status(), StatusCode::OK);
    let logs_body = json(logs_response).await;
    let logs = logs_body["data"]["items"]
        .as_array()
        .expect("items 应为数组");
    assert!(
        logs.iter()
            .all(|l| l["pipeline_id"].as_i64() != Some(1)),
        "已删除调度的触发记录不应出现在列表中"
    );
}

#[tokio::test]
async fn deleting_nonexistent_schedule_returns_404() {
    let app = seeded_app().await;

    let response = send(
        &app,
        Request::delete("/api/schedules/999")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let body = json(response).await;
    assert_eq!(body["error"]["code"], "not_found");
}
