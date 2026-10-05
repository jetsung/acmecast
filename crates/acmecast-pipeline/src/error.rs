//! 流水线引擎错误类型。

/// `acmecast-pipeline` 的错误类型。
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// 引用了一个本次运行尚未产出的产物。
    ///
    /// 列出已产出的名字：这个错误几乎总是「步骤顺序写反了」或「产物名打错了」，
    /// 而「现在到底有哪些可用」正是此时第一个要问的问题。
    #[error("缺少所需产物 `{name}`；本次运行目前产出的是：{}", if available.is_empty() { "（暂无）".to_owned() } else { available.join("、") })]
    MissingArtifact {
        /// 请求的产物名。
        name: String,
        /// 目前可用的产物名。
        available: Vec<String>,
    },

    /// 步骤输入不满足它声明的定义。
    ///
    /// 带上字段名：spec 要求「返回指明缺失字段名的校验错误」，
    /// 而前端要据此高亮到具体的输入框。
    #[error("步骤输入不合法：字段 `{field}` {reason}")]
    InvalidInput {
        /// 出错的字段，嵌套时形如 `dns.provider`。
        field: String,
        /// 不合法的原因。
        reason: String,
    },

    /// 引用了一个未注册的任务类型。
    ///
    /// 错误里带上已注册的清单：这个错误基本只在配置写错时出现，
    /// 而「到底有哪些可用」正是此时第一个要问的问题。
    #[error("未知的任务类型: {type_id}（已注册: {}）", known.join(", "))]
    UnknownStepType {
        /// 请求的类型标识。
        type_id: String,
        /// 当前已注册的类型标识。
        known: Vec<String>,
    },

    /// 同一个类型标识被注册了两次。
    #[error("任务类型重复注册: {0}")]
    DuplicateStepType(String),

    /// 凭据体系错误。
    #[error(transparent)]
    Access(#[from] acmecast_access::Error),

    /// 持久层错误。
    #[error(transparent)]
    Store(#[from] acmecast_store::Error),

    /// 上游 core 层的错误。
    #[error(transparent)]
    Core(#[from] acmecast_core::Error),
}

/// 本 crate 统一的返回类型别名。
pub type Result<T> = std::result::Result<T, Error>;

impl From<sea_orm::DbErr> for Error {
    /// 数据库错误归到持久层错误之下，与 `acmecast-access` 的处理保持一致。
    fn from(err: sea_orm::DbErr) -> Self {
        Self::Store(acmecast_store::Error::from(err))
    }
}
