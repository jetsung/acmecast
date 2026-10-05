//! 5.2 凭据的静态加密存储。
//!
//! spec 的两个场景：
//! - **落库为密文**，且无任何日志记录其明文；
//! - **使用时解密**，明文只在内存中出现。
//!
//! 这里刻意跑真实链路（校验 → 加密 → 写进 `acmecast_credential` → 读回解密），
//! 并**捕获 tracing 输出**后断言其中不含明文。捕获之所以有意义，是因为
//! `store::connect` 开了 sqlx 的 SQL 日志：若哪一步把明文当成语句参数，
//! 它会原样出现在日志里，这条断言就会红。

use std::sync::{Arc, Mutex};

use acmecast_access::{CredentialRegistry, CredentialType};
use acmecast_core::CredentialCipher;
use acmecast_store::entity::credential;
use acmecast_store::migrate;
use chrono::Utc;
use schemars::JsonSchema;
use schemars::schema::RootSchema;
use schemars::schema_for;
use sea_orm::{
    ActiveModelTrait, ConnectionTrait, Database, DatabaseBackend, DatabaseConnection, EntityTrait,
    Set, Statement,
};
use serde::Deserialize;
use tracing::subscriber::DefaultGuard;

/// 测试用的明文，形如真实密钥且足够独特，不会与日志里的其他文本偶然相撞。
const SECRET: &str = "sk-live-DO-NOT-LEAK-4f8a2c";

// ---- 测试用凭据类型 ----

#[derive(Debug, Deserialize, JsonSchema)]
struct FakeSecretFields {
    api_token: String,
}

#[derive(Debug)]
struct FakeSecretType;

impl CredentialType for FakeSecretType {
    fn type_id(&self) -> &'static str {
        "fake.secret"
    }

    fn display_name(&self) -> &'static str {
        "假密钥"
    }

    fn fields_schema(&self) -> RootSchema {
        schema_for!(FakeSecretFields)
    }

    fn validate(&self, fields: &serde_json::Value) -> acmecast_core::Result<()> {
        serde_json::from_value::<FakeSecretFields>(fields.clone())
            .map(|_| ())
            .map_err(|err| {
                acmecast_core::Error::validation("api_token", format!("字段不合法: {err}"))
            })
    }
}

// ---- 日志捕获 ----

/// 把 tracing 输出写进内存缓冲区的 writer。
#[derive(Clone, Default)]
struct CapturedLogs(Arc<Mutex<Vec<u8>>>);

impl CapturedLogs {
    fn contents(&self) -> String {
        String::from_utf8_lossy(&self.0.lock().expect("锁不应中毒")).into_owned()
    }
}

impl std::io::Write for CapturedLogs {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().expect("锁不应中毒").extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for CapturedLogs {
    type Writer = Self;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

// ---- 辅助 ----

/// 建一个跑完迁移的内存库，并接上捕获日志的 subscriber。
///
/// 用 `#[tokio::test]` 默认的当前线程运行时，因此 `set_default` 的线程局部
/// subscriber 覆盖得到后续所有 await 点。返回的 guard 必须由调用方**持有到测试结束**
/// ——一旦被 drop，订阅者就随之撤销，捕获会变空。
async fn setup() -> (DatabaseConnection, CapturedLogs, DefaultGuard) {
    let logs = CapturedLogs::default();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(logs.clone())
        .with_max_level(tracing::Level::TRACE)
        .finish();
    let guard = tracing::subscriber::set_default(subscriber);

    let db = Database::connect("sqlite::memory:")
        .await
        .expect("应能连上内存库");
    migrate(&db).await.expect("迁移应成功");
    (db, logs, guard)
}

fn registry() -> CredentialRegistry {
    let mut registry = CredentialRegistry::new();
    registry.register(FakeSecretType).expect("注册应成功");
    registry
}

/// 走一遍「校验 → 加密 → 落库」，返回落库后的记录。
async fn save_secret(db: &DatabaseConnection, cipher: &CredentialCipher) -> credential::Model {
    let registry = registry();
    let fields = serde_json::json!({ "api_token": SECRET });

    // 1) 先校验字段：类型定义说它合法才允许入库。
    registry
        .require("fake.secret")
        .expect("类型应已注册")
        .validate(&fields)
        .expect("字段应合法");

    // 2) 整体加密后落库——库里那一列存的就是这串密文。
    let sealed = cipher
        .encrypt_string(&fields.to_string())
        .expect("应能加密");

    let now = Utc::now();
    credential::ActiveModel {
        name: Set("测试凭据".to_owned()),
        type_id: Set("fake.secret".to_owned()),
        encrypted_fields: Set(sealed),
        created_at: Set(now),
        updated_at: Set(now),
        ..Default::default()
    }
    .insert(db)
    .await
    .expect("应能插入凭据")
}

// ---- Scenario: 落库为密文 ----

#[tokio::test]
async fn secrets_are_never_stored_in_plaintext() {
    let (db, _logs, _guard) = setup().await;
    let cipher = CredentialCipher::from_base64(&CredentialCipher::generate_key_base64()).unwrap();

    let saved = save_secret(&db, &cipher).await;

    assert!(
        !saved.encrypted_fields.contains(SECRET),
        "实体上就不应出现明文"
    );
    // 再绕过实体直接查原始列，确认库里也没有。
    let rows = db
        .query_all(Statement::from_string(
            DatabaseBackend::Sqlite,
            "SELECT encrypted_fields FROM acmecast_credential".to_owned(),
        ))
        .await
        .expect("应能直查原始列");
    let raw: String = rows[0].try_get("", "encrypted_fields").expect("应能取值");
    assert!(!raw.contains(SECRET), "库中必须是密文: {raw}");
    assert!(!raw.is_empty(), "密文不应为空");
}

#[tokio::test]
async fn secrets_round_trip_back_through_decryption() {
    let (db, _logs, _guard) = setup().await;
    let cipher = CredentialCipher::from_base64(&CredentialCipher::generate_key_base64()).unwrap();

    let saved = save_secret(&db, &cipher).await;

    // 使用时解密：明文只在内存中存在。
    let stored = credential::Entity::find_by_id(saved.id)
        .one(&db)
        .await
        .expect("应能读回")
        .expect("记录应仍在");

    let opened = cipher
        .decrypt_string(&stored.encrypted_fields)
        .expect("应能解密");
    let fields: FakeSecretFields = serde_json::from_str(&opened).expect("应能还原字段结构");
    assert_eq!(fields.api_token, SECRET, "解密后应得到原始明文");
}

#[tokio::test]
async fn a_different_key_cannot_open_the_stored_secret() {
    let (db, _logs, _guard) = setup().await;
    let cipher = CredentialCipher::from_base64(&CredentialCipher::generate_key_base64()).unwrap();
    let saved = save_secret(&db, &cipher).await;

    // 换一把密钥：密文不可解，且失败不应把明文带出来。
    let other = CredentialCipher::from_base64(&CredentialCipher::generate_key_base64()).unwrap();
    let err = other
        .decrypt_string(&saved.encrypted_fields)
        .expect_err("换密钥后不应解得开");
    assert!(
        !err.to_string().contains(SECRET),
        "解密失败的信息里不应夹带明文: {err}"
    );
}

// ---- Scenario: 日志中不出现明文 ----

#[tokio::test]
async fn no_log_line_ever_carries_the_plaintext() {
    let (db, logs, _guard) = setup().await;
    let cipher = CredentialCipher::from_base64(&CredentialCipher::generate_key_base64()).unwrap();

    let saved = save_secret(&db, &cipher).await;

    // 把这些对象按调试格式打印出来——这是最可能泄漏明文的两处。
    tracing::info!(?cipher, "加密器");
    tracing::info!(?saved, "已保存的凭据记录");
    tracing::info!(id = saved.id, "凭据已落库");

    let captured = logs.contents();
    assert!(!captured.is_empty(), "本用例的前提是确实捕获到了日志输出");
    assert!(
        !captured.contains(SECRET),
        "日志中不应出现凭据明文。捕获到的内容：\n{captured}"
    );
    // 反面确认：日志确实记录了我们打印的对象，而不是恰好什么都没记。
    assert!(
        captured.contains("加密器") || captured.contains("凭据已落库"),
        "应捕获到测试输出的日志行：\n{captured}"
    );
}
