//! 通知模块的错误类型。

/// 通知错误。
///
/// 投递失败把渠道名与原因一起带给调用方：测试端点要逐渠道展示结果，
/// 日志要能定位到具体渠道，缺一不可。
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// 渠道声明的 `provider` 没有对应的内置适配器。
    #[error("未知通知渠道类型: {provider}")]
    UnknownProvider {
        /// 渠道声明的适配器类型标识。
        provider: String,
    },
    /// 渠道类型重复注册。
    #[error("重复注册通知渠道类型: {provider}")]
    DuplicateProvider {
        /// 被重复注册的适配器类型标识。
        provider: String,
    },
    /// 消息投递在重试后仍失败。
    #[error("渠道 {name} 投递失败: {cause}")]
    Delivery {
        /// 渠道名。
        name: String,
        /// 失败原因。
        cause: String,
    },
}

/// 通知模块的结果类型。
pub type Result<T> = std::result::Result<T, Error>;
