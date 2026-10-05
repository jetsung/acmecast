//! 持久化层错误类型。

/// `acmecast-store` 的错误类型。
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// 数据库连接或查询失败。
    #[error("数据库错误: {0}")]
    Database(#[from] sea_orm::DbErr),

    /// 数据目录的文件操作失败。
    #[error("文件操作失败: {0}")]
    Io(#[from] std::io::Error),

    /// 数据库迁移失败，并指明是哪一条迁移版本挂掉的。
    ///
    /// SeaORM 自己抛出的 `DbErr` 不含迁移版本——版本名只出现在日志里。
    /// 但「哪一步失败」是运维排障时第一个要问的问题，因此本层把它补进错误。
    #[error("迁移失败于版本 {version}: {source}")]
    Migration {
        /// 失败的迁移版本名；无法从版本表推导时为占位说明。
        version: String,
        /// 底层数据库错误。
        #[source]
        source: sea_orm::DbErr,
    },

    /// 配置项非法，例如无法识别的数据库方言。
    #[error("配置错误: {0}")]
    Config(String),

    /// 待写入的数据不满足业务约束，例如证书没有覆盖任何域名。
    #[error("校验失败: {0}")]
    Validation(String),

    /// 与既有数据冲突，例如同一指纹的证书已存在。
    #[error("数据冲突: {0}")]
    Conflict(String),

    /// 目标记录不存在。
    #[error("{entity} 不存在: {id}")]
    NotFound {
        /// 业务对象类型，如「证书」。
        entity: String,
        /// 对象标识。
        id: String,
    },

    /// 记录存在，但缺少完成该操作所必需的信息。
    ///
    /// 典型场景：手动上传的证书没有绑定 ACME 账号，因而无法确定签发账号、无法吊销。
    #[error("记录缺少必要信息: {0}")]
    MissingField(String),

    /// 上游 core 层的错误。
    #[error(transparent)]
    Core(#[from] acmecast_core::Error),
}

impl Error {
    /// 构造 [`Error::NotFound`]。
    pub fn not_found(entity: impl Into<String>, id: impl Into<String>) -> Self {
        Self::NotFound {
            entity: entity.into(),
            id: id.into(),
        }
    }

    /// 构造 [`Error::MissingField`]。
    pub fn missing_field(reason: impl Into<String>) -> Self {
        Self::MissingField(reason.into())
    }
}

/// 本 crate 统一的返回类型别名。
pub type Result<T> = std::result::Result<T, Error>;
