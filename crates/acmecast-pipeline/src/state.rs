//! 流水线级键值存储。

use acmecast_store::entity::storage;
use async_trait::async_trait;
use chrono::Utc;
use sea_orm::sea_query::OnConflict;
use sea_orm::{ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter, Set};

use crate::error::{Error, Result};

/// 流水线级键值存储。
///
/// 供任务保存**跨次运行**的状态——上次成功的 DNS 记录、上次的部署路径。
/// 存储限定在流水线作用域内：不同流水线读到的是各自独立的一份。
///
/// 这里只定义能力，落库实现见 6.7。让执行上下文依赖接口而非具体类型，
/// 是为了这一层的测试不必拖上数据库。
#[async_trait]
pub trait PipelineStateStore: Send + Sync + std::fmt::Debug {
    /// 读一个键；不存在时返回 `None`。
    async fn get(&self, pipeline_id: i64, key: &str) -> Result<Option<serde_json::Value>>;

    /// 写一个键。
    async fn set(&self, pipeline_id: i64, key: &str, value: serde_json::Value) -> Result<()>;
}

/// 基于数据库的流水线级状态存储。
///
/// 每个键在库里是一行。隔离性由**查询条件**保证（每个读写都带 `pipeline_id`），
/// 并由 `(pipeline_id, store_key)` 上的唯一索引兜底——不同流水线即便用了
/// 同一个键名，也是各自独立的两行。
#[derive(Debug)]
pub struct DatabaseStateStore<'db> {
    db: &'db DatabaseConnection,
}

impl<'db> DatabaseStateStore<'db> {
    /// 绑定数据库连接。
    #[must_use]
    pub fn new(db: &'db DatabaseConnection) -> Self {
        Self { db }
    }
}

#[async_trait]
impl PipelineStateStore for DatabaseStateStore<'_> {
    async fn get(&self, pipeline_id: i64, key: &str) -> Result<Option<serde_json::Value>> {
        let found = storage::Entity::find()
            .filter(storage::Column::PipelineId.eq(pipeline_id))
            .filter(storage::Column::StoreKey.eq(key))
            .one(self.db)
            .await?;

        match found {
            None => Ok(None),
            Some(row) => serde_json::from_str(&row.store_value)
                .map(Some)
                .map_err(|e| {
                    Error::Core(acmecast_core::Error::Serialization(format!(
                        "流水线 {pipeline_id} 的键 `{key}` 存的不是合法 JSON: {e}"
                    )))
                }),
        }
    }

    async fn set(&self, pipeline_id: i64, key: &str, value: serde_json::Value) -> Result<()> {
        let encoded = serde_json::to_string(&value)
            .map_err(|e| Error::Core(acmecast_core::Error::Serialization(e.to_string())))?;

        storage::Entity::insert(storage::ActiveModel {
            pipeline_id: Set(pipeline_id),
            store_key: Set(key.to_owned()),
            store_value: Set(encoded),
            updated_at: Set(Utc::now()),
            ..Default::default()
        })
        .on_conflict(
            // 交给数据库一次完成 upsert：先查再写在同一流水线并发写同一个键时会
            // 互相覆盖，而那条唯一索引（pipeline_id, store_key）正好是冲突目标。
            OnConflict::columns([storage::Column::PipelineId, storage::Column::StoreKey])
                .update_columns([storage::Column::StoreValue, storage::Column::UpdatedAt])
                .to_owned(),
        )
        .exec(self.db)
        .await?;

        Ok(())
    }
}
