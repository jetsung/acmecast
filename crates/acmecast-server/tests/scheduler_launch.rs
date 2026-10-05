//! 调度启动器（`PipelineRunLauncher`）对停用流水线的处理。
//!
//! 钉住两个契约：调度触发命中停用流水线时**跳过而非报错**（调度循环
//! 不因此中断），且不产生运行历史（没有真实启动就没有运行记录）。

use std::sync::Arc;

use acmecast_core::CredentialCipher;
use acmecast_pipeline::{HistoryQuery, HistoryRepository, StepRegistry};
use acmecast_scheduler::{LaunchRequest, PipelineLauncher};
use acmecast_store::migrate;
use acmecast_store::repository::{PipelineInput, PipelineRepository};
use sea_orm::{Database, DatabaseConnection};

/// 内存库 + 一条指定启用状态的流水线，返回 (db, pipeline_id)。
async fn setup_with_pipeline(enabled: bool) -> (DatabaseConnection, i64) {
    let db = Database::connect("sqlite::memory:")
        .await
        .expect("应能连上内存库");
    migrate(&db).await.expect("迁移应成功");

    let pipeline_id = PipelineRepository::new(&db)
        .save(
            None,
            PipelineInput {
                name: "调度触发用例".to_owned(),
                enabled,
                description: None,
                // 保存校验要求至少一个步骤；停用分支在执行前就返回，
                // 步骤不会被真正运行。
                steps: vec![acmecast_store::repository::PipelineStepInput {
                    type_id: "test.never-run".to_owned(),
                    input: serde_json::json!({}),
                    enabled: true,
                }],
            },
        )
        .await
        .expect("流水线应能保存");

    (db, pipeline_id)
}

fn launcher(db: &DatabaseConnection) -> acmecast_server::scheduler::PipelineRunLauncher {
    acmecast_server::scheduler::PipelineRunLauncher::new(
        Arc::new(StepRegistry::new()),
        db,
        acmecast_server::dependencies::credential_registry(),
        Some(&Arc::new(
            CredentialCipher::from_base64(&CredentialCipher::generate_key_base64())
                .expect("生成的密钥应可用"),
        )),
        None,
    )
}

#[tokio::test]
async fn launch_skips_disabled_pipeline_without_creating_history() {
    let (db, pipeline_id) = setup_with_pipeline(false).await;
    let launcher = launcher(&db);

    let outcome = launcher
        .launch(LaunchRequest {
            pipeline_id,
            source: acmecast_store::entity::pipeline::TriggerSource::Cron,
            detail: None,
        })
        .await;

    // 跳过不是失败：调度循环拿到 Ok 后照常推进触发点。
    outcome.expect("停用流水线的调度触发应静默跳过而非报错");

    let histories = HistoryRepository::new(&db)
        .list(HistoryQuery::default())
        .await
        .expect("应能查询运行历史");
    assert_eq!(histories.items.len(), 0, "停用流水线不应产生运行历史");
}

#[tokio::test]
async fn launch_of_missing_pipeline_is_an_error() {
    let (db, _) = setup_with_pipeline(true).await;
    let launcher = launcher(&db);

    let outcome = launcher
        .launch(LaunchRequest {
            pipeline_id: 999_999,
            source: acmecast_store::entity::pipeline::TriggerSource::Cron,
            detail: None,
        })
        .await;

    assert!(outcome.is_err(), "不存在的流水线应是明确错误而非静默跳过");
}
