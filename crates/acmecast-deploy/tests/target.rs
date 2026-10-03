//! 8.1 部署目标 Trait 的可用性。
//!
//! Trait 与注册表的**契约**在单元测试里钉住了，这里验证它真的能跑：
//! 一份证书交给目标，目标读到输入、凭据入口与材料，并返回结果。

use std::sync::{Arc, Mutex};

use acmecast_access::{CredentialRegistry, CredentialStore};
use acmecast_core::CredentialCipher;
use acmecast_deploy::{
    CertMaterials, DeployMode, DeployOutcome, DeploymentRegistry, DeploymentTarget, Error, Result,
    parse_input,
};
use acmecast_store::migrate;
use async_trait::async_trait;
use schemars::schema::RootSchema;
use schemars::schema_for;
use sea_orm::Database;
use serde::Deserialize;
use serde_json::{Value, json};

/// 测试目标的输入定义。
#[derive(Debug, Deserialize, schemars::JsonSchema)]
// 字段由 serde 与 schemars 使用，代码里不逐个读取。
#[allow(dead_code)]
struct TargetInput {
    /// 证书写入路径。
    cert_path: String,
    /// 私钥写入路径。
    key_path: String,
}

/// 会把调用记下来的假目标。
///
/// 记录器用 `Arc` 共享：注册表拿走的是一份克隆，测试手里这份仍能看到记录。
#[derive(Debug, Default, Clone)]
struct RecordingTarget {
    calls: Arc<Mutex<Vec<String>>>,
}

impl RecordingTarget {
    fn calls(&self) -> Vec<String> {
        self.calls.lock().expect("锁不应中毒").clone()
    }
}

#[async_trait]
impl DeploymentTarget for RecordingTarget {
    fn type_id(&self) -> &'static str {
        "recording"
    }

    fn display_name(&self) -> &'static str {
        "记录用的假目标"
    }

    fn input_schema(&self) -> RootSchema {
        schema_for!(TargetInput)
    }

    fn example_input(&self) -> Value {
        json!({ "cert_path": "/tmp/recording.crt", "key_path": "/tmp/recording.key" })
    }

    async fn deploy(
        &self,
        input: &Value,
        materials: &CertMaterials,
        credentials: &CredentialStore<'_>,
        mode: DeployMode,
    ) -> Result<DeployOutcome> {
        assert_eq!(mode, DeployMode::Write, "本用例只走正常写入");
        // 输入不合法时应当在这里被拦下，而不是带着半个配置去写文件。
        let parsed: TargetInput = parse_input(input)?;

        // 凭据入口确实接通了：查一个不存在的标识返回 false 而非报错。
        assert!(!credentials.exists(4242).await?);

        self.calls.lock().expect("锁不应中毒").push(format!(
            "{} {} {}",
            parsed.cert_path, parsed.key_path, materials.fingerprint
        ));

        Ok(DeployOutcome::written(vec![
            parsed.cert_path,
            parsed.key_path,
        ]))
    }
}

/// 建一个跑完迁移的内存库，并返回它上面的凭据存储。
async fn setup() -> (sea_orm::DatabaseConnection, RecordingTarget) {
    let db = Database::connect("sqlite::memory:")
        .await
        .expect("应能连上内存库");
    migrate(&db).await.expect("迁移应成功");
    (db, RecordingTarget::default())
}

fn credentials_of(db: &sea_orm::DatabaseConnection) -> CredentialStore<'_> {
    let cipher = CredentialCipher::from_base64(&CredentialCipher::generate_key_base64())
        .expect("密钥应可用");
    CredentialStore::new(db, Arc::new(CredentialRegistry::new()), Arc::new(cipher))
}

fn materials() -> CertMaterials {
    CertMaterials::new("CERT-PEM", "KEY-PEM", "sha256:abc123")
}

#[tokio::test]
async fn a_target_receives_the_input_materials_and_credential_entry() {
    let (db, recorder) = setup().await;
    let credentials = credentials_of(&db);

    let mut registry = DeploymentRegistry::new();
    registry.register(recorder.clone()).expect("应能注册");

    let target = registry.require("recording").expect("应能查到");
    let outcome = target
        .deploy(
            &json!({ "cert_path": "/etc/ssl/a.pem", "key_path": "/etc/ssl/a.key" }),
            &materials(),
            &credentials,
            DeployMode::Write,
        )
        .await
        .expect("应能部署");

    assert_eq!(
        outcome.paths,
        vec!["/etc/ssl/a.pem".to_owned(), "/etc/ssl/a.key".to_owned()]
    );
    assert!(!outcome.skipped_write, "这是真写入而非跳过");

    // 注册表里那个实例确实执行到了：输入、路径与指纹都对上了。
    assert_eq!(
        recorder.calls(),
        vec!["/etc/ssl/a.pem /etc/ssl/a.key sha256:abc123".to_owned()]
    );
}

#[tokio::test]
async fn bad_input_is_refused_before_the_target_does_anything() {
    let (db, recorder) = setup().await;
    let credentials = credentials_of(&db);

    let registry = registry_with(&recorder);
    let target = registry.require("recording").unwrap();
    let err = target
        .deploy(
            // 少了 key_path。
            &json!({ "cert_path": "/etc/ssl/a.pem" }),
            &materials(),
            &credentials,
            DeployMode::Write,
        )
        .await
        .expect_err("缺必填输入应被拒绝");

    assert!(matches!(err, Error::InvalidInput { .. }), "{err:?}");
    assert!(
        err.to_string().contains("key_path"),
        "应指明缺哪个字段: {err}"
    );
    assert!(recorder.calls().is_empty(), "不该开始写任何东西");
}

/// 建一个只装了记录器的注册表。
fn registry_with(target: &RecordingTarget) -> DeploymentRegistry {
    let mut registry = DeploymentRegistry::new();
    registry.register(target.clone()).expect("应能注册");
    registry
}
