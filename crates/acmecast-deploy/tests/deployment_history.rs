//! 8.7 部署结果记录的查询。
//!
//! spec 的场景很直白：查某一部署步骤的历史执行记录，返回每次的目标、时间与
//! 是否跳过写入。这里跑**真实部署**来产生记录，再按三个维度筛。

use std::path::PathBuf;
use std::sync::Arc;

use acmecast_access::{CredentialRegistry, CredentialStore};
use acmecast_core::CredentialCipher;
use acmecast_deploy::{
    CertMaterials, DatabaseDeploymentState, Deployer, DeploymentQuery, DeploymentStateStore,
    LocalTarget,
};
use acmecast_store::migrate;
use sea_orm::Database;
use serde_json::json;

/// 临时目录，Drop 时清理。
struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("acmecast-hist-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&path).expect("应能建临时目录");
        Self(path)
    }

    fn path(&self) -> &std::path::Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).ok();
    }
}

/// 建一次部署环境：内存库（已跑全部迁移）+ 落库的记录存储 + 执行器。
async fn setup() -> (
    sea_orm::DatabaseConnection,
    CredentialStore<'static>,
    Arc<DatabaseDeploymentState>,
    Deployer,
) {
    let db = Database::connect("sqlite::memory:")
        .await
        .expect("应能连上内存库");
    migrate(&db).await.expect("迁移应成功");

    // 记录存储现在持有连接本身，凭据存储仍借引用——用 Box::leak 把
    // 连接放进 'static，两个存储共享同一连接池。
    let leaked: &'static sea_orm::DatabaseConnection = Box::leak(Box::new(db.clone()));
    let cipher = CredentialCipher::from_base64(&CredentialCipher::generate_key_base64()).unwrap();
    let credentials = CredentialStore::new(
        leaked,
        Arc::new(CredentialRegistry::new()),
        Arc::new(cipher),
    );

    let state = Arc::new(DatabaseDeploymentState::new(db.clone()));
    let deployer = Deployer::new(Arc::clone(&state) as Arc<dyn DeploymentStateStore>);
    (db, credentials, state, deployer)
}

/// 往某个目录部署一次，带重载命令。
async fn deploy_into(
    deployer: &Deployer,
    store: &CredentialStore<'_>,
    temp: &TempDir,
    fingerprint: &str,
    force: bool,
) -> acmecast_deploy::Result<acmecast_deploy::DeployOutcome> {
    deployer
        .deploy(
            &LocalTarget,
            &json!({
                "cert_path": temp.path().join("cert.pem").display().to_string(),
                "key_path": temp.path().join("key.pem").display().to_string(),
                "reload_command": "echo 重载完成",
            }),
            &CertMaterials::new("CHAIN", "KEY", fingerprint),
            store,
            force,
        )
        .await
}

#[tokio::test]
async fn a_deployment_is_recorded_with_its_paths_and_reload_output() {
    let (_db, store, state, deployer) = setup().await;
    let temp = TempDir::new();

    deploy_into(&deployer, &store, &temp, "sha256:aaa", false)
        .await
        .expect("应能部署");

    let page = state
        .history(DeploymentQuery::default())
        .await
        .expect("应能查询");
    assert_eq!(page.total, 1);

    let entry = &page.items[0];
    assert_eq!(entry.target.target_type, "local");
    assert_eq!(entry.fingerprint, "sha256:aaa");
    assert!(!entry.skipped_write);

    // spec 点名的「写入路径」与「重载命令执行结果」都在。
    assert_eq!(
        entry.paths,
        vec![
            temp.path().join("cert.pem").display().to_string(),
            temp.path().join("key.pem").display().to_string(),
        ]
    );
    assert!(
        entry
            .reload_output
            .as_deref()
            .unwrap_or_default()
            .contains("重载完成"),
        "重载输出应被记下: {:?}",
        entry.reload_output
    );
}

#[tokio::test]
async fn history_reflects_the_skip_flag_across_runs() {
    let (_db, store, state, deployer) = setup().await;
    let temp = TempDir::new();

    // 第一次真写，第二次同指纹跳过。
    deploy_into(&deployer, &store, &temp, "sha256:aaa", false)
        .await
        .expect("首次应成功");
    deploy_into(&deployer, &store, &temp, "sha256:aaa", false)
        .await
        .expect("重复应成功");

    let all = state.history(DeploymentQuery::default()).await.unwrap();
    assert_eq!(all.total, 2);
    assert!(!all.items[1].skipped_write, "较早那条是真写");
    assert!(all.items[0].skipped_write, "较新那条跳过了写入");

    let skipped = state
        .history(DeploymentQuery {
            skipped_write: Some(true),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(skipped.total, 1);
    assert_eq!(skipped.items[0].fingerprint, "sha256:aaa");
}

#[tokio::test]
async fn history_is_scoped_by_target_and_time() {
    let (_db, store, state, deployer) = setup().await;
    let first = TempDir::new();
    let second = TempDir::new();

    deploy_into(&deployer, &store, &first, "sha256:one", false)
        .await
        .unwrap();
    deploy_into(&deployer, &store, &second, "sha256:two", false)
        .await
        .unwrap();

    // 两处目录是两个目标，各自只有一条。
    let all = state.history(DeploymentQuery::default()).await.unwrap();
    assert_eq!(all.total, 2);
    let one_target = all
        .items
        .iter()
        .find(|entry| entry.fingerprint == "sha256:one")
        .expect("应有第一条")
        .target
        .clone();

    let scoped = state
        .history(DeploymentQuery {
            target: Some(one_target),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(scoped.total, 1);
    assert_eq!(scoped.items[0].fingerprint, "sha256:one");

    // 时间窗：把「未来」当作起点，应当什么都筛不出来。
    let future = state
        .history(DeploymentQuery {
            since: Some(chrono::Utc::now() + chrono::Duration::hours(1)),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(future.total, 0, "时间窗之外不该有记录");
}

#[tokio::test]
async fn history_pages_without_repeats() {
    let (_db, store, state, deployer) = setup().await;
    let temp = TempDir::new();

    // 每次都换指纹，产生 5 条真写入的记录。
    for index in 0..5 {
        deploy_into(&deployer, &store, &temp, &format!("sha256:{index}"), false)
            .await
            .unwrap();
    }

    let mut seen = Vec::new();
    for page_number in 1..=3 {
        let page = state
            .history(DeploymentQuery {
                page: page_number,
                page_size: 2,
                ..Default::default()
            })
            .await
            .unwrap();

        assert_eq!(page.total, 5, "总数与页码无关");
        seen.extend(page.items.into_iter().map(|entry| entry.fingerprint));
    }

    assert_eq!(seen.len(), 5, "三页应覆盖全部 5 条");
    let mut unique = seen.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(unique.len(), 5, "同一条不该出现在两页里");
}

#[tokio::test]
async fn history_survives_a_fresh_state_store() {
    // 换个存储实例来查（相当于进程重启后重新装配），记录仍在——
    // 这正是把它落库而不是放内存的意义。
    let (_db, store, _state, deployer) = setup().await;
    let temp = TempDir::new();
    deploy_into(&deployer, &store, &temp, "sha256:aaa", false)
        .await
        .unwrap();

    let reopened = DatabaseDeploymentState::new(_db.clone());
    let page = reopened.history(DeploymentQuery::default()).await.unwrap();

    assert_eq!(page.total, 1);
    assert!(
        page.items[0]
            .reload_output
            .as_deref()
            .unwrap_or_default()
            .contains("重载完成"),
        "重新装配后仍应读到完整记录"
    );
}
