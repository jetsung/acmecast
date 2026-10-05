//! 钉钉自定义机器人适配器。
//!
//! 消息格式为 `text`；配置了 `secret` 时启用官方加签：以密钥对
//! `timestamp + "\n" + secret` 计算 HmacSHA256，base64 后经 URL 编码，
//! 与毫秒时间戳一并附加到 webhook 地址的查询参数。签名方案见
//! [`crate::sign::SignKind::DingTalk`]，发送走统一请求路径。

use async_trait::async_trait;

use crate::channel::{ChannelConfig, NotificationChannel, Signature, apply_sign, send_request};
use crate::error::Result;
use crate::message::NotificationMessage;
use crate::sign::SignKind;

/// 钉钉自定义机器人。
#[derive(Debug, Default)]
pub struct DingTalkChannel;

#[async_trait]
impl NotificationChannel for DingTalkChannel {
    fn provider(&self) -> &'static str {
        "dingtalk"
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
            .map(|secret| Signature::compute(SignKind::DingTalk, secret));
        let body = message.dingtalk_text_json();
        let (url, body) = apply_sign(&channel.url, body, signature.as_ref());
        send_request(client, &url, "POST", &channel.headers, &body).await
    }
}
