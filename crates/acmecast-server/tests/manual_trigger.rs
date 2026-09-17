//! 手动触发流水线：`POST /api/pipelines/{id}/run`。
//!
//! 覆盖四点：触发后返回运行历史标识并可轮询到终态、运行来源记为 manual、
//! 未知流水线 404、停用流水线 400、未配置加密密钥 500。

mod support;

use std::{sync::Arc, time::Duration};

use acmecast_core::CredentialCipher;
use acmecast_pipeline::{PipelineStep, Result, StepContext, StepOutput, StepRegistry};
use acmecast_server::{auth::AuthMode, config::ServerConfig};
use async_trait::async_trait;
use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode},
};
use support::*;

/// 一个只记一行日志的步骤：手动触发的用例关心的是链路，不是具体任务。
#[derive(Debug)]
struct EchoStep;

#[async_trait]
impl PipelineStep for EchoStep {
    fn type_id(&self) -> &'static str {
        "test.echo"
    }

    async fn execute(&self, ctx: &mut StepContext<'_>) -> Result<StepOutput> {
        ctx.log_info("echo 已执行");
        Ok(StepOutput::empty().with_artifact("echoed", serde_json::json!(true)))
    }
}

fn step_registry() -> StepRegistry {
    let mut registry = StepRegistry::new();
    registry.register(EchoStep).expect("测试步骤不应重复");
    registry
}

fn test_cipher() -> Arc<CredentialCipher> {
    let key = CredentialCipher::generate_key_base64();
    Arc::new(CredentialCipher::from_base64(&key).expect("生成的密钥应可用"))
}

/// 装配一个关闭鉴权、带加密器与测试步骤的路由。
async fn runnable_app() -> Router {
    app_with_deps(
        ServerConfig::default(),
        AuthMode::Disabled,
        Some(test_cipher()),
        acmecast_server::dependencies::credential_registry(),
        Arc::new(step_registry()),
    )
    .await
}

/// 建一条只有单个 echo 步骤的流水线，返回其 id。
async fn create_pipeline(app: &Router, enabled: bool) -> i64 {
    let response = send(
        app,
        json_request(
            "POST",
            "/api/pipelines",
            &serde_json::json!({
                "name": "手动触发用例",
                "enabled": enabled,
                "steps": [{ "type_id": "test.echo", "input": {}, "enabled": true }]
            }),
        ),
    )
    .await;
    let status = response.status();
    let body = json(response).await;
    assert_eq!(status, StatusCode::OK, "流水线应能创建：{body}");
    body["data"]["id"].as_i64().expect("应返回流水线 id")
}

/// 轮询运行历史直到终态（或超时失败），返回最终状态字符串。
async fn wait_for_terminal(app: &Router, history_id: i64) -> String {
    for _ in 0..200 {
        let response = send(
            app,
            Request::get(format!("/api/histories/{history_id}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        let status = json(response).await["data"]["status"]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        if status != "running" {
            return status;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("运行在超时前未到达终态");
}

#[tokio::test]
async fn manual_trigger_runs_pipeline_and_records_history() {
    let app = runnable_app().await;
    let pipeline_id = create_pipeline(&app, true).await;

    let response = send(
        &app,
        json_request(
            "POST",
            &format!("/api/pipelines/{pipeline_id}/run"),
            &serde_json::json!({}),
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = json(response).await;
    assert_eq!(
        body["data"]["status"], "running",
        "触发接口应立即返回 running：{body}"
    );
    let history_id = body["data"]["history_id"]
        .as_i64()
        .expect("应返回运行历史 id");

    assert_eq!(wait_for_terminal(&app, history_id).await, "success");

    // 运行来源必须区分于调度触发，否则运行历史里看不出这是谁发起的。
    let detail = json(
        send(
            &app,
            Request::get(format!("/api/histories/{history_id}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await,
    )
    .await;
    assert_eq!(detail["data"]["trigger_source"], "manual");

    // 步骤日志要落进同一条历史——手动触发与调度触发共用写回链路。
    let logs = json(
        send(
            &app,
            Request::get(format!("/api/histories/{history_id}/logs"))
                .body(Body::empty())
                .unwrap(),
        )
        .await,
    )
    .await;
    let logs = logs["data"].as_array().expect("日志应是数组");
    assert!(
        logs.iter().any(|log| log["message"]
            .as_str()
            .is_some_and(|message| message.contains("echo 已执行"))),
        "应含步骤日志：{logs:?}"
    );
}

#[tokio::test]
async fn manual_trigger_returns_404_for_unknown_pipeline() {
    let app = runnable_app().await;

    let response = send(
        &app,
        json_request("POST", "/api/pipelines/999/run", &serde_json::json!({})),
    )
    .await;

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert_eq!(json(response).await["error"]["code"], "not_found");
}

#[tokio::test]
async fn manual_trigger_refuses_a_disabled_pipeline() {
    let app = runnable_app().await;
    let pipeline_id = create_pipeline(&app, false).await;

    let response = send(
        &app,
        json_request(
            "POST",
            &format!("/api/pipelines/{pipeline_id}/run"),
            &serde_json::json!({}),
        ),
    )
    .await;

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = json(response).await;
    assert_eq!(body["error"]["code"], "validation_error");
    assert_eq!(body["error"]["field"], "enabled");
}

#[tokio::test]
async fn manual_trigger_requires_a_credential_cipher() {
    // open_app 不带加密器：无法解密凭据，触发前就该明确报错。
    let app = open_app().await;
    let pipeline_id = create_pipeline(&app, true).await;

    let response = send(
        &app,
        json_request(
            "POST",
            &format!("/api/pipelines/{pipeline_id}/run"),
            &serde_json::json!({}),
        ),
    )
    .await;

    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(json(response).await["error"]["code"], "configuration_error");
}
