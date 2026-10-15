//! 证书部署错误类型。

/// `acmecast-deploy` 的错误类型。
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// 引用了一个未注册的部署目标类型。
    ///
    /// 附带已注册的清单：这个错误几乎只在配置写错时出现，
    /// 而「到底有哪些可用」正是此时第一个要问的问题。
    #[error("未知的部署目标类型: {type_id}（已注册: {}）", known.join(", "))]
    UnknownTargetType {
        /// 请求的类型标识。
        type_id: String,
        /// 当前已注册的类型标识。
        known: Vec<String>,
    },

    /// 同一个类型标识被注册了两次。
    #[error("部署目标类型重复注册: {0}")]
    DuplicateTargetType(String),

    /// 部署输入不满足目标声明的要求。
    #[error("部署输入不合法：字段 `{field}` {reason}")]
    InvalidInput {
        /// 出错的字段名。
        field: String,
        /// 不合法的原因。
        reason: String,
    },

    /// 写入目标失败。
    #[error("写入 {path} 失败: {reason}")]
    Write {
        /// 目标路径。
        path: String,
        /// 失败原因。
        reason: String,
    },

    /// 重载命令执行失败。
    ///
    /// 带上命令的输出：重载脚本往往会把「配置里哪一行有问题」打进 stderr，
    /// 只说「重载失败」等于把最有用的信息丢了。
    #[error("重载命令 `{command}` 执行失败（退出码 {}）: {output}", exit_code.map_or_else(|| "未知".to_owned(), |code| code.to_string()))]
    Reload {
        /// 执行的命令。
        command: String,
        /// 退出码。
        exit_code: Option<i32>,
        /// 命令输出（stdout 与 stderr）。
        output: String,
    },

    /// 与远程主机建立连接或通信失败。
    #[error("远程主机操作失败: {0}")]
    Remote(String),

    /// 凭据体系错误。
    #[error(transparent)]
    Access(#[from] acmecast_access::Error),

    /// 文件系统错误。
    #[error("文件操作失败: {0}")]
    Io(#[from] std::io::Error),

    /// 数据库错误（读写部署记录时）。
    #[error(transparent)]
    Db(#[from] sea_orm::DbErr),

    /// 上游 core 层的错误。
    #[error(transparent)]
    Core(#[from] acmecast_core::Error),
}

impl Error {
    /// 构造一条「输入字段不合法」。
    pub fn invalid_input(field: impl Into<String>, reason: impl Into<String>) -> Self {
        Self::InvalidInput {
            field: field.into(),
            reason: reason.into(),
        }
    }
}

/// 本 crate 统一的返回类型别名。
pub type Result<T> = std::result::Result<T, Error>;
