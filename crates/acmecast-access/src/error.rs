//! 凭据体系错误类型。

/// `acmecast-access` 的错误类型。
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// 引用了一个未注册的凭据类型。
    ///
    /// 错误里带上已知类型列表：这个错误基本只在配置写错时出现，
    /// 而「我到底能用哪些」正是此时第一个要问的问题。
    #[error("未知的凭据类型: {requested}（已注册: {}）", known.join(", "))]
    UnknownType {
        /// 请求的类型标识。
        requested: String,
        /// 当前已注册的类型标识。
        known: Vec<String>,
    },

    /// 同一个类型标识被注册了两次。
    #[error("凭据类型重复注册: {0}")]
    DuplicateType(String),

    /// 凭据仍被流水线引用，拒绝删除。
    ///
    /// 列出引用者而不只是说「有引用」：用户需要知道去哪里解绑，
    /// 否则只能自己在几十条流水线里翻。
    #[error("凭据 {credential_id} 仍被 {} 条流水线引用，无法删除：{}", referenced_by.len(), describe(referenced_by))]
    CredentialInUse {
        /// 被引用的凭据标识。
        credential_id: i64,
        /// 引用它的位置。
        referenced_by: Vec<crate::credential_store::PipelineReference>,
    },

    /// 持久层错误。
    #[error(transparent)]
    Store(#[from] acmecast_store::Error),

    /// 上游 core 层的错误。
    #[error(transparent)]
    Core(#[from] acmecast_core::Error),
}

impl Error {
    /// 构造「引用了不存在的凭据标识」。
    ///
    /// spec：流水线步骤引用已删除的凭据时，应当在**执行前**就带着这个原因失败。
    pub fn missing_credential(id: i64) -> Self {
        Self::Core(acmecast_core::Error::not_found("凭据", id.to_string()))
    }
}

impl From<sea_orm::DbErr> for Error {
    /// 数据库错误归到持久层错误之下，而不是另开一个变体——
    /// 对本 crate 而言，查询失败与持久层失败是同一类事情。
    fn from(err: sea_orm::DbErr) -> Self {
        Self::Store(acmecast_store::Error::from(err))
    }
}

/// 本 crate 统一的返回类型别名。
pub type Result<T> = std::result::Result<T, Error>;

/// 把引用位置渲染成一行可读的说明，供错误信息使用。
fn describe(references: &[crate::credential_store::PipelineReference]) -> String {
    references
        .iter()
        .map(|found| format!("{}（第 {} 步）", found.pipeline_name, found.step_order + 1))
        .collect::<Vec<_>>()
        .join("、")
}
