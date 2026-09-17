//! 5.5 按标识注入任务。
//!
//! spec 的两个场景：
//! - 引用有效凭据 → 执行时注入**解密后**的字段；
//! - 引用已删除的凭据 → **执行前**就带着明确原因失败。
//!
//! 这里用 5.4 的 ACME 账号凭据作为样例，顺带把「类型定义 → 加密落库 → 按标识取出」串通。

use std::sync::Arc;

use acmecast_access::{
    AcmeAccountFields, AcmeAccountType, CredentialRegistry, CredentialStore, Error,
};
use acmecast_core::CredentialCipher;
use acmecast_store::entity::credential;
use acmecast_store::migrate;
use chrono::Utc;
use sea_orm::{ActiveModelTrait, Database, DatabaseConnection, EntityTrait, Set};

/// 建一个跑完迁移的内存库。
async fn setup() -> DatabaseConnection {
    let db = Database::connect("sqlite::memory:")
        .await
        .expect("应能连上内存库");
    migrate(&db).await.expect("迁移应成功");
    db
}

/// 注册了 ACME 账号类型的注册表。
fn registry() -> Arc<CredentialRegistry> {
    let mut registry = CredentialRegistry::new();
    registry
        .register(AcmeAccountType::new())
        .expect("注册应成功");
    Arc::new(registry)
}

/// 一份形态合法的账号凭据 JSON，代表「已在 CA 侧注册过」。
fn registered_account() -> AcmeAccountFields {
    AcmeAccountFields {
        ca: "letsencrypt".to_owned(),
        directory_url: None,
        eab_kid: None,
        eab_hmac_key: None,
        credentials: Some(
            serde_json::json!({
                "id": "https://acme-v02.api.letsencrypt.org/acme/acct/424242",
                "key_pkcs8": "TUlJRXZnSUJBREFLQmdncWhrak9QUVFEQWc=",
                "directory": { "url": "https://acme-v02.api.letsencrypt.org/directory" },
            })
            .to_string(),
        ),
    }
}

/// 把字段加密后写入 credential 表，返回其主键。
async fn insert_credential(
    db: &DatabaseConnection,
    cipher: &CredentialCipher,
    type_id: &str,
    fields: &serde_json::Value,
) -> i64 {
    let sealed = cipher
        .encrypt_string(&fields.to_string())
        .expect("应能加密");
    let now = Utc::now();

    credential::ActiveModel {
        name: Set("测试凭据".to_owned()),
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

// ---- Scenario: 引用有效凭据 ----

#[tokio::test]
async fn a_valid_credential_is_injected_decrypted() {
    let db = setup().await;
    let cipher = Arc::new(
        CredentialCipher::from_base64(&CredentialCipher::generate_key_base64())
            .expect("密钥应可用"),
    );

    let account = registered_account();
    let id = insert_credential(
        &db,
        &cipher,
        "acme.account",
        &serde_json::to_value(&account).expect("应能序列化"),
    )
    .await;

    let store = CredentialStore::new(&db, registry(), Arc::clone(&cipher));
    let resolved = store.resolve(id).await.expect("有效凭据应能取出");

    assert_eq!(resolved.id, id);
    assert_eq!(resolved.type_id, "acme.account");
    assert_eq!(resolved.name, "测试凭据");

    // 步骤拿到的是解密后的字段，可直接反序列化成自己的结构体。
    let fields = resolved
        .as_fields::<AcmeAccountFields>()
        .expect("应能还原为 ACME 账号字段");
    assert_eq!(fields.ca, "letsencrypt");
    assert!(fields.is_registered(), "账号凭据应随字段一并注入");
    assert_eq!(
        fields.kid().as_deref(),
        Some("https://acme-v02.api.letsencrypt.org/acme/acct/424242")
    );
}

#[tokio::test]
async fn the_same_identifier_resolves_identically_every_time() {
    // 多个步骤引用同一标识：每次拿到的都应是同一份账号，而不是各自重新建立一份。
    let db = setup().await;
    let cipher = Arc::new(
        CredentialCipher::from_base64(&CredentialCipher::generate_key_base64())
            .expect("密钥应可用"),
    );
    let id = insert_credential(
        &db,
        &cipher,
        "acme.account",
        &serde_json::to_value(registered_account()).unwrap(),
    )
    .await;

    let store = CredentialStore::new(&db, registry(), Arc::clone(&cipher));

    let first = store.resolve(id).await.expect("首次应成功");
    let second = store.resolve(id).await.expect("再次应成功");

    assert_eq!(first.fields, second.fields, "重复注入应得到相同字段");
    assert_eq!(first.as_fields::<AcmeAccountFields>().unwrap().kid(), {
        second.as_fields::<AcmeAccountFields>().unwrap().kid()
    });
}

#[tokio::test]
async fn existence_checks_do_not_throw_for_missing_rows() {
    let db = setup().await;
    let cipher = Arc::new(
        CredentialCipher::from_base64(&CredentialCipher::generate_key_base64())
            .expect("密钥应可用"),
    );
    let id = insert_credential(
        &db,
        &cipher,
        "acme.account",
        &serde_json::to_value(registered_account()).unwrap(),
    )
    .await;

    let store = CredentialStore::new(&db, registry(), Arc::clone(&cipher));
    assert!(store.exists(id).await.expect("查询应成功"));
    assert!(!store.exists(id + 999).await.expect("查询应成功"));
}

// ---- Scenario: 引用已删除的凭据 ----

#[tokio::test]
async fn referring_to_a_deleted_credential_fails_before_execution() {
    let db = setup().await;
    let cipher = Arc::new(
        CredentialCipher::from_base64(&CredentialCipher::generate_key_base64())
            .expect("密钥应可用"),
    );
    let id = insert_credential(
        &db,
        &cipher,
        "acme.account",
        &serde_json::to_value(registered_account()).unwrap(),
    )
    .await;

    // 凭据被删除后再来引用：必须报错，而不是拿到一个空值继续跑下去。
    credential::Entity::delete_by_id(id)
        .exec(&db)
        .await
        .expect("应能删除");

    let store = CredentialStore::new(&db, registry(), Arc::clone(&cipher));
    let err = store.resolve(id).await.expect_err("已删除的凭据应报错");

    match &err {
        Error::Core(acmecast_core::Error::NotFound {
            entity,
            id: missing,
        }) => {
            assert_eq!(entity, "凭据");
            assert_eq!(missing, &id.to_string());
        }
        other => panic!("期望 NotFound，实际 {other:?}"),
    }
    assert!(
        err.to_string().contains(&id.to_string()),
        "错误里应能看到是哪个标识: {err}"
    );
}

// ---- 取不出凭据的其它情形 ----

#[tokio::test]
async fn a_credential_of_an_unregistered_type_is_refused() {
    // 类型没注册意味着字段无从校验——让它注入进步骤只会换成一个更晚的失败。
    let db = setup().await;
    let cipher = Arc::new(
        CredentialCipher::from_base64(&CredentialCipher::generate_key_base64())
            .expect("密钥应可用"),
    );
    let id = insert_credential(
        &db,
        &cipher,
        "nothing.like.this",
        &serde_json::json!({ "whatever": 1 }),
    )
    .await;

    let store = CredentialStore::new(&db, registry(), Arc::clone(&cipher));
    let err = store.resolve(id).await.expect_err("未知类型应被拒绝");
    assert!(matches!(err, Error::UnknownType { .. }), "{err:?}");
}

#[tokio::test]
async fn a_different_cipher_leaves_the_credential_unreadable() {
    let db = setup().await;
    let cipher = Arc::new(
        CredentialCipher::from_base64(&CredentialCipher::generate_key_base64())
            .expect("密钥应可用"),
    );
    let id = insert_credential(
        &db,
        &cipher,
        "acme.account",
        &serde_json::to_value(registered_account()).unwrap(),
    )
    .await;

    // 换了密钥：密文解不开，且错误信息里不该夹带任何明文。
    let other = Arc::new(
        CredentialCipher::from_base64(&CredentialCipher::generate_key_base64())
            .expect("密钥应可用"),
    );
    let store = CredentialStore::new(&db, registry(), other);
    let err = store.resolve(id).await.expect_err("换密钥后应解不开");

    assert!(
        !err.to_string().contains("acct/424242"),
        "解密失败的信息里不应出现凭据内容: {err}"
    );
}

#[tokio::test]
async fn stale_fields_that_no_longer_validate_are_refused() {
    // 库里的内容可能是在旧的类型定义下写入的；注入前再校验一次，
    // 好过让一份不合法的凭据流到步骤里。
    let db = setup().await;
    let cipher = Arc::new(
        CredentialCipher::from_base64(&CredentialCipher::generate_key_base64())
            .expect("密钥应可用"),
    );
    let id = insert_credential(
        &db,
        &cipher,
        "acme.account",
        &serde_json::json!({ "ca": "no-such-ca" }),
    )
    .await;

    let store = CredentialStore::new(&db, registry(), Arc::clone(&cipher));
    let err = store.resolve(id).await.expect_err("陈旧字段应被拒绝");
    match &err {
        Error::Core(acmecast_core::Error::Validation { field, .. }) => {
            assert_eq!(field, "ca", "错误应指向出错字段");
        }
        other => panic!("期望 Validation，实际 {other:?}"),
    }
}
