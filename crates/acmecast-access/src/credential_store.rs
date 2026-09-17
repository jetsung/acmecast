//! 按标识加载凭据：取出、解密、校验、交给步骤。
//!
//! 流水线步骤只持有凭据的**标识**。执行时再由这里把它还原成内存中的字段值——
//! 明文既不常驻磁盘，也不穿过流水线的配置与运行历史。
//!
//! 出错一律返回 `Err` 而非 `None`：spec 要求「引用不存在的标识在执行前明确报错」，
//! 若返回 `Option`，调用方很容易写成「取不到就算了」，把一个应当失败的步骤静默放过。

use std::sync::Arc;

use acmecast_core::CredentialCipher;
use acmecast_store::entity::{credential, pipeline, pipeline_step};
use sea_orm::{DatabaseConnection, EntityTrait};

use crate::error::{Error, Result};
use crate::registry::CredentialRegistry;

/// 步骤输入里用来引用凭据的字段名。
///
/// 约定：`input` 中任何层级名为 `credential_id` 的整数字段都算引用。
/// 用固定名字而非「以 `_id` 结尾」这类模糊规则，是为了不误伤——步骤输入里
/// 出现别的 `xxx_id` 时，删除凭据不该被无谓地挡住。
///
/// 需要多个凭据时嵌套即可（`{"dns": {"credential_id": 3}, "acme": {"credential_id": 7}}`），
/// 递归扫描能一并找到。
pub const CREDENTIAL_REFERENCE_FIELD: &str = "credential_id";

/// 一处对凭据的引用。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PipelineReference {
    /// 引用它的流水线。
    pub pipeline_id: i64,
    /// 流水线名称，便于直接呈现给用户。
    pub pipeline_name: String,
    /// 引用发生在第几个步骤，从 0 起。
    pub step_order: i32,
}

/// 递归判断步骤输入里是否引用了该凭据。
fn references_credential(value: &serde_json::Value, credential_id: i64) -> bool {
    match value {
        serde_json::Value::Object(map) => map.iter().any(|(key, value)| {
            (key == CREDENTIAL_REFERENCE_FIELD && value.as_i64() == Some(credential_id))
                || references_credential(value, credential_id)
        }),
        serde_json::Value::Array(items) => items
            .iter()
            .any(|item| references_credential(item, credential_id)),
        serde_json::Value::Null
        | serde_json::Value::Bool(_)
        | serde_json::Value::Number(_)
        | serde_json::Value::String(_) => false,
    }
}

/// 按标识取出的、已解密的凭据。
///
/// 字段以 JSON 呈现，步骤用 [`ResolvedCredential::as_fields`] 反序列化成自己的结构体。
///
/// `Debug` 手写：`fields` 是解密后的明文，只能标记「有一份」，不能打印内容。
#[derive(Clone)]
pub struct ResolvedCredential {
    /// 凭据记录主键。
    pub id: i64,
    /// 展示名称。
    pub name: String,
    /// 类型标识，如 `acme.account`。
    pub type_id: String,
    /// 解密后的字段值。
    pub fields: serde_json::Value,
}

impl std::fmt::Debug for ResolvedCredential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ResolvedCredential")
            .field("id", &self.id)
            .field("name", &self.name)
            .field("type_id", &self.type_id)
            .field("fields", &acmecast_core::REDACTED)
            .finish()
    }
}

impl ResolvedCredential {
    /// 把字段值反序列化成步骤期望的具体结构体。
    pub fn as_fields<T>(&self) -> Result<T>
    where
        T: serde::de::DeserializeOwned,
    {
        serde_json::from_value(self.fields.clone())
            .map_err(|e| Error::Core(acmecast_core::Error::Serialization(e.to_string())))
    }
}

/// 凭据的存储与还原。
///
/// 三个协作者各司其职：库负责取记录，注册表负责认识类型，加密器负责解出明文。
#[derive(Debug)]
pub struct CredentialStore<'db> {
    db: &'db DatabaseConnection,
    registry: Arc<CredentialRegistry>,
    cipher: Arc<CredentialCipher>,
}

impl<'db> CredentialStore<'db> {
    /// 绑定数据库、类型注册表与加密器。
    #[must_use]
    pub fn new(
        db: &'db DatabaseConnection,
        registry: Arc<CredentialRegistry>,
        cipher: Arc<CredentialCipher>,
    ) -> Self {
        Self {
            db,
            registry,
            cipher,
        }
    }

    /// 按标识加载、解密并校验一份凭据。
    ///
    /// 校验放在**取出时**而不只在写入时：库里的内容可能是在旧的类型定义下写进去的，
    /// 让一份不再合法的凭据注入到步骤里，只会换成一个更晚、更难定位的失败。
    pub async fn resolve(&self, id: i64) -> Result<ResolvedCredential> {
        let record = credential::Entity::find_by_id(id)
            .one(self.db)
            .await?
            .ok_or_else(|| Error::missing_credential(id))?;

        // 类型必须已注册：未注册类型的字段无从校验，也不该被注入到任何步骤。
        let credential_type = self.registry.require(&record.type_id)?;

        let plaintext = self
            .cipher
            .decrypt_string(&record.encrypted_fields)
            .map_err(Error::Core)?;

        let fields: serde_json::Value = serde_json::from_str(&plaintext)
            .map_err(|e| Error::Core(acmecast_core::Error::Serialization(e.to_string())))?;

        credential_type.validate(&fields).map_err(Error::Core)?;

        Ok(ResolvedCredential {
            id: record.id,
            name: record.name,
            type_id: record.type_id,
            fields,
        })
    }

    /// 该标识的凭据是否存在。
    ///
    /// 只用于「要不要提示」这类非关键判断；真正要注入时一律用 [`Self::resolve`]。
    pub async fn exists(&self, id: i64) -> Result<bool> {
        Ok(credential::Entity::find_by_id(id)
            .one(self.db)
            .await?
            .is_some())
    }

    /// 用新字段整体覆盖凭据内容。
    ///
    /// ACME 账号凭据在**首次使用时**注册：步骤建立账号后把签发的凭据写回，
    /// 后续运行才能复用而不是每次都注册一个新账号。字段整体替换与
    /// handler 的保存路径语义一致，不做局部合并。
    pub async fn update_fields(&self, id: i64, fields: &serde_json::Value) -> Result<()> {
        use sea_orm::{ActiveModelTrait, Set};

        let record = credential::Entity::find_by_id(id)
            .one(self.db)
            .await?
            .ok_or_else(|| Error::missing_credential(id))?;

        // 类型必须已注册：写回的明文要先通过它校验，坏字段不该被加密入库。
        let credential_type = self.registry.require(&record.type_id)?;
        credential_type.validate(fields).map_err(Error::Core)?;

        let plaintext = serde_json::to_string(fields)
            .map_err(|e| Error::Core(acmecast_core::Error::Serialization(e.to_string())))?;
        let encrypted = self
            .cipher
            .encrypt_string(&plaintext)
            .map_err(Error::Core)?;

        let mut model: credential::ActiveModel = record.into();
        model.encrypted_fields = Set(encrypted);
        model.updated_at = Set(chrono::Utc::now());
        model.update(self.db).await?;
        Ok(())
    }

    /// 找出仍引用该凭据的流水线步骤。
    ///
    /// 扫描全部步骤的输入，而不是用 SQL 去过滤 JSON：三方言对 JSON 的查询能力不一致，
    /// 而步骤数量级很小、删除凭据又是低频操作——全表扫描换来的是三方言行为完全一致。
    pub async fn referring_pipelines(&self, credential_id: i64) -> Result<Vec<PipelineReference>> {
        let steps = pipeline_step::Entity::find()
            .find_also_related(pipeline::Entity)
            .all(self.db)
            .await?;

        Ok(steps
            .into_iter()
            .filter(|(step, _)| references_credential(&step.input, credential_id))
            .map(|(step, owner)| PipelineReference {
                pipeline_id: step.pipeline_id,
                // 流水线理论上一定存在（外键级联删除），真取不到时退化成 id，
                // 总好过在错误信息里少列一处引用。
                pipeline_name: owner
                    .map(|pipeline| pipeline.name)
                    .unwrap_or_else(|| format!("#{}", step.pipeline_id)),
                step_order: step.order_index,
            })
            .collect())
    }

    /// 删除凭据；仍被流水线引用时拒绝并列出引用者。
    ///
    /// 引用检查与删除之间没有加锁：这里的前提是「删除凭据是低频的运维操作」，
    /// 与并发新建流水线撞上的概率可以忽略；真撞上了，那条流水线会在执行时
    /// 因凭据不存在而明确失败——这正是 spec 要求的行为，不会静默跑错。
    pub async fn delete(&self, credential_id: i64) -> Result<()> {
        let referenced_by = self.referring_pipelines(credential_id).await?;
        if !referenced_by.is_empty() {
            return Err(Error::CredentialInUse {
                credential_id,
                referenced_by,
            });
        }

        let outcome = credential::Entity::delete_by_id(credential_id)
            .exec(self.db)
            .await?;
        if outcome.rows_affected == 0 {
            return Err(Error::missing_credential(credential_id));
        }
        Ok(())
    }
}
