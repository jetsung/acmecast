//! webhook 通知：把证书生命周期事件推送到外部 IM 群机器人。
//!
//! 职责分三层：[`NotificationChannel`] 适配器隔离各平台的消息格式与签名细节；
//! [`WebhookEventSink`] 订阅流水线事件、按渠道订阅过滤并组装消息素材；
//! [`delivery::deliver`] 统一超时、退避重试与可观测日志。引擎只保证
//! 「事件已交给订阅者」，投递在本 crate 内自行 spawn，不阻塞流水线。
//!
//! 本期事件词汇：`cert.apply`（证书申请成功）与 `cert.deploy`（部署成功）；
//! 订阅模型按事件标识列表设计，新增事件只需扩常量与标题映射。

#![allow(missing_docs)]

pub mod channel;
pub mod delivery;
pub mod dingtalk;
pub mod error;
pub mod feishu;
pub mod generic;
pub mod message;
pub mod sign;
pub mod sink;

pub use channel::{ChannelConfig, ChannelRegistry, NotificationChannel, default_registry};
pub use delivery::{DEFAULT_TIMEOUT, DeliveryOptions, deliver, http_client};
pub use dingtalk::DingTalkChannel;
pub use error::{Error, Result};
pub use feishu::FeishuChannel;
pub use generic::{ALLOWED_METHODS, GenericChannel, TEMPLATE_VARIABLES, validate_channel};
pub use message::NotificationMessage;
pub use sign::{SIGN_DINGTALK, SIGN_FEISHU, SIGN_KINDS, SignKind};
pub use sink::{EVENT_CERT_APPLY, EVENT_CERT_DEPLOY, WebhookEventSink, event_title};
