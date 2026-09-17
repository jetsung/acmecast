//! 调度器的启动装配：装载全部调度并常驻运行，随停机信号退出。
//!
//! [`PipelineRunLauncher`] 把触发请求接到流水线执行器上：创建运行历史
//! （状态 running）、驱动流水线跑完、把结果写回历史——调度触发的运行
//! 与手动触发走同一条历史链路。连接被泄漏成 `'static`：启动器是进程级
//! 单例，连接本就活到进程结束，仓储因此可以安全借用同一份句柄。

use acmecast_pipeline::{
    DatabaseStateStore, HistoryRecord, HistoryRepository, PipelineDefinition, PipelineRunner,
    StepDefinition, StepRegistry,
};
use acmecast_scheduler::{
    Error, LaunchRequest, PipelineLauncher, Result, SchedulerConfig, SchedulerEngine,
};
use acmecast_store::entity::history::RunStatus;
use acmecast_store::repository::PipelineRepository;
use async_trait::async_trait;
use sea_orm::DatabaseConnection;
use std::sync::Arc;
use tokio::sync::watch;
use tokio::task::JoinHandle;

/// 真正执行流水线的启动器。
pub struct PipelineRunLauncher {
    steps: Arc<StepRegistry>,
    db: &'static DatabaseConnection,
    credentials: acmecast_access::CredentialStore<'static>,
}

impl std::fmt::Debug for PipelineRunLauncher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PipelineRunLauncher")
            .field("steps", &self.steps)
            .finish_non_exhaustive()
    }
}

impl PipelineRunLauncher {
    /// 用步骤注册表与数据库连接装配。
    /// 用步骤注册表、数据库连接、凭据注册表与加密器装配。
    ///
    /// 加密器与注册表都显式传入而不是读环境变量：与 HTTP 层共用同一实例，
    /// 凭据的加解密才有同一把密钥、类型识别才与 HTTP 层一致；`None` 时
    /// 用现场生成的占位密钥兜底（凭据解析会失败并留下明确错误），
    /// 不让启动器把整个服务拖垮。
    #[must_use]
    pub fn new(
        steps: Arc<StepRegistry>,
        db: &DatabaseConnection,
        credential_registry: Arc<acmecast_access::CredentialRegistry>,
        cipher: Option<&Arc<acmecast_core::CredentialCipher>>,
    ) -> Self {
        let leaked: &'static DatabaseConnection = Box::leak(Box::new(db.clone()));
        let cipher = cipher.cloned().or_else(|| {
            tracing::error!("未配置凭据加密密钥，调度触发的流水线无法取用凭据");
            acmecast_core::CredentialCipher::from_base64(
                &acmecast_core::CredentialCipher::generate_key_base64(),
            )
            .ok()
            .map(Arc::new)
        });
        let credentials = acmecast_access::CredentialStore::new(
            leaked,
            credential_registry,
            cipher.expect("占位密钥必可用"),
        );
        Self {
            steps,
            db: leaked,
            credentials,
        }
    }
}

#[async_trait]
impl PipelineLauncher for PipelineRunLauncher {
    async fn launch(&self, request: LaunchRequest) -> Result<()> {
        let pipeline = PipelineRepository::new(self.db)
            .find(request.pipeline_id)
            .await
            .map_err(Error::Store)?
            .ok_or_else(|| {
                Error::InvalidConfig(format!("流水线 {} 不存在", request.pipeline_id))
            })?;
        if !pipeline.enabled {
            tracing::info!(
                pipeline_id = request.pipeline_id,
                "流水线已停用，跳过本次触发"
            );
            return Ok(());
        }

        let definition = PipelineDefinition {
            id: pipeline.id,
            steps: pipeline
                .steps
                .iter()
                .map(|step| StepDefinition {
                    order_index: step.order_index,
                    type_id: step.type_id.clone(),
                    input: step.input.clone(),
                    enabled: step.enabled,
                })
                .collect(),
        };

        let started_at = chrono::Utc::now();
        let history_id = HistoryRepository::new(self.db)
            .record(HistoryRecord {
                pipeline_id: request.pipeline_id,
                trigger_source: request.source,
                status: RunStatus::Running,
                started_at,
                finished_at: None,
                error_message: None,
                logs: Vec::new(),
            })
            .await
            .map_err(Error::Pipeline)?;

        let state = DatabaseStateStore::new(self.db);
        let runner = PipelineRunner::new(&self.steps, &self.credentials, &state)
            .with_trigger(request.source);
        let outcome = runner.run(&definition, history_id).await?;

        // 终态**原地更新**进同一条历史：另插一行会让原 running 记录永远
        // 停在 running，所有「查运行中」的判定都会把流水线当成还在跑。
        // 运行失败要让调用方（调度器）知道：cron 触发靠 Err 保留触发点重试，
        // 续期触发靠它落「启动失败」的审计记录。吞掉失败 = 静默丢续期。
        let failure = if outcome.is_success() {
            None
        } else {
            Some(
                outcome
                    .failure
                    .as_ref()
                    .map(|failure| {
                        format!(
                            "第 {} 步（{}）：{}",
                            failure.step_order + 1,
                            failure.type_id,
                            failure.reason
                        )
                    })
                    .unwrap_or_else(|| "未知原因".to_owned()),
            )
        };
        HistoryRepository::new(self.db)
            .finish(
                history_id,
                if outcome.is_success() {
                    RunStatus::Success
                } else {
                    RunStatus::Failed
                },
                failure.clone(),
                outcome
                    .steps
                    .iter()
                    .flat_map(|step| {
                        step.logs
                            .iter()
                            .map(move |log| acmecast_pipeline::StepLogRecord {
                                step_index: step.order_index,
                                level: log.level,
                                message: log.message.clone(),
                                created_at: log.created_at,
                            })
                    })
                    .collect(),
            )
            .await
            .map_err(Error::Pipeline)?;
        if let Some(failure) = failure {
            return Err(Error::InvalidConfig(format!(
                "流水线 {} 运行失败：{failure}",
                request.pipeline_id
            )));
        }

        tracing::info!(
            pipeline_id = request.pipeline_id,
            history_id,
            success = outcome.is_success(),
            "流水线运行结束"
        );
        Ok(())
    }
}

/// 装载全部调度并在后台常驻运行。
///
/// 装载失败（数据库不可用等）只记日志不 panic——服务仍可提供 HTTP
/// 接口供排障，调度在下次重启时重试装载。
#[must_use]
pub fn spawn_scheduler(
    db: DatabaseConnection,
    steps: Arc<StepRegistry>,
    credential_registry: Arc<acmecast_access::CredentialRegistry>,
    cipher: Option<&std::sync::Arc<acmecast_core::CredentialCipher>>,
    shutdown: watch::Receiver<bool>,
) -> JoinHandle<()> {
    let cipher = cipher.cloned();
    tokio::spawn(async move {
        let launcher = PipelineRunLauncher::new(
            Arc::clone(&steps),
            &db,
            credential_registry,
            cipher.as_ref(),
        );
        let engine = SchedulerEngine::new(&db, &launcher, SchedulerConfig::default());

        if let Err(error) = engine.load(chrono::Utc::now()).await {
            tracing::error!(%error, "调度装载失败，本进程内调度不可用");
            return;
        }
        tracing::info!("调度装载完成，开始常驻运行");

        if let Err(error) = engine.serve(shutdown).await {
            tracing::error!(%error, "调度循环异常退出");
        }
    })
}
