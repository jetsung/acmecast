//! 飞书（Lark）自定义机器人适配器。
//!
//! 消息格式为 `text`；配置了 `secret` 时启用官方签名校验：以
//! `timestamp + "\n" + secret` 为 HMAC 密钥对空串计算 HmacSHA256，
//! 结果 base64 后与时间戳一并放入请求体。签名方案见
//! [`crate::sign::SignKind::Feishu`]，发送走统一请求路径。

use async_trait::async_trait;

use crate::channel::{ChannelConfig, NotificationChannel, Signature, apply_sign, send_request};
use crate::error::Result;
use crate::message::NotificationMessage;
use crate::sign::SignKind;

/// 飞书自定义机器人。
#[derive(Debug, Default)]
pub struct FeishuChannel;

#[async_trait]
impl NotificationChannel for FeishuChannel {
    fn provider(&self) -> &'static str {
        "feishu"
    }

    async fn send(
        &self,
        client: &reqwest::Client,
        channel: &ChannelConfig,
        message: &NotificationMessage,
    ) -> Result<()> {
        let signature = channel
            .secret
            .as_deref()
            .map(|secret| Signature::compute(SignKind::Feishu, secret));
        let body = message.feishu_text_json();
        let (url, body) = apply_sign(&channel.url, body, signature.as_ref());
        send_request(client, &url, "POST", &channel.headers, &body).await
    }
}
