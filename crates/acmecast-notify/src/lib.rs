//! 通知渠道：把流水线运行结果推送到外部 IM 群机器人或邮箱收件人。
//!
//! 职责分三层：[`NotificationChannel`] 适配器隔离各平台的消息格式与签名
//! 细节（webhook 三种，邮件走 SMTP）；[`WebhookEventSink`] 订阅流水线
//! 运行结束事件、按流水线的 `notification_channels` 解析凭据并组装简讯；
//! [`delivery::deliver`] 统一超时、退避重试与可观测日志。引擎只保证
//! 「事件已交给订阅者」，投递在本 crate 内自行 spawn，不阻塞流水线。
//!
//! 事件词汇：`pipeline.succeeded`（运行成功）与 `pipeline.failed`（运行
//! 失败）；通知渠道作为凭据记录加密存储（见 [`notify_credential`]），
//! 不再有 `config.toml` 渠道与逐步骤推送。

#![allow(missing_docs)]

pub mod channel;
pub mod delivery;
pub mod dingtalk;
pub mod email;
pub mod error;
pub mod feishu;
pub mod generic;
pub mod message;
pub mod notify_credential;
pub mod sign;
pub mod sink;
pub mod smtp_proxy;

pub use channel::{ChannelConfig, ChannelRegistry, NotificationChannel, default_registry};
pub use delivery::{DEFAULT_TIMEOUT, DeliveryOptions, deliver, http_client};
pub use dingtalk::DingTalkChannel;
pub use email::{EmailChannel, SmtpSettings, TlsMode};
pub use error::{Error, Result};
pub use feishu::FeishuChannel;
pub use generic::{ALLOWED_METHODS, GenericChannel, TEMPLATE_VARIABLES, validate_channel};
pub use message::{
    FailureDetail, NotificationMessage, SummaryDetail, SummaryStep, SummaryStepStatus,
};
pub use notify_credential::{
    DINGTALK_TYPE_ID, DingtalkCredentialType, EMAIL_TYPE_ID, EmailCredentialType, FEISHU_TYPE_ID,
    FeishuCredentialType, GENERIC_TYPE_ID, GenericCredentialType, NOTIFY_TYPE_IDS,
    channel_config_from_credential, is_notify_type_id,
};
pub use sign::{SIGN_DINGTALK, SIGN_FEISHU, SIGN_KINDS, SignKind};
pub use sink::{
    EVENT_CERT_APPLY, EVENT_PIPELINE_FAILED, EVENT_PIPELINE_SUCCEEDED, WebhookEventSink,
    event_title,
};
