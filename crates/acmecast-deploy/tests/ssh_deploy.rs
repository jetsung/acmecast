//! 8.4 SSH 远程部署。
//!
//! CI 里起一台 sshd 既不现实也不稳定，因此连接被抽象成可替换的
//! [`SshConnector`]。这里验证的是**部署逻辑**：两种认证都能拿到凭据、
//! 文件写到指定远程路径、远程重载的成败如何决定整个步骤的成败。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use acmecast_access::{CredentialRegistry, CredentialStore, CredentialType};
use acmecast_core::CredentialCipher;
use acmecast_deploy::{
    CertMaterials, DeployMode, DeploymentRegistry, DeploymentTarget, Error, Result, SshAuth,
    SshConnector, SshInput, SshTarget, SshTransport,
};
use acmecast_store::entity::credential;
use acmecast_store::migrate;
use async_trait::async_trait;
use chrono::Utc;
use schemars::schema::RootSchema;
use schemars::schema_for;
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
    input: SshInput,
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
        Ok((0, format!("{}@{} ok", self.input.user, self.input.host)))
    }
}

/// 假的连接器：按认证方式取一次凭据，然后给出假传输。
#[derive(Debug)]
struct FakeConnector {
    remote: Arc<Remote>,
    /// 记录每次认证用的凭据标识，用于断言「值确实来自凭据系统」。
    used_credentials: Arc<Mutex<Vec<i64>>>,
}

#[async_trait]
impl SshConnector for FakeConnector {
    async fn connect(
        &self,
        input: &SshInput,
        credentials: &CredentialStore<'_>,
    ) -> Result<Box<dyn SshTransport>> {
        // 真的去凭据系统取一次——这正是「私钥不在任务输入里」的体现。
        let credential = credentials
            .resolve(input.auth.credential_id())
            .await
            .map_err(|e| Error::Remote(format!("取凭据失败: {e}")))?;

        // 两种方式各查一个字段，缺哪个就报哪个。
        match &input.auth {
            SshAuth::PrivateKey { .. } => {
                credential
                    .fields
                    .get("private_key")
                    .and_then(Value::as_str)
                    .ok_or_else(|| Error::invalid_input("auth", "凭据里没有 `private_key` 字段"))?;
            }
            SshAuth::Password { .. } => {
                credential
                    .fields
                    .get("password")
                    .and_then(Value::as_str)
                    .ok_or_else(|| Error::invalid_input("auth", "凭据里没有 `password` 字段"))?;
            }
        }

        self.used_credentials
            .lock()
            .expect("锁不应中毒")
            .push(credential.id);

        Ok(Box::new(FakeTransport {
            remote: Arc::clone(&self.remote),
            input: input.clone(),
        }))
    }
}

/// 一个够用的凭据类型。
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
        schema_for!(Value)
    }

    fn validate(&self, _fields: &Value) -> acmecast_core::Result<()> {
        Ok(())
    }
}

/// 插一份凭据并返回其标识。
///
/// `cipher` 必须与凭据存储用的是同一把——否则取凭据时会解密失败，
/// 而错误信息（「可能已被篡改」）会把人往完全错误的方向带。
async fn seed_credential(
    db: &sea_orm::DatabaseConnection,
    cipher: &CredentialCipher,
    fields: Value,
) -> i64 {
    let sealed = cipher
        .encrypt_string(&fields.to_string())
        .expect("应能加密");
    let now = Utc::now();

    credential::ActiveModel {
        name: Set("SSH 凭据".to_owned()),
        type_id: Set("ssh.key".to_owned()),
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
    let id = seed_credential(db, &cipher, json!({ "private_key": "PRIVATE-KEY-PEM" })).await;

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
    let id = seed_credential(db, &cipher, json!({ "password": "s3cret" })).await;

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
    let id = seed_credential(db, &cipher, json!({ "password": "s3cret" })).await;

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

// ---- Scenario: 远程写入后重载 ----

#[tokio::test]
async fn a_successful_remote_reload_keeps_the_deployment_successful() {
    let (db, store, cipher, remote, used) = setup().await;
    let id = seed_credential(db, &cipher, json!({ "password": "s3cret" })).await;

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
    let id = seed_credential(db, &cipher, json!({ "password": "s3cret" })).await;
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
