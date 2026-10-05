//! 11.4 三方言等价性：同一套持久层断言在 SQLite / MySQL / PostgreSQL 上各跑一遍。
//!
//! 覆盖的是**行为等价**中最容易分叉的部分：迁移、类型映射（布尔/时间/JSON/文本）、
//! 唯一约束、去重更新、事务内的状态推进、分页排序。HTTP 层测试不重复跑三方言——
//! 它们断言的是 HTTP 语义，持久层等价由本套件保证。
//!
//! - SQLite：内存库，始终运行；
//! - MySQL / PostgreSQL：优先用 `ACMECAST_TEST_MYSQL_URL` /
//!   `ACMECAST_TEST_POSTGRES_URL` 直连外部实例；否则自动起 Docker 容器
//!   （测试结束清理）；Docker 不可用时跳过并说明。
//!
//! MySQL 的 `DATETIME` 不带小数秒，时间比较统一截断到秒——这是方言差异
//! 本身的一部分，等价性断言必须容下它。

use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant};

use acmecast_deploy::{Deployer, DeploymentEntry};
use acmecast_deploy::{DeploymentStateStore as _, state::DatabaseDeploymentState};
use acmecast_pipeline::{HistoryRecord, HistoryRepository};
use acmecast_scheduler::SchedulerEngine;
use acmecast_store::entity::history::RunStatus;
use acmecast_store::entity::pipeline::TriggerSource;
use acmecast_store::migrate;
use acmecast_store::repository::{
    CertInput, CertRepository, PipelineInput, PipelineRepository, PipelineStepInput, ScheduleInput,
    ScheduleRepository, TriggerLogInput, TriggerLogRepository,
};
use sea_orm::{ActiveModelTrait, Database, DatabaseConnection, Set};

mod support;

use support::TempDir;

// ---- 共享断言套件 ----

/// 同一套持久层断言，三种方言各执行一次。
async fn run_suite(db: DatabaseConnection) {
    migrate(&db).await.expect("迁移应成功");

    let cipher = Arc::new(
        acmecast_core::CredentialCipher::from_base64(
            &acmecast_core::CredentialCipher::generate_key_base64(),
        )
        .expect("密钥应可用"),
    );
    let now = chrono::Utc::now();

    // 资源标识带随机后缀：同一容器库多次执行套件时，唯一键不会撞上
    // 上一次留下的数据（测试容器/实例常被复用）。
    let unique = uuid::Uuid::new_v4().simple().to_string();
    let domain = format!("a-{unique}.example.com");
    let domain_b = format!("b-{unique}.example.com");

    // ---- 证书：去重更新、吊销、续期扫描排除 ----
    let repository = CertRepository::new(&db);
    let outcome = repository
        .save(CertInput {
            domains: vec![domain.clone(), domain_b.clone()],
            cert_pem_path: "certs/one.pem".to_owned(),
            key_pem_path: "keys/one.pem".to_owned(),
            fingerprint: format!("sha256:one-{unique}"),
            issuer: Some("test-ca".to_owned()),
            not_before: now - chrono::Duration::days(1),
            not_after: now + chrono::Duration::days(30),
            acme_account_access_id: None,
        })
        .await
        .expect("证书应能写入");
    assert!(outcome.is_created(), "新域名集合应新建记录");
    let first_id = outcome.model().id;

    let again = repository
        .save(CertInput {
            domains: vec![domain_b.clone(), domain.clone()],
            cert_pem_path: "certs/two.pem".to_owned(),
            key_pem_path: "keys/two.pem".to_owned(),
            fingerprint: format!("sha256:two-{unique}"),
            issuer: Some("test-ca".to_owned()),
            not_before: now - chrono::Duration::days(1),
            not_after: now + chrono::Duration::days(40),
            acme_account_access_id: None,
        })
        .await
        .expect("同域名集合应能更新");
    assert!(!again.is_created(), "同一域名集合应更新而非新增");
    let updated = again.model();
    assert_eq!(updated.id, first_id, "去重键相同应命中同一条记录");
    assert_eq!(
        updated.fingerprint,
        format!("sha256:two-{unique}"),
        "内容应被更新"
    );

    repository
        .mark_revoked(first_id, now)
        .await
        .expect("应能标记吊销");
    let revoked = repository.find(first_id).await.expect("应能查询").unwrap();
    assert!(revoked.is_revoked(), "吊销状态应已更新");

    let due = repository
        .list_due(now + chrono::Duration::days(90))
        .await
        .expect("应能扫描");
    assert!(
        due.iter().all(|cert| cert.id != first_id),
        "已吊销的证书不应进入续期窗口"
    );

    // ---- 凭据：类型校验下的存取 ----
    let account = acmecast_store::entity::credential::ActiveModel {
        name: Set("测试账号".to_owned()),
        type_id: Set("acme.account".to_owned()),
        encrypted_fields: Set("encrypted-blob".to_owned()),
        created_at: Set(now),
        updated_at: Set(now),
        ..Default::default()
    }
    .insert(&db)
    .await
    .expect("凭据应能写入");
    assert!(account.id > 0, "自增主键应回填");

    // ---- 流水线：步骤顺序与整体替换 ----
    let pipelines = PipelineRepository::new(&db);
    let pipeline_id = pipelines
        .save(
            None,
            PipelineInput {
                name: "等价性流水线".to_owned(),
                description: None,
                enabled: true,
                steps: vec![
                    PipelineStepInput {
                        type_id: "step.one".to_owned(),
                        input: serde_json::json!({ "order": 1 }),
                        enabled: true,
                    },
                    PipelineStepInput {
                        type_id: "step.two".to_owned(),
                        input: serde_json::json!({ "order": 2 }),
                        enabled: true,
                    },
                ],
            },
        )
        .await
        .expect("流水线应能保存");
    let loaded = pipelines
        .find(pipeline_id)
        .await
        .expect("应能查询")
        .expect("流水线应存在");
    let orders: Vec<i32> = loaded.steps.iter().map(|step| step.order_index).collect();
    assert_eq!(orders, vec![0, 1], "步骤顺序应保持");
    let type_ids: Vec<&str> = loaded
        .steps
        .iter()
        .map(|step| step.type_id.as_str())
        .collect();
    assert_eq!(type_ids, vec!["step.one", "step.two"]);

    // ---- 运行历史：running → 原地终态（日志随行） ----
    let histories = HistoryRepository::new(&db);
    let history_id = histories
        .record(HistoryRecord {
            pipeline_id,
            trigger_source: TriggerSource::Cron,
            status: RunStatus::Running,
            started_at: now,
            finished_at: None,
            error_message: None,
            logs: Vec::new(),
        })
        .await
        .expect("运行记录应能写入");
    histories
        .finish(
            history_id,
            RunStatus::Success,
            None,
            vec![acmecast_pipeline::StepLogRecord {
                step_index: 0,
                level: acmecast_pipeline::StepLogLevel::Info,
                message: "等价性检查".to_owned(),
                created_at: now,
            }],
        )
        .await
        .expect("终态应能写入");
    let page = histories
        .list(acmecast_pipeline::HistoryQuery {
            pipeline_id: Some(pipeline_id),
            ..Default::default()
        })
        .await
        .expect("应能查询历史");
    assert_eq!(page.items.len(), 1, "running 行应被原地更新为终态");
    assert_eq!(page.items[0].status, RunStatus::Success);
    assert!(page.items[0].finished_at.is_some());
    let logs = histories.logs_of(history_id).await.expect("应能读取日志");
    assert_eq!(logs.len(), 1, "日志应随终态写入");

    // ---- 调度：保存校验 + 触发时间推进 ----
    let schedules = ScheduleRepository::new(&db);
    let config = schedules
        .save(
            pipeline_id,
            ScheduleInput {
                cron: Some("0 3 * * *".to_owned()),
                enabled: true,
                catch_up: false,
                renewal_domains: Some(vec![domain.clone()]),
            },
        )
        .await
        .expect("调度应能保存");
    assert!(config.next_trigger_at.is_some(), "保存时应算出下一次触发");

    let rejected = schedules
        .save(
            pipeline_id,
            ScheduleInput {
                cron: Some("not a cron".to_owned()),
                enabled: true,
                catch_up: false,
                renewal_domains: None,
            },
        )
        .await;
    assert!(rejected.is_err(), "非法 cron 应被拒绝");

    schedules
        .mark_triggered(
            pipeline_id,
            Some(Some(now)),
            Some(now + chrono::Duration::hours(1)),
        )
        .await
        .expect("触发时间应能推进");
    let updated = schedules
        .find(pipeline_id)
        .await
        .expect("应能查询")
        .expect("调度应存在");
    // MySQL 的 DATETIME 在插入时按四舍五入到秒（而非截断），时间等价断言
    // 统一给 1 秒容差——这是方言差异的一部分，而不是行为分叉。
    assert!(
        (updated.last_triggered_at.unwrap() - now).abs() <= chrono::Duration::seconds(1),
        "触发时间应与写入值一致（允许方言的秒级舍入）"
    );
    let next = updated.next_trigger_at.expect("推进后应有下一次触发时间");
    assert!(
        truncated(next) > truncated(now),
        "下一次触发应排到未来（方言时间截断内容忍同秒）"
    );

    // ---- 触发记录：落库 + 最近一次 + 倒序分页 ----
    let triggers = TriggerLogRepository::new(&db);
    for (offset, source) in [
        (-30_i64, TriggerSource::Cron),
        (-10, TriggerSource::Renewal),
    ] {
        triggers
            .record(TriggerLogInput {
                pipeline_id,
                source,
                detail: None,
                triggered_at: now + chrono::Duration::minutes(offset),
            })
            .await
            .expect("触发记录应能写入");
    }
    let last = triggers
        .last_triggered_at(pipeline_id, TriggerSource::Renewal)
        .await
        .expect("应能查询");
    // 方言时间舍入（MySQL 四舍五入到秒）给 1 秒容差。
    let expected = now - chrono::Duration::minutes(10);
    assert!(
        (last.expect("应有续期触发记录") - expected).abs() <= chrono::Duration::seconds(1),
        "最近一次续期触发时间应与写入值一致"
    );
    let page = triggers
        .list(Some(pipeline_id), 1, 20)
        .await
        .expect("应能分页查询");
    assert_eq!(page.total, 2, "应有两条触发记录");

    // ---- 部署：幂等指纹 + 历史分页 ----
    let state = Arc::new(DatabaseDeploymentState::new(db.clone()));
    let deployer =
        Deployer::new(Arc::clone(&state) as Arc<dyn acmecast_deploy::DeploymentStateStore>);
    let temp = TempDir::new("dialect-deploy");
    let target_input = serde_json::json!({
        "cert_path": temp.path().join("cert.pem").display().to_string(),
        "key_path": temp.path().join("key.pem").display().to_string()
    });
    let materials = acmecast_deploy::CertMaterials::new(
        "-----BEGIN CERTIFICATE-----\nMIIA\n-----END CERTIFICATE-----\n",
        "-----BEGIN PRIVATE KEY-----\nMIIA\n-----END PRIVATE KEY-----\n",
        "sha256:deploy-one",
    );
    let credentials = acmecast_access::CredentialStore::new(
        &db,
        Arc::new(acmecast_access::CredentialRegistry::new()),
        cipher,
    );
    deployer
        .deploy(
            &acmecast_deploy::LocalTarget,
            &target_input,
            &materials,
            &credentials,
            false,
        )
        .await
        .expect("首次部署应成功");

    let target = acmecast_deploy::target_ref_of("local", &target_input);
    let fingerprint = state
        .deployed_fingerprint(&target)
        .await
        .expect("应能查询指纹");
    assert_eq!(fingerprint.as_deref(), Some("sha256:deploy-one"));
    // target 在此之后移交给按目标过滤的历史查询。

    state
        .record(DeploymentEntry {
            target: target.clone(),
            fingerprint: "sha256:deploy-two".to_owned(),
            skipped_write: false,
            // deployer 内部用真实时钟记录（比测试的 now 晚）；这条再往后推，
            // 倒序断言才与写入顺序无关。
            deployed_at: now + chrono::Duration::seconds(10),
            paths: vec![],
            reload_output: None,
        })
        .await
        .expect("部署记录应能写入");
    let history = state
        .history(acmecast_deploy::DeploymentQuery {
            target: Some(target),
            ..Default::default()
        })
        .await
        .expect("应能查询历史");
    assert_eq!(
        history.total, 2,
        "同一目标的两次部署应有两条历史（容器库可能残留其它目标的记录）"
    );
    assert_eq!(
        history.items[0].fingerprint, "sha256:deploy-two",
        "历史应按时间倒序"
    );

    // ---- 调度引擎扫描在方言上的等价行为（仅读路径） ----
    let launcher = NoopLauncher;
    let engine = SchedulerEngine::new(
        &db,
        &launcher,
        acmecast_scheduler::SchedulerConfig {
            renewal_threshold: chrono::Duration::days(90),
            ..Default::default()
        },
    );
    engine.scan_expiring(now).await.expect("扫描应成功");
}

/// MySQL `DATETIME` 无小数秒：时间等价性断言统一截断到秒。
fn truncated(time: chrono::DateTime<chrono::Utc>) -> chrono::DateTime<chrono::Utc> {
    time - chrono::Duration::nanoseconds(time.timestamp_subsec_nanos() as i64)
}

/// 什么都不做的启动器：等价性套件不需要真的执行流水线。
#[derive(Debug)]
struct NoopLauncher;

#[async_trait::async_trait]
impl acmecast_scheduler::PipelineLauncher for NoopLauncher {
    async fn launch(
        &self,
        _request: acmecast_scheduler::LaunchRequest,
    ) -> acmecast_scheduler::Result<()> {
        Ok(())
    }
}

// ---- 方言入口 ----

#[tokio::test(flavor = "multi_thread")]
async fn sqlite_behaves_the_same() {
    let db = Database::connect("sqlite::memory:")
        .await
        .expect("应能连上内存库");
    run_suite(db).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn mysql_behaves_the_same() {
    let Some(url) = Container::mysql().await else {
        return; // 跳过原因已在容器准备阶段输出
    };
    let Some(db) = connect_with_retry(&url, "MySQL").await else {
        return;
    };
    run_suite(db).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn postgres_behaves_the_same() {
    let Some(url) = Container::postgres().await else {
        return;
    };
    let Some(db) = connect_with_retry(&url, "PostgreSQL").await else {
        return;
    };
    run_suite(db).await;
}

/// 带重试的连接：容器的「服务进程存活」与「可接受连接」之间有窗口，
/// `mysqladmin ping` 过早成功后 TCP 仍可能拒绝一小会儿。
///
/// 超时未就绪返回 `None`（测试跳过并说明）——环境变量直连外部实例
/// （`ACMECAST_TEST_*_URL`）才是稳定的执行路径。
async fn connect_with_retry(url: &str, label: &str) -> Option<DatabaseConnection> {
    let deadline = Instant::now() + Duration::from_secs(60);
    let mut last_error = None;
    while Instant::now() < deadline {
        match Database::connect(url).await {
            Ok(db) => return Some(db),
            Err(error) => last_error = Some(error),
        }
        tokio::time::sleep(Duration::from_secs(3)).await;
    }
    eprintln!(
        "跳过 {label} 等价性测试：连接在 60 秒内未就绪（{:?}）",
        last_error
    );
    None
}

// ---- Docker 容器管理 ----

/// 一个测试用数据库容器；析构时强制删除。
struct Container {
    id: String,
    host_port: u16,
}

impl Drop for Container {
    fn drop(&mut self) {
        let _ = Command::new("docker").args(["rm", "-f", &self.id]).output();
    }
}

impl Container {
    /// 提供 MySQL 连接串：外部实例优先，否则自起容器。
    async fn mysql() -> Option<String> {
        if let Ok(url) = std::env::var("ACMECAST_TEST_MYSQL_URL") {
            return Some(url);
        }
        let container = Self::start(
            "mysql:latest",
            &[
                "-e",
                "MYSQL_ROOT_PASSWORD=root",
                "-e",
                "MYSQL_DATABASE=acmecast_test",
            ],
            "3306",
        )?;
        // MySQL 首次初始化较慢：轮询到能接受连接。
        let deadline = Instant::now() + Duration::from_secs(120);
        while Instant::now() < deadline {
            let ready = Command::new("docker")
                .args([
                    "exec",
                    &container.id,
                    "mysqladmin",
                    "ping",
                    "-uroot",
                    "-proot",
                    "--silent",
                ])
                .output()
                .map(|output| output.status.success())
                .unwrap_or(false);
            if ready && Self::tcp_ready(container.host_port) {
                let port = container.host_port;
                return Some(format!("mysql://root:root@127.0.0.1:{port}/acmecast_test"));
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
        eprintln!("跳过 MySQL 等价性测试：容器在 120 秒内未就绪");
        None
    }

    /// 提供 PostgreSQL 连接串：外部实例优先，否则自起容器。
    async fn postgres() -> Option<String> {
        if let Ok(url) = std::env::var("ACMECAST_TEST_POSTGRES_URL") {
            return Some(url);
        }
        let container = Self::start(
            "postgres:18",
            &[
                "-e",
                "POSTGRES_PASSWORD=postgres",
                "-e",
                "POSTGRES_DB=acmecast_test",
            ],
            "5432",
        )?;
        let deadline = Instant::now() + Duration::from_secs(60);
        while Instant::now() < deadline {
            let ready = Command::new("docker")
                .args(["exec", &container.id, "pg_isready", "-U", "postgres"])
                .output()
                .map(|output| output.status.success())
                .unwrap_or(false);
            if ready && Self::tcp_ready(container.host_port) {
                let port = container.host_port;
                return Some(format!(
                    "postgres://postgres:postgres@127.0.0.1:{port}/acmecast_test"
                ));
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
        eprintln!("跳过 PostgreSQL 等价性测试：容器在 60 秒内未就绪");
        None
    }

    /// 宿主侧 TCP 可达性探测。
    fn tcp_ready(port: u16) -> bool {
        std::net::TcpStream::connect(("127.0.0.1", port)).is_ok()
    }

    /// 起一个把内部端口映射到宿主随机端口的容器。
    fn start(image: &str, env: &[&str], container_port: &str) -> Option<Self> {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").ok()?;
        let host_port = listener.local_addr().ok()?.port();
        drop(listener);

        let mut args = vec![
            "run".to_owned(),
            "-d".to_owned(),
            "--rm".to_owned(),
            "-p".to_owned(),
            format!("{host_port}:{container_port}"),
        ];
        args.extend(env.iter().map(|item| item.to_string()));
        args.push(image.to_owned());

        let output = Command::new("docker").args(&args).output().ok()?;
        if !output.status.success() {
            eprintln!(
                "跳过方言等价性测试：Docker 容器启动失败（{}）",
                String::from_utf8_lossy(&output.stderr).trim()
            );
            return None;
        }
        Some(Self {
            id: String::from_utf8_lossy(&output.stdout).trim().to_owned(),
            host_port,
        })
    }
}
