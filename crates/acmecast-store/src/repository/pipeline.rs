//! 流水线定义仓储。
//!
//! 一条流水线由「若干有序步骤」构成，步骤顺序**由输入数组的位置决定**——
//! 输入里不带 `order_index` 字段，避免出现「数组里排第一、字段写着 5」这种
//! 自相矛盾的定义。
//!
//! 保存采用**整体替换**语义：以本次输入为准重建步骤集合。不做增量比对是因为
//! 「哪些步骤被保留了、哪些是新增的」这类推断需要引入步骤身份，而身份一旦
//! 传错就会静默改错定义；全量替换语义明确，代价只是多几条 INSERT。

use chrono::{DateTime, Utc};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter, QueryOrder, Set,
    TransactionTrait,
};

use crate::entity::{pipeline, pipeline_step};
use crate::error::{Error, Result};

/// 一个步骤的输入。
#[derive(Debug, Clone, PartialEq)]
pub struct PipelineStepInput {
    /// 任务类型标识。
    pub type_id: String,
    /// JSON 输入配置。
    pub input: serde_json::Value,
    /// 单步开关；停用后执行时跳过。
    pub enabled: bool,
}

/// 一条流水线的输入。
#[derive(Debug, Clone, PartialEq)]
pub struct PipelineInput {
    /// 展示名称。
    pub name: String,
    /// 可选描述。
    pub description: Option<String>,
    /// 流水线级开关。
    pub enabled: bool,
    /// 按执行顺序排列的步骤，位置即顺序。
    pub steps: Vec<PipelineStepInput>,
}

/// 读出的一个步骤。
#[derive(Debug, Clone, PartialEq)]
pub struct PipelineStep {
    /// 主键。
    pub id: i64,
    /// 执行顺序，从 0 起。
    pub order_index: i32,
    /// 任务类型标识。
    pub type_id: String,
    /// JSON 输入配置。
    pub input: serde_json::Value,
    /// 单步开关。
    pub enabled: bool,
}

/// 读出的完整流水线定义。
#[derive(Debug, Clone, PartialEq)]
pub struct Pipeline {
    /// 主键。
    pub id: i64,
    /// 展示名称。
    pub name: String,
    /// 可选描述。
    pub description: Option<String>,
    /// 流水线级开关。
    pub enabled: bool,
    /// 按执行顺序排列的步骤。
    pub steps: Vec<PipelineStep>,
    /// 创建时间。
    pub created_at: DateTime<Utc>,
    /// 最后更新时间。
    pub updated_at: DateTime<Utc>,
}

/// 列表项：只带概要，不含步骤。
#[derive(Debug, Clone, PartialEq)]
pub struct PipelineSummary {
    /// 主键。
    pub id: i64,
    /// 展示名称。
    pub name: String,
    /// 可选描述。
    pub description: Option<String>,
    /// 流水线级开关。
    pub enabled: bool,
    /// 步骤数量。
    pub step_count: usize,
    /// 最后更新时间。
    pub updated_at: DateTime<Utc>,
}

/// 流水线定义仓储。
#[derive(Debug)]
pub struct PipelineRepository<'db> {
    db: &'db DatabaseConnection,
}

impl<'db> PipelineRepository<'db> {
    /// 绑定数据库连接。
    #[must_use]
    pub fn new(db: &'db DatabaseConnection) -> Self {
        Self { db }
    }

    /// 新建（`id` 为 `None`）或整体更新（`id` 为 `Some`）一条流水线。
    ///
    /// 步骤集合被替换为本次输入的内容；返回流水线主键。
    pub async fn save(&self, id: Option<i64>, input: PipelineInput) -> Result<i64> {
        let name = input.name.trim();
        if name.is_empty() {
            return Err(Error::Validation("流水线名称不能为空".to_owned()));
        }
        if input.steps.is_empty() {
            // 没有步骤的流水线跑起来什么也不做，多半是保存了个半成品。
            return Err(Error::Validation(
                "流水线至少需要一个步骤；若想暂时停用请改 enabled 而不是删光步骤".to_owned(),
            ));
        }

        // 整个定义（含步骤集合）在一个事务里落库：中途失败时不留下半个定义。
        let transaction = self.db.begin().await?;
        let now = Utc::now();
        let description = normalize_description(input.description);

        let pipeline_id = match id {
            Some(existing) => {
                if pipeline::Entity::find_by_id(existing)
                    .one(&transaction)
                    .await?
                    .is_none()
                {
                    return Err(Error::not_found("流水线", existing.to_string()));
                }
                pipeline::ActiveModel {
                    id: Set(existing),
                    name: Set(name.to_owned()),
                    description: Set(description),
                    enabled: Set(input.enabled),
                    updated_at: Set(now),
                    ..Default::default()
                }
                .update(&transaction)
                .await?;
                existing
            }
            None => {
                pipeline::ActiveModel {
                    name: Set(name.to_owned()),
                    description: Set(description),
                    enabled: Set(input.enabled),
                    created_at: Set(now),
                    updated_at: Set(now),
                    ..Default::default()
                }
                .insert(&transaction)
                .await?
                .id
            }
        };

        // 整体替换：先清空，再按输入顺序重建。
        pipeline_step::Entity::delete_many()
            .filter(pipeline_step::Column::PipelineId.eq(pipeline_id))
            .exec(&transaction)
            .await?;

        for (position, step) in input.steps.into_iter().enumerate() {
            let type_id = step.type_id.trim();
            if type_id.is_empty() {
                return Err(Error::Validation(format!(
                    "第 {} 个步骤缺少任务类型标识",
                    position + 1
                )));
            }

            pipeline_step::ActiveModel {
                pipeline_id: Set(pipeline_id),
                // 顺序由数组位置决定，不从输入里读。
                order_index: Set(position as i32),
                type_id: Set(type_id.to_owned()),
                input: Set(step.input),
                enabled: Set(step.enabled),
                ..Default::default()
            }
            .insert(&transaction)
            .await?;
        }

        transaction.commit().await?;
        Ok(pipeline_id)
    }

    /// 按 ID 读取完整定义（含按顺序排列的步骤）。
    pub async fn find(&self, id: i64) -> Result<Option<Pipeline>> {
        let Some(found) = pipeline::Entity::find_by_id(id).one(self.db).await? else {
            return Ok(None);
        };

        Ok(Some(Pipeline {
            id: found.id,
            name: found.name,
            description: found.description,
            enabled: found.enabled,
            steps: self.steps_of(id).await?,
            created_at: found.created_at,
            updated_at: found.updated_at,
        }))
    }

    /// 列出全部流水线概要，按主键升序。
    pub async fn list(&self) -> Result<Vec<PipelineSummary>> {
        let rows = pipeline::Entity::find()
            .order_by_asc(pipeline::Column::Id)
            .all(self.db)
            .await?;

        let mut summaries = Vec::with_capacity(rows.len());
        for row in rows {
            let step_count = self.steps_of(row.id).await?.len();
            summaries.push(PipelineSummary {
                id: row.id,
                name: row.name,
                description: row.description,
                enabled: row.enabled,
                step_count,
                updated_at: row.updated_at,
            });
        }
        Ok(summaries)
    }

    /// 删除流水线；返回它此前是否存在。
    ///
    /// 步骤由外键的级联删除一并清掉（见迁移里的 `on_delete = Cascade`），
    /// 这里不手工删——手工删反而会在级联失效时留下孤儿步骤。
    pub async fn delete(&self, id: i64) -> Result<bool> {
        let outcome = pipeline::Entity::delete_by_id(id).exec(self.db).await?;
        Ok(outcome.rows_affected > 0)
    }

    /// 读取某条流水线的步骤，按 `order_index` 升序。
    async fn steps_of(&self, pipeline_id: i64) -> Result<Vec<PipelineStep>> {
        let rows = pipeline_step::Entity::find()
            .filter(pipeline_step::Column::PipelineId.eq(pipeline_id))
            .order_by_asc(pipeline_step::Column::OrderIndex)
            .all(self.db)
            .await?;

        Ok(rows
            .into_iter()
            .map(|row| PipelineStep {
                id: row.id,
                order_index: row.order_index,
                type_id: row.type_id,
                input: row.input,
                enabled: row.enabled,
            })
            .collect())
    }
}

/// 把描述里的空白归一成 `None`，免得库里出现「有值但是空的」描述。
fn normalize_description(description: Option<String>) -> Option<String> {
    description
        .map(|text| text.trim().to_owned())
        .filter(|text| !text.is_empty())
}
