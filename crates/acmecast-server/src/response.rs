//! HTTP 响应与错误封装。
//!
//! 所有 API 成功响应都包在 `{ "data": ... }` 中；错误响应都包在
//! `{ "error": { "code", "message", "field" } }` 中。这样前端无需按端点
//! 猜测响应形状，也不会因为某个 handler 直接返回字符串而失去机器可读错误码。

use axum::{
    Json,
    extract::{FromRequest, FromRequestParts, Query, rejection::JsonRejection},
    http::{StatusCode, request::Parts},
    response::{IntoResponse, Response},
};
use serde::{Serialize, de::DeserializeOwned};
use utoipa::ToSchema;

/// 统一成功响应。
#[derive(Debug, Clone, Serialize, PartialEq, ToSchema)]
pub struct ApiResponse<T: ToSchema> {
    /// 端点返回的数据。
    pub data: T,
}

impl<T: ToSchema> ApiResponse<T> {
    /// 用数据构造成功响应。
    #[must_use]
    pub fn new(data: T) -> Self {
        Self { data }
    }
}

impl<T> IntoResponse for ApiResponse<T>
where
    T: Serialize + ToSchema,
{
    fn into_response(self) -> Response {
        Json(self).into_response()
    }
}

/// 返回统一错误的 JSON 请求体提取器。
#[derive(Debug)]
pub struct ApiJson<T>(
    /// 已反序列化的请求体。
    pub T,
);

impl<S, T> FromRequest<S> for ApiJson<T>
where
    S: Send + Sync,
    T: DeserializeOwned,
{
    type Rejection = ApiError;

    async fn from_request(
        request: axum::extract::Request,
        state: &S,
    ) -> Result<Self, Self::Rejection> {
        Json::<T>::from_request(request, state)
            .await
            .map(|Json(value)| Self(value))
            .map_err(|rejection: JsonRejection| {
                // 体积超限是 413，其余反序列化问题才是 400——沿用提取器
                // 给出的状态码，否则超限会被报成「JSON 不合法」，误导排查方向。
                let status = rejection.status();
                let code = if status == StatusCode::PAYLOAD_TOO_LARGE {
                    "payload_too_large"
                } else {
                    "invalid_json"
                };
                ApiError::new(status, code, rejection.body_text())
            })
    }
}

/// 返回统一错误的查询参数提取器。
#[derive(Debug)]
pub struct ApiQuery<T>(
    /// 已反序列化的查询参数。
    pub T,
);

impl<S, T> FromRequestParts<S> for ApiQuery<T>
where
    S: Send + Sync,
    T: DeserializeOwned,
{
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        Query::<T>::try_from_uri(&parts.uri)
            .map(|query| Self(query.0))
            .map_err(|rejection| {
                ApiError::new(
                    StatusCode::BAD_REQUEST,
                    "invalid_query",
                    rejection.to_string(),
                )
            })
    }
}

/// 统一错误响应的内层对象。
#[derive(Debug, Clone, Serialize, PartialEq, Eq, ToSchema)]
pub struct ApiErrorBody {
    /// 稳定的机器可读错误码。
    pub code: String,
    /// 可直接展示给用户的人类可读消息。
    pub message: String,
    /// 校验错误对应的字段；非字段错误为 `None`。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub field: Option<String>,
}

/// HTTP 层错误。
#[derive(Debug, Clone)]
pub struct ApiError {
    status: StatusCode,
    body: ApiErrorBody,
}

impl ApiError {
    /// 构造带状态码和错误码的错误。
    #[must_use]
    pub fn new(status: StatusCode, code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            status,
            body: ApiErrorBody {
                code: code.into(),
                message: message.into(),
                field: None,
            },
        }
    }

    /// 构造字段校验错误。
    #[must_use]
    pub fn validation(field: impl Into<String>, message: impl Into<String>) -> Self {
        let field = field.into();
        Self {
            status: StatusCode::BAD_REQUEST,
            body: ApiErrorBody {
                code: "validation_error".to_owned(),
                message: message.into(),
                field: Some(field),
            },
        }
    }

    /// 取 HTTP 状态码。
    #[must_use]
    pub fn status(&self) -> StatusCode {
        self.status
    }

    /// 取统一错误体。
    #[must_use]
    pub fn body(&self) -> &ApiErrorBody {
        &self.body
    }

    /// 取错误码。
    #[must_use]
    pub fn code(&self) -> &str {
        &self.body.code
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.status, Json(serde_json::json!({ "error": self.body }))).into_response()
    }
}

impl From<acmecast_core::Error> for ApiError {
    fn from(error: acmecast_core::Error) -> Self {
        match error {
            acmecast_core::Error::Validation { field, reason } => Self::validation(field, reason),
            acmecast_core::Error::NotFound { entity, id } => Self::new(
                StatusCode::NOT_FOUND,
                "not_found",
                format!("{entity} 不存在: {id}"),
            ),
            acmecast_core::Error::Conflict(message) => {
                Self::new(StatusCode::CONFLICT, "conflict", message)
            }
            acmecast_core::Error::Unauthorized(message) => {
                Self::new(StatusCode::UNAUTHORIZED, "unauthorized", message)
            }
            acmecast_core::Error::Config(message) => Self::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "configuration_error",
                message,
            ),
            acmecast_core::Error::Timeout(message) => {
                Self::new(StatusCode::GATEWAY_TIMEOUT, "timeout", message)
            }
            acmecast_core::Error::External(message) => {
                Self::new(StatusCode::BAD_GATEWAY, "external_service_error", message)
            }
            acmecast_core::Error::Decryption(_) => Self::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "credential_decryption_error",
                "凭据无法解密",
            ),
            acmecast_core::Error::Crypto(message) => Self::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "cryptography_error",
                message,
            ),
            acmecast_core::Error::Io(message) => Self::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "io_error",
                message.to_string(),
            ),
            acmecast_core::Error::Serialization(message) => {
                Self::new(StatusCode::BAD_REQUEST, "serialization_error", message)
            }
            acmecast_core::Error::Internal(message) => {
                Self::new(StatusCode::INTERNAL_SERVER_ERROR, "internal_error", message)
            }
            _ => Self::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal_error",
                "发生未识别的内部错误",
            ),
        }
    }
}

impl From<acmecast_store::Error> for ApiError {
    fn from(error: acmecast_store::Error) -> Self {
        match error {
            acmecast_store::Error::Validation(message) => {
                Self::new(StatusCode::BAD_REQUEST, "validation_error", message)
            }
            acmecast_store::Error::Conflict(message) => {
                Self::new(StatusCode::CONFLICT, "conflict", message)
            }
            acmecast_store::Error::NotFound { entity, id } => Self::new(
                StatusCode::NOT_FOUND,
                "not_found",
                format!("{entity} 不存在: {id}"),
            ),
            acmecast_store::Error::MissingField(message) => {
                Self::new(StatusCode::UNPROCESSABLE_ENTITY, "missing_field", message)
            }
            acmecast_store::Error::Core(error) => error.into(),
            acmecast_store::Error::Database(error) => {
                tracing::error!(%error, "数据库操作失败");
                Self::new(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "database_error",
                    "数据库操作失败",
                )
            }
            acmecast_store::Error::Io(error) => {
                tracing::error!(%error, "文件操作失败");
                Self::new(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "io_error",
                    "文件操作失败",
                )
            }
            acmecast_store::Error::Migration { version, source } => {
                tracing::error!(%source, migration = %version, "数据库迁移失败");
                Self::new(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "migration_error",
                    format!("数据库迁移失败于版本 {version}"),
                )
            }
            acmecast_store::Error::Config(message) => Self::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "configuration_error",
                message,
            ),
            _ => Self::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal_error",
                "发生未识别的内部错误",
            ),
        }
    }
}

impl From<acmecast_access::Error> for ApiError {
    fn from(error: acmecast_access::Error) -> Self {
        match error {
            acmecast_access::Error::UnknownType { requested, known } => Self::new(
                StatusCode::BAD_REQUEST,
                "unknown_credential_type",
                format!(
                    "未知的凭据类型: {requested}（已注册: {}）",
                    known.join(", ")
                ),
            ),
            acmecast_access::Error::DuplicateType(type_id) => {
                Self::new(StatusCode::CONFLICT, "duplicate_credential_type", type_id)
            }
            acmecast_access::Error::CredentialInUse {
                credential_id,
                referenced_by,
            } => Self::new(
                StatusCode::CONFLICT,
                "credential_in_use",
                format!(
                    "凭据 {credential_id} 仍被 {} 条流水线引用",
                    referenced_by.len()
                ),
            ),
            acmecast_access::Error::Store(error) => error.into(),
            acmecast_access::Error::Core(error) => error.into(),
            _ => Self::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal_error",
                "发生未识别的内部错误",
            ),
        }
    }
}

impl From<acmecast_pipeline::Error> for ApiError {
    fn from(error: acmecast_pipeline::Error) -> Self {
        match error {
            acmecast_pipeline::Error::InvalidInput { field, reason } => {
                Self::validation(field, reason)
            }
            acmecast_pipeline::Error::UnknownStepType { type_id, known } => Self::new(
                StatusCode::BAD_REQUEST,
                "unknown_step_type",
                format!("未知的任务类型: {type_id}（已注册: {}）", known.join(", ")),
            ),
            acmecast_pipeline::Error::DuplicateStepType(type_id) => {
                Self::new(StatusCode::CONFLICT, "duplicate_step_type", type_id)
            }
            acmecast_pipeline::Error::MissingArtifact { name, available } => Self::new(
                StatusCode::UNPROCESSABLE_ENTITY,
                "missing_artifact",
                format!("缺少所需产物 `{name}`；可用产物：{}", available.join("、")),
            ),
            acmecast_pipeline::Error::Access(error) => error.into(),
            acmecast_pipeline::Error::Store(error) => error.into(),
            acmecast_pipeline::Error::Core(error) => error.into(),
            _ => Self::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal_error",
                "发生未识别的内部错误",
            ),
        }
    }
}

impl From<acmecast_cert::Error> for ApiError {
    fn from(error: acmecast_cert::Error) -> Self {
        Self::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "certificate_error",
            error.to_string(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validation_error_is_bad_request_and_keeps_field() {
        let error = ApiError::from(acmecast_core::Error::validation("domains", "不能为空"));
        assert_eq!(error.status(), StatusCode::BAD_REQUEST);
        assert_eq!(error.code(), "validation_error");
        assert_eq!(error.body().field.as_deref(), Some("domains"));
    }

    #[test]
    fn not_found_error_is_not_found() {
        let error = ApiError::from(acmecast_core::Error::not_found("证书", "7"));
        assert_eq!(error.status(), StatusCode::NOT_FOUND);
        assert_eq!(error.code(), "not_found");
    }

    #[tokio::test]
    async fn response_is_json_with_a_stable_error_shape() {
        let response = ApiError::validation("name", "不能为空").into_response();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body = axum::body::to_bytes(response.into_body(), 4096)
            .await
            .expect("应能读取响应体");
        let json: serde_json::Value = serde_json::from_slice(&body).expect("响应应为 JSON");
        assert_eq!(json["error"]["code"], "validation_error");
        assert_eq!(json["error"]["field"], "name");
    }
}
