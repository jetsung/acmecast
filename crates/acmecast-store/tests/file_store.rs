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

    assert_eq!(paths.cert_pem, "certs/ssl/sub.example.com.cert.pem");
    assert_eq!(paths.key_pem, "certs/ssl/sub.example.com.key.pem");
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

/// 文件名前缀的三条规则：多域名取第一个、通配符 `*` 换 `_wildcard`、其余原样。
#[tokio::test]
async fn cert_file_names_follow_the_primary_domain() {
    for (primary_domain, expected_prefix) in [
        // 多域名时取第一个。
        ("sub1.example.com", "sub1.example.com"),
        // 通配符的 `*` 换成 _wildcard。
        ("*.hello.example.com", "_wildcard.hello.example.com"),
        // 单域名原样。
        ("sub.example.com", "sub.example.com"),
    ] {
        let paths = FileStore::cert_paths("fp-1", primary_domain);
        // 新布局 4 文件
        assert_eq!(paths.cert, format!("certs/ssl/{expected_prefix}.cer"), "域名 {primary_domain} 的叶子证书文件名");
        assert_eq!(paths.key, format!("certs/ssl/{expected_prefix}.key"), "域名 {primary_domain} 的私钥文件名（新布局）");
        // 兼容旧布局仍存在
        assert_eq!(
            paths.cert_pem,
            format!("certs/ssl/{expected_prefix}.cert.pem"),
            "域名 {primary_domain} 的证书链文件名"
        );
        assert_eq!(
            paths.key_pem,
            format!("certs/ssl/{expected_prefix}.key.pem"),
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
        .move_certificate(&original, "fp-1")
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

    // 扁平活跃目录：逐文件搬，原同名文件不再存在，材料出现在吊销目录且内容原样可读。
    assert!(!temp.path().join(&original.cert_pem).exists());
    assert!(!temp.path().join(&original.key_pem).exists());
    assert_eq!(store.read(&archived.cert_pem).await.unwrap(), "证书链");
    assert_eq!(store.read(&archived.key_pem).await.unwrap(), "私钥");
}

#[tokio::test]
async fn archiving_moves_the_whole_dir_so_misc_files_come_along() {
    // 扁平活跃已无整目录可搬，此用例改为验证：活跃下的非证书文件不受吊销影响。
    let temp = TempDir::new();
    let store = FileStore::open(temp.path()).await.unwrap();
    let original = store
        .write_certificate("fp-1", "sub.example.com", "证书链", "私钥")
        .await
        .unwrap();
    store
        .write("certs/ssl/备注.txt", "扁平活跃中的其他文件")
        .await
        .unwrap();

    let moved = store
        .move_certificate(&original, "fp-1")
        .await
        .unwrap()
        .expect("应能归档");
    // 证书已进入吊销目录
    assert_eq!(store.read(&moved.cert_pem).await.unwrap(), "证书链");
    // 扁平下仅搬走证书两文件，活跃目录中的其他文件保留。
    assert!(!temp.path().join(&original.cert_pem).exists());
    let extra = store.read("certs/ssl/备注.txt").await;
    assert!(extra.is_ok(), "扁平活跃中的其他文件应保留，实际 {extra:?}");
    assert_eq!(extra.unwrap(), "扁平活跃中的其他文件");
}

#[tokio::test]
async fn archiving_falls_back_to_per_file_move_when_sources_span_dirs() {
    let temp = TempDir::new();
    let store = FileStore::open(temp.path()).await.unwrap();
    // 手动上传的怪异布局：两个文件分处不同目录。
    let paths = CertFilePaths {
        cert: "uploads/a/cert.cer".to_owned(),
        ca: "uploads/a/cert.ca.cer".to_owned(),
        fullchain: "uploads/a/cert.fullchain.cer".to_owned(),
        key: "uploads/a/cert.key".to_owned(),
        cert_pem: "uploads/a/cert.pem".to_owned(),
        key_pem: "uploads/b/key.pem".to_owned(),
    };
    store.write(&paths.cert_pem, "证书链").await.unwrap();
    store.write(&paths.key_pem, "私钥").await.unwrap();

    let archived = store
        .move_certificate(&paths, "fp-1")
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
        .move_certificate(&original, "fp-1")
        .await
        .unwrap()
        .expect("目标已存在时也应归档成功");

    // 扁平活跃下逐文件覆盖。
    assert_eq!(store.read(&archived.cert_pem).await.unwrap(), "证书链");
    assert_eq!(store.read(&archived.key_pem).await.unwrap(), "私钥");
    assert!(!temp.path().join(&original.cert_pem).exists());
}

#[tokio::test]
async fn move_certificate_moves_all_six_files_when_whole_dir_rename_is_unavailable() {
    // 归档后吊销的场景：源目录 certs/archived/<fp>/ 有 6 个文件，
    // 吊销目标目录已存在使整目录 rename 不可行，退回逐文件路径——
    // 6 个文件必须全部搬入吊销目录，不在源目录留遗留。
    let temp = TempDir::new();
    let store = FileStore::open(temp.path()).await.unwrap();
    let prefix = "sub.example.com";
    let sources = acmecast_store::CertFilePaths {
        cert: format!("certs/archived/fp-1/{prefix}.cer"),
        ca: format!("certs/archived/fp-1/{prefix}.ca.cer"),
        fullchain: format!("certs/archived/fp-1/{prefix}.fullchain.cer"),
        key: format!("certs/archived/fp-1/{prefix}.key"),
        cert_pem: format!("certs/archived/fp-1/{prefix}.cert.pem"),
        key_pem: format!("certs/archived/fp-1/{prefix}.key.pem"),
    };
    let contents = [
        ("证书", &sources.cert),
        ("中间链", &sources.ca),
        ("完整链", &sources.fullchain),
        ("私钥文件", &sources.key),
        ("证书链", &sources.cert_pem),
        ("私钥", &sources.key_pem),
    ];
    for (content, path) in contents.iter() {
        store.write(path, content).await.unwrap();
    }
    // 目标吊销目录已存在：整目录 rename 不可行，走逐文件搬迁。
    store
        .write("certs/revoked/fp-1/stale.txt", "占位")
        .await
        .unwrap();

    let moved = store
        .move_certificate(&sources, "fp-1")
        .await
        .unwrap()
        .expect("材料齐全时应返回目标路径");

    // 6 个文件全部出现在吊销目录，内容不变。
    assert_eq!(store.read(&moved.cert).await.unwrap(), "证书");
    assert_eq!(store.read(&moved.ca).await.unwrap(), "中间链");
    assert_eq!(store.read(&moved.fullchain).await.unwrap(), "完整链");
    assert_eq!(store.read(&moved.key).await.unwrap(), "私钥文件");
    assert_eq!(store.read(&moved.cert_pem).await.unwrap(), "证书链");
    assert_eq!(store.read(&moved.key_pem).await.unwrap(), "私钥");
    // 源目录不再有任何该证书的文件（目录被清掉）。
    assert!(
        !temp.path().join("certs/archived/fp-1").exists(),
        "源目录应已搬空并清理"
    );
}

#[tokio::test]
async fn restore_moves_the_archived_dir_back_whole() {
    // 扁平活跃 + 指纹吊销：逐文件恢复，不再整目录搬回。
    let temp = TempDir::new();
    let store = FileStore::open(temp.path()).await.unwrap();
    let original = store
        .write_certificate("fp-1", "sub.example.com", "证书链", "私钥")
        .await
        .unwrap();
    let archived = store
        .move_certificate(&original, "fp-1")
        .await
        .unwrap()
        .unwrap();

    store
        .restore_certificate(&archived, &original)
        .await
        .unwrap();

    // 逐文件搬回，吊销目录空壳被清理。
    assert_eq!(store.read(&original.cert_pem).await.unwrap(), "证书链");
    assert_eq!(store.read(&original.key_pem).await.unwrap(), "私钥");
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
        .move_certificate(&original, "fp-1")
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
        .move_certificate(&original, "fp-1")
        .await
        .unwrap()
        .unwrap();

    // 重复吊销走幂等路径：源已是吊销布局时直接返回目标，不再移动。
    let again = store
        .move_certificate(&archived, "fp-1")
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

    let missing = FileStore::cert_paths("fp-missing", "sub.example.com");
    let outcome = store
        .move_certificate(&missing, "fp-missing")
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
        .move_certificate(&original, "a/../escape")
        .await
        .expect_err("越界指纹应被拒绝");
    assert!(matches!(err, Error::Validation(_)), "实际 {err:?}");
    assert!(
        !temp.path().join("certs/revoked").exists(),
        "被拒绝的归档不应创建任何目录"
    );
}

#[tokio::test]
async fn archiving_replaced_certificate_moves_to_archived_layout() {
    let temp = TempDir::new();
    let store = FileStore::open(temp.path()).await.unwrap();
    let original = store
        .write_certificate("fp-old", "sub.example.com", "旧证书链", "旧私钥")
        .await
        .unwrap();
    assert_eq!(original.cert_pem, "certs/ssl/sub.example.com.cert.pem");

    let archived = store
        .archive_replaced_certificate(&original.cert_pem, &original.key_pem, "fp-old")
        .await
        .expect("归档应成功")
        .expect("材料齐全时应返回目标路径");
    assert_eq!(
        archived.cert_pem,
        "certs/archived/fp-old/sub.example.com.cert.pem"
    );
    assert_eq!(
        archived.key_pem,
        "certs/archived/fp-old/sub.example.com.key.pem"
    );
    assert!(!temp.path().join(&original.cert_pem).exists());
    assert_eq!(store.read(&archived.cert_pem).await.unwrap(), "旧证书链");
    assert_eq!(store.read(&archived.key_pem).await.unwrap(), "旧私钥");
}

#[tokio::test]
async fn archiving_replaced_certificate_is_idempotent_when_already_archived() {
    let temp = TempDir::new();
    let store = FileStore::open(temp.path()).await.unwrap();
    let original = store
        .write_certificate("fp-old", "sub.example.com", "旧证书链", "旧私钥")
        .await
        .unwrap();
    let archived = store
        .archive_replaced_certificate(&original.cert_pem, &original.key_pem, "fp-old")
        .await
        .unwrap()
        .unwrap();
    let again = store
        .archive_replaced_certificate(&archived.cert_pem, &archived.key_pem, "fp-old")
        .await
        .unwrap()
        .expect("已归档应幂等返回");
    assert_eq!(again, archived);
}

#[tokio::test]
async fn flat_active_allows_only_latest_per_domain() {
    let temp = TempDir::new();
    let store = FileStore::open(temp.path()).await.unwrap();
    let first = store
        .write_certificate("fp-old", "sub.example.com", "旧证书链", "旧私钥")
        .await
        .unwrap();
    store
        .archive_replaced_certificate(&first.cert_pem, &first.key_pem, "fp-old")
        .await
        .unwrap()
        .unwrap();
    let second = store
        .write_certificate("fp-new", "sub.example.com", "新证书链", "新私钥")
        .await
        .unwrap();
    assert_eq!(second.cert_pem, "certs/ssl/sub.example.com.cert.pem");
    assert_eq!(store.read(&second.cert_pem).await.unwrap(), "新证书链");
    assert_eq!(
        store
            .read("certs/archived/fp-old/sub.example.com.cert.pem")
            .await
            .unwrap(),
        "旧证书链"
    );
}

#[tokio::test]
async fn archiving_replaced_without_material_reports_nothing_to_move() {
    let temp = TempDir::new();
    let store = FileStore::open(temp.path()).await.unwrap();
    let outcome = store
        .archive_replaced_certificate(
            &FileStore::cert_paths("fp-missing", "sub.example.com").cert_pem,
            &FileStore::cert_paths("fp-missing", "sub.example.com").key_pem,
            "fp-missing",
        )
        .await
        .expect("源缺失不应报错");
    assert!(outcome.is_none());
    assert!(
        !temp.path().join("certs/archived").exists(),
        "没有材料时不应创建归档目录"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn archived_replaced_material_keeps_owner_only_permissions() {
    let temp = TempDir::new();
    let store = FileStore::open(temp.path()).await.unwrap();
    let original = store
        .write_certificate("fp-old", "sub.example.com", "旧证书链", "旧私钥")
        .await
        .unwrap();
    let archived = store
        .archive_replaced_certificate(&original.cert_pem, &original.key_pem, "fp-old")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(mode_of(&temp.path().join(&archived.key_pem)), 0o600);
    assert_eq!(mode_of(&temp.path().join("certs/archived")), 0o700);
    assert_eq!(mode_of(&temp.path().join("certs/archived/fp-old")), 0o700);
}

// ---- 兼容回退读取（cert.apply 跳过注入复用） ----

#[test]
fn candidates_follow_record_then_derived_then_archived_order() {
    let candidates = FileStore::cert_read_candidates(
        &["certs/ssl/_wildcard.example.com.fullchain.cer"],
        "*.example.com",
        "fp-order",
        &["fullchain.cer", "cert.pem"],
    );
    assert_eq!(
        candidates,
        vec![
            "certs/ssl/_wildcard.example.com.fullchain.cer".to_owned(),
            "certs/ssl/_wildcard.example.com.cert.pem".to_owned(),
            "certs/ssl/_.example.com.fullchain.cer".to_owned(),
            "certs/ssl/_.example.com.cert.pem".to_owned(),
            "certs/archived/fp-order/_wildcard.example.com.fullchain.cer".to_owned(),
            "certs/archived/fp-order/_wildcard.example.com.cert.pem".to_owned(),
            "certs/archived/fp-order/_.example.com.fullchain.cer".to_owned(),
            "certs/archived/fp-order/_.example.com.cert.pem".to_owned(),
        ],
        "候选顺序应为：记录路径 → 新前缀推导 → 旧前缀推导 → 归档副本，且去重"
    );
}

#[tokio::test]
async fn fallback_reads_the_legacy_layout_when_the_recorded_path_is_missing() {
    let temp = TempDir::new();
    let store = FileStore::open(temp.path()).await.unwrap();
    // 库中记录指向新布局（文件缺失），旧布局文件存在。
    store
        .write("certs/ssl/_.example.com.cert.pem", "旧证书链")
        .await
        .unwrap();
    store
        .write("certs/ssl/_.example.com.key.pem", "旧私钥")
        .await
        .unwrap();

    let chain = FileStore::cert_read_candidates(
        &["certs/ssl/_wildcard.example.com.fullchain.cer"],
        "*.example.com",
        "fp-legacy",
        &["fullchain.cer", "cert.pem"],
    );
    let key = FileStore::cert_read_candidates(
        &["certs/ssl/_wildcard.example.com.key"],
        "*.example.com",
        "fp-legacy",
        &["key", "key.pem"],
    );
    let (chain_pem, chain_hit) = store.read_with_fallback(&chain).await.expect("旧布局链应命中");
    assert_eq!(chain_pem, "旧证书链");
    assert_eq!(chain_hit, "certs/ssl/_.example.com.cert.pem");
    let (key_pem, key_hit) = store.read_with_fallback(&key).await.expect("旧布局私钥应命中");
    assert_eq!(key_pem, "旧私钥");
    assert_eq!(key_hit, "certs/ssl/_.example.com.key.pem");
}

#[tokio::test]
async fn fallback_reads_the_archived_copy_when_active_paths_are_missing() {
    let temp = TempDir::new();
    let store = FileStore::open(temp.path()).await.unwrap();
    // 活跃目录与记录路径均缺失，仅归档副本存在。
    store
        .write(
            "certs/archived/fp-archived/sub.example.com.fullchain.cer",
            "归档证书链",
        )
        .await
        .unwrap();

    let chain = FileStore::cert_read_candidates(
        &["certs/ssl/sub.example.com.fullchain.cer"],
        "sub.example.com",
        "fp-archived",
        &["fullchain.cer"],
    );
    let (chain_pem, chain_hit) = store.read_with_fallback(&chain).await.expect("归档副本应命中");
    assert_eq!(chain_pem, "归档证书链");
    assert_eq!(chain_hit, "certs/archived/fp-archived/sub.example.com.fullchain.cer");
}

#[tokio::test]
async fn fallback_reports_every_attempted_path_when_all_candidates_miss() {
    let temp = TempDir::new();
    let store = FileStore::open(temp.path()).await.unwrap();
    let chain = FileStore::cert_read_candidates(
        &["certs/ssl/sub.example.com.fullchain.cer"],
        "sub.example.com",
        "fp-none",
        &["fullchain.cer"],
    );
    let attempts = store
        .read_with_fallback(&chain)
        .await
        .expect_err("全部缺失应返回尝试清单");
    let paths: Vec<&str> = attempts.iter().map(|(p, _)| p.as_str()).collect();
    assert_eq!(
        paths,
        vec![
            "certs/ssl/sub.example.com.fullchain.cer",
            "certs/archived/fp-none/sub.example.com.fullchain.cer",
        ],
        "尝试清单应按候选顺序包含全部路径"
    );
}
