//! 8.4 SSH 远程部署。
//!
//! CI 里起一台 sshd 既不现实也不稳定，因此连接被抽象成可替换的
//! [`SshConnector`]。这里验证的是**部署逻辑**：两种认证都能拿到凭据、
//! 主机档案能补齐连接配置、文件写到指定远程路径、远程重载的成败如何
//! 决定整个步骤的成败。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use acmecast_access::{CredentialRegistry, CredentialStore, CredentialType};
use acmecast_core::CredentialCipher;
use acmecast_deploy::{
    CertMaterials, DeployMode, DeploymentRegistry, DeploymentTarget, Error, Result, ResolvedSshConfig,
    SshAuth, SshAuthSource, SshConnector, SshTarget, SshTransport, SshHostFields,
    ssh_host_fields_schema,
};
use acmecast_store::entity::credential;
use acmecast_store::migrate;
use async_trait::async_trait;
use chrono::Utc;
use schemars::schema::RootSchema;
use sea_orm::{ActiveModelTrait, Database, Set};
use serde_json::{Value, json};

/// 假的「远程主机」状态。
#[derive(Debug, Default)]
struct Remote {
    /// 路径 → （内容、权限）。
    files: Mutex<HashMap<String, (String, String)>>,
    /// 收到的远程命令。
    commands: Mutex<Vec<String>>,
    /// 让 exec 返回非零退出码。
    fail_reload: Mutex<bool>,
}

impl Remote {
    fn file(&self, path: &str) -> Option<(String, String)> {
        self.files.lock().expect("锁不应中毒").get(path).cloned()
    }

    fn commands(&self) -> Vec<String> {
        self.commands.lock().expect("锁不应中毒").clone()
    }
}

/// 假的传输层。
#[derive(Debug)]
struct FakeTransport {
    remote: Arc<Remote>,
    config: ResolvedSshConfig,
}

#[async_trait]
impl SshTransport for FakeTransport {
    async fn write_file(&self, path: &str, content: &str, mode: &str) -> Result<()> {
        self.remote
            .files
            .lock()
            .expect("锁不应中毒")
            .insert(path.to_owned(), (content.to_owned(), mode.to_owned()));
        Ok(())
    }

    async fn exec(&self, command: &str) -> Result<(i32, String)> {
        self.remote
            .commands
            .lock()
            .expect("锁不应中毒")
            .push(command.to_owned());

        if *self.remote.fail_reload.lock().expect("锁不应中毒") {
            return Ok((7, "远程重载失败：配置有语法错误".to_owned()));
        }
        // 走一下 user/host，免得字段被判成未使用。
        Ok((0, format!("{}@{} ok", self.config.user, self.config.host)))
    }
}

/// 假的连接器：校验认证来源确实可取到材料，然后给出假传输。
#[derive(Debug)]
struct FakeConnector {
    remote: Arc<Remote>,
    /// 记录每次**凭据解析**的标识，用于断言「值确实来自凭据系统」；
    /// 档案自带材料不经过这里。
    used_credentials: Arc<Mutex<Vec<i64>>>,
}

#[async_trait]
impl SshConnector for FakeConnector {
    async fn connect(
        &self,
        config: &ResolvedSshConfig,
        credentials: &CredentialStore<'_>,
    ) -> Result<Box<dyn SshTransport>> {
        match &config.auth {
            SshAuthSource::Credential(auth) => {
                // 真的去凭据系统取一次——这正是「私钥不在任务输入里」的体现。
                let credential = credentials
                    .resolve(auth.credential_id())
                    .await
                    .map_err(|e| Error::Remote(format!("取凭据失败: {e}")))?;

                // 两种方式各查一个字段，缺哪个就报哪个。
                match auth {
                    SshAuth::PrivateKey { .. } => {
                        credential
                            .fields
                            .get("private_key")
                            .and_then(Value::as_str)
                            .ok_or_else(|| {
                                Error::invalid_input("auth", "凭据里没有 `private_key` 字段")
                            })?;
                    }
                    SshAuth::Password { .. } => {
                        credential
                            .fields
                            .get("password")
                            .and_then(Value::as_str)
                            .ok_or_else(|| {
                                Error::invalid_input("auth", "凭据里没有 `password` 字段")
                            })?;
                    }
                }

                self.used_credentials
                    .lock()
                    .expect("锁不应中毒")
                    .push(credential.id);
            }
            // 档案自带材料已经过凭据存储的解密与校验，这里只认它可用。
            SshAuthSource::PrivateKey(_) | SshAuthSource::Password(_) => {}
        }

        Ok(Box::new(FakeTransport {
            remote: Arc::clone(&self.remote),
            config: config.clone(),
        }))
    }
}

/// 一个够用的认证凭据类型。
#[derive(Debug)]
struct SshKeyType;

impl CredentialType for SshKeyType {
    fn type_id(&self) -> &'static str {
        "ssh.key"
    }

    fn display_name(&self) -> &'static str {
        "SSH 凭据"
    }

    fn fields_schema(&self) -> RootSchema {
        schemars::schema_for!(Value)
    }

    fn validate(&self, _fields: &Value) -> acmecast_core::Result<()> {
        Ok(())
    }
}

/// 主机档案凭据类型：校验委托给 [`SshHostFields`]，与 server 侧的真实现同构。
#[derive(Debug)]
struct SshHostType;

impl CredentialType for SshHostType {
    fn type_id(&self) -> &'static str {
        "ssh"
    }

    fn display_name(&self) -> &'static str {
        "SSH 主机（部署）"
    }

    fn fields_schema(&self) -> RootSchema {
        ssh_host_fields_schema()
    }

    fn validate(&self, fields: &Value) -> acmecast_core::Result<()> {
        let parsed: SshHostFields = serde_json::from_value(fields.clone())
            .map_err(|error| acmecast_core::Error::validation("fields", format!("字段不合法: {error}")))?;
        parsed.validate()
    }
}

/// 插一份凭据并返回其标识。
///
/// `cipher` 必须与凭据存储用的是同一把——否则取凭据时会解密失败，
/// 而错误信息（「可能已被篡改」）会把人往完全错误的方向带。
async fn seed_credential(
    db: &sea_orm::DatabaseConnection,
    cipher: &CredentialCipher,
    type_id: &str,
    name: &str,
    fields: Value,
) -> i64 {
    let sealed = cipher
        .encrypt_string(&fields.to_string())
        .expect("应能加密");
    let now = Utc::now();

    credential::ActiveModel {
        name: Set(name.to_owned()),
        type_id: Set(type_id.to_owned()),
        encrypted_fields: Set(sealed),
        created_at: Set(now),
        updated_at: Set(now),
        ..Default::default()
    }
    .insert(db)
    .await
    .expect("应能插入凭据")
    .id
}

/// 建一次部署环境：内存库、注册了凭据类型的存储、假远程主机。
///
/// `Box::leak` 是为了让连接与凭据存储都拿到 `'static`——在测试里这是最简单的
/// 办法，进程退出即回收。
#[allow(clippy::type_complexity)]
async fn setup() -> (
    &'static sea_orm::DatabaseConnection,
    CredentialStore<'static>,
    Arc<CredentialCipher>,
    Arc<Remote>,
    Arc<Mutex<Vec<i64>>>,
) {
    let db = Database::connect("sqlite::memory:")
        .await
        .expect("应能连上内存库");
    migrate(&db).await.expect("迁移应成功");
    let db: &'static sea_orm::DatabaseConnection = Box::leak(Box::new(db));

    let cipher =
        Arc::new(CredentialCipher::from_base64(&CredentialCipher::generate_key_base64()).unwrap());
    let mut registry = CredentialRegistry::new();
    registry.register(SshKeyType).expect("应能注册");
    registry.register(SshHostType).expect("应能注册");
    let store = CredentialStore::new(db, Arc::new(registry), Arc::clone(&cipher));

    (
        db,
        store,
        cipher,
        Arc::new(Remote::default()),
        Arc::new(Mutex::new(Vec::new())),
    )
}

fn materials() -> CertMaterials {
    CertMaterials::new("CERT-PEM", "KEY-PEM", "sha256:xyz")
}

/// 装配一个带假连接器的 SSH 目标。
fn target(remote: &Arc<Remote>, used: &Arc<Mutex<Vec<i64>>>) -> SshTarget {
    SshTarget::with_connector(Arc::new(FakeConnector {
        remote: Arc::clone(remote),
        used_credentials: Arc::clone(used),
    }))
}

// ---- Scenario: 私钥认证写入远程主机 ----

#[tokio::test]
async fn a_private_key_credential_writes_to_the_remote_path() {
    let (db, store, cipher, remote, used) = setup().await;
    let id = seed_credential(
        db,
        &cipher,
        "ssh.key",
        "SSH 凭据",
        json!({ "private_key": "PRIVATE-KEY-PEM" }),
    )
    .await;

    let target = target(&remote, &used);
    let outcome = target
        .deploy(
            &json!({
                "host": "web-1.example.com",
                "user": "root",
                "auth": { "kind": "private_key", "credential_id": id },
                "cert_path": "/etc/nginx/ssl/fullchain.pem",
                "key_path": "/etc/nginx/ssl/privkey.pem",
            }),
            &materials(),
            &store,
            DeployMode::Write,
        )
        .await
        .expect("应能部署");

    assert_eq!(
        outcome.paths,
        vec![
            "/etc/nginx/ssl/fullchain.pem".to_owned(),
            "/etc/nginx/ssl/privkey.pem".to_owned()
        ]
    );

    let (chain, chain_mode) = remote
        .file("/etc/nginx/ssl/fullchain.pem")
        .expect("远程应有证书");
    assert_eq!(chain, "CERT-PEM");
    assert_eq!(chain_mode, "0644");

    let (key, key_mode) = remote
        .file("/etc/nginx/ssl/privkey.pem")
        .expect("远程应有私钥");
    assert_eq!(key, "KEY-PEM");
    assert_eq!(key_mode, "0600", "私钥默认更严");

    // 凭据是在部署时按标识取出的，而不是从输入里抄的。
    assert_eq!(*used.lock().unwrap(), vec![id]);
}

// ---- Scenario: 口令认证 ----

#[tokio::test]
async fn a_password_credential_also_writes_to_the_remote_path() {
    let (db, store, cipher, remote, used) = setup().await;
    let id = seed_credential(
        db,
        &cipher,
        "ssh.key",
        "SSH 凭据",
        json!({ "password": "s3cret" }),
    )
    .await;

    let outcome = target(&remote, &used)
        .deploy(
            &json!({
                "host": "web-2.example.com",
                "user": "deploy",
                "auth": { "kind": "password", "credential_id": id },
                "cert_path": "/etc/ssl/cert.pem",
                "key_path": "/etc/ssl/key.pem",
            }),
            &materials(),
            &store,
            DeployMode::Write,
        )
        .await
        .expect("应能部署");

    assert_eq!(outcome.paths.len(), 2);
    assert!(remote.file("/etc/ssl/cert.pem").is_some());
    assert!(remote.file("/etc/ssl/key.pem").is_some());
    assert_eq!(*used.lock().unwrap(), vec![id]);
}

#[tokio::test]
async fn a_credential_missing_the_expected_field_is_reported() {
    // 拿口令凭据去走私钥认证：应当指出凭据里缺哪个字段。
    let (db, store, cipher, remote, used) = setup().await;
    let id = seed_credential(
        db,
        &cipher,
        "ssh.key",
        "SSH 凭据",
        json!({ "password": "s3cret" }),
    )
    .await;

    let err = target(&remote, &used)
        .deploy(
            &json!({
                "host": "web-1",
                "user": "root",
                "auth": { "kind": "private_key", "credential_id": id },
                "cert_path": "/c.pem",
                "key_path": "/k.pem",
            }),
            &materials(),
            &store,
            DeployMode::Write,
        )
        .await
        .expect_err("字段不匹配应被拒绝");

    assert!(err.to_string().contains("private_key"), "{err}");
    assert!(remote.file("/c.pem").is_none(), "不该写入任何东西");
}

// ---- Scenario: 引用主机档案 ----

/// 一份完整的档案字段：只含连接、认证与权限缺省值，不含路径与重载命令。
fn profile_fields() -> Value {
    json!({
        "host": "web-1.example.com",
        "port": 22,
        "user": "deploy",
        "private_key": "PROFILE-PRIVATE-KEY",
        "cert_mode": "0644",
    })
}

#[tokio::test]
async fn a_host_profile_supplies_connection_and_materials() {
    let (db, store, cipher, remote, used) = setup().await;
    let id = seed_credential(db, &cipher, "ssh", "生产机", profile_fields()).await;

    let outcome = target(&remote, &used)
        .deploy(
            // 输入给档案引用与远端路径：连接、认证与权限来自档案。
            &json!({
                "credential_id": id,
                "cert_path": "/srv/ssl/site.crt",
                "key_path": "/srv/ssl/site.key",
            }),
            &materials(),
            &store,
            DeployMode::Write,
        )
        .await
        .expect("档案应补齐连接与认证");

    assert_eq!(
        outcome.paths,
        vec![
            "/srv/ssl/site.crt".to_owned(),
            "/srv/ssl/site.key".to_owned()
        ]
    );

    let (chain, chain_mode) = remote
        .file("/srv/ssl/site.crt")
        .expect("应写到输入给出的证书路径");
    assert_eq!(chain, "CERT-PEM");
    assert_eq!(chain_mode, "0644", "输入未给权限时回退档案值");

    let (_, key_mode) = remote
        .file("/srv/ssl/site.key")
        .expect("应写到输入给出的私钥路径");
    assert_eq!(key_mode, "0600", "档案未给权限时用系统缺省");

    assert!(
        remote.commands().is_empty(),
        "重载命令不属于档案，输入未给时不执行"
    );

    // 材料来自档案本身：认证凭据解析一条都没发生。
    assert!(used.lock().unwrap().is_empty());
}

#[tokio::test]
async fn explicit_input_values_override_the_profile() {
    let (db, store, cipher, remote, used) = setup().await;
    let id = seed_credential(db, &cipher, "ssh", "生产机", profile_fields()).await;

    target(&remote, &used)
        .deploy(
            &json!({
                "credential_id": id,
                "cert_path": "/override/site.crt",
                "key_path": "/override/site.key",
                "cert_mode": "0600",
                "reload_command": "systemctl reload caddy",
            }),
            &materials(),
            &store,
            DeployMode::Write,
        )
        .await
        .expect("覆盖后仍应部署成功");

    let (_, cert_mode) = remote
        .file("/override/site.crt")
        .expect("输入显式路径应生效");
    assert_eq!(cert_mode, "0600", "输入权限应覆盖档案默认值");
    assert!(
        remote.file("/srv/ssl/site.crt").is_none(),
        "被覆盖的档案默认路径不应再被写入"
    );
    assert_eq!(
        remote.commands(),
        vec!["systemctl reload caddy".to_owned()],
        "输入的重载命令应被执行"
    );
}

#[tokio::test]
async fn a_profile_without_auth_material_fails_before_writing() {
    let (db, store, cipher, remote, used) = setup().await;
    let mut broken = profile_fields();
    broken["private_key"] = Value::Null;
    let id = seed_credential(db, &cipher, "ssh", "坏档案", broken).await;

    let err = target(&remote, &used)
        .deploy(
            &json!({ "credential_id": id }),
            &materials(),
            &store,
            DeployMode::Write,
        )
        .await
        .expect_err("档案缺认证材料应部署失败");

    // 取档案时凭据存储就会先跑一遍类型校验，因此失败发生在连接之前，
    // 报错同时带上档案标识与出错的字段。
    let text = err.to_string();
    assert!(text.contains("1"), "应包含被引用的标识: {text}");
    assert!(text.contains("private_key"), "应指出档案缺认证材料: {text}");
    assert!(remote.file("/srv/ssl/site.crt").is_none(), "不该写入任何东西");
}

#[tokio::test]
async fn a_missing_profile_is_reported_with_its_id() {
    let (_db, store, _cipher, remote, used) = setup().await;

    let err = target(&remote, &used)
        .deploy(
            &json!({ "credential_id": 4242 }),
            &materials(),
            &store,
            DeployMode::Write,
        )
        .await
        .expect_err("引用不存在的档案应报错");

    let text = err.to_string();
    assert!(text.contains("4242"), "应包含被引用的标识: {text}");
    assert!(remote.file("/srv/ssl/site.crt").is_none());
}

// ---- Scenario: 远程写入后重载 ----

#[tokio::test]
async fn a_successful_remote_reload_keeps_the_deployment_successful() {
    let (db, store, cipher, remote, used) = setup().await;
    let id = seed_credential(
        db,
        &cipher,
        "ssh.key",
        "SSH 凭据",
        json!({ "password": "s3cret" }),
    )
    .await;

    let outcome = target(&remote, &used)
        .deploy(
            &json!({
                "host": "web-1",
                "user": "root",
                "auth": { "kind": "password", "credential_id": id },
                "cert_path": "/c.pem",
                "key_path": "/k.pem",
                "reload_command": "nginx -s reload",
            }),
            &materials(),
            &store,
            DeployMode::Write,
        )
        .await
        .expect("重载成功则部署成功");

    assert_eq!(remote.commands(), vec!["nginx -s reload".to_owned()]);
    assert!(
        outcome
            .reload_output
            .as_deref()
            .unwrap_or_default()
            .contains("ok"),
        "{:?}",
        outcome.reload_output
    );
}

#[tokio::test]
async fn a_nonzero_remote_exit_code_fails_the_whole_deployment() {
    // spec：远程重载退出码非零视为部署失败。
    let (db, store, cipher, remote, used) = setup().await;
    let id = seed_credential(
        db,
        &cipher,
        "ssh.key",
        "SSH 凭据",
        json!({ "password": "s3cret" }),
    )
    .await;
    *remote.fail_reload.lock().unwrap() = true;

    let err = target(&remote, &used)
        .deploy(
            &json!({
                "host": "web-1",
                "user": "root",
                "auth": { "kind": "password", "credential_id": id },
                "cert_path": "/c.pem",
                "key_path": "/k.pem",
                "reload_command": "nginx -s reload",
            }),
            &materials(),
            &store,
            DeployMode::Write,
        )
        .await
        .expect_err("远程重载失败应导致部署失败");

    let text = err.to_string();
    assert!(text.contains("配置有语法错误"), "应含远程输出: {text}");
    assert!(text.contains('7'), "应含退出码: {text}");
}

// ---- 注册表 ----

#[test]
fn local_and_ssh_are_both_reachable_through_the_registry() {
    // spec 场景：内置的本地与 SSH 目标都可在注册表中查得。
    let mut registry = DeploymentRegistry::new();
    registry
        .register(acmecast_deploy::LocalTarget)
        .expect("应能注册");
    registry.register(SshTarget::live()).expect("应能注册");

    assert_eq!(
        registry.type_ids(),
        vec!["local".to_owned(), "ssh".to_owned()]
    );
    assert_eq!(
        registry.require("ssh").unwrap().display_name(),
        "SSH 远程主机"
    );
}
