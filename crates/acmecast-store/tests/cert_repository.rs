//! 4.6 证书仓库 CRUD 与按域名集合去重；4.7 检索排序分页；4.8 与签发账号的绑定。
//!
//! 这里跑**真实迁移**（而非 `Schema::create_table_from_entity`），因此索引与外键
//! 也一并受检；数据库用 SQLite 内存库，无需外部依赖。
//!
//! 两套写入语义是刻意的，用例分别钉住：
//! - `save` 按**域名集合**去重，供签发流程使用（重复签发 = 更新既有记录）
//! - `create` 只按**指纹**判重，供手动上传使用（允许同名不同源的证书共存）

use acmecast_store::entity::{cert, credential, pipeline};
use acmecast_store::repository::{
    CertInput, CertQuery, CertRepository, CertSort, DEFAULT_PAGE_SIZE, MAX_PAGE_SIZE,
};
use acmecast_store::{CertPage, Error, Migrator};
use chrono::{Duration, Utc};
use sea_orm::{ActiveModelTrait, Database, DatabaseConnection, EntityTrait, PaginatorTrait, Set};
use sea_orm_migration::MigratorTrait;

/// 插入一份 ACME 账号凭据，返回其主键。
async fn add_account(db: &DatabaseConnection, name: &str) -> i64 {
    let now = Utc::now();
    credential::ActiveModel {
        name: Set(name.to_owned()),
        type_id: Set("acme.account".to_owned()),
        encrypted_fields: Set("{}".to_owned()),
        created_at: Set(now),
        updated_at: Set(now),
        ..Default::default()
    }
    .insert(db)
    .await
    .expect("应能插入账号凭据")
    .id
}

/// 建一个跑完全部迁移的内存库，并插入一个可供证书引用的 ACME 账号凭据。
///
/// 返回账号主键，供构造 [`CertInput`] 使用——证书表的 `acme_account_access_id`
/// 是外键，指向不存在的账号会被约束拒绝。
async fn setup() -> (DatabaseConnection, i64) {
    let db = Database::connect("sqlite::memory:")
        .await
        .expect("应能连上内存库");
    Migrator::up(&db, None).await.expect("迁移应成功");

    let account_id = add_account(&db, "测试 ACME 账号").await;
    (db, account_id)
}

/// 构造一条证书输入；除域名集合、指纹与到期天数外均取固定值。
fn input(domains: &[&str], fingerprint: &str, valid_days: i64, account_id: i64) -> CertInput {
    let now = Utc::now();
    CertInput {
        domains: domains.iter().map(|domain| (*domain).to_owned()).collect(),
        cert_pem_path: format!("certs/{fingerprint}/cert.pem"),
        key_pem_path: format!("certs/{fingerprint}/key.pem"),
        fingerprint: fingerprint.to_owned(),
        issuer: Some("CN=Test CA".to_owned()),
        not_before: now,
        not_after: now + Duration::days(valid_days),
        acme_account_access_id: Some(account_id),
    }
}

/// 仓库当前的证书条数。
async fn count_certs(db: &DatabaseConnection) -> u64 {
    cert::Entity::find().count(db).await.expect("应能统计条数")
}

// ---- 基础 CRUD ----

#[tokio::test]
async fn create_then_find_by_id_domain_set_and_fingerprint() {
    let (db, account) = setup().await;
    let repo = CertRepository::new(&db);

    let created = repo
        .create(input(&["example.com"], "fp-1", 90, account))
        .await
        .expect("应能插入证书");

    assert_eq!(created.domains, "example.com");
    assert!(created.revoked_at.is_none(), "新入库的证书不应是已吊销状态");

    assert_eq!(repo.find(created.id).await.unwrap().unwrap().id, created.id);
    assert_eq!(
        repo.find_by_domains(&["example.com"])
            .await
            .unwrap()
            .unwrap()
            .id,
        created.id
    );
    assert_eq!(
        repo.find_by_fingerprint("fp-1").await.unwrap().unwrap().id,
        created.id
    );
}

#[tokio::test]
async fn lookups_miss_cleanly() {
    let (db, _account) = setup().await;
    let repo = CertRepository::new(&db);

    assert!(repo.find(4242).await.unwrap().is_none());
    assert!(
        repo.find_by_domains(&["nobody.example.com"])
            .await
            .unwrap()
            .is_none()
    );
    assert!(repo.find_by_domains::<&str>(&[]).await.unwrap().is_none());
    assert!(repo.find_by_fingerprint("nope").await.unwrap().is_none());
}

#[tokio::test]
async fn delete_removes_the_record_and_reports_whether_it_existed() {
    let (db, account) = setup().await;
    let repo = CertRepository::new(&db);

    let created = repo
        .save(input(&["example.com"], "fp-1", 90, account))
        .await
        .unwrap()
        .model();

    assert!(
        repo.delete(created.id).await.unwrap(),
        "首次删除应报告删掉了记录"
    );
    assert!(
        repo.find(created.id).await.unwrap().is_none(),
        "删除后不应再查到"
    );
    assert!(
        !repo.delete(created.id).await.unwrap(),
        "重复删除应报告没有记录可删"
    );
}

// ---- 需求：同一域名集合重复签发时更新既有记录而非新增 ----

#[tokio::test]
async fn reissuing_the_same_domain_set_updates_in_place() {
    let (db, account) = setup().await;
    let repo = CertRepository::new(&db);

    let first = repo
        .save(input(&["example.com"], "fp-old", 90, account))
        .await
        .unwrap();
    assert!(first.is_created(), "首次写入应是新建");
    let first = first.model();

    // 再次签发：证书内容、指纹与到期时间都变了。
    let second = repo
        .save(input(&["example.com"], "fp-new", 180, account))
        .await
        .unwrap();
    assert!(!second.is_created(), "同一域名集合的第二次写入应是更新");
    let second = second.model();

    assert_eq!(second.id, first.id, "应更新既有记录，主键不变");
    assert_eq!(second.fingerprint, "fp-new", "证书内容应被更新");
    assert_eq!(
        second.cert_pem_path, "certs/fp-new/cert.pem",
        "路径应指向新证书"
    );
    assert!(
        second.not_after > first.not_after,
        "到期时间应被更新：{} 应晚于 {}",
        second.not_after,
        first.not_after
    );
    assert_eq!(
        second.created_at, first.created_at,
        "created_at 是记录身份，不随续期变化"
    );

    // 核心断言：记录总数不增加。
    assert_eq!(count_certs(&db).await, 1, "重复签发不应新增记录");
}

#[tokio::test]
async fn update_refreshes_updated_at() {
    let (db, account) = setup().await;
    let repo = CertRepository::new(&db);

    let first = repo
        .save(input(&["example.com"], "fp-old", 90, account))
        .await
        .unwrap()
        .model();

    tokio::time::sleep(std::time::Duration::from_millis(20)).await;

    let second = repo
        .save(input(&["example.com"], "fp-new", 90, account))
        .await
        .unwrap()
        .model();

    assert!(
        second.updated_at > first.updated_at,
        "更新时间应被刷新：{} 应晚于 {}",
        second.updated_at,
        first.updated_at
    );
}

#[tokio::test]
async fn reissuing_clears_a_previous_revocation() {
    // 已吊销的记录若在续期后仍带着 revoked_at，会被永久当作已吊销，从此不再触发续期。
    let (db, account) = setup().await;
    let repo = CertRepository::new(&db);

    let first = repo
        .save(input(&["example.com"], "fp-old", 90, account))
        .await
        .unwrap()
        .model();

    // 人为把记录标成已吊销（吊销流程属于另一个任务）。
    cert::ActiveModel {
        id: Set(first.id),
        revoked_at: Set(Some(Utc::now())),
        ..Default::default()
    }
    .update(&db)
    .await
    .expect("应能标记吊销");

    let renewed = repo
        .save(input(&["example.com"], "fp-new", 90, account))
        .await
        .unwrap()
        .model();

    assert_eq!(renewed.id, first.id);
    assert!(
        renewed.revoked_at.is_none(),
        "续期后的记录不应保持已吊销状态"
    );
    assert!(renewed.is_revocable(), "续期后的新证书应可再次吊销");
}

// ---- 需求：以域名集合为去重依据 ----

#[tokio::test]
async fn domain_set_matching_ignores_order_case_and_whitespace() {
    let (db, account) = setup().await;
    let repo = CertRepository::new(&db);

    repo.save(input(
        &["b.example.com", "a.example.com"],
        "fp-1",
        90,
        account,
    ))
    .await
    .unwrap();

    // 换序、换大小写、带空白、含重复项：都应命中同一条记录。
    let again = repo
        .save(input(
            &["A.EXAMPLE.com", " b.example.com ", "a.example.com"],
            "fp-2",
            90,
            account,
        ))
        .await
        .unwrap();

    assert!(!again.is_created(), "同一集合的不同写法应视为同一记录");
    assert_eq!(again.model().domains, "a.example.com,b.example.com");
    assert_eq!(count_certs(&db).await, 1);
}

#[tokio::test]
async fn wildcard_and_base_domain_stay_distinct_entries_of_one_set() {
    let (db, account) = setup().await;
    let repo = CertRepository::new(&db);

    repo.save(input(
        &["example.com", "*.example.com"],
        "fp-1",
        90,
        account,
    ))
    .await
    .unwrap();
    let again = repo
        .save(input(
            &["*.example.com", "example.com"],
            "fp-2",
            90,
            account,
        ))
        .await
        .unwrap();

    assert!(!again.is_created());
    assert_eq!(count_certs(&db).await, 1);

    // 通配符与裸域名是两个 SAN，不能被合并成同一个。
    let model = again.model();
    assert_eq!(model.domain_set().len(), 2);
    assert!(model.domain_set().contains(&"*.example.com".to_owned()));
}

#[tokio::test]
async fn different_domain_sets_are_separate_records() {
    let (db, account) = setup().await;
    let repo = CertRepository::new(&db);

    repo.save(input(&["example.com"], "fp-1", 90, account))
        .await
        .unwrap();
    let other = repo
        .save(input(&["other.example.com"], "fp-2", 90, account))
        .await
        .unwrap();

    assert!(other.is_created());
    assert_eq!(count_certs(&db).await, 2, "不同域名集合应是两条记录");
}

#[tokio::test]
async fn save_rejects_an_empty_domain_set() {
    let (db, account) = setup().await;
    let repo = CertRepository::new(&db);

    for domains in [vec![], vec!["".to_owned()], vec!["   ".to_owned()]] {
        let mut candidate = input(&["example.com"], "fp", 90, account);
        candidate.domains = domains.clone();

        let err = repo.save(candidate).await.expect_err("空域名集合应被拒绝");
        assert!(matches!(err, Error::Validation(_)), "{err:?}");
    }

    assert_eq!(count_certs(&db).await, 0, "被拒绝的写入不应留下记录");
}

#[tokio::test]
async fn saving_the_identical_certificate_twice_is_idempotent() {
    // 流水线重试时同一份证书可能被重复写入：应原地更新，而不是报指纹冲突。
    let (db, account) = setup().await;
    let repo = CertRepository::new(&db);

    let first = repo
        .save(input(&["example.com"], "fp-1", 90, account))
        .await
        .unwrap()
        .model();
    let second = repo
        .save(input(&["example.com"], "fp-1", 90, account))
        .await
        .unwrap();

    assert!(!second.is_created(), "同域名集合的重复写入仍应是更新");
    assert_eq!(second.model().id, first.id);
    assert_eq!(count_certs(&db).await, 1);
}

#[tokio::test]
async fn account_reference_is_enforced_by_the_foreign_key() {
    // SQLite 默认**不**启用外键约束，要靠连接层显式打开。这条用例确认它确实生效——
    // 否则「证书绑定到某个账号」在库里只是个普通整数，写错也没有东西会拦。
    let (db, _account) = setup().await;
    let repo = CertRepository::new(&db);

    let result = repo.create(input(&["example.com"], "fp-1", 90, 9999)).await;

    assert!(result.is_err(), "引用不存在的账号应被外键约束拒绝");
    assert_eq!(count_certs(&db).await, 0);
}

// ---- create 与 save 的语义差异 ----

#[tokio::test]
async fn create_rejects_a_fingerprint_that_already_exists() {
    let (db, account) = setup().await;
    let repo = CertRepository::new(&db);

    repo.create(input(&["example.com"], "same-fp", 90, account))
        .await
        .unwrap();

    let err = repo
        .create(input(&["another.example.com"], "same-fp", 90, account))
        .await
        .expect_err("同一张证书不应入库两次");
    assert!(matches!(err, Error::Conflict(_)), "{err:?}");
    assert_eq!(count_certs(&db).await, 1);
}

#[tokio::test]
async fn create_does_not_deduplicate_by_domain_set() {
    // 手动上传路径：用户可能有意同时保留两张覆盖相同域名、但来源不同的证书
    // （例如不同 CA 签发的）。按域名集合去重只适用于签发流程。
    let (db, account) = setup().await;
    let repo = CertRepository::new(&db);

    repo.create(input(&["example.com"], "fp-a", 90, account))
        .await
        .unwrap();
    repo.create(input(&["example.com"], "fp-b", 90, account))
        .await
        .unwrap();

    assert_eq!(
        count_certs(&db).await,
        2,
        "create 应按指纹判重，而非按域名集合"
    );
}

// ---- 需求：按域名模糊检索、按到期时间排序、分页 ----

/// 写入 `count` 条证书，域名依次为 `host0.example.com`…，到期时间依次递增。
async fn seed_expiring_in_order(db: &DatabaseConnection, account_id: i64, count: usize) {
    let repo = CertRepository::new(db);
    for index in 0..count {
        repo.save(input(
            &[&format!("host{index}.example.com")],
            &format!("fp-{index}"),
            10 * (index as i64 + 1),
            account_id,
        ))
        .await
        .expect("应能写入种子数据");
    }
}

/// 把一页记录化简为其唯一域名，便于断言排序结果。
fn page_domains(page: &CertPage) -> Vec<String> {
    page.items
        .iter()
        .map(|model| model.domain_set().into_iter().next().unwrap_or_default())
        .collect()
}

/// 断言一页内的到期时间单调不减。
fn assert_monotonic(page: &CertPage) {
    let times: Vec<_> = page.items.iter().map(|model| model.not_after).collect();
    assert!(
        times.windows(2).all(|pair| pair[0] <= pair[1]),
        "到期时间应单调不减，实际为 {times:?}"
    );
}

#[tokio::test]
async fn listing_sorts_by_expiry_in_both_directions() {
    let (db, account) = setup().await;
    // 刻意乱序写入：排序结果应与写入顺序无关。
    let repo = CertRepository::new(&db);
    for (domain, valid_days) in [
        ("mid.example.com", 60),
        ("late.example.com", 120),
        ("soon.example.com", 10),
    ] {
        repo.save(input(&[domain], domain, valid_days, account))
            .await
            .unwrap();
    }

    let ascending = repo
        .list(CertQuery {
            sort: CertSort::Ascending,
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(
        page_domains(&ascending),
        vec!["soon.example.com", "mid.example.com", "late.example.com"],
        "升序应从最快到期的开始"
    );
    // 直接校验时间单调，而不只依赖域名顺序的间接证据。
    assert_monotonic(&ascending);

    let descending = repo
        .list(CertQuery {
            sort: CertSort::Descending,
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(
        page_domains(&descending),
        vec!["late.example.com", "mid.example.com", "soon.example.com"],
        "降序应是升序的逆序"
    );
    let times: Vec<_> = descending
        .items
        .iter()
        .map(|model| model.not_after)
        .collect();
    assert!(
        times.windows(2).all(|pair| pair[0] >= pair[1]),
        "降序应单调不增，实际为 {times:?}"
    );
}

#[tokio::test]
async fn default_query_lists_soonest_expiring_first() {
    let (db, account) = setup().await;
    seed_expiring_in_order(&db, account, 3).await;
    let repo = CertRepository::new(&db);

    let page = repo.list(CertQuery::default()).await.unwrap();
    assert_eq!(page.page, 1);
    assert_eq!(page.page_size, DEFAULT_PAGE_SIZE);
    assert_eq!(
        page_domains(&page),
        vec![
            "host0.example.com",
            "host1.example.com",
            "host2.example.com"
        ],
        "默认应按到期时间升序"
    );
}

#[tokio::test]
async fn paging_covers_every_record_exactly_once() {
    let (db, account) = setup().await;
    // 7 条记录、每页 3 条 → 3 页，其中最后一页只有 1 条。
    seed_expiring_in_order(&db, account, 7).await;
    let repo = CertRepository::new(&db);

    let mut ids = Vec::new();
    let mut times = Vec::new();

    for page_number in 1..=3u64 {
        let page = repo
            .list(CertQuery {
                page: page_number,
                page_size: 3,
                ..Default::default()
            })
            .await
            .unwrap();

        assert_eq!(page.total, 7, "总数应与页码无关");
        assert_eq!(page.total_pages(), 3);
        // 7 条按每页 3 条切分：前两页各 3 条，末页剩 1 条。
        let expected_len = if page_number == 3 { 1 } else { 3 };
        assert_eq!(
            page.items.len(),
            expected_len,
            "第 {page_number} 页条数不对"
        );

        ids.extend(page.items.iter().map(|model| model.id));
        times.extend(page.items.iter().map(|model| model.not_after));
        assert_monotonic(&page);
    }

    assert_eq!(ids.len(), 7, "三页合计应恰好覆盖全部记录");
    let mut unique = ids.clone();
    unique.sort_unstable();
    unique.dedup();
    assert_eq!(unique.len(), 7, "各页之间不应出现重复记录");

    // 更强的证据：逐页拼接后整体仍单调，说明分页切分点没有打乱全序。
    assert!(
        times.windows(2).all(|pair| pair[0] <= pair[1]),
        "跨页拼接后排序应仍然单调"
    );
}

#[tokio::test]
async fn paging_past_the_last_page_returns_empty() {
    let (db, account) = setup().await;
    seed_expiring_in_order(&db, account, 2).await;
    let repo = CertRepository::new(&db);

    let page = repo
        .list(CertQuery {
            page: 99,
            page_size: 3,
            ..Default::default()
        })
        .await
        .unwrap();

    assert!(page.items.is_empty(), "越界页应返回空列表而不是报错");
    assert_eq!(page.total, 2, "越界页的总数仍应正确");
}

#[tokio::test]
async fn paging_is_stable_when_expiry_times_tie() {
    // 同一批签发的证书到期时间完全相同。缺少次级排序键（id）时，
    // 数据库对等值行的返回顺序没有保证，翻页可能出现重复或遗漏。
    let (db, account) = setup().await;
    let repo = CertRepository::new(&db);
    let tied_expiry = Utc::now() + Duration::days(30);

    for index in 0..6 {
        let mut candidate = input(
            &[&format!("tie{index}.example.com")],
            &format!("fp-{index}"),
            30,
            account,
        );
        candidate.not_after = tied_expiry;
        repo.save(candidate).await.unwrap();
    }

    let mut ids = Vec::new();
    for page_number in 1..=3u64 {
        let page = repo
            .list(CertQuery {
                page: page_number,
                page_size: 2,
                ..Default::default()
            })
            .await
            .unwrap();
        ids.extend(page.items.iter().map(|model| model.id));
    }

    let mut unique = ids.clone();
    unique.sort_unstable();
    unique.dedup();
    assert_eq!(ids.len(), 6, "三页应取回 6 条");
    assert_eq!(unique.len(), 6, "到期时间相同时翻页也不应重复或遗漏");
}

#[tokio::test]
async fn domain_search_matches_substrings_case_insensitively() {
    let (db, account) = setup().await;
    let repo = CertRepository::new(&db);

    repo.save(input(&["example.com"], "fp-1", 90, account))
        .await
        .unwrap();
    repo.save(input(
        &["example.com", "*.example.com"],
        "fp-2",
        90,
        account,
    ))
    .await
    .unwrap();
    repo.save(input(&["other.org"], "fp-3", 90, account))
        .await
        .unwrap();

    let hits = repo
        .list(CertQuery {
            domain: Some("example.com".to_owned()),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(hits.total, 2, "两条含 example.com 的记录都应命中");

    let uppercase = repo
        .list(CertQuery {
            domain: Some("EXAMPLE.COM".to_owned()),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(uppercase.total, 2, "检索应大小写不敏感");

    let partial = repo
        .list(CertQuery {
            domain: Some("other".to_owned()),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(partial.total, 1, "应支持只输入域名的一部分");
    assert_eq!(page_domains(&partial), vec!["other.org"]);

    let miss = repo
        .list(CertQuery {
            domain: Some("nobody".to_owned()),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(miss.total, 0);
    assert!(miss.items.is_empty());

    // 空白关键字等同于不筛选，而不是去匹配空串。
    let blank = repo
        .list(CertQuery {
            domain: Some("   ".to_owned()),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(blank.total, 3, "空白关键字应被忽略");
}

#[tokio::test]
async fn domain_search_treats_like_wildcards_as_literals() {
    // `%` 与 `_` 是 SQL LIKE 的通配符。若不转义，搜 `%` 会命中全部记录，
    // 模糊检索就成了意料之外的全表通配。
    let (db, account) = setup().await;
    seed_expiring_in_order(&db, account, 3).await;
    let repo = CertRepository::new(&db);

    for keyword in ["%", "_", "%%", r"\_%"] {
        let page = repo
            .list(CertQuery {
                domain: Some(keyword.to_owned()),
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(
            page.total, 0,
            "通配符 {keyword:?} 应按字面量处理，不应命中任何记录"
        );
    }
}

#[tokio::test]
async fn search_and_paging_compose() {
    let (db, account) = setup().await;
    let repo = CertRepository::new(&db);

    for index in 0..6 {
        repo.save(input(
            &[&format!("web{index}.example.com")],
            &format!("fp-web-{index}"),
            10 * (index as i64 + 1),
            account,
        ))
        .await
        .unwrap();
    }
    for index in 0..3 {
        repo.save(input(
            &[&format!("mail{index}.other.org")],
            &format!("fp-mail-{index}"),
            10 * (index as i64 + 1),
            account,
        ))
        .await
        .unwrap();
    }

    let filter = Some("example.com".to_owned());
    let first = repo
        .list(CertQuery {
            domain: filter.clone(),
            page: 1,
            page_size: 2,
            ..Default::default()
        })
        .await
        .unwrap();

    // 总数必须是**筛选后**的数量，而不是全表数量。
    assert_eq!(first.total, 6, "总数应只统计命中筛选的记录");
    assert_eq!(first.total_pages(), 3);

    let mut all = page_domains(&first);
    for page_number in 2..=3u64 {
        let page = repo
            .list(CertQuery {
                domain: filter.clone(),
                page: page_number,
                page_size: 2,
                ..Default::default()
            })
            .await
            .unwrap();
        all.extend(page_domains(&page));
    }

    assert_eq!(
        all,
        vec![
            "web0.example.com",
            "web1.example.com",
            "web2.example.com",
            "web3.example.com",
            "web4.example.com",
            "web5.example.com",
        ],
        "筛选后逐页取回应覆盖全部命中记录且保持排序"
    );
}

#[tokio::test]
async fn invalid_paging_parameters_are_rejected() {
    let (db, _account) = setup().await;
    let repo = CertRepository::new(&db);

    for query in [
        CertQuery {
            page: 0,
            ..Default::default()
        },
        CertQuery {
            page_size: 0,
            ..Default::default()
        },
        CertQuery {
            page_size: MAX_PAGE_SIZE + 1,
            ..Default::default()
        },
    ] {
        let err = repo.list(query).await.expect_err("非法分页参数应被拒绝");
        assert!(matches!(err, Error::Validation(_)), "{err:?}");
    }

    // 上限本身应当可用。
    repo.list(CertQuery {
        page_size: MAX_PAGE_SIZE,
        ..Default::default()
    })
    .await
    .expect("每页条数取上限时应可用");
}

// ---- 4.8 需求：证书记录与签发账号绑定 ----

#[tokio::test]
async fn saving_binds_the_issuing_account() {
    let (db, account) = setup().await;
    let repo = CertRepository::new(&db);

    let saved = repo
        .save(input(&["example.com"], "fp-1", 90, account))
        .await
        .unwrap()
        .model();

    // spec：入库时记录本次签发所用的 ACME 账号凭据标识。
    assert_eq!(
        saved.acme_account_access_id,
        Some(account),
        "入库后账号字段必须有值"
    );
    assert!(saved.is_revocable(), "绑定了账号的证书应可吊销");

    // 并且能顺着这个标识取回账号本身。
    let resolved = repo.account_for_cert(saved.id).await.unwrap();
    assert_eq!(resolved.id, account);
    assert_eq!(resolved.name, "测试 ACME 账号");
}

#[tokio::test]
async fn renewal_rebinds_the_account_used_for_issuance() {
    // 换一个账号重新签发时绑定必须跟着更新——否则吊销会拿错账号的密钥去签名。
    let (db, first_account) = setup().await;
    let second_account = add_account(&db, "另一个 ACME 账号").await;
    let repo = CertRepository::new(&db);

    let first = repo
        .save(input(&["example.com"], "fp-old", 90, first_account))
        .await
        .unwrap()
        .model();
    let renewed = repo
        .save(input(&["example.com"], "fp-new", 90, second_account))
        .await
        .unwrap()
        .model();

    assert_eq!(renewed.id, first.id, "续期仍是同一条记录");
    assert_eq!(renewed.acme_account_access_id, Some(second_account));
    assert_eq!(
        repo.account_for_cert(renewed.id).await.unwrap().name,
        "另一个 ACME 账号"
    );
}

#[tokio::test]
async fn resolving_the_account_never_touches_pipeline_data() {
    // spec：吊销时直接读证书行上的账号标识，「不查询任何流水线数据」。
    // 库里连一条流水线都没有，取账号依然成功——这就是该要求的直接证据。
    let (db, account) = setup().await;
    let repo = CertRepository::new(&db);

    let saved = repo
        .save(input(&["example.com"], "fp-1", 90, account))
        .await
        .unwrap()
        .model();

    let pipeline_count = pipeline::Entity::find().count(&db).await.unwrap();
    assert_eq!(pipeline_count, 0, "本用例的前提是库里没有任何流水线");

    assert_eq!(repo.account_for_cert(saved.id).await.unwrap().id, account);
}

#[tokio::test]
async fn account_lookup_reports_a_missing_certificate() {
    let (db, _account) = setup().await;
    let repo = CertRepository::new(&db);

    let err = repo
        .account_for_cert(4242)
        .await
        .expect_err("证书不存在时应报错");

    match &err {
        Error::NotFound { entity, id } => {
            assert_eq!(entity, "证书");
            assert_eq!(id, "4242");
        }
        other => panic!("期望 NotFound，实际 {other:?}"),
    }
}

#[tokio::test]
async fn an_uploaded_certificate_has_no_account_to_resolve() {
    // spec 场景：对账号标识为空的记录发起吊销时，应返回**明确错误**而不是静默跳过。
    let (db, account) = setup().await;
    let repo = CertRepository::new(&db);

    let mut uploaded = input(&["uploaded.example.com"], "fp-uploaded", 365, account);
    uploaded.acme_account_access_id = None;
    let saved = repo.create(uploaded).await.unwrap();

    assert_eq!(saved.acme_account_access_id, None);
    assert!(!saved.is_revocable(), "没有账号的证书不可吊销");

    let err = repo
        .account_for_cert(saved.id)
        .await
        .expect_err("没有账号时应报错");
    match &err {
        Error::MissingField(reason) => assert!(
            reason.contains("未绑定 ACME 账号"),
            "错误信息应说明原因: {reason}"
        ),
        other => panic!("期望 MissingField，实际 {other:?}"),
    }
}

#[tokio::test]
async fn deleting_the_account_leaves_the_certificate_unresolvable() {
    // 凭据被删除时，外键的 `SetNull` 应把证书行上的引用一并清掉，
    // 让这条证书立刻表现为「不可吊销」，而不是留下一个指向空处的悬垂标识。
    let (db, account) = setup().await;
    let repo = CertRepository::new(&db);

    let saved = repo
        .save(input(&["example.com"], "fp-1", 90, account))
        .await
        .unwrap()
        .model();

    credential::Entity::delete_by_id(account)
        .exec(&db)
        .await
        .expect("应能删除账号");

    let reloaded = repo.find(saved.id).await.unwrap().unwrap();
    assert!(
        reloaded.acme_account_access_id.is_none(),
        "删除账号后证书行上的引用应被置空"
    );

    let err = repo
        .account_for_cert(saved.id)
        .await
        .expect_err("账号已删除，应取不到");
    assert!(matches!(&err, Error::MissingField(_)), "{err:?}");
}

// ---- 吊销归档的路径更新 ----

#[tokio::test]
async fn updating_paths_moves_the_recorded_material_location() {
    // 吊销归档把文件移进吊销目录后，库中的相对路径必须跟着走，
    // 否则详情与下载会指向一个已经空了的目录。
    use acmecast_store::CertFilePaths;

    let (db, account) = setup().await;
    let repo = CertRepository::new(&db);

    let saved = repo
        .save(input(&["example.com"], "fp-1", 90, account))
        .await
        .unwrap()
        .model();

    repo.update_paths(
        saved.id,
        &CertFilePaths {
            cert_pem: "certs/revoked/fp-1/cert.pem".to_owned(),
            key_pem: "certs/revoked/fp-1/key.pem".to_owned(),
        },
    )
    .await
    .expect("应能更新路径");

    let reloaded = repo.find(saved.id).await.unwrap().unwrap();
    assert_eq!(reloaded.cert_pem_path, "certs/revoked/fp-1/cert.pem");
    assert_eq!(reloaded.key_pem_path, "certs/revoked/fp-1/key.pem");
    assert!(
        reloaded.updated_at > saved.updated_at,
        "路径更新应刷新 updated_at"
    );
    // 其余字段不应被这次更新波及。
    assert_eq!(reloaded.fingerprint, "fp-1");
    assert!(reloaded.revoked_at.is_none(), "路径更新不应改动吊销状态");
}

#[tokio::test]
async fn updating_paths_of_a_missing_certificate_reports_not_found() {
    let (db, _account) = setup().await;
    let repo = CertRepository::new(&db);
    let paths = acmecast_store::CertFilePaths {
        cert_pem: "certs/revoked/fp-x/cert.pem".to_owned(),
        key_pem: "certs/revoked/fp-x/key.pem".to_owned(),
    };

    let err = repo
        .update_paths(404, &paths)
        .await
        .expect_err("证书不存在应报错");
    assert!(matches!(&err, Error::NotFound { .. }), "{err:?}");
}
