//! 8.5 幂等部署与强制开关。
//!
//! 三个场景：**指纹未变 → 跳过写入但仍重载；勾了强制 → 重新写入；指纹变了 → 写入。

use std::sync::Arc;

use acmecast_access::{CredentialRegistry, CredentialStore};
use acmecast_core::CredentialCipher;
use acmecast_deploy::{CertMaterials, DatabaseDeploymentState, Deployer, LocalTarget};
use acmecast_store::migrate;
use sea_orm::Database;
use serde_json::json;

/// 临时目录，Drop 时清理。
struct TempDir(std::path::PathBuf);

impl TempDir {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("acmecast-idem-{}", uuid::Uuid::new_v4()));
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

/// 建一次部署环境：内存库 + 落库的部署状态 + 本地目标。
async fn setup() -> (
    sea_orm::DatabaseConnection,
    CredentialStore<'static>,
    Deployer,
) {
    let db = Database::connect("sqlite::memory:")
        .await
        .expect("应能连上内存库");
    migrate(&db).await.expect("迁移应成功");
    let leaked: &'static sea_orm::DatabaseConnection = Box::leak(Box::new(db.clone()));

    let cipher = CredentialCipher::from_base64(&CredentialCipher::generate_key_base64()).unwrap();
    let store = CredentialStore::new(
        leaked,
        Arc::new(CredentialRegistry::new()),
        Arc::new(cipher),
    );

    let state = DatabaseDeploymentState::new(db.clone());
    let deployer = Deployer::new(Arc::new(state));
    (db, store, deployer)
}

fn materials(fingerprint: &str) -> CertMaterials {
    CertMaterials::new("CERT", "KEY", fingerprint)
}

/// 往临时目录部署一次。
async fn deploy(
    deployer: &Deployer,
    store: &CredentialStore<'_>,
    temp: &TempDir,
    fingerprint: &str,
    force: bool,
) -> acmecast_deploy::Result<acmecast_deploy::DeployOutcome> {
    let cert = temp.path().join("cert.pem");
    let key = temp.path().join("key.pem");
    let marker = temp.path().join("reload-ran");

    let outcome = deployer
        .deploy(
            &LocalTarget,
            &json!({
                "cert_path": cert.display().to_string(),
                "key_path": key.display().to_string(),
                "reload_command": format!("touch {}", marker.display()),
            }),
            &materials(fingerprint),
            store,
            force,
        )
        .await?;

    // 重载命令是否真的跑过——用它来证明「跳过写入 ≠ 跳过重载」。
    Ok(outcome.with_reload_output(if marker.exists() { "ran" } else { "not-run" }))
}

// ---- Scenario: 指纹未变化 ----

#[tokio::test]
async fn an_unchanged_fingerprint_skips_the_write_but_still_reloads() {
    let (_db, store, deployer) = setup().await;
    let temp = TempDir::new();

    let first = deploy(&deployer, &store, &temp, "sha256:aaa", false)
        .await
        .expect("首次应部署成功");
    assert!(!first.skipped_write, "首次要真写");
    assert_eq!(first.reload_output.as_deref(), Some("ran"));

    // 同一份证书再来一次。
    let second = deploy(&deployer, &store, &temp, "sha256:aaa", false)
        .await
        .expect("重复部署应成功");

    assert!(second.skipped_write, "指纹一致时应跳过写入: {:?}", second);
    // 关键：写入省了，重载没省。
    assert_eq!(
        second.reload_output.as_deref(),
        Some("ran"),
        "跳过写入时重载仍要走完"
    );
}

#[tokio::test]
async fn a_changed_fingerprint_writes_again() {
    let (_db, store, deployer) = setup().await;
    let temp = TempDir::new();

    deploy(&deployer, &store, &temp, "sha256:aaa", false)
        .await
        .expect("首次应成功");

    let renewed = deploy(&deployer, &store, &temp, "sha256:bbb", false)
        .await
        .expect("续期后应成功");
    assert!(!renewed.skipped_write, "指纹变了就必须重新写入");

    // 文件确实是新内容。
    let content = std::fs::read_to_string(temp.path().join("cert.pem")).unwrap();
    assert_eq!(content, "CERT");

    // 并且这次写入后的指纹被记住了。
    let again = deploy(&deployer, &store, &temp, "sha256:bbb", false)
        .await
        .expect("再部署应成功");
    assert!(again.skipped_write, "新指纹应被记下");

    // 回退到旧指纹也要重写。
    let rolled_back = deploy(&deployer, &store, &temp, "sha256:aaa", false)
        .await
        .expect("回退应成功");
    assert!(!rolled_back.skipped_write);
}

// ---- Scenario: 强制部署 ----

#[tokio::test]
async fn force_rewrites_even_when_the_fingerprint_matches() {
    let (_db, store, deployer) = setup().await;
    let temp = TempDir::new();

    deploy(&deployer, &store, &temp, "sha256:aaa", false)
        .await
        .expect("首次应成功");

    let forced = deploy(&deployer, &store, &temp, "sha256:aaa", true)
        .await
        .expect("强制部署应成功");
    assert!(
        !forced.skipped_write,
        "勾选强制时即使指纹一致也重新写入: {:?}",
        forced
    );
}

#[tokio::test]
async fn a_different_target_is_judged_independently() {
    // 幂等是按目标判的：换一处目录就该重新写。
    let (_db, store, deployer) = setup().await;
    let first = TempDir::new();
    let second = TempDir::new();

    deploy(&deployer, &store, &first, "sha256:aaa", false)
        .await
        .expect("首个目标应成功");

    let elsewhere = deploy(&deployer, &store, &second, "sha256:aaa", false)
        .await
        .expect("另一处应成功");
    assert!(
        !elsewhere.skipped_write,
        "不同目标互不影响: {:?}",
        elsewhere
    );

    // 它自己再部署一次就该跳过了。
    let repeated = deploy(&deployer, &store, &second, "sha256:aaa", false)
        .await
        .expect("重复应成功");
    assert!(repeated.skipped_write);
}

#[tokio::test]
async fn a_failed_deployment_is_not_remembered() {
    // 失败的那次不该被当成「已部署」——否则重试时会被判成指纹一致、跳过写入，
    // 那份证书就永远不会真的被写进去。

    let (_db, store, deployer) = setup().await;
    let temp = TempDir::new();
    let cert = temp.path().join("cert.pem");

    let bad = json!({
        "cert_path": cert.display().to_string(),
        "key_path": temp.path().join("key.pem").display().to_string(),
        // 权限写错 → 部署失败。
        "cert_mode": "oops",
    });

    deployer
        .deploy(&LocalTarget, &bad, &materials("sha256:aaa"), &store, false)
        .await
        .expect_err("坏的部署应失败");

    // 换成正确的配置再部署一次——如果失败被记住了，这里会被判成跳过写入。
    let retried = deploy(&deployer, &store, &temp, "sha256:aaa", false)
        .await
        .expect("重试应成功");
    assert!(
        !retried.skipped_write,
        "失败的那次不该被记为已部署: {:?}",
        retried
    );
}
