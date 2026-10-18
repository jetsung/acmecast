//! 8.2 本地文件系统部署。
//!
//! 真的往临时目录里写文件——权限与属主这类事，只有落到真实文件系统上
//! 才有意义（mock 掉的正是要验证的东西）。

use std::path::{Path, PathBuf};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

use acmecast_access::{CredentialRegistry, CredentialStore};
use acmecast_core::CredentialCipher;
use acmecast_deploy::{
    CertMaterials, DeployMode, DeploymentRegistry, DeploymentTarget, LocalTarget,
};
use acmecast_store::migrate;
use sea_orm::Database;
use serde_json::json;
use std::sync::Arc;

/// 临时目录，Drop 时清理。
struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("acmecast-deploy-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&path).expect("应能建临时目录");
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

/// 文件模式位。
#[cfg(unix)]
fn mode_of(path: &Path) -> u32 {
    std::fs::metadata(path)
        .expect("应能读元数据")
        .permissions()
        .mode()
        & 0o777
}

/// 单证书链的材料：没有中间链可写，ca 文件应被跳过。
fn materials() -> CertMaterials {
    CertMaterials::new(
        "-----BEGIN CERTIFICATE-----\n链\n-----END CERTIFICATE-----\n",
        "-----BEGIN PRIVATE KEY-----\n钥\n-----END PRIVATE KEY-----\n",
        "sha256:abc",
    )
}

/// 叶子 + 中间两段的材料：ca 文件有内容可写。
fn chained_materials() -> CertMaterials {
    CertMaterials::new(
        concat!(
            "-----BEGIN CERTIFICATE-----\n叶子\n-----END CERTIFICATE-----\n",
            "-----BEGIN CERTIFICATE-----\n中间\n-----END CERTIFICATE-----\n"
        ),
        "-----BEGIN PRIVATE KEY-----\n钥\n-----END PRIVATE KEY-----\n",
        "sha256:chain",
    )
}

/// 建一个带凭据入口的部署环境。
async fn setup() -> (TempDir, sea_orm::DatabaseConnection, LocalTarget) {
    let temp = TempDir::new();
    let db = Database::connect("sqlite::memory:")
        .await
        .expect("应能连上内存库");
    migrate(&db).await.expect("迁移应成功");
    (temp, db, LocalTarget)
}

fn credentials_of(db: &sea_orm::DatabaseConnection) -> CredentialStore<'_> {
    let cipher = CredentialCipher::from_base64(&CredentialCipher::generate_key_base64())
        .expect("密钥应可用");
    CredentialStore::new(db, Arc::new(CredentialRegistry::new()), Arc::new(cipher))
}

/// 走一遍部署（单证书链）。
async fn deploy(
    target: &LocalTarget,
    db: &sea_orm::DatabaseConnection,
    input: serde_json::Value,
) -> acmecast_deploy::Result<acmecast_deploy::DeployOutcome> {
    deploy_with(target, db, input, materials()).await
}

/// 指定材料走一遍部署。
async fn deploy_with(
    target: &LocalTarget,
    db: &sea_orm::DatabaseConnection,
    input: serde_json::Value,
    materials: CertMaterials,
) -> acmecast_deploy::Result<acmecast_deploy::DeployOutcome> {
    let credentials = credentials_of(db);
    target
        .deploy(&input, &materials, &credentials, DeployMode::Write)
        .await
}

// ---- Scenario: 写入指定路径 ----

#[tokio::test]
async fn a_certificate_and_its_key_land_on_the_configured_paths() {
    let (temp, db, target) = setup().await;
    let cert = temp.path().join("ssl/fullchain.pem");
    let key = temp.path().join("ssl/privkey.pem");

    let outcome = deploy(
        &target,
        &db,
        json!({
            "fullchain_path": cert.display().to_string(),
            "key_path": key.display().to_string(),
        }),
    )
    .await
    .expect("应能部署");

    assert!(!outcome.skipped_write);
    assert_eq!(
        std::fs::read_to_string(&cert).unwrap(),
        materials().chain_pem
    );
    assert_eq!(std::fs::read_to_string(&key).unwrap(), materials().key_pem);

    // 返回的路径与配置一致。
    assert_eq!(
        outcome.paths,
        vec![cert.display().to_string(), key.display().to_string()]
    );
}

#[tokio::test]
async fn missing_parent_directories_are_created() {
    let (temp, db, target) = setup().await;
    let cert = temp.path().join("deeply/nested/dir/cert.pem");
    let key = temp.path().join("deeply/nested/dir/key.pem");

    deploy(
        &target,
        &db,
        json!({
            "fullchain_path": cert.display().to_string(),
            "key_path": key.display().to_string(),
        }),
    )
    .await
    .expect("应能创建缺失的目录再写入");

    assert!(cert.is_file());
    assert!(key.is_file());
}

// ---- 三文件布局 ----

#[tokio::test]
async fn three_configured_paths_get_the_full_chain_ca_and_key() {
    let (temp, db, target) = setup().await;
    let fullchain = temp.path().join("ssl/example.com.fullchain.cer");
    let ca = temp.path().join("ssl/example.com.ca.cer");
    let key = temp.path().join("ssl/example.com.key");
    let materials = chained_materials();

    let outcome = deploy_with(
        &target,
        &db,
        json!({
            "fullchain_path": fullchain.display().to_string(),
            "ca_path": ca.display().to_string(),
            "key_path": key.display().to_string(),
        }),
        materials.clone(),
    )
    .await
    .expect("应能部署");

    // fullchain 原样落链，ca 落拆链剩余，key 原样。
    assert_eq!(
        std::fs::read_to_string(&fullchain).unwrap(),
        materials.chain_pem
    );
    let ca_content = std::fs::read_to_string(&ca).unwrap();
    assert_eq!(ca_content, acmecast_cert::ca_pem_of(&materials.chain_pem));
    assert!(ca_content.contains("中间"), "ca 应含中间证书");
    assert_eq!(std::fs::read_to_string(&key).unwrap(), materials.key_pem);

    // 返回的路径按 fullchain → ca → key 顺序。
    assert_eq!(
        outcome.paths,
        vec![
            fullchain.display().to_string(),
            ca.display().to_string(),
            key.display().to_string(),
        ]
    );
}

#[tokio::test]
async fn an_absent_ca_path_derives_the_sibling_ca_file() {
    let (temp, db, target) = setup().await;
    let fullchain = temp.path().join("ssl/example.com.fullchain.cer");
    let key = temp.path().join("ssl/example.com.key");
    let materials = chained_materials();

    let outcome = deploy_with(
        &target,
        &db,
        json!({
            "fullchain_path": fullchain.display().to_string(),
            "key_path": key.display().to_string(),
        }),
        materials.clone(),
    )
    .await
    .expect("应能部署");

    // 未配置 ca_path：按 fullchain 同目录派生 `<stem>.ca.cer`。
    let derived = temp.path().join("ssl/example.com.ca.cer");
    assert_eq!(
        std::fs::read_to_string(&derived).unwrap(),
        acmecast_cert::ca_pem_of(&materials.chain_pem)
    );
    assert_eq!(
        outcome.paths,
        vec![
            fullchain.display().to_string(),
            derived.display().to_string(),
            key.display().to_string(),
        ]
    );
}

#[tokio::test]
async fn a_blank_ca_path_writes_only_fullchain_and_key() {
    let (temp, db, target) = setup().await;
    let fullchain = temp.path().join("ssl/fullchain.cer");
    let key = temp.path().join("ssl/key");

    let outcome = deploy_with(
        &target,
        &db,
        json!({
            "fullchain_path": fullchain.display().to_string(),
            "ca_path": "",
            "key_path": key.display().to_string(),
        }),
        chained_materials(),
    )
    .await
    .expect("应能部署");

    assert!(fullchain.is_file());
    assert!(key.is_file());
    assert!(
        !temp.path().join("ssl/ca.cer").exists(),
        "显式留空时不应写 ca 文件"
    );
    assert_eq!(outcome.paths.len(), 2, "只应写两个文件");
}

#[tokio::test]
async fn directory_paths_get_the_default_file_names() {
    let (temp, db, target) = setup().await;
    // 已有的目录（is_dir）与以 `/` 结尾的目录都按默认文件名落盘。
    let fullchain_dir = temp.path().join("ssl");
    std::fs::create_dir_all(&fullchain_dir).unwrap();
    let ca_dir = temp.path().join("ca-dir");
    let key_dir = temp.path().join("key-dir");

    let outcome = deploy_with(
        &target,
        &db,
        json!({
            "fullchain_path": fullchain_dir.display().to_string(),
            "ca_path": format!("{}/", ca_dir.display()),
            "key_path": format!("{}/", key_dir.display()),
        }),
        chained_materials(),
    )
    .await
    .expect("目录路径应能部署");

    let fullchain = fullchain_dir.join("fullchain.cer");
    let ca = ca_dir.join("ca.cer");
    let key = key_dir.join("key");
    assert!(fullchain.is_file(), "应写入 fullchain.cer");
    assert!(ca.is_file(), "应写入 ca.cer");
    assert!(key.is_file(), "应写入 key");
    assert_eq!(
        outcome.paths,
        vec![
            fullchain.display().to_string(),
            ca.display().to_string(),
            key.display().to_string(),
        ]
    );
}

#[tokio::test]
async fn a_single_cert_chain_skips_the_ca_file() {
    let (temp, db, target) = setup().await;
    let cert = temp.path().join("ssl/cert.pem");
    let key = temp.path().join("ssl/key.pem");

    let outcome = deploy(
        &target,
        &db,
        json!({
            "fullchain_path": cert.display().to_string(),
            "key_path": key.display().to_string(),
        }),
    )
    .await
    .expect("应能部署");

    assert!(cert.is_file());
    assert!(
        !temp.path().join("ssl/cert.ca.cer").exists(),
        "链里没有中间证书时不应写 ca 文件"
    );
    assert_eq!(
        outcome.paths,
        vec![cert.display().to_string(), key.display().to_string()]
    );
}

// ---- 旧输入兼容 ----

#[tokio::test]
async fn a_legacy_cert_path_input_still_deploys() {
    let (temp, db, target) = setup().await;
    let cert = temp.path().join("ssl/cert.pem");
    let key = temp.path().join("ssl/key.pem");

    let outcome = deploy(
        &target,
        &db,
        json!({
            "cert_path": cert.display().to_string(),
            "key_path": key.display().to_string(),
        }),
    )
    .await
    .expect("旧输入应经 serde alias 兼容");

    assert_eq!(
        std::fs::read_to_string(&cert).unwrap(),
        materials().chain_pem
    );
    assert_eq!(outcome.paths.len(), 2, "单证书链下只写两个文件");
}

#[tokio::test]
async fn an_input_carrying_both_path_keys_uses_the_new_one() {
    // 前端把旧值回显成 `cert_path` 又补写 `fullchain_path`，保存时两个键同在；
    // 后端应丢弃旧键而不是报 `duplicate field`。
    let (temp, db, target) = setup().await;
    let fullchain = temp.path().join("ssl/new.fullchain.cer");
    let legacy = temp.path().join("ssl/old.pem");
    let key = temp.path().join("ssl/key.pem");

    let outcome = deploy_with(
        &target,
        &db,
        json!({
            "fullchain_path": fullchain.display().to_string(),
            "cert_path": legacy.display().to_string(),
            "key_path": key.display().to_string(),
        }),
        chained_materials(),
    )
    .await
    .expect("双键同在时应去重后部署");

    assert!(fullchain.is_file());
    assert!(!legacy.exists(), "旧键不应被写入");
    // 派生 ca 跟随新路径。
    assert!(temp.path().join("ssl/new.ca.cer").is_file());
    assert!(outcome.paths.contains(&fullchain.display().to_string()));
}

// ---- 权限 ----

#[cfg(unix)]
#[tokio::test]
async fn the_default_modes_are_applied() {
    let (temp, db, target) = setup().await;
    let fullchain = temp.path().join("example.com.fullchain.cer");
    let key = temp.path().join("example.com.key");

    deploy_with(
        &target,
        &db,
        json!({
            "fullchain_path": fullchain.display().to_string(),
            "key_path": key.display().to_string(),
        }),
        chained_materials(),
    )
    .await
    .expect("应能部署");

    let ca = temp.path().join("example.com.ca.cer");
    assert_eq!(mode_of(&fullchain), 0o644, "证书默认对所有人可读");
    assert_eq!(mode_of(&ca), 0o644, "中间链与证书同权限");
    assert_eq!(mode_of(&key), 0o600, "私钥默认只给属主");
}

#[cfg(unix)]
#[tokio::test]
async fn configured_modes_override_the_defaults() {
    let (temp, db, target) = setup().await;
    let fullchain = temp.path().join("fullchain.cer");
    let ca = temp.path().join("ca.cer");
    let key = temp.path().join("key");

    deploy_with(
        &target,
        &db,
        json!({
            "fullchain_path": fullchain.display().to_string(),
            "ca_path": ca.display().to_string(),
            "key_path": key.display().to_string(),
            "cert_mode": "0640",
            "ca_mode": "0444",
            "key_mode": "0400",
        }),
        chained_materials(),
    )
    .await
    .expect("应能部署");

    assert_eq!(mode_of(&fullchain), 0o640);
    assert_eq!(mode_of(&ca), 0o444);
    assert_eq!(mode_of(&key), 0o400, "配置的权限应覆盖默认值");
}

#[cfg(unix)]
#[tokio::test]
async fn an_existing_file_gets_its_mode_corrected() {
    // 覆盖一个权限被改过的旧文件时，权限应当被拉回配置值——
    // 否则「部署成功」与「权限正确」之间会悄悄脱节。
    let (temp, db, target) = setup().await;
    let key = temp.path().join("key.pem");
    std::fs::write(&key, "旧的").unwrap();
    std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert_eq!(mode_of(&key), 0o644, "用例前提：旧文件权限过宽");

    deploy(
        &target,
        &db,
        json!({
            "fullchain_path": temp.path().join("cert.pem").display().to_string(),
            "key_path": key.display().to_string(),
        }),
    )
    .await
    .expect("应能部署");

    assert_eq!(mode_of(&key), 0o600, "权限应被纠正回配置值");
}

#[tokio::test]
async fn an_invalid_mode_is_refused_before_writing_anything() {
    let (temp, db, target) = setup().await;
    let cert = temp.path().join("cert.pem");
    let key = temp.path().join("key.pem");

    let err = deploy(
        &target,
        &db,
        json!({
            "fullchain_path": cert.display().to_string(),
            "key_path": key.display().to_string(),
            "cert_mode": "rw-r--r--",
        }),
    )
    .await
    .expect_err("非法权限应被拒绝");

    assert!(err.to_string().contains("cert_mode"), "{err}");
    assert!(!cert.exists(), "不该留下写了一半的文件");
    assert!(!key.exists());
}

// ---- 属主 ----

#[cfg(unix)]
#[tokio::test]
async fn setting_the_owner_to_oneself_succeeds() {
    // chown 到自己不需要特权，因此这条能在普通环境下跑通。
    let (temp, db, target) = setup().await;
    let cert = temp.path().join("cert.pem");
    let key = temp.path().join("key.pem");
    let me = nix::unistd::getuid().as_raw();

    deploy(
        &target,
        &db,
        json!({
            "fullchain_path": cert.display().to_string(),
            "key_path": key.display().to_string(),
            "uid": me,
        }),
    )
    .await
    .expect("chown 到自己应成功");

    use std::os::unix::fs::MetadataExt;
    assert_eq!(std::fs::metadata(&cert).unwrap().uid(), me);
}

#[cfg(unix)]
#[tokio::test]
async fn an_owner_change_that_needs_privileges_says_so() {
    // 非 root 改成别人所有必然失败；错误信息要指出这一点，
    // 而不是丢一个光秃秃的 EPERM 出来。
    if nix::unistd::geteuid().is_root() {
        // root 下这条前提不成立，跳过。
        return;
    }

    let (temp, db, target) = setup().await;
    let cert = temp.path().join("cert.pem");
    let key = temp.path().join("key.pem");

    let err = deploy(
        &target,
        &db,
        json!({
            "fullchain_path": cert.display().to_string(),
            "key_path": key.display().to_string(),
            "uid": 0,
        }),
    )
    .await
    .expect_err("非特权用户 chown 到 root 应失败");

    assert!(err.to_string().contains("root"), "应提示需要特权: {err}");
}

// ---- Scenario: 写入后执行重载命令 ----

#[tokio::test]
async fn a_successful_reload_keeps_the_deployment_successful() {
    let (temp, db, target) = setup().await;
    let cert = temp.path().join("cert.pem");
    let key = temp.path().join("key.pem");

    let outcome = deploy(
        &target,
        &db,
        json!({
            "fullchain_path": cert.display().to_string(),
            "key_path": key.display().to_string(),
            "reload_command": "echo reloaded",
        }),
    )
    .await
    .expect("重载成功则部署成功");

    assert!(!outcome.skipped_write);
    assert!(
        outcome
            .reload_output
            .as_deref()
            .unwrap_or_default()
            .contains("reloaded"),
        "命令输出应被记下: {:?}",
        outcome.reload_output
    );
}

#[cfg(unix)]
#[tokio::test]
async fn a_failing_reload_fails_the_whole_deployment() {
    // spec：重载命令执行失败时，整个部署步骤标记为失败。
    let (temp, db, target) = setup().await;
    let cert = temp.path().join("cert.pem");
    let key = temp.path().join("key.pem");

    let err = deploy(
        &target,
        &db,
        json!({
            "fullchain_path": cert.display().to_string(),
            "key_path": key.display().to_string(),
            // 像 nginx 那样把问题写进 stderr 再返回非零。
            "reload_command": "echo 'nginx: 配置文件第 42 行有误' >&2; exit 3",
        }),
    )
    .await
    .expect_err("重载失败应导致部署失败");

    let text = err.to_string();
    assert!(
        text.contains("nginx: 配置文件第 42 行有误"),
        "应含命令输出: {text}"
    );
    assert!(text.contains('3'), "应含退出码: {text}");
}

#[tokio::test]
async fn a_failed_write_skips_the_reload_entirely() {
    // 写一个文件就出错时不该去重载——那会让服务去加载一份不完整的证书。
    //
    // 注意：这只保证「没重载」。第一个文件已写、第二个失败时留下的半个部署
    // 由 8.6 的原子替换负责，不在这里断言。
    let (temp, db, target) = setup().await;
    let cert = temp.path().join("cert.pem");
    let key = temp.path().join("key.pem");
    let marker = temp.path().join("reload-ran");

    let err = deploy(
        &target,
        &db,
        json!({
            "fullchain_path": cert.display().to_string(),
            "key_path": key.display().to_string(),
            "key_mode": "not-a-mode",
            "reload_command": format!("touch {}", marker.display()),
        }),
    )
    .await
    .expect_err("写失败应报错");

    assert!(err.to_string().contains("key_mode"), "{err}");
    assert!(!marker.exists(), "写失败时不该执行重载");
}

#[tokio::test]
async fn without_a_reload_command_nothing_is_executed() {
    let (temp, db, target) = setup().await;
    let cert = temp.path().join("cert.pem");
    let key = temp.path().join("key.pem");

    let outcome = deploy(
        &target,
        &db,
        json!({
            "fullchain_path": cert.display().to_string(),
            "key_path": key.display().to_string(),
        }),
    )
    .await
    .expect("应能部署");

    assert!(outcome.reload_output.is_none(), "未配置就不该有重载输出");
}

#[cfg(unix)]
#[tokio::test]
async fn stderr_is_part_of_the_recorded_output() {
    // 重载脚本常把真正的原因写进 stderr；只留 stdout 会丢掉它。
    let (temp, db, target) = setup().await;
    let cert = temp.path().join("cert.pem");
    let key = temp.path().join("key.pem");

    let outcome = deploy(
        &target,
        &db,
        json!({
            "fullchain_path": cert.display().to_string(),
            "key_path": key.display().to_string(),
            "reload_command": "echo '配置语法正确' >&2",
        }),
    )
    .await
    .expect("stderr 不影响退出码");

    assert!(
        outcome
            .reload_output
            .as_deref()
            .unwrap_or_default()
            .contains("配置语法正确"),
        "stderr 应被记下: {:?}",
        outcome.reload_output
    );
}

#[tokio::test]
async fn an_unwritable_path_is_reported() {
    // 写不进去时不该假装成功。
    let (temp, db, target) = setup().await;
    let cert = temp.path().join("cert.pem");

    let err = deploy(
        &target,
        &db,
        json!({
            "fullchain_path": cert.display().to_string(),
            // 一个不可能是目录的路径（父目录是刚写的文件）。
            "key_path": format!("{}/child/key.pem", cert.display()),
        }),
    )
    .await
    .expect_err("父目录是文件时应报错");

    assert!(err.to_string().contains("父目录"), "{err}");
}

// ---- 注册表 ----

#[test]
fn the_local_target_is_reachable_through_the_registry() {
    let mut registry = DeploymentRegistry::new();
    registry.register(LocalTarget).expect("应能注册");

    assert_eq!(
        registry.require("local").unwrap().display_name(),
        "本地文件系统"
    );
    assert_eq!(registry.type_ids(), vec!["local".to_owned()]);
}
