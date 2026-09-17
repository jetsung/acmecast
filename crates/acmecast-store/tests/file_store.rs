//! 2.5 数据目录文件存储。
//!
//! 两条断言最要紧：
//! - 落盘后得到的是**相对路径**——库里存的就是它，因此必须能直接拼到数据目录上；
//! - 私钥文件的权限**仅限服务运行用户可读**。

use std::path::{Path, PathBuf};

use acmecast_store::{Error, FileStore};

/// 临时目录守卫。
///
/// 刻意**不**在构造时创建目录：让 `FileStore::open` 自己去建，
/// 这样它建出来的权限才是被测到的那一份。
struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        let path =
            std::env::temp_dir().join(format!("acmecast-filestore-{}", uuid::Uuid::new_v4()));
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).ok();
    }
}

/// 取路径的权限位（去掉文件类型位）。
#[cfg(unix)]
fn mode_of(path: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .expect("应能读取元数据")
        .permissions()
        .mode()
        & 0o777
}

// ---- 数据目录 ----

#[tokio::test]
async fn opening_creates_the_data_directory() {
    let temp = TempDir::new();
    assert!(!temp.path().exists(), "用例前提：目录起初不存在");

    let store = FileStore::open(temp.path())
        .await
        .expect("应能创建数据目录");

    assert!(temp.path().is_dir(), "数据目录应被创建");
    assert_eq!(store.root(), temp.path());
}

#[cfg(unix)]
#[tokio::test]
async fn the_data_directory_is_not_open_to_others() {
    let temp = TempDir::new();
    FileStore::open(temp.path()).await.unwrap();

    assert_eq!(
        mode_of(temp.path()),
        0o700,
        "数据目录不应让属主之外的任何人有权限"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn an_already_open_directory_is_tightened() {
    use std::os::unix::fs::PermissionsExt;

    // 模拟运维事先建好、但权限开得过宽的数据目录。
    let temp = TempDir::new();
    std::fs::create_dir_all(temp.path()).unwrap();
    std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(mode_of(temp.path()), 0o755, "用例前提：目录起初是 0755");

    FileStore::open(temp.path()).await.unwrap();

    assert_eq!(mode_of(temp.path()), 0o700, "过宽的权限应被收紧");
}

// ---- 证书落盘 ----

#[tokio::test]
async fn certificate_material_round_trips_through_relative_paths() {
    let temp = TempDir::new();
    let store = FileStore::open(temp.path()).await.unwrap();

    let paths = store
        .write_certificate(
            "fp-1",
            "-----BEGIN CERTIFICATE-----\n链\n",
            "-----BEGIN PRIVATE KEY-----\n钥\n",
        )
        .await
        .expect("应能写入证书与私钥");

    // 返回的必须是相对路径——库里存的就是这两个值。
    assert_eq!(paths.cert_pem, "certs/fp-1/cert.pem");
    assert_eq!(paths.key_pem, "certs/fp-1/key.pem");
    assert!(Path::new(&paths.cert_pem).is_relative());
    assert!(Path::new(&paths.key_pem).is_relative());

    // 相对路径能直接拼到数据目录上，说明它与落盘位置是自洽的。
    assert!(temp.path().join(&paths.cert_pem).is_file());
    assert!(temp.path().join(&paths.key_pem).is_file());

    // 内容可原样读回。
    assert!(
        store
            .read(&paths.cert_pem)
            .await
            .unwrap()
            .contains("BEGIN CERTIFICATE")
    );
    assert!(
        store
            .read(&paths.key_pem)
            .await
            .unwrap()
            .contains("BEGIN PRIVATE KEY")
    );
}

#[cfg(unix)]
#[tokio::test]
async fn the_private_key_is_only_readable_by_the_owner() {
    let temp = TempDir::new();
    let store = FileStore::open(temp.path()).await.unwrap();

    let paths = store
        .write_certificate("fp-1", "证书", "私钥")
        .await
        .unwrap();

    assert_eq!(
        mode_of(&temp.path().join(&paths.key_pem)),
        0o600,
        "私钥文件不应开放给同机其他用户"
    );
    // 证书本身不是秘密，但数据目录整体受限，文件同样按最小权限处理。
    assert_eq!(mode_of(&temp.path().join(&paths.cert_pem)), 0o600);
}

#[tokio::test]
async fn writing_again_replaces_the_content() {
    let temp = TempDir::new();
    let store = FileStore::open(temp.path()).await.unwrap();
    // 路径规则是纯函数，与具体的 store 实例无关。
    let paths = FileStore::cert_paths("fp-1");

    store.write(&paths.cert_pem, "第一版").await.unwrap();
    store.write(&paths.cert_pem, "第二版").await.unwrap();

    assert_eq!(store.read(&paths.cert_pem).await.unwrap(), "第二版");
}

#[tokio::test]
async fn deleting_reports_whether_the_file_existed() {
    let temp = TempDir::new();
    let store = FileStore::open(temp.path()).await.unwrap();
    let paths = store
        .write_certificate("fp-1", "证书", "私钥")
        .await
        .unwrap();

    assert!(
        store.delete(&paths.key_pem).await.unwrap(),
        "首次删除应报告删掉了"
    );
    assert!(
        !store.delete(&paths.key_pem).await.unwrap(),
        "重复删除应报告文件不存在"
    );
    assert!(store.read(&paths.key_pem).await.is_err());
    assert!(
        temp.path().join(&paths.cert_pem).is_file(),
        "只应删掉被指定的那个文件"
    );
}

// ---- 路径逃逸防护 ----

#[tokio::test]
async fn paths_escaping_the_data_directory_are_rejected() {
    let temp = TempDir::new();
    let store = FileStore::open(temp.path()).await.unwrap();

    for evil in [
        "../acmecast-escape-probe.pem",
        "certs/../../acmecast-escape-probe.pem",
        "/etc/passwd",
        "./x.pem",
        "",
    ] {
        let err = store
            .write(evil, "不该被写出去")
            .await
            .expect_err("越界路径应被拒绝");
        assert!(matches!(err, Error::Validation(_)), "{evil:?} 实际 {err:?}");

        // 读与删走的是同一条校验，同样不该放行。
        assert!(store.read(evil).await.is_err(), "{evil:?} 读也应被拒绝");
        assert!(store.delete(evil).await.is_err(), "{evil:?} 删也应被拒绝");
    }

    // 数据目录之外不应留下任何东西。
    let escape_target = temp
        .path()
        .parent()
        .unwrap()
        .join("acmecast-escape-probe.pem");
    assert!(
        !escape_target.exists(),
        "越界写入不应真的落地: {}",
        escape_target.display()
    );
}
