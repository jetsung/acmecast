//! 统一事件消息：文本渠道与通用 JSON 渠道共用的消息模型。
//!
//! 素材由 `WebhookEventSink` 查库组装；这里只负责「拿到了素材怎么表达」。
//! 消息不含私钥等凭据内容——域名与目标描述来自流水线配置，天然无敏感信息。

use chrono::{DateTime, Utc};
use serde_json::json;

/// 一条待投递的通知消息。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotificationMessage {
    /// 事件标识（如 `cert.apply`）。
    pub event: String,
    /// 事件标题（如「证书申请成功」）。
    pub title: String,
    /// 流水线名称。
    pub pipeline: String,
    /// 触发来源标识（`manual` / `cron` / `renewal`）。
    pub trigger: String,
    /// 事件发生时间。
    pub occurred_at: DateTime<Utc>,
    /// 申请的域名列表；仅 `cert.apply` 类事件携带。
    pub domains: Vec<String>,
    /// 部署目标描述；仅 `cert.deploy` 类事件携带。
    pub target: Option<String>,
}

impl NotificationMessage {
    /// 渲染为 IM 文本消息。
    ///
    /// 单段纯文本：标题起头，字段逐行列出；缺省的素材（无域名/目标）直接
    /// 省略对应行，不输出「无」之类的占位。
    #[must_use]
    pub fn render_text(&self) -> String {
        let mut lines = vec![
            format!("【acmecast】{}", self.title),
            format!("流水线：{}", self.pipeline),
            format!("触发来源：{}", trigger_label(&self.trigger)),
            format!("时间：{}", self.occurred_at.format("%Y-%m-%d %H:%M:%S UTC")),
        ];
        if !self.domains.is_empty() {
            lines.push(format!("域名：{}", self.domains.join(", ")));
        }
        if let Some(target) = &self.target {
            lines.push(format!("部署目标：{target}"));
        }
        lines.join("\n")
    }

    /// 渲染为飞书（Lark）自定义机器人的 text 消息。
    ///
    /// 签名字段由发送路径按 [`crate::sign::SignKind::Feishu`] 附加，
    /// 这里只负责消息本体。
    #[must_use]
    pub fn feishu_text_json(&self) -> serde_json::Value {
        json!({
            "msg_type": "text",
            "content": { "text": self.render_text() },
        })
    }

    /// 渲染为钉钉自定义机器人的 text 消息。
    ///
    /// 签名走地址查询参数，由发送路径按 [`crate::sign::SignKind::DingTalk`]
    /// 附加，这里只负责消息本体。
    #[must_use]
    pub fn dingtalk_text_json(&self) -> serde_json::Value {
        json!({
            "msgtype": "text",
            "text": { "content": self.render_text() },
        })
    }

    /// 渲染为通用 webhook 的 JSON 负载。
    ///
    /// 字段名对机器友好（触发来源用原始标识而非中文标签），可选字段
    /// 缺省时省略键，消费方按存在性判断即可。
    #[must_use]
    pub fn payload_json(&self) -> serde_json::Value {
        let mut payload = json!({
            "event": self.event,
            "title": self.title,
            "pipeline": self.pipeline,
            "trigger": self.trigger,
            "occurred_at": self.occurred_at.to_rfc3339(),
        });
        if !self.domains.is_empty() {
            payload["domains"] = json!(self.domains);
        }
        if let Some(target) = &self.target {
            payload["target"] = json!(target);
        }
        payload
    }
}

/// 触发来源的中文标签。
///
/// 未知标识原样返回：文案缺失不该让消息丢内容。
#[must_use]
pub fn trigger_label(trigger: &str) -> &str {
    match trigger {
        "manual" => "手动",
        "cron" => "定时",
        "renewal" => "续期",
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::EVENT_CERT_APPLY;
    use chrono::TimeZone;

    fn sample() -> NotificationMessage {
        NotificationMessage {
            event: EVENT_CERT_APPLY.to_owned(),
            title: "证书申请成功".to_owned(),
            pipeline: "example-pipeline".to_owned(),
            trigger: "cron".to_owned(),
            occurred_at: Utc.with_ymd_and_hms(2026, 10, 4, 12, 0, 0).unwrap(),
            domains: vec!["a.example.com".to_owned(), "b.example.com".to_owned()],
            target: None,
        }
    }

    #[test]
    fn text_contains_all_required_fields() {
        let text = sample().render_text();
        assert!(text.contains("证书申请成功"), "{text}");
        assert!(text.contains("example-pipeline"), "{text}");
        assert!(text.contains("定时"), "{text}");
        assert!(text.contains("2026-10-04 12:00:00 UTC"), "{text}");
        assert!(text.contains("a.example.com, b.example.com"), "{text}");
    }

    #[test]
    fn text_omits_missing_sections() {
        let mut message = sample();
        message.domains.clear();
        message.target = Some("SSH host.example.com".to_owned());
        let text = message.render_text();
        assert!(!text.contains("域名"), "{text}");
        assert!(text.contains("部署目标：SSH host.example.com"), "{text}");
    }

    #[test]
    fn text_never_carries_private_key_material() {
        let text = sample().render_text();
        assert!(!text.contains("PRIVATE KEY"), "{text}");
        assert!(!text.to_lowercase().contains("secret"), "{text}");
    }

    #[test]
    fn payload_json_keeps_raw_trigger_and_omits_empty() {
        let payload = sample().payload_json();
        assert_eq!(payload["event"], "cert.apply");
        assert_eq!(payload["trigger"], "cron");
        assert!(payload.get("target").is_none());
        assert_eq!(payload["domains"][0], "a.example.com");
        assert!(
            payload["occurred_at"]
                .as_str()
                .is_some_and(|value| value.ends_with('+')
                    || value.ends_with('Z')
                    || value.contains('+'))
        );
    }

    #[test]
    fn trigger_label_covers_known_sources() {
        assert_eq!(trigger_label("manual"), "手动");
        assert_eq!(trigger_label("cron"), "定时");
        assert_eq!(trigger_label("renewal"), "续期");
        assert_eq!(trigger_label("whatever"), "whatever");
    }
}
