//! 10.3 REST 资源端点的最小闭环测试。

use acmecast_server::{AppState, RuntimeState, assemble_router_with_runtime};
use acmecast_store::migrate;
use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use sea_orm::Database;
use tower::ServiceExt;

/// 这些用例只关心资源端点的响应格式，鉴权交给专门的用例覆盖，
/// 这里显式关闭以免每个请求都要先换令牌。
async fn app() -> axum::Router {
    let db = Database::connect("sqlite::memory:")
        .await
        .expect("应能连接内存数据库");
    migrate(&db).await.expect("迁移应成功");
    assemble_router_with_runtime(
        AppState {
            db,
            config: acmecast_server::config::ServerConfig::default(),
        },
        RuntimeState::new(
            acmecast_server::dependencies::credential_registry(),
            acmecast_server::dependencies::step_registry(),
            None,
            acmecast_server::auth::AuthMode::Disabled,
        ),
    )
}

async fn json_body(response: axum::response::Response) -> serde_json::Value {
    let bytes = to_bytes(response.into_body(), 1024 * 1024)
        .await
        .expect("应能读取响应体");
    serde_json::from_slice(&bytes).expect("响应应是 JSON")
}

#[tokio::test]
async fn pipeline_crud_uses_uniform_data_envelope_and_returns_404() {
    let app = app().await;
    let create = Request::post("/api/pipelines")
        .header("content-type", "application/json")
        .body(Body::from(
            r#"{"name":"daily renewal","steps":[{"type_id":"test.step","input":{"value":1}}]}"#,
        ))
        .unwrap();
    let response = app.clone().oneshot(create).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = json_body(response).await;
    let id = body["data"]["id"].as_i64().expect("应返回流水线 id");

    let list = app
        .clone()
        .oneshot(
            Request::get("/api/pipelines?page=1&page_size=10")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(list.status(), StatusCode::OK);
    let list_body = json_body(list).await;
    assert_eq!(list_body["data"]["total"], 1);
    assert_eq!(list_body["data"]["items"][0]["name"], "daily renewal");

    let missing = app
        .oneshot(
            Request::get(format!("/api/pipelines/{}", id + 1))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);
    let missing_body = json_body(missing).await;
    assert_eq!(missing_body["error"]["code"], "not_found");
}

#[tokio::test]
async fn deleting_a_schedule_bound_pipeline_is_rejected_with_409() {
    let app = app().await;
    // 创建两个流水线：一个绑定调度，一个不绑定。
    let mut ids = Vec::new();
    for name in ["bound", "unbound"] {
        let create = Request::post("/api/pipelines")
            .header("content-type", "application/json")
            .body(Body::from(format!(
                r#"{{"name":"{name}","steps":[{{"type_id":"test.step","input":{{"value":1}}}}]}}"#
            )))
            .unwrap();
        let response = app.clone().oneshot(create).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = json_body(response).await;
        ids.push(body["data"]["id"].as_i64().expect("应返回流水线 id"));
    }
    let (bound_id, unbound_id) = (ids[0], ids[1]);

    // 给第一个流水线绑定调度。
    let bind = Request::post("/api/schedules")
        .header("content-type", "application/json")
        .body(Body::from(format!(
            r#"{{"pipeline_id":{bound_id},"cron":"0 3 * * *","enabled":true,"catch_up":false,"renewal_domains":null}}"#
        )))
        .unwrap();
    let response = app.clone().oneshot(bind).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK, "调度应能保存");

    // 已绑定时删除被拒：409 + schedule_bound + 中文消息。
    let blocked = app
        .clone()
        .oneshot(
            Request::delete(format!("/api/pipelines/{bound_id}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(blocked.status(), StatusCode::CONFLICT);
    let blocked_body = json_body(blocked).await;
    assert_eq!(blocked_body["error"]["code"], "schedule_bound");
    assert!(
        blocked_body["error"]["message"]
            .as_str()
            .unwrap_or("")
            .contains("该流水线已被调度绑定"),
        "应返回可诊断的中文提示"
    );

    // 无绑定时删除成功。
    let ok = app
        .clone()
        .oneshot(
            Request::delete(format!("/api/pipelines/{unbound_id}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(ok.status(), StatusCode::OK);

    // 已不存在的流水线删除返回 404。
    let gone = app
        .oneshot(
            Request::delete(format!("/api/pipelines/{unbound_id}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(gone.status(), StatusCode::NOT_FOUND);
    assert_eq!(json_body(gone).await["error"]["code"], "not_found");
}

#[tokio::test]
async fn malformed_json_returns_uniform_validation_error() {
    let app = app().await;
    let response = app
        .oneshot(
            Request::post("/api/pipelines")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"name":"broken","steps":}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = json_body(response).await;
    assert_eq!(body["error"]["code"], "invalid_json");
}

/// 带加密密钥的运行装配：凭据端点没有 cipher 会直接报配置错误。
async fn app_with_cipher() -> axum::Router {
    let db = Database::connect("sqlite::memory:")
        .await
        .expect("应能连接内存数据库");
    migrate(&db).await.expect("迁移应成功");
    let cipher = acmecast_core::CredentialCipher::from_bytes(&[7u8; 32]).expect("密钥应合法");
    assemble_router_with_runtime(
        AppState {
            db,
            config: acmecast_server::config::ServerConfig::default(),
        },
        RuntimeState::new(
            acmecast_server::dependencies::credential_registry(),
            acmecast_server::dependencies::step_registry(),
            Some(std::sync::Arc::new(cipher)),
            acmecast_server::auth::AuthMode::Disabled,
        ),
    )
}

#[tokio::test]
async fn credential_detail_returns_decrypted_fields_for_editing() {
    // spec（2.4）：编辑弹窗要回填本条记录的现值——PUT 是整体替换语义，
    // GET 详情若不带字段，用户一保存就会把密钥清成空。
    let app = app_with_cipher().await;
    let create = Request::post("/api/credentials")
        .header("content-type", "application/json")
        .body(Body::from(
            r#"{"name":"cf","type_id":"cloud.cloudflare","fields":{"api_token":"  secret-tok  "}}"#,
        ))
        .unwrap();
    let response = app.clone().oneshot(create).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = json_body(response).await;
    let id = body["data"]["id"].as_i64().expect("应返回凭据 id");
    // 列表项不带字段值。
    assert!(body["data"].get("fields").is_none());

    let detail = app
        .clone()
        .oneshot(
            Request::get(format!("/api/credentials/{id}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(detail.status(), StatusCode::OK);
    let detail_body = json_body(detail).await;
    assert_eq!(detail_body["data"]["name"], "cf");
    assert_eq!(detail_body["data"]["type_id"], "cloud.cloudflare");
    assert_eq!(
        detail_body["data"]["fields"]["api_token"].as_str(),
        Some("secret-tok"),
        "详情应带回保存时已去空白的字段值"
    );

    // 列表依旧不外泄字段值。
    let list = app
        .oneshot(
            Request::get("/api/credentials?page=1&page_size=10")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let list_body = json_body(list).await;
    assert!(list_body["data"]["items"][0].get("fields").is_none());
}

#[tokio::test]
async fn credential_remark_round_trips_through_api() {
    // 备注非敏感、列表与详情都要返回；创建不携带时缺省为空，更新可改写。
    let app = app_with_cipher().await;

    // 创建携带备注。
    let create = Request::post("/api/credentials")
        .header("content-type", "application/json")
        .body(Body::from(
            r#"{"name":"cf","type_id":"cloud.cloudflare","fields":{"api_token":"tok"},"remark":"生产 DNS"}"#,
        ))
        .unwrap();
    let response = app.clone().oneshot(create).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = json_body(response).await;
    let id = body["data"]["id"].as_i64().expect("应返回凭据 id");
    assert_eq!(body["data"]["remark"], "生产 DNS");

    // 详情返回备注。
    let detail = app
        .clone()
        .oneshot(
            Request::get(format!("/api/credentials/{id}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let detail_body = json_body(detail).await;
    assert_eq!(detail_body["data"]["remark"], "生产 DNS");

    // 列表返回备注。
    let list = app
        .clone()
        .oneshot(
            Request::get("/api/credentials?page=1&page_size=10")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let list_body = json_body(list).await;
    assert_eq!(list_body["data"]["items"][0]["remark"], "生产 DNS");

    // 创建不携带 remark：缺省为空串。
    let create_no_remark = Request::post("/api/credentials")
        .header("content-type", "application/json")
        .body(Body::from(
            r#"{"name":"no-remark","type_id":"cloud.cloudflare","fields":{"api_token":"tok2"}}"#,
        ))
        .unwrap();
    let response = app.clone().oneshot(create_no_remark).await.unwrap();
    let body = json_body(response).await;
    assert_eq!(body["data"]["remark"], "");

    // 更新可改写备注。
    let update = Request::put(format!("/api/credentials/{id}"))
        .header("content-type", "application/json")
        .body(Body::from(
            r#"{"name":"cf","type_id":"cloud.cloudflare","fields":{"api_token":"tok"},"remark":"测试 DNS"}"#,
        ))
        .unwrap();
    let response = app.oneshot(update).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = json_body(response).await;
    assert_eq!(body["data"]["remark"], "测试 DNS");
}

#[tokio::test]
async fn history_logs_expose_structured_output_with_tencent_cert_ids() {
    use acmecast_pipeline::{HistoryRecord, StepLogRecord, StepLogLevel};
    use acmecast_store::entity::history;

    use chrono::Utc;
    use sea_orm::ActiveModelTrait;

    let db = Database::connect("sqlite::memory:")
        .await
        .expect("应能连接内存数据库");
    acmecast_store::migrate(&db)
        .await
        .expect("迁移应成功");

    // 先创建一条流水线以满足 history.pipeline_id 外键。
    let pipeline_id = {
        let app = assemble_router_with_runtime(
            AppState {
                db: db.clone(),
                config: acmecast_server::config::ServerConfig::default(),
            },
            RuntimeState::new(
                acmecast_server::dependencies::credential_registry(),
                acmecast_server::dependencies::step_registry(),
                None,
                acmecast_server::auth::AuthMode::Disabled,
            ),
        );
        let response = app
            .oneshot(
                Request::post("/api/pipelines")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"name":"tencent-echo","steps":[{"type_id":"test.echo","input":{}}]}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = json_body(response).await;
        body["data"]["id"].as_i64().expect("应返回流水线 id")
    };

    let with_output_id: i64 = {
        let record = HistoryRecord {
            pipeline_id,
            trigger_source: acmecast_store::entity::pipeline::TriggerSource::Manual,
            status: history::RunStatus::Success,
            started_at: Utc::now(),
            finished_at: Some(Utc::now()),
            error_message: None,
            logs: vec![StepLogRecord {
                step_index: 1,
                level: StepLogLevel::Info,
                message: "腾讯云 SSL 部署结果".into(),
                created_at: Utc::now(),
                output: Some(serde_json::json!({
                    "cert_ids": ["cert-abc123", "cert-xyz"],
                    "skipped_write": false,
                })),
            }],
        };
        acmecast_pipeline::HistoryRepository::new(&db)
            .record(record)
            .await
            .expect("应能落库带 output 的历史")
    };

    let without_output_id: i64 = history::ActiveModel {
        pipeline_id: sea_orm::Set(pipeline_id),
        trigger_source: sea_orm::Set(
            acmecast_store::entity::pipeline::TriggerSource::Manual
                .as_str()
                .to_owned(),
        ),
        status: sea_orm::Set(history::RunStatus::Success.as_str().to_owned()),
        started_at: sea_orm::Set(Utc::now()),
        finished_at: sea_orm::Set(Some(Utc::now())),
        ..Default::default()
    }
    .insert(&db)
    .await
    .expect("应能插入无日志的历史")
    .id;

    let app = assemble_router_with_runtime(
        AppState {
            db: db.clone(),
            config: acmecast_server::config::ServerConfig::default(),
        },
        RuntimeState::new(
            acmecast_server::dependencies::credential_registry(),
            acmecast_server::dependencies::step_registry(),
            None,
            acmecast_server::auth::AuthMode::Disabled,
        ),
    );

    // 带 output 的日志：API 原样透出 cert_ids 与 skipped_write。
    let response = app
        .clone()
        .oneshot(
            Request::get(format!("/api/histories/{with_output_id}/logs"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = json_body(response).await;
    let logs = body["data"].as_array().expect("data 应为数组");
    assert_eq!(logs.len(), 1);
    assert_eq!(logs[0]["output"]["cert_ids"][0], "cert-abc123");
    assert_eq!(logs[0]["output"]["cert_ids"][1], "cert-xyz");
    assert_eq!(logs[0]["output"]["skipped_write"], false);

    // 无 output 的历史：logs 为空数组，接口仍 200。
    let response = app
        .oneshot(
            Request::get(format!("/api/histories/{without_output_id}/logs"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = json_body(response).await;
    assert_eq!(body["data"].as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn pipeline_list_defaults_to_id_desc() {
    let app = app().await;
    // 创建 3 条流水线，验证默认按 ID 降序返回。
    let mut ids = Vec::new();
    for name in ["alpha", "beta", "gamma"] {
        let create = Request::post("/api/pipelines")
            .header("content-type", "application/json")
            .body(Body::from(format!(
                r#"{{"name":"{name}","steps":[{{"type_id":"test.step","input":{{"value":1}}}}]}}"#
            )))
            .unwrap();
        let response = app.clone().oneshot(create).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = json_body(response).await;
        ids.push(body["data"]["id"].as_i64().expect("应返回流水线 id"));
    }

    // 不传 sort，验证默认按 ID 降序。
    let list = app
        .oneshot(
            Request::get("/api/pipelines?page=1&page_size=10")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(list.status(), StatusCode::OK);
    let list_body = json_body(list).await;
    assert_eq!(list_body["data"]["total"], 3);
    let items = list_body["data"]["items"].as_array().expect("应有 items");
    let returned_ids: Vec<i64> = items
        .iter()
        .map(|item| item["id"].as_i64().expect("应有 id"))
        .collect();
    // 降序：最后创建的在最前。
    assert_eq!(returned_ids, vec![ids[2], ids[1], ids[0]], "默认应按 ID 降序");
}

#[tokio::test]
async fn credential_list_defaults_to_id_desc() {
    let app = app_with_cipher().await;
    let mut ids = Vec::new();
    for index in 0..3 {
        let create = Request::post("/api/credentials")
            .header("content-type", "application/json")
            .body(Body::from(format!(
                r#"{{"name":"cf-{index}","type_id":"cloud.cloudflare","fields":{{"api_token":"tok{index}"}}}}"#
            )))
            .unwrap();
        let response = app.clone().oneshot(create).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = json_body(response).await;
        ids.push(body["data"]["id"].as_i64().expect("应返回凭据 id"));
    }

    let list = app
        .oneshot(
            Request::get("/api/credentials?page=1&page_size=10")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(list.status(), StatusCode::OK);
    let list_body = json_body(list).await;
    assert_eq!(list_body["data"]["total"], 3);
    let items = list_body["data"]["items"].as_array().expect("应有 items");
    let returned_ids: Vec<i64> = items
        .iter()
        .map(|item| item["id"].as_i64().expect("应有 id"))
        .collect();
    assert_eq!(returned_ids, vec![ids[2], ids[1], ids[0]], "默认应按 ID 降序");
}

