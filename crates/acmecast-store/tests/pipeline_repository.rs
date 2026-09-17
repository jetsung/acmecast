//! 6.1 流水线定义与持久化。
//!
//! spec 场景「保存含多步骤的流水线」要求：持久化后重新读取，**步骤顺序与各步输入
//! 保持不变**。这里另外把启用状态与级联删除一并钉住——它们同样是「定义」的一部分。

use acmecast_store::entity::pipeline_step;
use acmecast_store::repository::{PipelineInput, PipelineRepository, PipelineStepInput};
use acmecast_store::{Error, migrate};
use sea_orm::{Database, DatabaseConnection, EntityTrait};

async fn setup() -> DatabaseConnection {
    let db = Database::connect("sqlite::memory:")
        .await
        .expect("应能连上内存库");
    migrate(&db).await.expect("迁移应成功");
    db
}

/// 造一个步骤；输入里刻意带上域名，便于断言「各步输入保持不变」。
fn step(type_id: &str, domains: &[&str]) -> PipelineStepInput {
    PipelineStepInput {
        type_id: type_id.to_owned(),
        input: serde_json::json!({ "domains": domains }),
        enabled: true,
    }
}

fn pipeline(name: &str, steps: Vec<PipelineStepInput>) -> PipelineInput {
    PipelineInput {
        name: name.to_owned(),
        description: None,
        enabled: true,
        steps,
    }
}

// ---- Scenario: 保存含多步骤的流水线 ----

#[tokio::test]
async fn a_multi_step_pipeline_round_trips_with_its_order_and_inputs() {
    let db = setup().await;
    let repo = PipelineRepository::new(&db);

    let id = repo
        .save(
            None,
            pipeline(
                "签发并部署",
                vec![
                    step("cert.apply", &["example.com"]),
                    step("cert.deploy.local", &[]),
                ],
            ),
        )
        .await
        .expect("应能保存");

    let found = repo.find(id).await.expect("应能读取").expect("应存在");

    assert_eq!(found.name, "签发并部署");
    assert_eq!(found.steps.len(), 2, "两个步骤都应落库");
    // 顺序与各步输入，逐项对照输入。
    assert_eq!(found.steps[0].type_id, "cert.apply");
    assert_eq!(found.steps[0].order_index, 0);
    assert_eq!(
        found.steps[0].input,
        serde_json::json!({ "domains": ["example.com"] }),
        "输入应原样保留"
    );
    assert_eq!(found.steps[1].type_id, "cert.deploy.local");
    assert_eq!(found.steps[1].order_index, 1);
}

#[tokio::test]
async fn step_order_comes_from_position_in_the_input_array() {
    let db = setup().await;
    let repo = PipelineRepository::new(&db);

    // 输入里没有 order_index 这种字段——顺序完全由位置决定，
    // 因此不可能出现「数组里排第一、字段写着 5」的自相矛盾。
    let id = repo
        .save(
            None,
            pipeline("三步", vec![step("a", &[]), step("b", &[]), step("c", &[])]),
        )
        .await
        .unwrap();

    let found = repo.find(id).await.unwrap().unwrap();
    let order: Vec<i32> = found.steps.iter().map(|s| s.order_index).collect();
    let types: Vec<&str> = found.steps.iter().map(|s| s.type_id.as_str()).collect();

    assert_eq!(order, vec![0, 1, 2]);
    assert_eq!(types, vec!["a", "b", "c"], "读取顺序应与写入顺序一致");
}

#[tokio::test]
async fn enable_flags_are_persisted_at_both_levels() {
    let db = setup().await;
    let repo = PipelineRepository::new(&db);

    let mut input = pipeline(
        "半停用",
        vec![
            PipelineStepInput {
                enabled: true,
                ..step("keep", &[])
            },
            PipelineStepInput {
                enabled: false,
                ..step("skip", &[])
            },
        ],
    );
    input.enabled = false;

    let id = repo.save(None, input).await.unwrap();
    let found = repo.find(id).await.unwrap().unwrap();

    assert!(!found.enabled, "流水线级开关应落库");
    assert!(found.steps[0].enabled, "单步开关应落库");
    assert!(!found.steps[1].enabled, "停用的步骤不应被当成启用的");
}

// ---- 更新语义 ----

#[tokio::test]
async fn updating_replaces_the_whole_step_set() {
    let db = setup().await;
    let repo = PipelineRepository::new(&db);

    let id = repo
        .save(None, pipeline("原名", vec![step("a", &[]), step("b", &[])]))
        .await
        .unwrap();

    // 改成三个步骤：旧的应被整体替换，而不是与新的混在一起。
    repo.save(
        Some(id),
        pipeline("新名", vec![step("x", &[]), step("y", &[]), step("z", &[])]),
    )
    .await
    .expect("应能更新");

    let found = repo.find(id).await.unwrap().unwrap();
    assert_eq!(found.name, "新名");
    assert_eq!(found.id, id, "更新不该产生新记录");
    let types: Vec<&str> = found.steps.iter().map(|s| s.type_id.as_str()).collect();
    assert_eq!(types, vec!["x", "y", "z"], "步骤集合应被整体替换");
}

#[tokio::test]
async fn updating_a_missing_pipeline_reports_not_found() {
    let db = setup().await;
    let repo = PipelineRepository::new(&db);

    let err = repo
        .save(Some(4242), pipeline("不存在", vec![step("a", &[])]))
        .await
        .expect_err("更新不存在的流水线应报错");

    assert!(
        matches!(&err, Error::NotFound { entity, .. } if entity == "流水线"),
        "{err:?}"
    );
    assert!(repo.list().await.unwrap().is_empty(), "不应留下半条记录");
}

// ---- 删除与级联 ----

#[tokio::test]
async fn deleting_a_pipeline_cascades_to_its_steps() {
    let db = setup().await;
    let repo = PipelineRepository::new(&db);

    let doomed = repo
        .save(None, pipeline("待删", vec![step("a", &[]), step("b", &[])]))
        .await
        .unwrap();
    let kept = repo
        .save(None, pipeline("保留", vec![step("c", &[])]))
        .await
        .unwrap();

    assert!(repo.delete(doomed).await.unwrap(), "删除应报告确实删掉了");

    // 目标流水线的步骤应随外键级联一并清掉。
    assert!(repo.find(doomed).await.unwrap().is_none());
    let orphans = pipeline_step::Entity::find()
        .all(&db)
        .await
        .expect("应能查询步骤表");
    assert_eq!(orphans.len(), 1, "只应剩下另一条流水线的步骤");
    assert_eq!(orphans[0].pipeline_id, kept, "留下的应是无关那条的步骤");

    // 重复删除报告「没删到」，而不是报错。
    assert!(!repo.delete(doomed).await.unwrap());
}

// ---- 输入校验 ----

#[tokio::test]
async fn a_pipeline_needs_a_name() {
    let db = setup().await;
    let repo = PipelineRepository::new(&db);

    let err = repo
        .save(None, pipeline("   ", vec![step("a", &[])]))
        .await
        .expect_err("空白名称应被拒绝");
    assert!(matches!(err, Error::Validation(_)), "{err:?}");
}

#[tokio::test]
async fn a_pipeline_needs_at_least_one_step() {
    let db = setup().await;
    let repo = PipelineRepository::new(&db);

    let err = repo
        .save(None, pipeline("空的", vec![]))
        .await
        .expect_err("空步骤应被拒绝");
    assert!(
        err.to_string().contains("至少需要一个步骤"),
        "错误应说明原因: {err}"
    );
}

#[tokio::test]
async fn a_step_needs_a_task_type() {
    let db = setup().await;
    let repo = PipelineRepository::new(&db);

    let err = repo
        .save(
            None,
            pipeline("缺类型", vec![step("a", &[]), step("  ", &[])]),
        )
        .await
        .expect_err("缺任务类型应被拒绝");
    assert!(
        err.to_string().contains("第 2 个步骤"),
        "错误应指出是第几步: {err}"
    );

    // 被拒绝的保存不该留下半条记录——整个定义在一个事务里。
    assert!(repo.list().await.unwrap().is_empty(), "失败应整体回滚");
}

#[tokio::test]
async fn blank_descriptions_become_none() {
    let db = setup().await;
    let repo = PipelineRepository::new(&db);

    let mut input = pipeline("有描述", vec![step("a", &[])]);
    input.description = Some("   ".to_owned());
    let id = repo.save(None, input).await.unwrap();

    assert_eq!(
        repo.find(id).await.unwrap().unwrap().description,
        None,
        "空白描述不应在库里留下「有值但是空的」状态"
    );
}

// ---- 列表 ----

#[tokio::test]
async fn list_reports_step_counts() {
    let db = setup().await;
    let repo = PipelineRepository::new(&db);

    repo.save(None, pipeline("甲", vec![step("a", &[])]))
        .await
        .unwrap();
    repo.save(
        None,
        pipeline("乙", vec![step("a", &[]), step("b", &[]), step("c", &[])]),
    )
    .await
    .unwrap();

    let summaries = repo.list().await.unwrap();
    assert_eq!(summaries.len(), 2);
    let names: Vec<&str> = summaries.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, vec!["甲", "乙"], "列表按主键升序");
    assert_eq!(summaries[0].step_count, 1);
    assert_eq!(summaries[1].step_count, 3);
}
