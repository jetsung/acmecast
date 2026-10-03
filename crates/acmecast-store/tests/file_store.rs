//! 2.5 数据目录文件存储。
//!
//! 两条断言最要紧：
//! - 落盘后得到的是**相对路径**——库里存的就是它，因此必须能直接拼到数据目录上；
//! - 私钥文件的权限**仅限服务运行用户可读**。

use std::path::{Path, PathBuf};

use acmecast_store::{CertFilePaths, Error, FileStore};

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
            "sub.example.com",
            "-----BEGIN CERTIFICATE-----\n链\n",
            "-----BEGIN PRIVATE KEY-----\n钥\n",
        )
        .await
        .expect("应能写入证书与私钥");

    // 返回的必须是相对路径——库里存的就是这两个值。
    // 文件名带主域名：sub.example.com.cert.pem / sub.example.com.key.pem。
    assert_eq!(paths.cert_pem, "certs/fp-1/sub.example.com.cert.pem");
    assert_eq!(paths.key_pem, "certs/fp-1/sub.example.com.key.pem");
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
        .write_certificate("fp-1", "sub.example.com", "证书", "私钥")
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
    let paths = FileStore::cert_paths("fp-1", "sub.example.com");

    store.write(&paths.cert_pem, "第一版").await.unwrap();
    store.write(&paths.cert_pem, "第二版").await.unwrap();

    assert_eq!(store.read(&paths.cert_pem).await.unwrap(), "第二版");
}

#[tokio::test]
async fn deleting_reports_whether_the_file_existed() {
    let temp = TempDir::new();
    let store = FileStore::open(temp.path()).await.unwrap();
    let paths = store
        .write_certificate("fp-1", "sub.example.com", "证书", "私钥")
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

/// 文件名前缀的三条规则：多域名取第一个、通配符 `*` 换 `_`、其余原样。
#[tokio::test]
async fn cert_file_names_follow_the_primary_domain() {
    for (primary_domain, expected_prefix) in [
        // 多域名时取第一个。
        ("sub1.example.com", "sub1.example.com"),
        // 通配符的 `*` 换成下划线。
        ("*.hello.example.com", "_.hello.example.com"),
        // 单域名原样。
        ("sub.example.com", "sub.example.com"),
    ] {
        let paths = FileStore::cert_paths("fp-1", primary_domain);
        assert_eq!(
            paths.cert_pem,
            format!("certs/fp-1/{expected_prefix}.cert.pem"),
            "域名 {primary_domain} 的证书链文件名"
        );
        assert_eq!(
            paths.key_pem,
            format!("certs/fp-1/{expected_prefix}.key.pem"),
            "域名 {primary_domain} 的私钥文件名"
        );
    }
}

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

// ---- 吊销归档 ----

#[tokio::test]
async fn archiving_moves_certificate_material_into_the_revoked_layout() {
    let temp = TempDir::new();
    let store = FileStore::open(temp.path()).await.unwrap();
    let original = store
        .write_certificate("fp-1", "sub.example.com", "证书链", "私钥")
        .await
        .unwrap();

    let archived = store
        .move_certificate(&original.cert_pem, &original.key_pem, "fp-1")
        .await
        .expect("归档应成功")
        .expect("材料齐全时应返回目标路径");

    // 归档只换目录不改名：域名文件名原样进入吊销目录。
    assert_eq!(
        archived.cert_pem,
        "certs/revoked/fp-1/sub.example.com.cert.pem"
    );
    assert_eq!(
        archived.key_pem,
        "certs/revoked/fp-1/sub.example.com.key.pem"
    );

    // 原位置不再保留（连空目录一并清掉），材料出现在吊销目录且内容原样可读。
    assert!(!temp.path().join(&original.cert_pem).exists());
    assert!(!temp.path().join(&original.key_pem).exists());
    assert!(
        !temp.path().join("certs/fp-1").exists(),
        "搬空的源目录不应留下空壳"
    );
    assert_eq!(store.read(&archived.cert_pem).await.unwrap(), "证书链");
    assert_eq!(store.read(&archived.key_pem).await.unwrap(), "私钥");
}

#[tokio::test]
async fn archiving_moves_the_whole_dir_so_misc_files_come_along() {
    let temp = TempDir::new();
    let store = FileStore::open(temp.path()).await.unwrap();
    let original = store
        .write_certificate("fp-1", "sub.example.com", "证书链", "私钥")
        .await
        .unwrap();
    store
        .write("certs/fp-1/备注.txt", "手动上传时混放的其他文件")
        .await
        .unwrap();

    store
        .move_certificate(&original.cert_pem, &original.key_pem, "fp-1")
        .await
        .unwrap()
        .unwrap();

    // 整目录归档：混放的其他文件跟着目录一起进吊销目录，原地什么都不剩。
    assert!(!temp.path().join("certs/fp-1").exists());
    assert_eq!(
        store.read("certs/revoked/fp-1/备注.txt").await.unwrap(),
        "手动上传时混放的其他文件"
    );
}

#[tokio::test]
async fn archiving_falls_back_to_per_file_move_when_sources_span_dirs() {
    let temp = TempDir::new();
    let store = FileStore::open(temp.path()).await.unwrap();
    // 手动上传的怪异布局：两个文件分处不同目录。
    let paths = CertFilePaths {
        cert_pem: "uploads/a/cert.pem".to_owned(),
        key_pem: "uploads/b/key.pem".to_owned(),
    };
    store.write(&paths.cert_pem, "证书链").await.unwrap();
    store.write(&paths.key_pem, "私钥").await.unwrap();

    let archived = store
        .move_certificate(&paths.cert_pem, &paths.key_pem, "fp-1")
        .await
        .unwrap()
        .expect("材料齐全时应返回目标路径");

    assert_eq!(archived.cert_pem, "certs/revoked/fp-1/cert.pem");
    assert_eq!(store.read(&archived.cert_pem).await.unwrap(), "证书链");
    assert_eq!(store.read(&archived.key_pem).await.unwrap(), "私钥");
    // 各自的源目录已搬空：空壳不应留下。
    assert!(!temp.path().join("uploads/a").exists());
    assert!(!temp.path().join("uploads/b").exists());
}

#[tokio::test]
async fn archiving_overwrites_when_the_revoked_dir_already_exists() {
    let temp = TempDir::new();
    let store = FileStore::open(temp.path()).await.unwrap();
    let original = store
        .write_certificate("fp-1", "sub.example.com", "证书链", "私钥")
        .await
        .unwrap();
    store
        .write("certs/revoked/fp-1/sub.example.com.cert.pem", "旧材料")
        .await
        .unwrap();

    let archived = store
        .move_certificate(&original.cert_pem, &original.key_pem, "fp-1")
        .await
        .unwrap()
        .expect("目标已存在时也应归档成功");

    // 整搬不可行（目标已在），退回逐文件覆盖。
    assert_eq!(store.read(&archived.cert_pem).await.unwrap(), "证书链");
    assert_eq!(store.read(&archived.key_pem).await.unwrap(), "私钥");
    assert!(!temp.path().join("certs/fp-1").exists());
}

#[tokio::test]
async fn restore_moves_the_archived_dir_back_whole() {
    let temp = TempDir::new();
    let store = FileStore::open(temp.path()).await.unwrap();
    let original = store
        .write_certificate("fp-1", "sub.example.com", "证书链", "私钥")
        .await
        .unwrap();
    store
        .write("certs/fp-1/备注.txt", "混放文件")
        .await
        .unwrap();
    let archived = store
        .move_certificate(&original.cert_pem, &original.key_pem, "fp-1")
        .await
        .unwrap()
        .unwrap();

    store
        .restore_certificate(&archived, &original)
        .await
        .unwrap();

    // 整目录搬回：混放文件也跟着回来，吊销目录下不留残留。
    assert_eq!(store.read(&original.cert_pem).await.unwrap(), "证书链");
    assert_eq!(store.read(&original.key_pem).await.unwrap(), "私钥");
    assert_eq!(store.read("certs/fp-1/备注.txt").await.unwrap(), "混放文件");
    assert!(!temp.path().join("certs/revoked/fp-1").exists());
}

#[cfg(unix)]
#[tokio::test]
async fn archived_material_keeps_owner_only_permissions() {
    let temp = TempDir::new();
    let store = FileStore::open(temp.path()).await.unwrap();
    let original = store
        .write_certificate("fp-1", "sub.example.com", "证书链", "私钥")
        .await
        .unwrap();

    let archived = store
        .move_certificate(&original.cert_pem, &original.key_pem, "fp-1")
        .await
        .unwrap()
        .unwrap();

    assert_eq!(
        mode_of(&temp.path().join(&archived.key_pem)),
        0o600,
        "归档后的私钥仍应仅属主可读写"
    );
    assert_eq!(
        mode_of(&temp.path().join("certs/revoked")),
        0o700,
        "吊销根目录不应开放给其他用户"
    );
    assert_eq!(
        mode_of(&temp.path().join("certs/revoked/fp-1")),
        0o700,
        "指纹目录同样不应开放给其他用户"
    );
}

#[tokio::test]
async fn archiving_an_already_archived_certificate_is_a_no_op() {
    let temp = TempDir::new();
    let store = FileStore::open(temp.path()).await.unwrap();
    let original = store
        .write_certificate("fp-1", "sub.example.com", "证书链", "私钥")
        .await
        .unwrap();
    let archived = store
        .move_certificate(&original.cert_pem, &original.key_pem, "fp-1")
        .await
        .unwrap()
        .unwrap();

    // 重复吊销走幂等路径：源已是吊销布局时直接返回目标，不再移动。
    let again = store
        .move_certificate(&archived.cert_pem, &archived.key_pem, "fp-1")
        .await
        .unwrap()
        .expect("已归档的证书应幂等返回目标路径");
    assert_eq!(again, archived);
    assert_eq!(store.read(&again.cert_pem).await.unwrap(), "证书链");
}

#[tokio::test]
async fn archiving_without_material_reports_nothing_to_move() {
    let temp = TempDir::new();
    let store = FileStore::open(temp.path()).await.unwrap();

    let outcome = store
        .move_certificate(
            &FileStore::cert_paths("fp-missing", "sub.example.com").cert_pem,
            &FileStore::cert_paths("fp-missing", "sub.example.com").key_pem,
            "fp-missing",
        )
        .await
        .expect("源缺失不应报错");

    assert!(outcome.is_none(), "没有材料时应返回 None 而不是目标路径");
    assert!(
        !temp.path().join("certs/revoked").exists(),
        "没有材料时不应创建吊销目录"
    );
}

#[tokio::test]
async fn an_evil_fingerprint_is_rejected_when_archiving() {
    let temp = TempDir::new();
    let store = FileStore::open(temp.path()).await.unwrap();
    let original = store
        .write_certificate("fp-1", "sub.example.com", "证书链", "私钥")
        .await
        .unwrap();

    let err = store
        .move_certificate(&original.cert_pem, &original.key_pem, "a/../escape")
        .await
        .expect_err("越界指纹应被拒绝");
    assert!(matches!(err, Error::Validation(_)), "实际 {err:?}");
    assert!(
        !temp.path().join("certs/revoked").exists(),
        "被拒绝的归档不应创建任何目录"
    );
}
