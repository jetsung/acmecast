//! 5.7 删除凭据前的引用检查。
//!
//! spec 的两个场景：
//! - 有引用时 **拒绝删除并列出引用者**，不静默删掉导致后续任务失败；
//! - 无引用时删除成功。
//!
//! 引用的判定规则见 [`CREDENTIAL_REFERENCE_FIELD`]：步骤输入里任何层级
//! 名为 `credential_id` 的整数字段。

use std::sync::Arc;

use acmecast_access::{CREDENTIAL_REFERENCE_FIELD, CredentialRegistry, CredentialStore, Error};
use acmecast_core::CredentialCipher;
use acmecast_store::entity::{credential, pipeline, pipeline_step};
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

/// 插入一份明文无关紧要的凭据，返回其主键。
///
/// 本模块只关心「有没有被引用」，字段内容不参与判定。
async fn insert_credential(db: &DatabaseConnection) -> i64 {
    let cipher = CredentialCipher::from_base64(&CredentialCipher::generate_key_base64())
        .expect("密钥应可用");
    let sealed = cipher
        .encrypt_string(&serde_json::json!({ "ca": "letsencrypt" }).to_string())
        .expect("应能加密");
    let now = Utc::now();

    credential::ActiveModel {
        name: Set("被引用的凭据".to_owned()),
        type_id: Set("acme.account".to_owned()),
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

async fn insert_pipeline(db: &DatabaseConnection, name: &str) -> i64 {
    let now = Utc::now();
    pipeline::ActiveModel {
        name: Set(name.to_owned()),
        enabled: Set(true),
        description: Set(None),
        created_at: Set(now),
        updated_at: Set(now),
        ..Default::default()
    }
    .insert(db)
    .await
    .expect("应能插入流水线")
    .id
}

async fn insert_step(
    db: &DatabaseConnection,
    pipeline_id: i64,
    order_index: i32,
    input: serde_json::Value,
) {
    pipeline_step::ActiveModel {
        pipeline_id: Set(pipeline_id),
        order_index: Set(order_index),
        type_id: Set("cert.apply".to_owned()),
        input: Set(input),
        enabled: Set(true),
        ..Default::default()
    }
    .insert(db)
    .await
    .expect("应能插入步骤");
}

/// 建一个只认识 ACME 账号类型的 store。
///
/// 这里不注册任何类型也能跑——引用检查与删除都不需要认识字段定义；
/// 留空正好说明这条路径不依赖注册表。
fn store(db: &DatabaseConnection) -> CredentialStore<'_> {
    let cipher = CredentialCipher::from_base64(&CredentialCipher::generate_key_base64())
        .expect("密钥应可用");
    CredentialStore::new(db, Arc::new(CredentialRegistry::new()), Arc::new(cipher))
}

// ---- Scenario: 无引用时删除 ----

#[tokio::test]
async fn an_unreferenced_credential_can_be_deleted() {
    let db = setup().await;
    let credential_id = insert_credential(&db).await;
    let store = store(&db);

    store
        .delete(credential_id)
        .await
        .expect("无引用时应删除成功");
    assert!(!store.exists(credential_id).await.unwrap());
}

#[tokio::test]
async fn steps_that_do_not_reference_anything_do_not_block_deletion() {
    let db = setup().await;
    let credential_id = insert_credential(&db).await;

    let pipeline_id = insert_pipeline(&db, "无关流水线").await;
    insert_step(
        &db,
        pipeline_id,
        0,
        serde_json::json!({ "domains": ["example.com"] }),
    )
    .await;

    store(&db)
        .delete(credential_id)
        .await
        .expect("无关步骤不该挡住删除");
}

#[tokio::test]
async fn deleting_a_missing_credential_reports_not_found() {
    let db = setup().await;
    let err = store(&db)
        .delete(4242)
        .await
        .expect_err("删除不存在的凭据应报错");

    assert!(
        matches!(err, Error::Core(acmecast_core::Error::NotFound { .. })),
        "{err:?}"
    );
}

// ---- Scenario: 有引用时删除 ----

#[tokio::test]
async fn a_referenced_credential_is_refused_and_the_referrers_are_listed() {
    let db = setup().await;
    let credential_id = insert_credential(&db).await;

    let first = insert_pipeline(&db, "甲流水线").await;
    insert_step(
        &db,
        first,
        0,
        serde_json::json!({ CREDENTIAL_REFERENCE_FIELD: credential_id }),
    )
    .await;

    let second = insert_pipeline(&db, "乙流水线").await;
    insert_step(
        &db,
        second,
        0,
        serde_json::json!({ "domains": ["a.example.com"] }),
    )
    .await;
    insert_step(
        &db,
        second,
        1,
        serde_json::json!({ CREDENTIAL_REFERENCE_FIELD: credential_id }),
    )
    .await;

    let err = store(&db)
        .delete(credential_id)
        .await
        .expect_err("被引用时应拒绝删除");

    let referenced_by = match &err {
        Error::CredentialInUse {
            credential_id: reported,
            referenced_by,
        } => {
            assert_eq!(*reported, credential_id);
            referenced_by.clone()
        }
        other => panic!("期望 CredentialInUse，实际 {other:?}"),
    };

    assert_eq!(referenced_by.len(), 2, "应列出全部两处引用");
    assert_eq!(referenced_by[0].pipeline_name, "甲流水线");
    assert_eq!(referenced_by[0].step_order, 0);
    assert_eq!(referenced_by[1].pipeline_name, "乙流水线");
    assert_eq!(referenced_by[1].step_order, 1, "应指出是第几步引用");

    // 错误文本要能直接告诉用户去哪里解绑。
    let text = err.to_string();
    assert!(text.contains("甲流水线"), "{text}");
    assert!(text.contains("乙流水线"), "{text}");
    assert!(
        text.contains("第 2 步"),
        "步骤序号应是从 1 数给用户看的: {text}"
    );

    // 关键：被拒绝时不能真的删掉。
    assert!(store(&db).exists(credential_id).await.unwrap());
}

#[tokio::test]
async fn the_credential_survives_a_refused_deletion() {
    let db = setup().await;
    let credential_id = insert_credential(&db).await;
    let pipeline_id = insert_pipeline(&db, "持引用者").await;
    insert_step(
        &db,
        pipeline_id,
        0,
        serde_json::json!({ CREDENTIAL_REFERENCE_FIELD: credential_id }),
    )
    .await;

    let store = store(&db);
    store.delete(credential_id).await.expect_err("应被拒绝");

    // 解除引用之后应当可以删掉——拒绝不等于永久锁定。
    pipeline_step::Entity::delete_many()
        .exec(&db)
        .await
        .expect("应能清掉步骤");
    store
        .delete(credential_id)
        .await
        .expect("解除引用后应可删除");
}

// ---- 引用判定的边界 ----

#[tokio::test]
async fn only_the_exact_identifier_counts_as_a_reference() {
    let db = setup().await;
    let credential_id = insert_credential(&db).await;

    let pipeline_id = insert_pipeline(&db, "引用别的凭据").await;
    insert_step(
        &db,
        pipeline_id,
        0,
        serde_json::json!({ CREDENTIAL_REFERENCE_FIELD: credential_id + 1 }),
    )
    .await;

    store(&db)
        .delete(credential_id)
        .await
        .expect("引用的是另一个标识，不该挡住删除");
}

#[tokio::test]
async fn nested_references_are_found() {
    let db = setup().await;
    let credential_id = insert_credential(&db).await;

    let pipeline_id = insert_pipeline(&db, "嵌套引用").await;
    insert_step(
        &db,
        pipeline_id,
        0,
        serde_json::json!({ "dns": { "provider": "cloudflare", CREDENTIAL_REFERENCE_FIELD: credential_id } }),
    )
    .await;
    // 数组里的引用同样要能找到。
    insert_step(
        &db,
        pipeline_id,
        1,
        serde_json::json!({ "steps": [{ CREDENTIAL_REFERENCE_FIELD: credential_id }] }),
    )
    .await;

    let found = store(&db).referring_pipelines(credential_id).await.unwrap();
    assert_eq!(found.len(), 2, "嵌套与数组中的引用都应被找到: {found:?}");
}

// ---- Scenario: SSH 主机档案被 cert.deploy 引用时删除 ----

#[tokio::test]
async fn an_ssh_host_profile_referenced_by_a_deploy_step_is_refused() {
    // cert.deploy 的档案引用长在 `config` 里一层：{"target":"ssh",
    // "config":{"credential_id":N,…}}。递归扫描必须穿透这层结构，
    // 否则删掉一份被部署步骤引用的档案，流水线要到执行时才炸。
    let db = setup().await;
    let credential_id = insert_credential(&db).await;

    let pipeline_id = insert_pipeline(&db, "证书下发").await;
    insert_step(
        &db,
        pipeline_id,
        0,
        serde_json::json!({
            "target": "ssh",
            "config": {
                "credential_id": credential_id,
                "cert_path": "/srv/ssl/site.crt",
                "key_path": "/srv/ssl/site.key",
            },
        }),
    )
    .await;

    let err = store(&db)
        .delete(credential_id)
        .await
        .expect_err("部署步骤引用的档案应拒绝删除");

    match &err {
        Error::CredentialInUse {
            credential_id: reported,
            referenced_by,
        } => {
            assert_eq!(*reported, credential_id);
            assert_eq!(referenced_by.len(), 1);
            assert_eq!(referenced_by[0].pipeline_name, "证书下发");
        }
        other => panic!("期望 CredentialInUse，实际 {other:?}"),
    }
    assert!(store(&db).exists(credential_id).await.unwrap());
}

#[tokio::test]
async fn lookalike_fields_are_not_treated_as_references() {
    let db = setup().await;
    let credential_id = insert_credential(&db).await;

    let pipeline_id = insert_pipeline(&db, "形似引用").await;
    // 四种都不算：字符串形式的 id、别的 _id 字段、null、以及作为对象键名的一部分。
    insert_step(
        &db,
        pipeline_id,
        0,
        serde_json::json!({ CREDENTIAL_REFERENCE_FIELD: credential_id.to_string() }),
    )
    .await;
    insert_step(
        &db,
        pipeline_id,
        1,
        serde_json::json!({ "pipeline_id": credential_id }),
    )
    .await;
    insert_step(
        &db,
        pipeline_id,
        2,
        serde_json::json!({ CREDENTIAL_REFERENCE_FIELD: null }),
    )
    .await;
    insert_step(
        &db,
        pipeline_id,
        3,
        serde_json::json!({ "note": format!("{CREDENTIAL_REFERENCE_FIELD} 已弃用") }),
    )
    .await;

    let found = store(&db).referring_pipelines(credential_id).await.unwrap();
    assert!(found.is_empty(), "形似引用不应被当成引用: {found:?}");

    store(&db)
        .delete(credential_id)
        .await
        .expect("没有真实引用时应能删除");
}
