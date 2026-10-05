//! 部署记录：既是幂等判断的依据，也是运维回溯的历史。
//!
//! 一份数据服务两件事，因此只有一张表：取最新一条就是「目标上现在是哪一份证书」
//! （幂等判断的依据，没有它每次部署都会重写一遍文件），全量就是部署历史。

use std::sync::Mutex;

use acmecast_store::entity::deployment::{self, TargetRef};
use acmecast_store::repository::{DEFAULT_PAGE_SIZE, MAX_PAGE_SIZE};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait, PaginatorTrait, QueryFilter,
    QueryOrder, Set,
};
use serde_json::Value;

use crate::error::Result;

/// 一次部署的记录。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeploymentEntry {
    /// 部署目标。
    pub target: TargetRef,
    /// 本次部署的证书指纹。
    pub fingerprint: String,
    /// 是否跳过了文件写入。
    pub skipped_write: bool,
    /// 部署时间。
    pub deployed_at: DateTime<Utc>,
    /// 本次写入的路径。
    pub paths: Vec<String>,
    /// 重载命令的输出；未配置或未执行时为 `None`。
    pub reload_output: Option<String>,
}

/// 部署历史的查询条件。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DeploymentQuery {
    /// 只看某个目标；`None` 表示全部。
    pub target: Option<TargetRef>,
    /// 只看跳过写入的（`Some(true)`）或只看真写入的（`Some(false)`）。
    pub skipped_write: Option<bool>,
    /// 只看这个时刻之后（含）。
    pub since: Option<DateTime<Utc>>,
    /// 只看这个时刻之前（含）。
    pub until: Option<DateTime<Utc>>,
    /// 页码，从 1 起。
    pub page: u64,
    /// 每页条数。
    pub page_size: u64,
}

/// 一页部署历史。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeploymentPage {
    /// 本页记录，按部署时间倒序。
    pub items: Vec<DeploymentEntry>,
    /// 符合条件的总条数。
    pub total: u64,
    /// 当前页码。
    pub page: u64,
    /// 每页条数（已归一）。
    pub page_size: u64,
}

/// 归一分页参数。
///
/// 页码至少 1；每页条数缺省（0）时用 [`DEFAULT_PAGE_SIZE`]，越界时收拢进
/// `1..=`[`MAX_PAGE_SIZE`]。两种存储实现共用，行为必须一致。
fn normalize_pagination(page: u64, page_size: u64) -> (u64, u64) {
    let page = page.max(1);
    let page_size = if page_size == 0 {
        DEFAULT_PAGE_SIZE
    } else {
        page_size.clamp(1, MAX_PAGE_SIZE)
    };
    (page, page_size)
}

/// 部署记录的存取。
#[async_trait]
pub trait DeploymentStateStore: Send + Sync + std::fmt::Debug {
    /// 取该目标**最新**一次部署的指纹；从未部署过则为 `None`。
    async fn deployed_fingerprint(&self, target: &TargetRef) -> Result<Option<String>>;

    /// 记录一次部署。
    async fn record(&self, entry: DeploymentEntry) -> Result<()>;

    /// 按条件查询部署历史，按时间倒序。
    async fn history(&self, query: DeploymentQuery) -> Result<DeploymentPage>;
}

/// 落在数据库的部署记录。
///
/// 持有连接本身（内部是句柄，克隆廉价）而不是借用：注册表与步骤单例
/// 需要 `'static` 的 `Arc<dyn DeploymentStateStore>`，借用形态做不到。
#[derive(Debug, Clone)]
pub struct DatabaseDeploymentState {
    db: DatabaseConnection,
}

impl DatabaseDeploymentState {
    /// 绑定数据库连接。
    #[must_use]
    pub fn new(db: DatabaseConnection) -> Self {
        Self { db }
    }
}

#[async_trait]
impl DeploymentStateStore for DatabaseDeploymentState {
    async fn deployed_fingerprint(&self, target: &TargetRef) -> Result<Option<String>> {
        // 「最新」按时间排，同刻的两条再退回到主键——否则时间相同时可能取到先写的那条，
        // 于是指纹看起来没变、写入被跳过，而实际上文件从没被更新过。
        let latest = deployment::Entity::find()
            .filter(deployment::Column::TargetType.eq(target.target_type.as_str()))
            .filter(deployment::Column::TargetKey.eq(target.target_key.as_str()))
            .order_by_desc(deployment::Column::DeployedAt)
            .order_by_desc(deployment::Column::Id)
            .one(&self.db)
            .await?;

        Ok(latest.map(|row| row.fingerprint))
    }

    async fn record(&self, entry: DeploymentEntry) -> Result<()> {
        deployment::ActiveModel {
            target_type: Set(entry.target.target_type),
            target_key: Set(entry.target.target_key),
            fingerprint: Set(entry.fingerprint),
            skipped_write: Set(entry.skipped_write),
            deployed_at: Set(entry.deployed_at),
            paths: Set(serde_json::json!(entry.paths)),
            reload_output: Set(entry.reload_output),
            ..Default::default()
        }
        .insert(&self.db)
        .await?;

        Ok(())
    }

    async fn history(&self, query: DeploymentQuery) -> Result<DeploymentPage> {
        let (page, page_size) = normalize_pagination(query.page, query.page_size);

        let mut select = deployment::Entity::find();
        if let Some(target) = &query.target {
            select = select
                .filter(deployment::Column::TargetType.eq(target.target_type.as_str()))
                .filter(deployment::Column::TargetKey.eq(target.target_key.as_str()));
        }
        if let Some(skipped) = query.skipped_write {
            select = select.filter(deployment::Column::SkippedWrite.eq(skipped));
        }
        if let Some(since) = query.since {
            select = select.filter(deployment::Column::DeployedAt.gte(since));
        }
        if let Some(until) = query.until {
            select = select.filter(deployment::Column::DeployedAt.lte(until));
        }

        let paginator = select
            .order_by_desc(deployment::Column::DeployedAt)
            // 同一时刻的记录靠主键定序，否则翻页时同一条可能出现在两页里。
            .order_by_desc(deployment::Column::Id)
            .paginate(&self.db, page_size);

        let total = paginator.num_items().await?;
        let rows = paginator.fetch_page(page - 1).await?;

        Ok(DeploymentPage {
            items: rows.into_iter().map(entry_from_row).collect(),
            total,
            page,
            page_size,
        })
    }
}

/// 把一行记录转成领域类型。
///
/// `paths` 存的是 JSON 数组；解析不出来时退化成空列表而不报错——一次历史查询
/// 不该因为某条记录的字段格式问题而整体失败。
fn entry_from_row(row: deployment::Model) -> DeploymentEntry {
    DeploymentEntry {
        target: TargetRef {
            target_type: row.target_type,
            target_key: row.target_key,
        },
        fingerprint: row.fingerprint,
        skipped_write: row.skipped_write,
        deployed_at: row.deployed_at,
        paths: serde_json::from_value(row.paths).unwrap_or_default(),
        reload_output: row.reload_output,
    }
}

/// 内存中的部署记录，供测试与不落库的场景使用。
///
/// 进程退出即失效——那意味着重启后第一次部署会重写一次文件。对测试无所谓，
/// 生产请用 [`DatabaseDeploymentState`]。
#[derive(Debug, Default)]
pub struct InMemoryDeploymentState {
    /// 全部记录，按写入顺序追加。
    ///
    /// 存全量而非「只留最新指纹」：这样 [`DeploymentStateStore::history`]
    /// 在两种实现下的行为一致，测试才说明得了问题。
    entries: Mutex<Vec<DeploymentEntry>>,
}

impl InMemoryDeploymentState {
    /// 建一个空存储。
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// 取锁。中毒只说明别的线程在持锁时 panic 过——内层数据仍然可用，恢复它继续。
    fn locked(&self) -> std::sync::MutexGuard<'_, Vec<DeploymentEntry>> {
        self.entries
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

#[async_trait]
impl DeploymentStateStore for InMemoryDeploymentState {
    async fn deployed_fingerprint(&self, target: &TargetRef) -> Result<Option<String>> {
        let entries = self.locked();
        Ok(entries
            .iter()
            .filter(|entry| &entry.target == target)
            .max_by_key(|entry| entry.deployed_at)
            .map(|entry| entry.fingerprint.clone()))
    }

    async fn record(&self, entry: DeploymentEntry) -> Result<()> {
        self.locked().push(entry);
        Ok(())
    }

    async fn history(&self, query: DeploymentQuery) -> Result<DeploymentPage> {
        let (page, page_size) = normalize_pagination(query.page, query.page_size);

        let entries = self.locked();
        let mut matched: Vec<DeploymentEntry> = entries
            .iter()
            .filter(|entry| {
                query
                    .target
                    .as_ref()
                    .is_none_or(|target| &entry.target == target)
            })
            .filter(|entry| {
                query
                    .skipped_write
                    .is_none_or(|skipped| entry.skipped_write == skipped)
            })
            .filter(|entry| query.since.is_none_or(|since| entry.deployed_at >= since))
            .filter(|entry| query.until.is_none_or(|until| entry.deployed_at <= until))
            .cloned()
            .collect();

        // 与数据库实现同样按时间倒序。
        matched.sort_by_key(|entry| std::cmp::Reverse(entry.deployed_at));

        let total = matched.len() as u64;
        let items = matched
            .into_iter()
            .skip(((page - 1) * page_size) as usize)
            .take(page_size as usize)
            .collect();

        Ok(DeploymentPage {
            items,
            total,
            page,
            page_size,
        })
    }
}

/// 按部署输入推导目标定位键。
///
/// 对输入的稳定序列化取摘要：同一份配置必得同一个键；配置改了则视作另一个目标
/// （于是重新写入——保守，但不会漏）。
#[must_use]
pub fn target_key_for(input: &Value) -> String {
    use base64::Engine as _;
    use sha2::{Digest, Sha256};

    let canonical = serde_json::to_string(input).unwrap_or_default();
    let digest = Sha256::digest(canonical.as_bytes());

    format!(
        "sha256:{}",
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(digest)
    )
}

/// 把目标类型与输入组装成一个目标引用。
#[must_use]
pub fn target_ref_of(target_type: &str, input: &Value) -> TargetRef {
    TargetRef {
        target_type: target_type.to_owned(),
        target_key: target_key_for(input),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use serde_json::json;

    fn a_target() -> TargetRef {
        TargetRef {
            target_type: "local".to_owned(),
            target_key: "k1".to_owned(),
        }
    }

    fn an_entry(fingerprint: &str, at: DateTime<Utc>, skipped: bool) -> DeploymentEntry {
        DeploymentEntry {
            target: a_target(),
            fingerprint: fingerprint.to_owned(),
            skipped_write: skipped,
            deployed_at: at,
            paths: vec!["/etc/ssl/cert.pem".to_owned()],
            reload_output: Some("reloaded".to_owned()),
        }
    }

    #[test]
    fn the_same_input_always_maps_to_the_same_target() {
        let one = json!({ "cert_path": "/a", "key_path": "/b" });
        let two = json!({ "cert_path": "/a", "key_path": "/b" });
        assert_eq!(target_key_for(&one), target_key_for(&two));
    }

    #[test]
    fn a_different_input_is_a_different_target() {
        assert_ne!(
            target_key_for(&json!({ "cert_path": "/a" })),
            target_key_for(&json!({ "cert_path": "/c" }))
        );
    }

    #[tokio::test]
    async fn the_in_memory_store_round_trips() {
        let store = InMemoryDeploymentState::new();
        let target = a_target();

        assert!(store.deployed_fingerprint(&target).await.unwrap().is_none());

        store
            .record(an_entry("sha256:abc", Utc::now(), false))
            .await
            .unwrap();

        assert_eq!(
            store
                .deployed_fingerprint(&target)
                .await
                .unwrap()
                .as_deref(),
            Some("sha256:abc")
        );
    }

    #[tokio::test]
    async fn the_latest_record_wins_regardless_of_insert_order() {
        let store = InMemoryDeploymentState::new();
        let now = Utc::now();

        store
            .record(an_entry("sha256:new", now, false))
            .await
            .unwrap();
        // 后写但时间更早——判断该按时间而不是插入顺序。
        store
            .record(an_entry(
                "sha256:old",
                now - chrono::Duration::hours(1),
                false,
            ))
            .await
            .unwrap();

        assert_eq!(
            store
                .deployed_fingerprint(&a_target())
                .await
                .unwrap()
                .as_deref(),
            Some("sha256:new")
        );
    }

    #[tokio::test]
    async fn history_filters_by_skip_flag_and_orders_newest_first() {
        let store = InMemoryDeploymentState::new();
        let now = Utc::now();

        store
            .record(an_entry(
                "sha256:a",
                now - chrono::Duration::minutes(30),
                false,
            ))
            .await
            .unwrap();
        store
            .record(an_entry(
                "sha256:b",
                now - chrono::Duration::minutes(20),
                true,
            ))
            .await
            .unwrap();
        store
            .record(an_entry(
                "sha256:c",
                now - chrono::Duration::minutes(10),
                false,
            ))
            .await
            .unwrap();

        let all = store.history(DeploymentQuery::default()).await.unwrap();
        let order: Vec<&str> = all.items.iter().map(|e| e.fingerprint.as_str()).collect();
        assert_eq!(
            order,
            vec!["sha256:c", "sha256:b", "sha256:a"],
            "应 newest first"
        );

        let skipped = store
            .history(DeploymentQuery {
                skipped_write: Some(true),
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(skipped.total, 1);
        assert_eq!(skipped.items[0].fingerprint, "sha256:b");
    }

    #[tokio::test]
    async fn history_filters_by_time_window() {
        let store = InMemoryDeploymentState::new();
        let now = Utc::now();

        store
            .record(an_entry(
                "sha256:old",
                now - chrono::Duration::hours(2),
                false,
            ))
            .await
            .unwrap();
        store
            .record(an_entry("sha256:new", now, false))
            .await
            .unwrap();

        let recent = store
            .history(DeploymentQuery {
                since: Some(now - chrono::Duration::hours(1)),
                ..Default::default()
            })
            .await
            .unwrap();

        assert_eq!(recent.total, 1);
        assert_eq!(recent.items[0].fingerprint, "sha256:new");
    }

    #[tokio::test]
    async fn history_is_scoped_to_the_requested_target() {
        let store = InMemoryDeploymentState::new();
        let now = Utc::now();

        let mut other = an_entry("sha256:other", now, false);
        other.target = TargetRef {
            target_type: "ssh".to_owned(),
            target_key: "k2".to_owned(),
        };

        store
            .record(an_entry("sha256:mine", now, false))
            .await
            .unwrap();
        store.record(other).await.unwrap();

        let scoped = store
            .history(DeploymentQuery {
                target: Some(a_target()),
                ..Default::default()
            })
            .await
            .unwrap();

        assert_eq!(scoped.total, 1);
        assert_eq!(scoped.items[0].fingerprint, "sha256:mine");
    }
}
