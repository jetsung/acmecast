//! OpenAPI 文档与交互式 Swagger UI。
//!
//! 下面的空函数是 utoipa 的**文档锚点**：`#[utoipa::path]` 宏从它们身上
//! 收集端点元数据，函数体永远不会被调用——真正的 handler 在 `handlers`
//! 里。新增或修改端点时必须同步这里，`tests/http_conventions.rs` 的
//! 覆盖清单会校验两者一致。

// 文档锚点的函数体不被调用，dead_code 是这类锚点的固有属性。
#![allow(dead_code)]

use utoipa::openapi::OpenApi as OpenApiDoc;
use utoipa::openapi::security::{Http, HttpAuthScheme, SecurityScheme};
use utoipa::{Modify, OpenApi};

use crate::handlers::PageResponse;
use crate::response::ApiResponse;

/// 当前 HTTP API 的 OpenAPI 文档。
#[derive(Debug, OpenApi)]
#[openapi(
    info(
        title = "acmecast API",
        version = "0.1.0",
        description = "证书生命周期、流水线与凭据管理接口"
    ),
    paths(
        healthz,
        login,
        list_pipelines,
        create_pipeline,
        get_pipeline,
        update_pipeline,
        delete_pipeline,
        list_certificates,
        create_certificate,
        get_certificate,
        delete_certificate,
        download_certificate,
        revoke_certificate,
        list_credentials,
        create_credential,
        get_credential,
        update_credential,
        delete_credential,
        test_credential,
        list_credential_types,
        test_notifications,
        list_tasks,
        task_schema,
        list_histories,
        get_history,
        list_pipeline_histories,
        run_pipeline,
        list_schedules,
        create_schedule,
        list_trigger_logs,
        history_logs
    ),
    components(
        schemas(
            crate::response::ApiErrorBody,
            crate::handlers::LoginRequest,
            crate::handlers::LoginResponse,
            crate::handlers::PipelineRequest,
            crate::handlers::PipelineStepRequest,
            crate::handlers::PipelineResponse,
            crate::handlers::PipelineStepResponse,
            crate::handlers::PipelineSummaryResponse,
            crate::handlers::RunResponse,
            crate::handlers::CertificateRequest,
            crate::handlers::CertificateResponse,
            crate::handlers::CertificateDownloadQuery,
            crate::handlers::RevokeRequest,
            crate::handlers::RevokeResponse,
            crate::handlers::ScheduleCreateRequest,
            crate::handlers::ScheduleResponse,
            crate::handlers::TriggerLogResponse,
            crate::handlers::TriggerLogQueryParams,
            crate::handlers::CredentialRequest,
            crate::handlers::CredentialResponse,
            crate::handlers::CredentialDetailResponse,
            crate::handlers::CredentialTypeResponse,
            crate::handlers::TaskTypeResponse,
            crate::handlers::HistoryResponse,
            crate::handlers::HistoryLogResponse,
            crate::handlers::ConnectivityResponse,
            crate::handlers::NotificationTestRequest,
            crate::handlers::NotificationTestResponse,
            crate::handlers::NotificationTestResult,
            PageResponse<crate::handlers::PipelineSummaryResponse>,
            PageResponse<crate::handlers::CertificateResponse>,
            PageResponse<crate::handlers::CredentialResponse>,
            PageResponse<crate::handlers::HistoryResponse>,
            PageResponse<crate::handlers::TriggerLogResponse>,
        )
    ),
    modifiers(&SecurityAddon)
)]
pub struct ApiDoc;

/// 给所有需要鉴权的资源声明 Bearer JWT 安全方案。
struct SecurityAddon;

impl Modify for SecurityAddon {
    fn modify(&self, openapi: &mut OpenApiDoc) {
        let mut http = Http::new(HttpAuthScheme::Bearer);
        http.bearer_format = Some("JWT".to_owned());
        let components = openapi.components.get_or_insert_with(Default::default);
        components.add_security_scheme("bearerAuth", SecurityScheme::Http(http));
    }
}

/// 健康探针的文档占位函数。
#[utoipa::path(get, path = "/healthz", responses((status = 200, description = "服务正常")))]
fn healthz() {}

/// 登录端点的文档占位函数。
#[utoipa::path(post, path = "/api/login", request_body = crate::handlers::LoginRequest, responses((status = 200, body = ApiResponse<crate::handlers::LoginResponse>), (status = 401, body = crate::response::ApiErrorBody), (status = 429, body = crate::response::ApiErrorBody)))]
fn login() {}

#[utoipa::path(get, path = "/api/pipelines", params(("page" = Option<u64>, Query, description = "页码"), ("page_size" = Option<u64>, Query, description = "每页条数"), ("name" = Option<String>, Query, description = "名称模糊过滤"), ("enabled" = Option<bool>, Query, description = "启用状态过滤")), responses((status = 200, body = ApiResponse<PageResponse<crate::handlers::PipelineSummaryResponse>>, description = "流水线分页列表"), (status = 400, body = crate::response::ApiErrorBody)), security(("bearerAuth" = [])))]
fn list_pipelines() {}

#[utoipa::path(post, path = "/api/pipelines", request_body = crate::handlers::PipelineRequest, responses((status = 200, body = ApiResponse<crate::handlers::PipelineResponse>), (status = 400, body = crate::response::ApiErrorBody)), security(("bearerAuth" = [])))]
fn create_pipeline() {}

#[utoipa::path(get, path = "/api/pipelines/{id}", params(("id" = i64, Path, description = "流水线标识")), responses((status = 200, body = ApiResponse<crate::handlers::PipelineResponse>), (status = 404, body = crate::response::ApiErrorBody)), security(("bearerAuth" = [])))]
fn get_pipeline() {}

#[utoipa::path(put, path = "/api/pipelines/{id}", params(("id" = i64, Path, description = "流水线标识")), request_body = crate::handlers::PipelineRequest, responses((status = 200, body = ApiResponse<crate::handlers::PipelineResponse>), (status = 400, body = crate::response::ApiErrorBody)), security(("bearerAuth" = [])))]
fn update_pipeline() {}

#[utoipa::path(delete, path = "/api/pipelines/{id}", params(("id" = i64, Path, description = "流水线标识")), responses((status = 200, description = "删除成功"), (status = 404, body = crate::response::ApiErrorBody)), security(("bearerAuth" = [])))]
fn delete_pipeline() {}

#[utoipa::path(get, path = "/api/certificates", params(("domain" = Option<String>, Query, description = "域名模糊过滤"), ("sort" = Option<String>, Query, description = "ascending / descending"), ("page" = Option<u64>, Query, description = "页码"), ("page_size" = Option<u64>, Query, description = "每页条数")), responses((status = 200, body = ApiResponse<PageResponse<crate::handlers::CertificateResponse>>, description = "证书分页列表")), security(("bearerAuth" = [])))]
fn list_certificates() {}

#[utoipa::path(post, path = "/api/certificates", request_body = crate::handlers::CertificateRequest, responses((status = 200, body = ApiResponse<crate::handlers::CertificateResponse>), (status = 400, body = crate::response::ApiErrorBody)), security(("bearerAuth" = [])))]
fn create_certificate() {}

#[utoipa::path(get, path = "/api/certificates/{id}", params(("id" = i64, Path, description = "证书标识")), responses((status = 200, body = ApiResponse<crate::handlers::CertificateResponse>), (status = 404, body = crate::response::ApiErrorBody)), security(("bearerAuth" = [])))]
fn get_certificate() {}

#[utoipa::path(get, path = "/api/certificates/{id}/download", params(("id" = i64, Path, description = "证书标识"), ("format" = Option<String>, Query, description = "pem / der / pfx / jks / p7b"), ("password" = Option<String>, Query, description = "PFX / JKS 口令")), responses((status = 200, description = "证书文件下载", content_type = "application/octet-stream"), (status = 404, body = crate::response::ApiErrorBody)), security(("bearerAuth" = [])))]
fn download_certificate() {}

#[utoipa::path(delete, path = "/api/certificates/{id}", params(("id" = i64, Path, description = "证书标识")), responses((status = 200, description = "删除成功"), (status = 404, body = crate::response::ApiErrorBody)), security(("bearerAuth" = [])))]
fn delete_certificate() {}

/// 吊销端点的文档占位函数。
#[utoipa::path(post, path = "/api/certificates/{id}/revoke", request_body = crate::handlers::RevokeRequest, responses((status = 200, body = ApiResponse<crate::handlers::RevokeResponse>), (status = 404, body = crate::response::ApiErrorBody), (status = 422, body = crate::response::ApiErrorBody)), security(("bearerAuth" = [])))]
fn revoke_certificate() {}

#[utoipa::path(get, path = "/api/credentials", params(("page" = Option<u64>, Query, description = "页码"), ("page_size" = Option<u64>, Query, description = "每页条数")), responses((status = 200, body = ApiResponse<PageResponse<crate::handlers::CredentialResponse>>, description = "凭据分页列表")), security(("bearerAuth" = [])))]
fn list_credentials() {}

#[utoipa::path(post, path = "/api/credentials", request_body = crate::handlers::CredentialRequest, responses((status = 200, body = ApiResponse<crate::handlers::CredentialResponse>), (status = 400, body = crate::response::ApiErrorBody)), security(("bearerAuth" = [])))]
fn create_credential() {}

#[utoipa::path(get, path = "/api/credentials/{id}", params(("id" = i64, Path, description = "凭据标识")), responses((status = 200, body = ApiResponse<crate::handlers::CredentialDetailResponse>, description = "凭据详情（含解密字段值，供编辑回填）"), (status = 404, body = crate::response::ApiErrorBody)), security(("bearerAuth" = [])))]
fn get_credential() {}

#[utoipa::path(put, path = "/api/credentials/{id}", params(("id" = i64, Path, description = "凭据标识")), request_body = crate::handlers::CredentialRequest, responses((status = 200, body = ApiResponse<crate::handlers::CredentialResponse>), (status = 400, body = crate::response::ApiErrorBody)), security(("bearerAuth" = [])))]
fn update_credential() {}

#[utoipa::path(delete, path = "/api/credentials/{id}", params(("id" = i64, Path, description = "凭据标识")), responses((status = 200, description = "删除成功"), (status = 404, body = crate::response::ApiErrorBody)), security(("bearerAuth" = [])))]
fn delete_credential() {}

#[utoipa::path(post, path = "/api/credentials/{id}/test", params(("id" = i64, Path, description = "凭据标识")), responses((status = 200, body = ApiResponse<crate::handlers::ConnectivityResponse>, description = "连通性结果"), (status = 404, body = crate::response::ApiErrorBody)), security(("bearerAuth" = [])))]
fn test_credential() {}

#[utoipa::path(get, path = "/api/credential-types", responses((status = 200, description = "凭据类型与字段 Schema")), security(("bearerAuth" = [])))]
fn list_credential_types() {}

/// 通知渠道测试的文档占位函数。
#[utoipa::path(post, path = "/api/notifications/test", request_body = Option<crate::handlers::NotificationTestRequest>, responses((status = 200, body = ApiResponse<crate::handlers::NotificationTestResponse>, description = "逐渠道投递结果"), (status = 404, body = crate::response::ApiErrorBody), (status = 409, body = crate::response::ApiErrorBody)), security(("bearerAuth" = [])))]
fn test_notifications() {}

#[utoipa::path(get, path = "/api/tasks", responses((status = 200, description = "任务类型列表")), security(("bearerAuth" = [])))]
fn list_tasks() {}

#[utoipa::path(get, path = "/api/tasks/{type_id}/schema", params(("type_id" = String, Path, description = "任务类型标识")), responses((status = 200, description = "任务输入 Schema"), (status = 404, body = crate::response::ApiErrorBody)), security(("bearerAuth" = [])))]
fn task_schema() {}

#[utoipa::path(get, path = "/api/histories", params(("pipeline_id" = Option<i64>, Query, description = "按流水线过滤"), ("page" = Option<u64>, Query, description = "页码"), ("page_size" = Option<u64>, Query, description = "每页条数")), responses((status = 200, body = ApiResponse<PageResponse<crate::handlers::HistoryResponse>>, description = "运行历史分页列表")), security(("bearerAuth" = [])))]
fn list_histories() {}

/// 调度列表的文档占位函数。
#[utoipa::path(get, path = "/api/schedules", responses((status = 200, body = ApiResponse<Vec<crate::handlers::ScheduleResponse>>, description = "调度配置列表")), security(("bearerAuth" = [])))]
fn list_schedules() {}

/// 新建调度的文档占位函数。
#[utoipa::path(post, path = "/api/schedules", request_body = crate::handlers::ScheduleCreateRequest, responses((status = 200, body = ApiResponse<crate::handlers::ScheduleResponse>, description = "调度配置"), (status = 400, body = crate::response::ApiErrorBody)), security(("bearerAuth" = [])))]
fn create_schedule() {}

/// 触发审计分页查询的文档占位函数。
#[utoipa::path(get, path = "/api/schedules/trigger-logs", params(("pipeline_id" = Option<i64>, Query, description = "按流水线过滤"), ("page" = Option<u64>, Query, description = "页码"), ("page_size" = Option<u64>, Query, description = "每页条数")), responses((status = 200, body = ApiResponse<PageResponse<crate::handlers::TriggerLogResponse>>, description = "触发审计分页列表")), security(("bearerAuth" = [])))]
fn list_trigger_logs() {}

/// 单条运行历史的文档占位函数。
#[utoipa::path(get, path = "/api/histories/{id}", params(("id" = i64, Path, description = "历史主键")), responses((status = 200, body = ApiResponse<crate::handlers::HistoryResponse>), (status = 404, body = crate::response::ApiErrorBody)), security(("bearerAuth" = [])))]
fn get_history() {}

#[utoipa::path(get, path = "/api/pipelines/{id}/histories", params(("id" = i64, Path, description = "流水线标识")), responses((status = 200, description = "流水线运行历史分页列表")), security(("bearerAuth" = [])))]
fn list_pipeline_histories() {}

/// 手动触发流水线的文档占位函数。
#[utoipa::path(post, path = "/api/pipelines/{id}/run", params(("id" = i64, Path, description = "流水线标识")), responses((status = 200, body = ApiResponse<crate::handlers::RunResponse>, description = "已开始运行，返回运行历史标识"), (status = 400, body = crate::response::ApiErrorBody, description = "流水线已停用"), (status = 404, body = crate::response::ApiErrorBody), (status = 409, body = crate::response::ApiErrorBody, description = "流水线正在运行中")), security(("bearerAuth" = [])))]
fn run_pipeline() {}

#[utoipa::path(get, path = "/api/histories/{id}/logs", params(("id" = i64, Path, description = "历史主键")), responses((status = 200, body = ApiResponse<Vec<crate::handlers::HistoryLogResponse>>, description = "步骤日志"), (status = 404, body = crate::response::ApiErrorBody)), security(("bearerAuth" = [])))]
fn history_logs() {}
