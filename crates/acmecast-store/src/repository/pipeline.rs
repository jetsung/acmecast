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
    /// 运行结束（成功/失败）时接收简讯的通知渠道凭据 ID 列表。
    pub notification_channels: Vec<i64>,
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
    /// 运行结束（成功/失败）时接收简讯的通知渠道凭据 ID 列表。
    pub notification_channels: Vec<i64>,
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
    /// 运行结束（成功/失败）时接收简讯的通知渠道凭据 ID 列表。
    pub notification_channels: Vec<i64>,
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
    pub async fn save(&self, id: Option<i64>, mut input: PipelineInput) -> Result<i64> {
        // 目录写法的补全发生在校验与入库之前：前端回显的必须是部署实际
        // 使用的路径，否则「表单里是目录、日志里是文件」会让排障多绕一圈。
        normalize_deploy_inputs(&mut input.steps);
        let name = input.name.trim();
        if name.is_empty() {
            return Err(Error::Validation("流水线名称不能为空".to_owned()));
        }
        if name.chars().count() > 255 {
            return Err(Error::Validation(
                "流水线名称不能超过 255 个字符".to_owned(),
            ));
        }
        if input.steps.is_empty() {
            // 没有步骤的流水线跑起来什么也不做，多半是保存了个半成品。
            return Err(Error::Validation(
                "流水线至少需要一个步骤；若想暂时停用请改 enabled 而不是删光步骤".to_owned(),
            ));
        }
        validate_step_limits(&input.steps)?;

        // 名称唯一：去空白后精确匹配，更新时排除自身。
        if let Some(conflict) = self.find_by_name(name).await? {
            let is_self = id.is_some_and(|own| own == conflict.id);
            if !is_self {
                return Err(Error::Conflict(format!("流水线名称已存在: {name}")));
            }
        }

        // 整个定义（含步骤集合）在一个事务里落库：中途失败时不留下半个定义。
        let transaction = self.db.begin().await?;
        let now = Utc::now();
        let description = normalize_description(input.description);
        let notification_channels = serde_json::to_string(&input.notification_channels)
            .expect("通知渠道 ID 数组的序列化不会失败");

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
                    notification_channels: Set(notification_channels),
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
                    notification_channels: Set(notification_channels),
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

        if let Err(err) = transaction.commit().await {
            // 并发下应用层判重与唯一索引之间有窗口，冲突时转为可诊断的 Conflict。
            let msg = err.to_string();
            if msg.contains("UNIQUE") || msg.contains("Duplicate") || msg.contains("unique") {
                return Err(Error::Conflict(format!("流水线名称已存在: {name}")));
            }
            return Err(Error::Database(err));
        }
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
            notification_channels: parse_notification_channels(&found.notification_channels),
            steps: self.steps_of(id).await?,
            created_at: found.created_at,
            updated_at: found.updated_at,
        }))
    }

    /// 列出全部流水线概要，按主键降序。
    pub async fn list(&self) -> Result<Vec<PipelineSummary>> {
        let rows = pipeline::Entity::find()
            .order_by_desc(pipeline::Column::Id)
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
                notification_channels: parse_notification_channels(&row.notification_channels),
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

    /// 按去空白后精确名称查找流水线（用于唯一性校验）。
    async fn find_by_name(&self, name: &str) -> Result<Option<pipeline::Model>> {
        let found = pipeline::Entity::find()
            .filter(pipeline::Column::Name.eq(name))
            .one(self.db)
            .await?;
        Ok(found)
    }
}

/// 对 `cert.deploy` 步骤的目录写法做补全。
///
/// 只处理文件类目标（`local` / `ssh` / `s3` / `webdav`）：路径字段以 `/`
/// 结尾时视为目录，补上该目标的默认文件名（与 `certs/ssl/` 落盘命名同源）。
/// 主域优先取同流水线 `cert.apply` 步骤的 `domains[0]`（经 `domain_file_prefix`
/// 转前缀，通配 `*.example.com` → `_wildcard.example.com`）；主域可解析时直接
/// 拼带前缀文件名入库，否则 `local/ssh` 回落静态名、`s3/webdav` 回落
/// `{domain}` 占位模板。非 `cert.deploy` 步骤与其它目标原样保留。
fn normalize_deploy_inputs(steps: &mut [PipelineStepInput]) {
    let prefix = steps
        .iter()
        .filter(|step| step.type_id.trim() == "cert.apply")
        .find_map(|step| find_primary_domain(&step.input))
        .map(|domain| crate::filestore::domain_file_prefix(&domain));
    for step in steps {
        if step.type_id.trim() != "cert.deploy" {
            continue;
        }
        normalize_deploy_config(&mut step.input, prefix.as_deref());
    }
}

/// 从 `cert.apply` 步骤输入里解析主域（`domains[0]`）。
///
/// 兼容顶层与 `config` 包裹两种 JSON 形态；`domains` 为空或非数组时视为未知。
fn find_primary_domain(input: &serde_json::Value) -> Option<String> {
    let root = input.get("config").unwrap_or(input);
    let domain = root
        .get("domains")
        .and_then(serde_json::Value::as_array)
        .and_then(|domains| domains.first())
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|domain| !domain.is_empty())?;
    Some(domain.to_owned())
}

/// 按 `target` 分支补全 `config` 里的目录字段。
fn normalize_deploy_config(input: &mut serde_json::Value, prefix: Option<&str>) {
    let Some(object) = input.as_object_mut() else {
        return;
    };
    let target = object
        .get("target")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("")
        .trim()
        .to_owned();
    let Some(config) = object
        .get_mut("config")
        .and_then(serde_json::Value::as_object_mut)
    else {
        return;
    };

    // 主域已知时入库即带前缀的真实路径；未知时 local/ssh 回落静态名、
    // s3/webdav 回落 `{domain}` 占位（部署时按本次主域展开）。
    let fullchain_name = prefix
        .map(|prefix| format!("{prefix}.fullchain.cer"))
        .unwrap_or_else(|| match target.as_str() {
            "s3" | "webdav" => "{domain}.fullchain.cer".to_owned(),
            _ => "fullchain.cer".to_owned(),
        });
    let ca_name = prefix
        .map(|prefix| format!("{prefix}.ca.cer"))
        .unwrap_or_else(|| match target.as_str() {
            "s3" | "webdav" => "{domain}.ca.cer".to_owned(),
            _ => "ca.cer".to_owned(),
        });
    let key_name = prefix
        .map(|prefix| format!("{prefix}.key"))
        .unwrap_or_else(|| match target.as_str() {
            "s3" | "webdav" => "{domain}.key".to_owned(),
            _ => "key".to_owned(),
        });

    match target.as_str() {
        "local" | "ssh" => {
            // `cert_path` 是 `fullchain_path` 的旧别名：仅剩旧键的存量配置
            // 也要补全，避免目录写法拖到部署期才被兜底。
            for (field, default_name) in [
                ("fullchain_path", &fullchain_name),
                ("cert_path", &fullchain_name),
                ("ca_path", &ca_name),
                ("key_path", &key_name),
            ] {
                fill_directory_field(config, field, default_name);
            }
        }
        "s3" => {
            normalize_joined_prefix(config, "prefix");
            for (field, default_name) in [
                ("fullchain_filename", &fullchain_name),
                ("cert_filename", &fullchain_name),
                ("ca_filename", &ca_name),
                ("key_filename", &key_name),
            ] {
                fill_directory_field(config, field, default_name);
            }
        }
        "webdav" => {
            normalize_joined_prefix(config, "base_path");
            for (field, default_name) in [
                ("fullchain_path", &fullchain_name),
                ("cert_path", &fullchain_name),
                ("ca_path", &ca_name),
                ("key_path", &key_name),
            ] {
                fill_directory_field(config, field, default_name);
            }
        }
        _ => {}
    }
}

/// 目录判据：`trim()` 后以 `/` 结尾；空白与缺省不触发。
fn fill_directory_field(
    config: &mut serde_json::Map<String, serde_json::Value>,
    field: &str,
    default_name: &str,
) {
    let Some(serde_json::Value::String(value)) = config.get_mut(field) else {
        return;
    };
    let trimmed = value.trim();
    if trimmed.is_empty() || !trimmed.ends_with('/') {
        return;
    }
    let filled = format!("{trimmed}{default_name}");
    *value = filled;
}

/// 对象键/基路径前缀：去空白与首尾 `/`，非空时补一个 `/`。
fn normalize_joined_prefix(config: &mut serde_json::Map<String, serde_json::Value>, field: &str) {
    let Some(serde_json::Value::String(value)) = config.get_mut(field) else {
        return;
    };
    let trimmed = value.trim().trim_matches('/').to_owned();
    *value = if trimmed.is_empty() {
        trimmed
    } else {
        format!("{trimmed}/")
    };
}

/// 校验步骤次数：`cert.apply` 与 `cert.store` 各至多一次，`cert.deploy` 不限。
fn validate_step_limits(steps: &[PipelineStepInput]) -> Result<()> {
    let apply = steps
        .iter()
        .filter(|s| s.type_id.trim() == "cert.apply")
        .count();
    if apply > 1 {
        return Err(Error::Validation("cert.apply 仅允许添加一次".to_owned()));
    }
    let store = steps
        .iter()
        .filter(|s| s.type_id.trim() == "cert.store")
        .count();
    if store > 1 {
        return Err(Error::Validation("cert.store 仅允许添加一次".to_owned()));
    }
    Ok(())
}

/// 把描述里的空白归一成 `None`，免得库里出现「有值但是空的」描述。
fn normalize_description(description: Option<String>) -> Option<String> {
    description
        .map(|text| text.trim().to_owned())
        .filter(|text| !text.is_empty())
}

/// 解析库中的通知渠道文本为凭据 ID 列表。
///
/// 非法 JSON 视作空集合（不投递）：通知是运行结束后的尽力而为动作，
/// 库中脏数据不应让读取流水线定义整体失败。
fn parse_notification_channels(text: &str) -> Vec<i64> {
    serde_json::from_str(text).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn deploy_step(target: &str, config: serde_json::Value) -> PipelineStepInput {
        PipelineStepInput {
            type_id: "cert.deploy".to_owned(),
            input: json!({ "target": target, "config": config }),
            enabled: true,
        }
    }

    fn normalized(mut step: PipelineStepInput) -> PipelineStepInput {
        normalize_deploy_inputs(std::slice::from_mut(&mut step));
        step
    }

    #[test]
    fn local_directory_paths_get_the_default_file_names() {
        let step = normalized(deploy_step(
            "local",
            json!({
                "fullchain_path": "/data/certs/ssl/",
                "ca_path": "/data/certs/ssl/",
                "key_path": "/data/certs/ssl/",
                "reload_command": "nginx -s reload"
            }),
        ));

        assert_eq!(
            step.input,
            json!({
                "target": "local",
                "config": {
                    "fullchain_path": "/data/certs/ssl/fullchain.cer",
                    "ca_path": "/data/certs/ssl/ca.cer",
                    "key_path": "/data/certs/ssl/key",
                    "reload_command": "nginx -s reload"
                }
            })
        );
    }

    #[test]
    fn ssh_directory_paths_get_the_default_file_names() {
        let step = normalized(deploy_step(
            "ssh",
            json!({
                "credential_id": 1,
                "fullchain_path": "/srv/ssl/",
                "key_path": "/srv/ssl/"
            }),
        ));
        let config = &step.input["config"];

        assert_eq!(config["fullchain_path"], "/srv/ssl/fullchain.cer");
        assert_eq!(config["key_path"], "/srv/ssl/key");
        assert!(config.get("ca_path").is_none(), "缺省字段不应被凭空创建");
    }

    #[test]
    fn a_relative_directory_is_filled_too() {
        let step = normalized(deploy_step(
            "local",
            json!({
                "fullchain_path": "certs/ssl/",
                "key_path": "certs/ssl/key.pem"
            }),
        ));
        let config = &step.input["config"];

        assert_eq!(config["fullchain_path"], "certs/ssl/fullchain.cer");
        // 非目录写法原样保留。
        assert_eq!(config["key_path"], "certs/ssl/key.pem");
    }

    #[test]
    fn s3_directory_fields_are_filled_and_prefix_is_normalized() {
        let step = normalized(deploy_step(
            "s3",
            json!({
                "credential_id": 1,
                "prefix": "acme",
                "fullchain_filename": "certs/",
                "ca_filename": "certs/",
                "key_filename": "my-keys/"
            }),
        ));
        let config = &step.input["config"];

        assert_eq!(config["prefix"], "acme/");
        assert_eq!(config["fullchain_filename"], "certs/{domain}.fullchain.cer");
        assert_eq!(config["ca_filename"], "certs/{domain}.ca.cer");
        assert_eq!(config["key_filename"], "my-keys/{domain}.key");
    }

    #[test]
    fn webdav_directory_fields_are_filled_and_base_path_is_normalized() {
        let step = normalized(deploy_step(
            "webdav",
            json!({
                "credential_id": 1,
                "base_path": "/dav/certs/",
                "cert_path": "certs/",
                "ca_path": "certs/",
                "key_path": "certs/"
            }),
        ));
        let config = &step.input["config"];

        assert_eq!(config["base_path"], "dav/certs/");
        assert_eq!(config["key_path"], "certs/{domain}.key");
        // `cert_path` 是 `fullchain_path` 的别名，目录写法同样补全。
        assert_eq!(config["cert_path"], "certs/{domain}.fullchain.cer");
        assert_eq!(config["ca_path"], "certs/{domain}.ca.cer");
    }

    fn deploy_steps(
        apply_domains: Option<&[&str]>,
        target: &str,
        config: serde_json::Value,
    ) -> Vec<PipelineStepInput> {
        let mut steps = Vec::new();
        if let Some(domains) = apply_domains {
            steps.push(PipelineStepInput {
                type_id: "cert.apply".to_owned(),
                input: json!({ "domains": domains }),
                enabled: true,
            });
        }
        steps.push(deploy_step(target, config));
        steps
    }

    fn normalized_steps(mut steps: Vec<PipelineStepInput>) -> Vec<PipelineStepInput> {
        normalize_deploy_inputs(&mut steps);
        steps
    }

    #[test]
    fn wildcard_primary_domain_prefills_prefixed_names() {
        let steps = normalized_steps(deploy_steps(
            Some(&["*.example.com"]),
            "local",
            json!({
                "fullchain_path": "/tmp/t1/",
                "ca_path": "/tmp/t1/",
                "key_path": "/tmp/t1/"
            }),
        ));

        assert_eq!(
            steps[1].input["config"],
            json!({
                "fullchain_path": "/tmp/t1/_wildcard.example.com.fullchain.cer",
                "ca_path": "/tmp/t1/_wildcard.example.com.ca.cer",
                "key_path": "/tmp/t1/_wildcard.example.com.key"
            })
        );
    }

    #[test]
    fn wildcard_primary_domain_prefills_s3_and_webdav_without_placeholder() {
        let steps = normalized_steps(deploy_steps(
            Some(&["*.example.com"]),
            "s3",
            json!({ "key_filename": "my-keys/" }),
        ));
        assert_eq!(
            steps[1].input["config"]["key_filename"],
            "my-keys/_wildcard.example.com.key"
        );

        let steps = normalized_steps(deploy_steps(
            Some(&["sub.example.com"]),
            "webdav",
            json!({ "ca_path": "certs/" }),
        ));
        assert_eq!(
            steps[1].input["config"]["ca_path"],
            "certs/sub.example.com.ca.cer"
        );
    }

    #[test]
    fn apply_primary_domain_inside_config_is_resolved_too() {
        let steps = vec![
            PipelineStepInput {
                type_id: "cert.apply".to_owned(),
                input: json!({ "config": { "domains": ["example.com"] } }),
                enabled: true,
            },
            deploy_step("local", json!({ "key_path": "/etc/ssl/" })),
        ];
        let steps = normalized_steps(steps);

        assert_eq!(
            steps[1].input["config"]["key_path"],
            "/etc/ssl/example.com.key"
        );
    }

    #[test]
    fn non_directory_values_keep_prefixed_names_untouched() {
        let steps = normalized_steps(deploy_steps(
            Some(&["*.example.com"]),
            "local",
            json!({
                "fullchain_path": "/tmp/t1/custom.pem",
                "key_path": "/tmp/t1/"
            }),
        ));

        assert_eq!(
            steps[1].input["config"]["fullchain_path"],
            "/tmp/t1/custom.pem"
        );
        assert_eq!(
            steps[1].input["config"]["key_path"],
            "/tmp/t1/_wildcard.example.com.key"
        );
    }

    #[test]
    fn blank_values_and_non_directory_paths_are_left_alone() {
        let step = normalized(deploy_step(
            "local",
            json!({
                "fullchain_path": "/data/certs/fullchain.pem",
                "ca_path": "   ",
                "key_path": "/data/certs/key.pem"
            }),
        ));
        let config = &step.input["config"];

        assert_eq!(config["fullchain_path"], "/data/certs/fullchain.pem");
        assert_eq!(config["ca_path"], "   ", "空白值不补全");
        assert_eq!(config["key_path"], "/data/certs/key.pem");
    }

    #[test]
    fn other_steps_and_targets_are_not_touched() {
        let apply = PipelineStepInput {
            type_id: "cert.apply".to_owned(),
            input: json!({ "domains": ["example.com"] }),
            enabled: true,
        };
        let unknown = deploy_step("tencent-ssl", json!({ "fullchain_path": "/etc/ssl/" }));

        assert_eq!(normalized(apply.clone()).input, apply.input);
        assert_eq!(normalized(unknown.clone()).input, unknown.input);
    }
}
