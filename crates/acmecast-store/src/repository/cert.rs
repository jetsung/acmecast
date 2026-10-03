//! 证书仓储：CRUD 与按域名集合去重写入。
//!
//! 去重依据是**域名集合**（见 [`cert::encode_domains`]），不是指纹也不是流水线：
//! 同一组域名重复签发时，用户期望的是「这份证书更新了」，而不是列表里凭空多出一条。
//!
//! 这里**没有**在数据库层给域名集合加唯一约束：`domains` 列是 `TEXT`，
//! 而 MySQL 拒绝在未指定长度的 `TEXT` 列上建唯一索引（要么改成有长度上限的
//! `VARCHAR`——而域名集合最长可达数 KB，要么另存一列哈希）。当前只由本层保证去重，
//! 前提是同一域名集合不会被并发签发；若将来调度与手动触发真的可能并发，
//! 应连同流水线执行锁一起解决，而不是只在这一层打补丁。

use chrono::{DateTime, Utc};
use sea_orm::sea_query::LikeExpr;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait, PaginatorTrait, QueryFilter,
    QueryOrder, Set,
};

use crate::entity::{cert, credential};
use crate::error::{Error, Result};
use crate::filestore::CertFilePaths;

/// 默认每页条数。
pub const DEFAULT_PAGE_SIZE: u64 = 20;

/// 每页条数上限。防止一次请求把整张表拉走。
pub const MAX_PAGE_SIZE: u64 = 100;

/// 写入一条证书所需的字段。
///
/// `created_at` / `updated_at` 由仓储自行设置，调用方既不必要也无法指定。
#[derive(Debug, Clone)]
pub struct CertInput {
    /// 该证书覆盖的域名。顺序与大小写不限，仓储会规范化后再比较。
    pub domains: Vec<String>,
    /// 证书 PEM 在数据目录下的相对路径。
    pub cert_pem_path: String,
    /// 私钥 PEM 在数据目录下的相对路径。
    pub key_pem_path: String,
    /// 证书 SHA-256 指纹。
    pub fingerprint: String,
    /// 签发者主题名。
    pub issuer: Option<String>,
    /// 生效时间。
    pub not_before: DateTime<Utc>,
    /// 到期时间。
    pub not_after: DateTime<Utc>,
    /// 签发所用的 ACME 账号凭据标识；手动上传时为 `None`。
    pub acme_account_access_id: Option<i64>,
}

/// 去重写入的结果。
///
/// 调用方需要区分二者：新建与更新在运行历史里是两条不同的记录，
/// 且只有新建会改变仓库中的证书总数。
#[derive(Debug)]
pub enum SaveOutcome {
    /// 该域名集合此前不存在，新建了一条记录。
    Created(cert::Model),
    /// 该域名集合已存在，原地更新了既有记录（主键不变）。
    Updated(cert::Model),
}

impl SaveOutcome {
    /// 取出记录本身。
    #[must_use]
    pub fn model(self) -> cert::Model {
        match self {
            Self::Created(model) | Self::Updated(model) => model,
        }
    }

    /// 本次写入是否新建了记录。
    #[must_use]
    pub fn is_created(&self) -> bool {
        matches!(self, Self::Created(_))
    }
}

/// 证书列表的排序方向。当前只按到期时间排序。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CertSort {
    /// 到期时间升序：最快到期的排在最前。
    ///
    /// 这是**默认**，对应运维视角下最常用的那个问题——「哪些证书最该先处理」。
    #[default]
    Ascending,
    /// 到期时间降序：最晚到期的排在最前，也就是最近签发的在前。
    Descending,
}

/// 证书列表查询条件。
#[derive(Debug, Clone)]
pub struct CertQuery {
    /// 按域名做模糊（子串）检索，大小写不敏感；`None` 或纯空白表示不筛选。
    pub domain: Option<String>,
    /// 排序方向。
    pub sort: CertSort,
    /// 页码，从 **1** 开始。
    pub page: u64,
    /// 每页条数，取值 `1..=`[`MAX_PAGE_SIZE`]。
    pub page_size: u64,
}

impl Default for CertQuery {
    fn default() -> Self {
        Self {
            domain: None,
            sort: CertSort::default(),
            page: 1,
            page_size: DEFAULT_PAGE_SIZE,
        }
    }
}

/// 证书列表的一页。
#[derive(Debug)]
pub struct CertPage {
    /// 本页记录。
    pub items: Vec<cert::Model>,
    /// 满足筛选条件的记录总数，不只是本页条数。
    pub total: u64,
    /// 当前页码，从 1 开始。
    pub page: u64,
    /// 每页条数。
    pub page_size: u64,
}

impl CertPage {
    /// 总页数；无记录时为 0。
    #[must_use]
    pub fn total_pages(&self) -> u64 {
        self.total.div_ceil(self.page_size)
    }
}

/// 转义 LIKE 模式中的通配符，使其按字面量匹配。
///
/// 这一步不能省：`ColumnTrait::like`（以及内部调用它的 `contains`）只是把入参
/// 拼进模式串，**不做任何转义**（见 sea-orm `entity/column.rs`）。若不自己转义，
/// 用户搜 `%` 会命中全部记录、搜 `_` 会命中任意单字符——模糊检索会变成意外的全表通配。
///
/// 反斜杠自身也要转义，否则用户输入的 `\` 会吞掉紧随其后的那个字符。
fn escape_like(keyword: &str) -> String {
    let mut escaped = String::with_capacity(keyword.len());
    for ch in keyword.chars() {
        if matches!(ch, '\\' | '%' | '_') {
            escaped.push('\\');
        }
        escaped.push(ch);
    }
    escaped
}

/// 证书仓储。
#[derive(Debug)]
pub struct CertRepository<'db> {
    db: &'db DatabaseConnection,
}

impl<'db> CertRepository<'db> {
    /// 绑定一个数据库连接。
    pub fn new(db: &'db DatabaseConnection) -> Self {
        Self { db }
    }

    /// 按主键查询。
    pub async fn find(&self, id: i64) -> Result<Option<cert::Model>> {
        Ok(cert::Entity::find_by_id(id).one(self.db).await?)
    }

    /// 按域名集合查询。入参先规范化，因此顺序与大小写不影响命中。
    pub async fn find_by_domains<S: AsRef<str>>(
        &self,
        domains: &[S],
    ) -> Result<Option<cert::Model>> {
        let encoded = cert::encode_domains(domains);
        if encoded.is_empty() {
            return Ok(None);
        }
        self.find_by_key(&encoded).await
    }

    /// 按证书指纹查询。
    pub async fn find_by_fingerprint(&self, fingerprint: &str) -> Result<Option<cert::Model>> {
        Ok(cert::Entity::find()
            .filter(cert::Column::Fingerprint.eq(fingerprint))
            .one(self.db)
            .await?)
    }

    /// 插入一条新证书，**不做域名集合去重**。
    ///
    /// 这是手动上传路径应有的语义：用户可能有意保留两张覆盖相同域名、
    /// 但内容不同的证书（例如来自不同 CA）。签发流程请用 [`Self::save`]。
    ///
    /// 指纹重复时返回 [`Error::Conflict`]——同一张证书不可能入库两次。
    pub async fn create(&self, input: CertInput) -> Result<cert::Model> {
        if let Some(existing) = self.find_by_fingerprint(&input.fingerprint).await? {
            return Err(Error::Conflict(format!(
                "指纹 {} 的证书已存在（id={}）",
                input.fingerprint, existing.id
            )));
        }
        self.insert(input).await
    }

    /// 按域名集合去重写入。
    ///
    /// 命中既有记录时**原地更新**（主键与 `created_at` 不变），未命中时新建。
    /// 这正是 spec 要求的「同一域名集合重复签发时更新而非新增」。
    pub async fn save(&self, input: CertInput) -> Result<SaveOutcome> {
        let encoded = self.encode_domains(&input.domains)?;

        match self.find_by_key(&encoded).await? {
            Some(existing) => {
                let updated = self.overwrite(existing.id, input).await?;
                Ok(SaveOutcome::Updated(updated))
            }
            None => Ok(SaveOutcome::Created(self.insert(input).await?)),
        }
    }

    /// 按主键删除，返回是否真的删掉了一条。
    pub async fn delete(&self, id: i64) -> Result<bool> {
        let outcome = cert::Entity::delete_by_id(id).exec(self.db).await?;
        Ok(outcome.rows_affected > 0)
    }

    /// 按条件列出证书：域名模糊检索 + 按到期时间排序 + 分页。
    ///
    /// 页码超出总页数时返回**空列表**而不是报错——翻过末页是正常操作。
    pub async fn list(&self, query: CertQuery) -> Result<CertPage> {
        self.validate_query(&query)?;

        let mut find = cert::Entity::find();

        let keyword = query
            .domain
            .as_deref()
            .map(|raw| raw.trim().to_lowercase())
            .unwrap_or_default();
        if !keyword.is_empty() {
            // 域名集合列存的就是小写，因此只需把关键字也小写化即可做到大小写不敏感。
            let pattern = format!("%{}%", escape_like(&keyword));
            find = find.filter(cert::Column::Domains.like(LikeExpr::new(pattern).escape('\\')));
        }

        // 次级排序键 id 不能省：到期时间相同的记录若没有稳定的先后，
        // 翻页时可能重复或漏掉同一条。补上 id 之后排序才是全序。
        find = match query.sort {
            CertSort::Ascending => find.order_by_asc(cert::Column::NotAfter),
            CertSort::Descending => find.order_by_desc(cert::Column::NotAfter),
        };
        find = find.order_by_asc(cert::Column::Id);

        let paginator = find.paginate(self.db, query.page_size);
        let numbers = paginator.num_items_and_pages().await?;
        let items = paginator.fetch_page(query.page - 1).await?;

        Ok(CertPage {
            items,
            total: numbers.number_of_items,
            page: query.page,
            page_size: query.page_size,
        })
    }

    /// 取全部「已进入续期窗口」的证书，按到期时间升序（9.3 到期扫描的数据源）。
    ///
    /// 「已进入窗口」即 `not_after <= upper_bound`（由调度引擎按
    /// `now + 阈值` 算出）；已吊销的证书不再需要续期，直接排除。
    /// 到期越早越紧迫，升序让扫描结果天然按紧迫度排列。
    pub async fn list_due(&self, upper_bound: DateTime<Utc>) -> Result<Vec<cert::Model>> {
        let rows = cert::Entity::find()
            .filter(cert::Column::NotAfter.lte(upper_bound))
            .filter(cert::Column::RevokedAt.is_null())
            .order_by_asc(cert::Column::NotAfter)
            .all(self.db)
            .await?;

        Ok(rows)
    }

    /// 取某条证书记录签发时所用的 ACME 账号凭据。
    ///
    /// 这是吊销流程取账号的**唯一**入口：直接读证书行上的外键去关联凭据行，
    /// 而不是解析流水线配置反推账号——后者是 certd 的旧做法（见 design 决策 6），
    /// 一旦流水线被改动或删除，历史证书就再也吊销不了。
    ///
    /// 证书不存在、或证书没有绑定账号（例如用户手动上传的证书）时**返回错误而非 `None`**：
    /// 在吊销语义下「取不到签发账号」是失败，不是一个可以顺手忽略的空值。
    pub async fn account_for_cert(&self, cert_id: i64) -> Result<credential::Model> {
        let certificate = self
            .find(cert_id)
            .await?
            .ok_or_else(|| Error::not_found("证书", cert_id.to_string()))?;

        let account_id = certificate.acme_account_access_id.ok_or_else(|| {
            Error::missing_field(format!(
                "证书 {cert_id} 未绑定 ACME 账号，无法确定签发账号（手动上传的证书需重新签发后才能吊销）"
            ))
        })?;

        credential::Entity::find_by_id(account_id)
            .one(self.db)
            .await?
            // 外键为 `SetNull`，正常情况下账号被删时证书行的外键会被置空，
            // 走到这里说明数据被外部改过。仍然显式报错，不做 unwrap。
            .ok_or_else(|| Error::not_found("ACME 账号凭据", account_id.to_string()))
    }

    /// 把证书记录标记为已吊销。
    ///
    /// 只负责本地状态；CA 侧的吊销由调用方先完成。重复调用无害：
    /// 吊销时间以最后一次写入为准，而「是否已吊销」的判定只看非空。
    pub async fn mark_revoked(&self, cert_id: i64, revoked_at: DateTime<Utc>) -> Result<()> {
        let row = cert::Entity::find_by_id(cert_id)
            .one(self.db)
            .await?
            .ok_or_else(|| Error::not_found("证书", cert_id.to_string()))?;

        cert::ActiveModel {
            id: Set(row.id),
            revoked_at: Set(Some(revoked_at)),
            updated_at: Set(Utc::now()),
            ..Default::default()
        }
        .update(self.db)
        .await?;

        Ok(())
    }

    /// 更新证书材料的存放路径。
    ///
    /// 吊销归档用：文件移进吊销目录后，库中的相对路径必须跟着走，
    /// 否则详情与下载会指向一个已经空了的目录。
    pub async fn update_paths(&self, cert_id: i64, paths: &CertFilePaths) -> Result<()> {
        let row = cert::Entity::find_by_id(cert_id)
            .one(self.db)
            .await?
            .ok_or_else(|| Error::not_found("证书", cert_id.to_string()))?;

        cert::ActiveModel {
            id: Set(row.id),
            cert_pem_path: Set(paths.cert_pem.clone()),
            key_pem_path: Set(paths.key_pem.clone()),
            updated_at: Set(Utc::now()),
            ..Default::default()
        }
        .update(self.db)
        .await?;

        Ok(())
    }

    /// 校验分页参数。
    ///
    /// 越界一律报错而不静默纠正：把 `page: 0` 悄悄当成 1，会让调用方的 off-by-one
    /// 一直藏着，直到某天翻页少了一页才被发现。
    fn validate_query(&self, query: &CertQuery) -> Result<()> {
        if query.page < 1 {
            return Err(Error::Validation("页码从 1 开始".to_owned()));
        }
        if query.page_size < 1 || query.page_size > MAX_PAGE_SIZE {
            return Err(Error::Validation(format!(
                "每页条数应在 1..={MAX_PAGE_SIZE} 之间，实际为 {}",
                query.page_size
            )));
        }
        Ok(())
    }

    /// 规范化域名集合并校验非空。
    fn encode_domains<S: AsRef<str>>(&self, domains: &[S]) -> Result<String> {
        let encoded = cert::encode_domains(domains);
        if encoded.is_empty() {
            return Err(Error::Validation("证书至少要覆盖一个域名".to_owned()));
        }
        Ok(encoded)
    }

    /// 按已规范化的去重键查询。
    async fn find_by_key(&self, encoded: &str) -> Result<Option<cert::Model>> {
        Ok(cert::Entity::find()
            .filter(cert::Column::Domains.eq(encoded))
            .one(self.db)
            .await?)
    }

    /// 插入新记录，`created_at` 与 `updated_at` 取同一时刻。
    async fn insert(&self, input: CertInput) -> Result<cert::Model> {
        let encoded = self.encode_domains(&input.domains)?;
        let now = Utc::now();

        cert::ActiveModel {
            domains: Set(encoded),
            cert_pem_path: Set(input.cert_pem_path),
            key_pem_path: Set(input.key_pem_path),
            fingerprint: Set(input.fingerprint),
            issuer: Set(input.issuer),
            not_before: Set(input.not_before),
            not_after: Set(input.not_after),
            acme_account_access_id: Set(input.acme_account_access_id),
            revoked_at: Set(None),
            created_at: Set(now),
            updated_at: Set(now),
            ..Default::default()
        }
        .insert(self.db)
        .await
        .map_err(Into::into)
    }

    /// 用新签发的证书覆盖既有记录。
    ///
    /// `domains` 与 `created_at` 是这条记录的身份，不随续期变化，因此不在此更新。
    async fn overwrite(&self, id: i64, input: CertInput) -> Result<cert::Model> {
        cert::ActiveModel {
            id: Set(id),
            cert_pem_path: Set(input.cert_pem_path),
            key_pem_path: Set(input.key_pem_path),
            fingerprint: Set(input.fingerprint),
            issuer: Set(input.issuer),
            not_before: Set(input.not_before),
            not_after: Set(input.not_after),
            acme_account_access_id: Set(input.acme_account_access_id),
            // 新证书天然未被吊销。若旧记录处于已吊销状态而这里不清空，
            // 续期后的证书会被永久当成已吊销，从此不再触发续期。
            revoked_at: Set(None),
            updated_at: Set(Utc::now()),
            ..Default::default()
        }
        .update(self.db)
        .await
        .map_err(Into::into)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn like_wildcards_are_escaped() {
        assert_eq!(escape_like("example.com"), "example.com");
        assert_eq!(escape_like("100%"), "100\\%");
        assert_eq!(escape_like("a_b"), "a\\_b");
        // 反斜杠自身也要转义，否则它会吞掉后一个字符。
        assert_eq!(escape_like(r"c:\temp"), r"c:\\temp");
        assert_eq!(escape_like(r"%_\"), r"\%\_\\");
    }

    #[test]
    fn default_query_is_the_first_page_soonest_first() {
        let query = CertQuery::default();
        assert_eq!(query.domain, None);
        assert_eq!(query.sort, CertSort::Ascending);
        assert_eq!(query.page, 1);
        assert_eq!(query.page_size, DEFAULT_PAGE_SIZE);
    }

    #[test]
    fn total_pages_rounds_up_and_is_zero_when_empty() {
        let page_of = |total| CertPage {
            items: Vec::new(),
            total,
            page: 1,
            page_size: 10,
        };

        assert_eq!(page_of(0).total_pages(), 0, "空结果不应算作一页");
        assert_eq!(page_of(1).total_pages(), 1);
        assert_eq!(page_of(10).total_pages(), 1);
        assert_eq!(page_of(11).total_pages(), 2, "有余数时应向上取整");
    }
}
