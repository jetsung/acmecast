//! 流水线启动契约：调度器只决定「何时触发」，真正把流水线跑起来的动作
//! 由外部注入。
//!
//! 这样切分让调度器的测试不需要真实的流水线引擎（注册表、步骤、凭据、
//! 证书……），只要一个记录调用的假实现；服务端组装时再把它接到
//! 流水线执行器上。

use async_trait::async_trait;

use crate::error::Result;

/// 触发请求：谁、以什么来源、附什么说明。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchRequest {
    /// 被触发的流水线。
    pub pipeline_id: i64,
    /// 触发来源（定时／续期／手动）。
    pub source: acmecast_store::entity::pipeline::TriggerSource,
    /// 触发说明；例如续期触发时写明命中的证书域名。
    pub detail: Option<String>,
}

/// 启动流水线的执行器。
///
/// 实现方负责「创建运行记录、驱动流水线跑完、把结果写回历史」——
/// 调度器对这些一概不知情。启动失败时返回错误，调度器据此决定是否
/// 推进触发时间（失败不推进，下一轮再试）。
#[async_trait]
pub trait PipelineLauncher: Send + Sync + std::fmt::Debug {
    /// 启动一次流水线运行。
    async fn launch(&self, request: LaunchRequest) -> Result<()>;
}
