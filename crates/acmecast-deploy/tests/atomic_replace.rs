//! 8.6 原子替换与失败清理。
//!
//! spec 的两条：写入失败时**临时产物被清理**，且**原有目标文件内容不被破坏**。
//! 后者尤其重要——目标文件可能正被一个运行中的 web 服务器读取。

use std::path::{Path, PathBuf};

use acmecast_access::{CredentialRegistry, CredentialStore};
use acmecast_core::CredentialCipher;
use acmecast_deploy::{CertMaterials, DeployMode, DeploymentTarget, LocalTarget};
use acmecast_store::migrate;
use sea_orm::Database;
use serde_json::json;
use std::sync::Arc;

/// 临时目录，Drop 时清理。
struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("acmecast-atomic-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&path).expect("应能建临时目录");
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        // 只读目录里的内容删不掉，先放开权限。
        let _ = std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(0o755));
        std::fs::remove_dir_all(&self.0).ok();
    }
}

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

/// 目录里留下的文件名（升序）。
fn entries_of(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .expect("应能读目录")
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

/// 有没有留下 `.acmecast-*.tmp` 这样的临时产物。
fn has_temp_leftover(dir: &Path) -> bool {
    entries_of(dir)
        .iter()
        .any(|name| name.contains(".acmecast-") && name.ends_with(".tmp"))
}

fn materials(chain: &str) -> CertMaterials {
    CertMaterials::new(chain, "KEY-PEM", "sha256:abc")
}

async fn credentials() -> (
    &'static sea_orm::DatabaseConnection,
    CredentialStore<'static>,
) {
    let db = Database::connect("sqlite::memory:")
        .await
        .expect("应能连上内存库");
    migrate(&db).await.expect("迁移应成功");
    let db: &'static sea_orm::DatabaseConnection = Box::leak(Box::new(db));

    let cipher = CredentialCipher::from_base64(&CredentialCipher::generate_key_base64()).unwrap();
    let store = CredentialStore::new(db, Arc::new(CredentialRegistry::new()), Arc::new(cipher));
    (db, store)
}

/// 往指定路径部署一份证书。
async fn deploy_to(
    store: &CredentialStore<'_>,
    cert: &Path,
    key: &Path,
    chain: &str,
) -> acmecast_deploy::Result<acmecast_deploy::DeployOutcome> {
    LocalTarget
        .deploy(
            &json!({
                "cert_path": cert.display().to_string(),
                "key_path": key.display().to_string(),
            }),
            &materials(chain),
            store,
            DeployMode::Write,
        )
        .await
}

// ---- 正常路径 ----

#[tokio::test]
async fn a_successful_write_leaves_only_the_two_targets() {
    let (_db, store) = credentials().await;
    let temp = TempDir::new();
    let cert = temp.path().join("cert.pem");
    let key = temp.path().join("key.pem");

    deploy_to(&store, &cert, &key, "CHAIN")
        .await
        .expect("应成功");

    assert_eq!(std::fs::read_to_string(&cert).unwrap(), "CHAIN");
    // 临时产物不该留在原地。
    assert_eq!(
        entries_of(temp.path()),
        vec!["cert.pem".to_owned(), "key.pem".to_owned()],
        "成功后不应有临时文件残留"
    );
    assert!(!has_temp_leftover(temp.path()));
}

#[tokio::test]
async fn an_existing_file_is_replaced_with_the_new_content() {
    let (_db, store) = credentials().await;
    let temp = TempDir::new();
    let cert = temp.path().join("cert.pem");
    let key = temp.path().join("key.pem");

    deploy_to(&store, &cert, &key, "OLD")
        .await
        .expect("首次应成功");
    deploy_to(&store, &cert, &key, "NEW")
        .await
        .expect("覆盖应成功");

    assert_eq!(std::fs::read_to_string(&cert).unwrap(), "NEW");
    assert!(!has_temp_leftover(temp.path()));
}

// ---- 失败路径 ----

#[tokio::test]
async fn a_failed_replace_cleans_up_its_temporary_file() {
    // 目标路径是个目录：写入临时文件会成功，替换那一步会失败。
    let (_db, store) = credentials().await;
    let temp = TempDir::new();
    let cert = temp.path().join("cert.pem");
    let key = temp.path().join("key.pem");

    std::fs::create_dir(&cert).expect("应能建目录占位");
    std::fs::write(cert.join("原有内容"), "别动我").unwrap();

    let err = deploy_to(&store, &cert, &key, "CHAIN")
        .await
        .expect_err("目标是目录时应失败");

    assert!(err.to_string().contains("原子替换失败"), "{err}");
    // 临时产物被清掉了。
    assert!(
        !has_temp_leftover(temp.path()),
        "失败后不该留下临时文件: {:?}",
        entries_of(temp.path())
    );
    // 原有目标（这里是目录及其内容）没被破坏。
    assert!(cert.is_dir(), "原有目标不该被替换成文件");
    assert_eq!(
        std::fs::read_to_string(cert.join("原有内容")).unwrap(),
        "别动我"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn a_write_that_cannot_start_leaves_the_existing_file_intact() {
    // 父目录只读 → 临时文件建不出来 → 部署失败，而原文件必须原封不动。
    if nix::unistd::geteuid().is_root() {
        // root 无视只读权限，这条前提在 root 下不成立。
        return;
    }

    let (_db, store) = credentials().await;
    let temp = TempDir::new();
    let dir = temp.path().join("ssl");
    std::fs::create_dir_all(&dir).unwrap();
    let cert = dir.join("cert.pem");
    let key = dir.join("key.pem");
    std::fs::write(&cert, "旧证书").unwrap();
    std::fs::write(&key, "旧私钥").unwrap();

    // 先把目录设为只读。
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o555)).unwrap();

    let result = deploy_to(&store, &cert, &key, "新证书").await;

    // 恢复权限，好让断言与清理都能进行。
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();

    result.expect_err("只读目录里无法写入");
    assert_eq!(
        std::fs::read_to_string(&cert).unwrap(),
        "旧证书",
        "写入失败时原文件内容不该被破坏"
    );
    assert_eq!(std::fs::read_to_string(&key).unwrap(), "旧私钥");
    assert!(
        !has_temp_leftover(&dir),
        "失败后不该留下临时文件: {:?}",
        entries_of(&dir)
    );
}

#[tokio::test]
async fn temporary_names_are_unique_per_write() {
    // 名字里带随机串：并发部署同一个目标时各自的临时文件不会互相踩。
    let one = acmecast_deploy::targets::local::temp_path_for(Path::new("/etc/ssl/cert.pem"));
    let two = acmecast_deploy::targets::local::temp_path_for(Path::new("/etc/ssl/cert.pem"));

    assert_ne!(one, two);
    // 与目标同目录，`rename` 才是原子的。
    assert_eq!(one.parent(), Path::new("/etc/ssl/cert.pem").parent());
}
