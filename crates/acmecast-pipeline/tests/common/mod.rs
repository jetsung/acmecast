//! 集成测试共用的脚手架。
//!
//! 每个测试文件各自 include 这个模块，因此它**不是**一个测试目标本身
//! （放在 `common/` 子目录下就不会被 cargo 当成测试用例收集）。
//!
//! 允许 `dead_code`：每个使用者只取自己要的那几件。
#![allow(dead_code)]

use std::sync::Arc;

use acmecast_access::{CredentialRegistry, CredentialStore};
use acmecast_core::CredentialCipher;
use acmecast_pipeline::{PipelineDefinition, StepDefinition};
use acmecast_store::entity::pipeline;
use acmecast_store::migrate;
use chrono::Utc;
use sea_orm::{ActiveModelTrait, Database, DatabaseConnection, Set};

/// 建一个跑完迁移的内存库。
pub(crate) async fn database() -> DatabaseConnection {
    let db = Database::connect("sqlite::memory:")
        .await
        .expect("应能连上内存库");
    migrate(&db).await.expect("迁移应成功");
    db
}

/// 一个不注册任何凭据类型的凭据存储。
///
/// 只用到「按标识取凭据」之外的步骤时，空注册表就够。
pub(crate) fn credential_store(db: &DatabaseConnection) -> CredentialStore<'_> {
    let cipher = CredentialCipher::from_base64(&CredentialCipher::generate_key_base64())
        .expect("密钥应可用");
    CredentialStore::new(db, Arc::new(CredentialRegistry::new()), Arc::new(cipher))
}

/// 构造一个步骤定义。
pub(crate) fn step(order_index: i32, type_id: &str) -> StepDefinition {
    StepDefinition {
        order_index,
        type_id: type_id.to_owned(),
        input: serde_json::json!({}),
        enabled: true,
    }
}

/// 构造一条流水线定义。
pub(crate) fn pipeline(id: i64, steps: Vec<StepDefinition>) -> PipelineDefinition {
    PipelineDefinition { id, steps }
}

/// 插入一条流水线（历史与状态表都有外键指向它），返回主键。
pub(crate) async fn insert_pipeline(db: &DatabaseConnection, name: &str) -> i64 {
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
