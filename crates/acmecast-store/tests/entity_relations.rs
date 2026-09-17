//! 实体关系验证：确认八张表能在真实数据库上建立、写入并按关系读回。
//!
//! 使用 SQLite 内存库，无需外部数据库即可运行。

use acmecast_store::entity::{
    cert, credential, history, history_log, pipeline, pipeline_step, schedule, storage,
};
use chrono::Utc;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, Database, DatabaseBackend, EntityTrait,
    QueryFilter, QueryOrder, Schema, Set,
};
use sea_orm::{LoaderTrait, ModelTrait};

/// 建立内存 SQLite 并依据实体定义建表。
async fn setup() -> sea_orm::DatabaseConnection {
    let db = Database::connect("sqlite::memory:")
        .await
        .expect("应能连上内存库");
    let backend = db.get_database_backend();
    let schema = Schema::new(backend);

    for entity_query in [
        schema.create_table_from_entity(pipeline::Entity),
        schema.create_table_from_entity(pipeline_step::Entity),
        schema.create_table_from_entity(history::Entity),
        schema.create_table_from_entity(history_log::Entity),
        schema.create_table_from_entity(storage::Entity),
        schema.create_table_from_entity(credential::Entity),
        schema.create_table_from_entity(cert::Entity),
        schema.create_table_from_entity(schedule::Entity),
    ] {
        db.execute(backend.build(&entity_query))
            .await
            .expect("建表应成功");
    }
    db
}

#[tokio::test]
async fn all_eight_tables_are_creatable() {
    let db = setup().await;
    // 表都能 SELECT 即说明创建成功。
    for entity_name in [
        "acmecast_pipeline",
        "acmecast_pipeline_step",
        "acmecast_history",
        "acmecast_history_log",
        "acmecast_storage",
        "acmecast_credential",
        "acmecast_cert",
        "acmecast_schedule",
    ] {
        let rows = db
            .query_all(sea_orm::Statement::from_string(
                DatabaseBackend::Sqlite,
                format!("SELECT COUNT(*) FROM {entity_name}"),
            ))
            .await
            .unwrap_or_else(|e| panic!("表 {entity_name} 应可查询: {e}"));
        assert_eq!(rows.len(), 1, "COUNT 应返回一行");
    }
}

#[tokio::test]
async fn pipeline_can_load_its_steps() {
    let db = setup().await;

    let pipeline = pipeline::ActiveModel {
        name: Set("示例流水线".to_owned()),
        enabled: Set(true),
        description: Set(None),
        created_at: Set(Utc::now()),
        updated_at: Set(Utc::now()),
        ..Default::default()
    }
    .insert(&db)
    .await
    .expect("插入流水线应成功");

    for (idx, type_id) in ["cert.apply", "deploy.local"].iter().enumerate() {
        pipeline_step::ActiveModel {
            pipeline_id: Set(pipeline.id),
            order_index: Set(idx as i32),
            type_id: Set(type_id.to_string()),
            input: Set(serde_json::json!({"domains": ["example.com"]})),
            enabled: Set(true),
            ..Default::default()
        }
        .insert(&db)
        .await
        .expect("插入步骤应成功");
    }

    let loaded = pipeline
        .find_related(pipeline_step::Entity)
        .all(&db)
        .await
        .expect("关联查询应成功");

    assert_eq!(loaded.len(), 2, "应读到两个步骤");
}

#[tokio::test]
async fn steps_are_returned_in_configured_order() {
    let db = setup().await;
    let pipeline = pipeline::ActiveModel {
        name: Set("顺序校验".to_owned()),
        enabled: Set(true),
        description: Set(None),
        created_at: Set(Utc::now()),
        updated_at: Set(Utc::now()),
        ..Default::default()
    }
    .insert(&db)
    .await
    .unwrap();

    // 刻意乱序插入，验证读取时按 order_index 排序。
    for (order, type_id) in [(2, "c"), (0, "a"), (1, "b")] {
        pipeline_step::ActiveModel {
            pipeline_id: Set(pipeline.id),
            order_index: Set(order),
            type_id: Set(type_id.to_owned()),
            input: Set(serde_json::json!({})),
            enabled: Set(true),
            ..Default::default()
        }
        .insert(&db)
        .await
        .unwrap();
    }

    let ordered = pipeline_step::Entity::find()
        .filter(pipeline_step::Column::PipelineId.eq(pipeline.id))
        .order_by_asc(pipeline_step::Column::OrderIndex)
        .all(&db)
        .await
        .unwrap();

    let type_ids: Vec<_> = ordered.iter().map(|s| s.type_id.as_str()).collect();
    assert_eq!(type_ids, vec!["a", "b", "c"], "应按 order_index 升序返回");
}

#[tokio::test]
async fn history_can_load_its_logs() {
    let db = setup().await;
    let pipeline = pipeline::ActiveModel {
        name: Set("日志校验".to_owned()),
        enabled: Set(true),
        description: Set(None),
        created_at: Set(Utc::now()),
        updated_at: Set(Utc::now()),
        ..Default::default()
    }
    .insert(&db)
    .await
    .unwrap();

    let run = history::ActiveModel {
        pipeline_id: Set(pipeline.id),
        trigger_source: Set(pipeline::TriggerSource::Cron.as_str().to_owned()),
        status: Set(history::RunStatus::Failed.as_str().to_owned()),
        started_at: Set(Utc::now()),
        finished_at: Set(Some(Utc::now())),
        error_message: Set(Some("第二步失败".to_owned())),
        ..Default::default()
    }
    .insert(&db)
    .await
    .unwrap();

    for index in 0..3 {
        history_log::ActiveModel {
            history_id: Set(run.id),
            step_index: Set(index),
            level: Set(history_log::Level::Info.as_str().to_owned()),
            message: Set(format!("第 {index} 步日志")),
            created_at: Set(Utc::now()),
            ..Default::default()
        }
        .insert(&db)
        .await
        .unwrap();
    }

    let logs = run
        .find_related(history_log::Entity)
        .all(&db)
        .await
        .unwrap();
    assert_eq!(logs.len(), 3, "应读到三条日志");
}

#[tokio::test]
async fn cert_can_resolve_its_acme_account_credential() {
    let db = setup().await;

    let account = credential::ActiveModel {
        name: Set("我的 ACME 账号".to_owned()),
        type_id: Set("acme.account".to_owned()),
        encrypted_fields: Set("{\"key\":\"<ciphertext>\"}".to_owned()),
        created_at: Set(Utc::now()),
        updated_at: Set(Utc::now()),
        ..Default::default()
    }
    .insert(&db)
    .await
    .unwrap();

    let now = Utc::now();
    let certificate = cert::ActiveModel {
        domains: Set("example.com".to_owned()),
        cert_pem_path: Set("certs/ab/cert.pem".to_owned()),
        key_pem_path: Set("certs/ab/key.pem".to_owned()),
        fingerprint: Set(" sha256:abcdef".to_owned()),
        issuer: Set(Some("Test CA".to_owned())),
        not_before: Set(now),
        not_after: Set(now + chrono::Duration::days(90)),
        // 关键：签发账号直接落在证书行上，吊销时无需反查流水线。
        acme_account_access_id: Set(Some(account.id)),
        revoked_at: Set(None),
        created_at: Set(now),
        updated_at: Set(now),
        ..Default::default()
    }
    .insert(&db)
    .await
    .unwrap();

    // 按 via 关系读回签发账号。
    let linked: Vec<credential::Model> = vec![certificate.clone()]
        .load_one(credential::Entity, &db)
        .await
        .unwrap()
        .into_iter()
        .flatten()
        .collect();
    assert_eq!(linked.len(), 1);
    assert_eq!(linked[0].id, account.id);

    // 反向：从凭据查它签发过的证书。
    let issued = account.find_related(cert::Entity).all(&db).await.unwrap();
    assert_eq!(issued.len(), 1);
    assert_eq!(issued[0].id, certificate.id);
}

#[tokio::test]
async fn uploaded_cert_has_no_account_and_resolves_to_none() {
    let db = setup().await;
    let now = Utc::now();

    let certificate = cert::ActiveModel {
        domains: Set("uploaded.example.com".to_owned()),
        cert_pem_path: Set("certs/cd/cert.pem".to_owned()),
        key_pem_path: Set("certs/cd/key.pem".to_owned()),
        fingerprint: Set("sha256:uploaded".to_owned()),
        issuer: Set(None),
        not_before: Set(now),
        not_after: Set(now + chrono::Duration::days(365)),
        acme_account_access_id: Set(None),
        revoked_at: Set(None),
        created_at: Set(now),
        updated_at: Set(now),
        ..Default::default()
    }
    .insert(&db)
    .await
    .unwrap();

    let linked = vec![certificate]
        .load_one(credential::Entity, &db)
        .await
        .unwrap();
    assert!(
        linked[0].is_none(),
        "手动上传的证书不应解析出 ACME 账号，吊销时应据此报错"
    );
}

#[tokio::test]
async fn storage_is_scoped_per_pipeline() {
    let db = setup().await;

    let mut pipeline_ids = Vec::new();
    for name in ["甲流水线", "乙流水线"] {
        let pipeline = pipeline::ActiveModel {
            name: Set(name.to_owned()),
            enabled: Set(true),
            description: Set(None),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            ..Default::default()
        }
        .insert(&db)
        .await
        .unwrap();
        pipeline_ids.push(pipeline.id);
    }

    storage::ActiveModel {
        pipeline_id: Set(pipeline_ids[0]),
        store_key: Set("last_dns_record".to_owned()),
        store_value: Set("\"abc\"".to_owned()),
        updated_at: Set(Utc::now()),
        ..Default::default()
    }
    .insert(&db)
    .await
    .unwrap();

    let mine = storage::Entity::find()
        .filter(storage::Column::PipelineId.eq(pipeline_ids[0]))
        .all(&db)
        .await
        .unwrap();
    assert_eq!(mine.len(), 1);

    // 另一条流水线读不到同一键。
    let others = storage::Entity::find()
        .filter(storage::Column::PipelineId.eq(pipeline_ids[1]))
        .all(&db)
        .await
        .unwrap();
    assert!(others.is_empty(), "键值存储应按流水线隔离");
}

#[tokio::test]
async fn credential_ciphertext_column_never_holds_plaintext() {
    let db = setup().await;

    let secret = "sk-live-abcdef123456";
    let cipher = acmecast_core::CredentialCipher::from_base64(
        &acmecast_core::CredentialCipher::generate_key_base64(),
    )
    .unwrap();
    let encoded = cipher.encrypt_string(secret).unwrap();

    let credential = credential::ActiveModel {
        name: Set("DNS 密钥".to_owned()),
        type_id: Set("dns.cloudflare".to_owned()),
        encrypted_fields: Set(serde_json::json!({ "api_token": encoded }).to_string()),
        created_at: Set(Utc::now()),
        updated_at: Set(Utc::now()),
        ..Default::default()
    }
    .insert(&db)
    .await
    .unwrap();

    assert!(
        !credential.encrypted_fields.contains("sk-live-abcdef123456"),
        "库中必须是密文"
    );

    // 读回并按 records 名解密。
    let stored: serde_json::Value = serde_json::from_str(&credential.encrypted_fields).unwrap();
    let round_tripped = cipher
        .decrypt_string(stored["api_token"].as_str().unwrap())
        .unwrap();
    assert_eq!(round_tripped, secret);
}
