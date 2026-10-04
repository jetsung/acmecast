//! 投递执行器：退避重试与可观测日志的统一出口。
//!
//! 通知是尽力而为的旁路：成功记 info、最终失败记 warn，中间尝试只记
//! debug——排障要的是「哪条没送到、为什么」，不是每次尝试刷一屏。

use std::time::Duration;

use crate::channel::{ChannelConfig, ChannelRegistry};
use crate::error::{Error, Result};
use crate::message::NotificationMessage;

/// 投递用的 HTTP 客户端。
///
/// 超时挂在客户端上：所有渠道共享同一个客户端实例，每次请求独立计时，
/// 上限即 [`DEFAULT_TIMEOUT`]。
#[must_use]
pub fn http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(DEFAULT_TIMEOUT)
        .build()
        .expect("默认 HTTP 客户端配置应可构建")
}

/// 单次请求的超时上限。
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);

/// 投递参数。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeliveryOptions {
    /// 最大尝试次数（含首次）。
    pub max_attempts: u32,
    /// 退避基数：第 n 次失败后等待 `backoff_base * 2^(n-1)`。
    pub backoff_base: Duration,
}

impl Default for DeliveryOptions {
    fn default() -> Self {
        Self {
            max_attempts: 3,
            backoff_base: Duration::from_secs(1),
        }
    }
}

/// 向一个渠道投递消息：按类型找到适配器，重试与退避统一施加。
///
/// 返回的 [`Result`] 与日志内容一致——调用方（事件旁路、测试端点）
/// 各取所需。
pub async fn deliver(
    client: &reqwest::Client,
    registry: &ChannelRegistry,
    channel: &ChannelConfig,
    message: &NotificationMessage,
    options: &DeliveryOptions,
) -> Result<()> {
    let adapter = registry
        .get(&channel.provider)
        .ok_or_else(|| Error::UnknownProvider {
            provider: channel.provider.clone(),
        })?;

    let mut attempt = 0u32;
    loop {
        attempt += 1;
        match adapter.send(client, channel, message).await {
            Ok(()) => {
                tracing::info!(
                    channel = %channel.name,
                    provider = %channel.provider,
                    event = %message.event,
                    attempt,
                    "webhook 通知投递成功"
                );
                return Ok(());
            }
            Err(error) if attempt < options.max_attempts => {
                let wait = options.backoff_base * 2u32.pow(attempt - 1);
                tracing::debug!(
                    channel = %channel.name,
                    attempt,
                    %error,
                    wait_ms = wait.as_millis() as u64,
                    "webhook 通知投递失败，退避后重试"
                );
                tokio::time::sleep(wait).await;
            }
            Err(error) => {
                tracing::warn!(
                    channel = %channel.name,
                    provider = %channel.provider,
                    event = %message.event,
                    attempts = attempt,
                    cause = %error,
                    "webhook 通知投递最终失败"
                );
                return Err(Error::Delivery {
                    name: channel.name.clone(),
                    cause: error.to_string(),
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::channel::default_registry;

    fn channel(provider: &str) -> ChannelConfig {
        ChannelConfig {
            name: "测试渠道".to_owned(),
            provider: provider.to_owned(),
            url: "http://127.0.0.1:9/hook".to_owned(),
            secret: None,
            events: vec!["cert.apply".to_owned()],
            enabled: true,
            sign: None,
            method: "POST".to_owned(),
            headers: std::collections::BTreeMap::new(),
            body_template: None,
        }
    }

    fn message_sample() -> NotificationMessage {
        NotificationMessage {
            event: "cert.apply".to_owned(),
            title: "证书申请成功".to_owned(),
            pipeline: "示例".to_owned(),
            trigger: "manual".to_owned(),
            occurred_at: chrono::Utc::now(),
            domains: vec![],
            target: None,
        }
    }

    #[tokio::test(start_paused = true)]
    async fn unknown_provider_fails_without_retry() {
        let start = tokio::time::Instant::now();
        let error = deliver(
            &http_client(),
            &default_registry(),
            &channel("no-such"),
            &message_sample(),
            &DeliveryOptions::default(),
        )
        .await
        .unwrap_err();
        assert!(matches!(error, Error::UnknownProvider { .. }), "{error}");
        assert_eq!(start.elapsed(), Duration::ZERO, "未知类型不应消耗退避时间");
    }

    #[tokio::test(start_paused = true)]
    async fn unreachable_endpoint_exhausts_backoff() {
        // 127.0.0.1:9（discard 端口）连接立即被拒；暂停时钟下退避零开销。
        let options = DeliveryOptions {
            max_attempts: 3,
            backoff_base: Duration::from_secs(1),
        };
        let error = deliver(
            &http_client(),
            &default_registry(),
            &channel("generic"),
            &message_sample(),
            &options,
        )
        .await
        .unwrap_err();
        let Error::Delivery { name, cause } = &error else {
            panic!("应为投递错误: {error}");
        };
        assert_eq!(name, "测试渠道");
        assert!(!cause.is_empty());
    }
}
