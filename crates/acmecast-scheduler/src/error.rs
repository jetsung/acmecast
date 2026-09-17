//! 调度器错误类型。

/// `acmecast-scheduler` 的错误类型。
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// 依赖的存储层出错。
    #[error(transparent)]
    Store(#[from] acmecast_store::Error),

    /// 依赖的流水线层出错。
    #[error(transparent)]
    Pipeline(#[from] acmecast_pipeline::Error),

    /// 数据库层出错。
    #[error("数据库错误: {0}")]
    Database(#[from] sea_orm::DbErr),

    /// 调度配置不合法。
    #[error("调度配置不合法：{0}")]
    InvalidConfig(String),
}

/// 本 crate 的 `Result` 别名。
pub type Result<T> = std::result::Result<T, Error>;
