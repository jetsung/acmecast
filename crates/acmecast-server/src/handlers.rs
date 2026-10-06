//! REST 资源端点。
//!
//! handler 只负责 HTTP DTO 与领域仓储之间的转换。数据库查询、证书去重、凭据解密
//! 等规则仍由各自 crate 的仓储或服务实现，避免 HTTP 层复制业务语义。

use std::sync::Arc;

use acmecast_access::{
    AcmeAccountFields, AcmeAccountType, ConnectivityOutcome, CredentialRegistry, CredentialStore,
};
use acmecast_acme::AcmeService;
use acmecast_pipeline::{HistoryQuery, HistoryRepository, StepRegistry};
use acmecast_store::{
    CertFilePaths, CertInput, CertQuery, CertRepository, CertSort, DEFAULT_PAGE_SIZE, FileStore,
    MAX_PAGE_SIZE, PipelineInput, PipelineRepository, PipelineStepInput,
};
use axum::{
    Router,
    body::Body,
    extract::{Path, State},
    http::{HeaderValue, StatusCode, header},
    response::Response,
    routing::{get, post},
};
use chrono::{DateTime, Utc};
use sea_orm::{ActiveModelTrait, EntityTrait, QueryOrder, Set};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::{
    HttpState,
    auth::{AuthMode, ClientIp, issue_token},
    credentials::{
        AliyunCredentialType, CloudflareCredentialType, SshHostCredentialType,
        TencentCredentialType, TencentEoCredentialType,
    },
    response::{ApiError, ApiJson, ApiQuery, ApiResponse},
};

/// PFX / JKS 未指定口令时的约定默认值。
const DEFAULT_KEYSTORE_PASSWORD: &str = "changeit";

/// 装配资源路由。
pub(crate) fn routes() -> Router<HttpState> {
    Router::new()
        .route("/api/login", post(login))
        .route("/api/pipelines", get(list_pipelines).post(create_pipeline))
        .route(
            "/api/pipelines/{id}",
            get(get_pipeline)
                .put(update_pipeline)
                .delete(delete_pipeline),
        )
        .route(
            "/api/pipelines/{id}/histories",
            get(list_pipeline_histories),
        )
        .route("/api/pipelines/{id}/run", post(run_pipeline))
        .route(
            "/api/certificates",
            get(list_certificates).post(create_certificate),
        )
        .route(
            "/api/certificates/{id}",
            get(get_certificate).delete(delete_certificate),
        )
        .route("/api/certificates/{id}/download", get(download_certificate))
        .route("/api/certificates/{id}/revoke", post(revoke_certificate))
        .route(
            "/api/credentials",
            get(list_credentials).post(create_credential),
        )
        .route(
            "/api/credentials/{id}",
            get(get_credential)
                .put(update_credential)
                .delete(delete_credential),
        )
        .route("/api/credentials/{id}/test", post(test_credential))
        .route("/api/credential-types", get(list_credential_types))
        .route("/api/notifications/test", post(test_notifications))
        .route("/api/schedules", get(list_schedules).post(create_schedule))
        .route("/api/schedules/trigger-logs", get(list_trigger_logs))
        .route("/api/tasks", get(list_tasks))
        .route("/api/tasks/{type_id}/schema", get(task_schema))
        .route("/api/histories", get(list_histories))
        .route("/api/histories/{id}", get(get_history))
        .route("/api/histories/{id}/logs", get(history_logs))
}

/// 登录请求 DTO。
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub(crate) struct LoginRequest {
    /// 管理员用户名。
    pub username: String,
    /// 管理员口令。
    pub password: String,
}

/// 登录响应 DTO。
#[derive(Debug, Clone, Serialize, ToSchema)]
pub(crate) struct LoginResponse {
    /// 访问令牌。
    pub token: String,
    /// 令牌类型，固定 `Bearer`。
    pub token_type: &'static str,
    /// 有效期（秒）。
    pub expires_in: u64,
}

/// 管理员登录：校验口令后签发有时效的访问令牌。
///
/// 连续失败按来源限流；错误响应统一为「用户名或密码错误」，
/// 不泄露用户名是否存在。
async fn login(
    State(state): State<HttpState>,
    ClientIp(client): ClientIp,
    ApiJson(request): ApiJson<LoginRequest>,
) -> Result<ApiResponse<LoginResponse>, ApiError> {
    let AuthMode::Enforced(auth) = &state.runtime.auth else {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "auth_disabled",
            "鉴权已关闭，无需登录",
        ));
    };
    let now = Utc::now();
    if let Err(remaining) = auth.limiter.check(&client, now) {
        tracing::warn!(client = %client, remaining, "登录失败次数过多，来源被临时锁定");
        return Err(ApiError::new(
            StatusCode::TOO_MANY_REQUESTS,
            "too_many_attempts",
            format!("登录失败次数过多，请在 {remaining} 秒后重试"),
        ));
    }
    if !auth.config.login_available() {
        return Err(ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "configuration_error",
            "未配置管理员口令哈希（ACMECAST_ADMIN_PASSWORD_HASH 或 \
             ACMECAST_ADMIN_PASSWORD_HASH_FILE），无法登录",
        ));
    }
    if !auth.verify_credentials(&request.username, &request.password) {
        auth.limiter.record_failure(&client, now);
        tracing::warn!(client = %client, "登录失败");
        return Err(ApiError::new(
            StatusCode::UNAUTHORIZED,
            "invalid_credentials",
            "用户名或密码错误",
        ));
    }
    auth.limiter.record_success(&client);
    let token = issue_token(&auth.config, now).map_err(ApiError::from)?;
    tracing::info!(client = %client, user = %auth.config.admin_username, "登录成功");
    Ok(ApiResponse::new(LoginResponse {
        token,
        token_type: "Bearer",
        expires_in: auth.config.token_ttl.as_secs(),
    }))
}

/// 证书下载查询参数。
#[derive(Debug, Clone, Deserialize, Default, ToSchema)]
pub(crate) struct CertificateDownloadQuery {
    /// 目标格式：`pem`（默认）、`der`、`pfx`、`jks`、`p7b`。
    pub format: Option<String>,
    /// PFX / JKS 的口令；未设置时使用约定默认值。
    pub password: Option<String>,
}

/// 下载指定格式的证书。
///
/// 格式转换在内存中完成，不改动仓库里已持久化的原始 PEM。
async fn download_certificate(
    State(state): State<HttpState>,
    Path(id): Path<i64>,
    ApiQuery(query): ApiQuery<CertificateDownloadQuery>,
) -> Result<Response, ApiError> {
    let cert = CertRepository::new(&state.app.db)
        .find(id)
        .await
        .map_err(ApiError::from)?
        .ok_or_else(|| {
            ApiError::new(
                StatusCode::NOT_FOUND,
                "not_found",
                format!("证书不存在: {id}"),
            )
        })?;

    let store = FileStore::open(&state.app.config.data_dir)
        .await
        .map_err(ApiError::from)?;
    let cert_pem = store
        .read(&cert.cert_pem_path)
        .await
        .map_err(ApiError::from)?;
    let key_pem = store
        .read(&cert.key_pem_path)
        .await
        .map_err(ApiError::from)?;

    let format = query
        .format
        .as_deref()
        .unwrap_or("pem")
        .trim()
        .to_ascii_lowercase();
    let password = query
        .password
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| DEFAULT_KEYSTORE_PASSWORD.to_owned());
    let alias = "acmecast";

    let (bytes, content_type, extension) = match format.as_str() {
        "pem" | "crt" => (
            cert_pem.clone().into_bytes(),
            "application/x-pem-file",
            "pem",
        ),
        "der" => (
            acmecast_cert::first_der(&cert_pem).map_err(ApiError::from)?,
            "application/pkix-cert",
            "der",
        ),
        "pfx" | "p12" => (
            acmecast_cert::to_pfx_default(&cert_pem, &key_pem, &password, alias)
                .map_err(ApiError::from)?,
            "application/x-pkcs12",
            "pfx",
        ),
        "jks" => (
            acmecast_cert::to_jks(&cert_pem, &key_pem, &password, alias).map_err(ApiError::from)?,
            "application/x-java-keystore",
            "jks",
        ),
        "p7b" => (
            acmecast_cert::to_p7b_der(&cert_pem).map_err(ApiError::from)?,
            "application/pkcs7-mime",
            "p7b",
        ),
        other => {
            return Err(ApiError::validation(
                "format",
                format!("不支持的证书格式 `{other}`，可用：pem、der、pfx、jks、p7b"),
            ));
        }
    };

    let leaf = cert
        .domain_set()
        .into_iter()
        .next()
        .unwrap_or_else(|| "certificate".to_owned());
    let filename = format!("{}.{extension}", sanitize_filename(&leaf));
    let disposition = HeaderValue::from_str(&format!("attachment; filename=\"{filename}\""))
        .map_err(|error| {
            ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal_error",
                error.to_string(),
            )
        })?;

    let mut response = Response::new(Body::from(bytes));
    let headers = response.headers_mut();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    headers.insert(header::CONTENT_DISPOSITION, disposition);
    Ok(response)
}

/// 吊销请求体：可选吊销原因（RFC 5280 的 RevocationReason）。
#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
pub(crate) struct RevokeRequest {
    /// 吊销原因代码（0..=10，RFC 5280）；缺省时由 CA 决定。
    #[serde(default)]
    pub reason: Option<u8>,
}

/// 吊销结果。
#[derive(Debug, Serialize, ToSchema)]
pub(crate) struct RevokeResponse {
    /// 证书记录标识。
    pub cert_id: i64,
    /// 是否本次调用完成了吊销（`false` 表示 CA 侧早已吊销，本次只是同步状态）。
    pub revoked_now: bool,
    /// 本地记录的吊销时间。
    pub revoked_at: DateTime<Utc>,
}

/// 吊销一条证书（11.3）。
///
/// 编排顺序刻意为「先 CA 后本地」：CA 侧失败时本地状态不变，用户可以重试；
/// CA 侧报「已吊销」视为目的已达成，只同步本地状态（spec：不因 CA 报错阻断）。
/// 账号一律读证书行上的 `acme_account_access_id`，不解析流水线配置——
/// 没有账号标识的记录（如手动上传）明确报错，不静默跳过。
async fn revoke_certificate(
    State(state): State<HttpState>,
    Path(cert_id): Path<i64>,
    ApiJson(request): ApiJson<RevokeRequest>,
) -> Result<ApiResponse<RevokeResponse>, ApiError> {
    let repository = CertRepository::new(&state.app.db);
    let cert = repository
        .find(cert_id)
        .await
        .map_err(ApiError::from)?
        .ok_or_else(|| {
            ApiError::new(
                StatusCode::NOT_FOUND,
                "not_found",
                format!("证书不存在: {cert_id}"),
            )
        })?;

    let access_id = cert.acme_account_access_id.ok_or_else(|| {
        ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "missing_account",
            format!(
                "证书 {cert_id} 未绑定 ACME 账号凭据，无法确定签发账号；手动上传的证书无法吊销"
            ),
        )
    })?;

    // 已吊销的记录直接返回现状：幂等，不重复调用 CA。
    if let Some(revoked_at) = cert.revoked_at {
        return Ok(ApiResponse::new(RevokeResponse {
            cert_id,
            revoked_now: false,
            revoked_at,
        }));
    }

    let cipher = state.runtime.cipher.as_ref().ok_or_else(|| {
        ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "configuration_error",
            "未配置凭据加密密钥，无法读取签发账号",
        )
    })?;
    let credentials = CredentialStore::new(
        &state.app.db,
        Arc::clone(&state.runtime.credential_registry),
        Arc::clone(cipher),
    );
    let resolved = credentials
        .resolve(access_id)
        .await
        .map_err(ApiError::from)?;
    let fields: AcmeAccountFields = resolved.as_fields().map_err(ApiError::from)?;
    let account = fields
        .credentials()
        .map_err(ApiError::from)?
        .ok_or_else(|| {
            ApiError::new(
                StatusCode::UNPROCESSABLE_ENTITY,
                "account_not_established",
                format!(
                    "账号凭据 {access_id} 尚未在 CA 侧建立账号，无法吊销；先执行一次签发流水线"
                ),
            )
        })?;
    let transport = acmecast_acme::ProxyConfig {
        accept_invalid_certs: state.app.config.accept_invalid_acme_certs,
        http: None,
        https: None,
        socks5: None,
    };
    let service = AcmeService::from_credentials(&account, Some(&transport))
        .await
        .map_err(|e| ApiError::from(acmecast_core::Error::External(e.to_string())))?;

    let store = FileStore::open(&state.app.config.data_dir)
        .await
        .map_err(ApiError::from)?;
    let cert_pem = store
        .read(&cert.cert_pem_path)
        .await
        .map_err(ApiError::from)?;
    let der = acmecast_cert::first_der(&cert_pem).map_err(ApiError::from)?;

    let mut revoked_now = true;
    let outcome = match request
        .reason
        .and_then(acmecast_acme::AcmeService::revocation_reason)
    {
        Some(reason) => service.revoke_with_reason(&der, reason).await,
        None => service.revoke(&der).await,
    };
    if let Err(error) = outcome {
        let text = error.to_string();
        // CA 报「已吊销」：CA 侧目的已达成，同步本地状态即可。
        if text.contains("alreadyRevoked") || text.contains("already revoked") {
            revoked_now = false;
        } else {
            return Err(ApiError::from(acmecast_core::Error::External(text)));
        }
    }

    let revoked_at = Utc::now();
    repository
        .mark_revoked(cert_id, revoked_at)
        .await
        .map_err(ApiError::from)?;

    archive_revoked_material(&store, &repository, &cert, cert_id).await;

    Ok(ApiResponse::new(RevokeResponse {
        cert_id,
        revoked_now,
        revoked_at,
    }))
}

/// 吊销成功后把证书材料归档进吊销目录，并同步库中的路径。
///
/// CA 侧吊销已不可逆，归档环节的失败一律**不回滚吊销**：文件留在原位、
/// 库路径保持不变，只记日志——下载能力不因此受损，代价只是吊销目录
/// 里暂时少一份（或库里暂时还指着旧位置）。库路径更新失败时会把文件
/// 尽力移回原位，保住「库路径 ↔ 文件位置」的一致。
async fn archive_revoked_material(
    store: &FileStore,
    repository: &CertRepository<'_>,
    cert: &acmecast_store::entity::cert::Model,
    cert_id: i64,
) {
    let paths = match store
        .move_certificate(&cert.cert_pem_path, &cert.key_pem_path, &cert.fingerprint)
        .await
    {
        Ok(Some(paths)) => paths,
        Ok(None) => {
            tracing::warn!(
                cert_id,
                fingerprint = %cert.fingerprint,
                "证书材料缺失，跳过吊销归档，库中路径保持不变"
            );
            return;
        }
        Err(error) => {
            tracing::error!(
                cert_id,
                fingerprint = %cert.fingerprint,
                %error,
                "吊销证书归档失败，文件保留在原位"
            );
            return;
        }
    };

    if let Err(error) = repository.update_paths(cert_id, &paths).await {
        tracing::error!(cert_id, %error, "吊销归档后更新证书路径失败，尝试把文件移回原位");
        let original = CertFilePaths {
            cert_pem: cert.cert_pem_path.clone(),
            key_pem: cert.key_pem_path.clone(),
        };
        if let Err(move_back) = store.restore_certificate(&paths, &original).await {
            tracing::error!(
                cert_id,
                target = %cert.cert_pem_path,
                %move_back,
                "移回失败，请人工核对证书文件位置"
            );
        }
    }
}

/// 把域名收敛为安全的文件名：只保留字母数字与 `.`、`-`、`_`。
fn sanitize_filename(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') {
                c
            } else {
                '_'
            }
        })
        .collect();
    if cleaned.is_empty() {
        "certificate".to_owned()
    } else {
        cleaned
    }
}

/// 流水线步骤输入 DTO。
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub(crate) struct PipelineStepRequest {
    /// 任务类型标识。
    pub type_id: String,
    /// 任务输入；未提供时使用空 JSON 对象。
    #[serde(default = "empty_object")]
    pub input: serde_json::Value,
    /// 是否启用，默认启用。
    #[serde(default = "default_enabled")]
    pub enabled: bool,
}

/// 流水线创建/更新 DTO。
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub(crate) struct PipelineRequest {
    /// 展示名称。
    pub name: String,
    /// 可选描述。
    #[serde(default)]
    pub description: Option<String>,
    /// 流水线级开关，默认启用。
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    /// 按执行顺序排列的步骤。
    pub steps: Vec<PipelineStepRequest>,
}

/// 流水线列表查询参数。
#[derive(Debug, Clone, Deserialize, Default, ToSchema)]
pub(crate) struct PipelineQuery {
    /// 名称子串过滤。
    pub name: Option<String>,
    /// 是否只看启用或停用的流水线。
    pub enabled: Option<bool>,
    /// 页码，从 1 开始。
    pub page: Option<u64>,
    /// 每页条数。
    pub page_size: Option<u64>,
    /// 排序方向：`asc` 或 `desc`，按更新时间排序。
    pub sort: Option<String>,
}

/// 流水线概要响应。
#[derive(Debug, Clone, Serialize, ToSchema)]
pub(crate) struct PipelineSummaryResponse {
    /// 主键。
    pub id: i64,
    /// 展示名称。
    pub name: String,
    /// 可选描述。
    pub description: Option<String>,
    /// 是否启用。
    pub enabled: bool,
    /// 步骤数量。
    pub step_count: usize,
    /// 更新时间。
    pub updated_at: DateTime<Utc>,
}

/// 流水线步骤响应。
#[derive(Debug, Clone, Serialize, ToSchema)]
pub(crate) struct PipelineStepResponse {
    /// 步骤主键。
    pub id: i64,
    /// 执行顺序。
    pub order_index: i32,
    /// 任务类型标识。
    pub type_id: String,
    /// JSON 输入。
    pub input: serde_json::Value,
    /// 是否启用。
    pub enabled: bool,
}

/// 完整流水线响应。
#[derive(Debug, Clone, Serialize, ToSchema)]
pub(crate) struct PipelineResponse {
    /// 主键。
    pub id: i64,
    /// 展示名称。
    pub name: String,
    /// 可选描述。
    pub description: Option<String>,
    /// 是否启用。
    pub enabled: bool,
    /// 有序步骤。
    pub steps: Vec<PipelineStepResponse>,
    /// 创建时间。
    pub created_at: DateTime<Utc>,
    /// 更新时间。
    pub updated_at: DateTime<Utc>,
}

/// 证书查询参数。
#[derive(Debug, Clone, Deserialize, Default, ToSchema)]
pub(crate) struct CertificateQuery {
    /// 域名子串。
    pub domain: Option<String>,
    /// 排序方向：`asc` 或 `desc`。
    pub sort: Option<String>,
    /// 页码，从 1 开始。
    pub page: Option<u64>,
    /// 每页条数。
    pub page_size: Option<u64>,
}

/// 证书创建 DTO。
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub(crate) struct CertificateRequest {
    /// 证书覆盖的域名集合。
    pub domains: Vec<String>,
    /// 证书 PEM 相对路径。
    pub cert_pem_path: String,
    /// 私钥 PEM 相对路径。
    pub key_pem_path: String,
    /// SHA-256 指纹。
    pub fingerprint: String,
    /// 签发者。
    #[serde(default)]
    pub issuer: Option<String>,
    /// 生效时间。
    pub not_before: DateTime<Utc>,
    /// 到期时间。
    pub not_after: DateTime<Utc>,
    /// ACME 账号凭据标识。
    #[serde(default)]
    pub acme_account_access_id: Option<i64>,
}

/// 证书响应。
#[derive(Debug, Clone, Serialize, ToSchema)]
pub(crate) struct CertificateResponse {
    /// 主键。
    pub id: i64,
    /// 规范化后的域名集合。
    pub domains: Vec<String>,
    /// 证书 PEM 相对路径。
    pub cert_pem_path: String,
    /// 私钥 PEM 相对路径。
    pub key_pem_path: String,
    /// SHA-256 指纹。
    pub fingerprint: String,
    /// 签发者。
    pub issuer: Option<String>,
    /// 生效时间。
    pub not_before: DateTime<Utc>,
    /// 到期时间。
    pub not_after: DateTime<Utc>,
    /// ACME 账号凭据标识。
    pub acme_account_access_id: Option<i64>,
    /// 吊销时间。
    pub revoked_at: Option<DateTime<Utc>>,
    /// 创建时间。
    pub created_at: DateTime<Utc>,
    /// 更新时间。
    pub updated_at: DateTime<Utc>,
}

/// 凭据创建/更新 DTO。
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub(crate) struct CredentialRequest {
    /// 展示名称。
    pub name: String,
    /// 已注册的凭据类型。
    pub type_id: String,
    /// 未加密字段值；仅在进程内短暂存在。
    pub fields: serde_json::Value,
}

/// 凭据列表查询参数。
#[derive(Debug, Clone, Deserialize, Default, ToSchema)]
pub(crate) struct CredentialQuery {
    /// 名称子串过滤。
    pub name: Option<String>,
    /// 凭据类型过滤。
    pub type_id: Option<String>,
    /// 页码。
    pub page: Option<u64>,
    /// 每页条数。
    pub page_size: Option<u64>,
}

/// 凭据列表项响应。不返回加密字段。
#[derive(Debug, Clone, Serialize, ToSchema)]
pub(crate) struct CredentialResponse {
    /// 主键。
    pub id: i64,
    /// 展示名称。
    pub name: String,
    /// 凭据类型。
    pub type_id: String,
    /// 创建时间。
    pub created_at: DateTime<Utc>,
    /// 更新时间。
    pub updated_at: DateTime<Utc>,
}

/// 凭据详情响应：额外带回**解密后的字段值**。
///
/// 编辑表单必须能回填现值——PUT 是整体替换语义，拿不到旧值的编辑等于
/// 一保存就把密钥清空。端点仅管理员令牌可达，与「谁能改凭据」同权限；
/// 列表响应（`CredentialResponse`）依旧不带字段。
#[derive(Debug, Clone, Serialize, ToSchema)]
pub(crate) struct CredentialDetailResponse {
    /// 主键。
    pub id: i64,
    /// 展示名称。
    pub name: String,
    /// 凭据类型。
    pub type_id: String,
    /// 解密后的字段值（编辑回填用）。
    pub fields: serde_json::Value,
    /// 创建时间。
    pub created_at: DateTime<Utc>,
    /// 更新时间。
    pub updated_at: DateTime<Utc>,
}

/// 任务类型响应。
#[derive(Debug, Clone, Serialize, ToSchema)]
pub(crate) struct TaskTypeResponse {
    /// 类型标识。
    pub type_id: String,
    /// 展示名称；当前步骤 Trait 没有单独的展示名时使用类型标识。
    pub display_name: String,
    /// 输入 JSON Schema；未声明时为 `None`。
    pub schema: Option<serde_json::Value>,
}

/// 凭据类型响应。
#[derive(Debug, Clone, Serialize, ToSchema)]
pub(crate) struct CredentialTypeResponse {
    /// 类型标识。
    pub type_id: String,
    /// 展示名称。
    pub display_name: String,
    /// 字段 JSON Schema。
    pub schema: serde_json::Value,
}

/// 通用分页响应。
#[derive(Debug, Clone, Serialize, ToSchema)]
pub(crate) struct PageResponse<T> {
    /// 当前页数据。
    pub items: Vec<T>,
    /// 符合条件的总条数。
    pub total: u64,
    /// 当前页码。
    pub page: u64,
    /// 每页条数。
    pub page_size: u64,
}

/// 历史查询参数。
#[derive(Debug, Clone, Deserialize, Default, ToSchema)]
pub(crate) struct HistoryQueryParams {
    /// 按流水线过滤。
    pub pipeline_id: Option<i64>,
    /// 页码。
    pub page: Option<u64>,
    /// 每页条数。
    pub page_size: Option<u64>,
}

/// 运行历史响应。
#[derive(Debug, Clone, Serialize, ToSchema)]
pub(crate) struct HistoryResponse {
    /// 主键。
    pub id: i64,
    /// 所属流水线。
    pub pipeline_id: i64,
    /// 触发来源。
    pub trigger_source: String,
    /// 运行状态。
    pub status: String,
    /// 开始时间。
    pub started_at: DateTime<Utc>,
    /// 结束时间。
    pub finished_at: Option<DateTime<Utc>>,
    /// 失败摘要。
    pub error_message: Option<String>,
}

/// 步骤日志响应。
#[derive(Debug, Clone, Serialize, ToSchema)]
pub(crate) struct HistoryLogResponse {
    /// 步骤顺序。
    pub step_index: i32,
    /// 日志级别。
    pub level: String,
    /// 日志消息。
    pub message: String,
    /// 产生时间。
    pub created_at: DateTime<Utc>,
}

/// 凭据连通性测试响应。
#[derive(Debug, Clone, Serialize, ToSchema)]
pub(crate) struct ConnectivityResponse {
    /// `ok`、`unavailable` 或 `not_testable`。
    pub status: &'static str,
    /// 不可用时的脱敏原因。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

async fn list_pipelines(
    State(state): State<HttpState>,
    ApiQuery(query): ApiQuery<PipelineQuery>,
) -> Result<ApiResponse<PageResponse<PipelineSummaryResponse>>, ApiError> {
    let page = query.page.unwrap_or(1);
    let page_size = query.page_size.unwrap_or(DEFAULT_PAGE_SIZE);
    validate_page("page", page, page_size)?;
    let mut rows = PipelineRepository::new(&state.app.db)
        .list()
        .await
        .map_err(ApiError::from)?;
    if let Some(name) = query
        .name
        .as_deref()
        .map(str::trim)
        .filter(|name| !name.is_empty())
    {
        let name = name.to_lowercase();
        rows.retain(|row| row.name.to_lowercase().contains(&name));
    }
    if let Some(enabled) = query.enabled {
        rows.retain(|row| row.enabled == enabled);
    }
    match query
        .sort
        .as_deref()
        .unwrap_or("desc")
        .to_ascii_lowercase()
        .as_str()
    {
        "asc" | "ascending" => rows.sort_by_key(|row| (row.updated_at, row.id)),
        "desc" | "descending" => {
            rows.sort_by_key(|row| (std::cmp::Reverse(row.updated_at), std::cmp::Reverse(row.id)))
        }
        other => {
            return Err(ApiError::validation(
                "sort",
                format!("排序方向 `{other}` 只支持 asc 或 desc"),
            ));
        }
    }
    let total = rows.len() as u64;
    let items = paginate(rows, page, page_size)
        .into_iter()
        .map(|item| PipelineSummaryResponse {
            id: item.id,
            name: item.name,
            description: item.description,
            enabled: item.enabled,
            step_count: item.step_count,
            updated_at: item.updated_at,
        })
        .collect();
    Ok(ApiResponse::new(PageResponse {
        items,
        total,
        page,
        page_size,
    }))
}

async fn create_pipeline(
    State(state): State<HttpState>,
    ApiJson(request): ApiJson<PipelineRequest>,
) -> Result<ApiResponse<PipelineResponse>, ApiError> {
    let input = pipeline_input(request);
    let id = PipelineRepository::new(&state.app.db)
        .save(None, input)
        .await
        .map_err(ApiError::from)?;
    tracing::info!(pipeline_id = id, "流水线已创建");
    get_pipeline_by_id(&state, id).await.map(ApiResponse::new)
}

async fn get_pipeline(
    State(state): State<HttpState>,
    Path(id): Path<i64>,
) -> Result<ApiResponse<PipelineResponse>, ApiError> {
    get_pipeline_by_id(&state, id).await.map(ApiResponse::new)
}

async fn get_pipeline_by_id(state: &HttpState, id: i64) -> Result<PipelineResponse, ApiError> {
    let pipeline = PipelineRepository::new(&state.app.db)
        .find(id)
        .await
        .map_err(ApiError::from)?
        .ok_or_else(|| {
            ApiError::new(
                axum::http::StatusCode::NOT_FOUND,
                "not_found",
                format!("流水线不存在: {id}"),
            )
        })?;
    Ok(PipelineResponse {
        id: pipeline.id,
        name: pipeline.name,
        description: pipeline.description,
        enabled: pipeline.enabled,
        steps: pipeline
            .steps
            .into_iter()
            .map(|step| PipelineStepResponse {
                id: step.id,
                order_index: step.order_index,
                type_id: step.type_id,
                input: step.input,
                enabled: step.enabled,
            })
            .collect(),
        created_at: pipeline.created_at,
        updated_at: pipeline.updated_at,
    })
}

async fn update_pipeline(
    State(state): State<HttpState>,
    Path(id): Path<i64>,
    ApiJson(request): ApiJson<PipelineRequest>,
) -> Result<ApiResponse<PipelineResponse>, ApiError> {
    let id = PipelineRepository::new(&state.app.db)
        .save(Some(id), pipeline_input(request))
        .await
        .map_err(ApiError::from)?;
    get_pipeline_by_id(&state, id).await.map(ApiResponse::new)
}

async fn delete_pipeline(
    State(state): State<HttpState>,
    Path(id): Path<i64>,
) -> Result<ApiResponse<serde_json::Value>, ApiError> {
    let deleted = PipelineRepository::new(&state.app.db)
        .delete(id)
        .await
        .map_err(ApiError::from)?;
    if !deleted {
        return Err(ApiError::new(
            axum::http::StatusCode::NOT_FOUND,
            "not_found",
            format!("流水线不存在: {id}"),
        ));
    }
    tracing::info!(pipeline_id = id, "流水线已删除");
    Ok(ApiResponse::new(serde_json::json!({ "deleted": true })))
}

/// 手动触发一次流水线运行的响应。
#[derive(Debug, Clone, Serialize, ToSchema)]
pub(crate) struct RunResponse {
    /// 本次运行的运行历史主键。
    pub history_id: i64,
    /// 运行状态；本接口立即返回，恒为 `running`。
    pub status: &'static str,
}

/// 手动触发一次流水线运行（`POST /api/pipelines/{id}/run`）。
///
/// 运行在后台进行：先落一条 `running` 的运行历史，再立即返回，调用方拿
/// `history_id` 轮询 `/api/histories/{id}` 与 `/api/histories/{id}/logs` 看结果。
/// 一次签发可能耗时几分钟（挑战传播等待、CA 验证），同步等待只会把请求挂在
/// 超时边缘，而且期间拿不到任何中间进展。
///
/// 与调度触发共用同一套执行与历史写回逻辑（见 `crate::run`），因此运行记录、
/// 日志与失败摘要的形状与自动触发完全一致。
async fn run_pipeline(
    State(state): State<HttpState>,
    Path(id): Path<i64>,
) -> Result<ApiResponse<RunResponse>, ApiError> {
    let pipeline = PipelineRepository::new(&state.app.db)
        .find(id)
        .await
        .map_err(ApiError::from)?
        .ok_or_else(|| {
            ApiError::new(
                StatusCode::NOT_FOUND,
                "not_found",
                format!("流水线不存在: {id}"),
            )
        })?;

    // 停用的流水线手动也不跑：否则「停用」这个开关会有一个不显眼的例外。
    if !pipeline.enabled {
        return Err(ApiError::validation(
            "enabled",
            "流水线已停用，请先启用后再运行",
        ));
    }

    // 与调度触发同一条约束：并发跑同一条流水线会让证书请求与文件写入互相踩踏。
    if HistoryRepository::new(&state.app.db)
        .has_running(id)
        .await
        .map_err(ApiError::from)?
    {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "conflict",
            format!("流水线 {id} 正在运行中，请等待本次运行结束"),
        ));
    }

    // 没有加密密钥就无法还原凭据，流水线必然失败——不如在触发前就说清楚。
    let cipher = state.runtime.cipher.as_ref().ok_or_else(|| {
        ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "configuration_error",
            "未配置凭据加密密钥，无法执行流水线",
        )
    })?;
    let cipher = Arc::clone(cipher);

    let source = acmecast_store::entity::pipeline::TriggerSource::Manual;
    let definition = crate::run::definition_of(&pipeline);
    let history_id = crate::run::start_run(&state.app.db, id, source)
        .await
        .map_err(ApiError::from)?;

    let db = state.app.db.clone();
    let steps = Arc::clone(&state.runtime.step_registry);
    let registry = Arc::clone(&state.runtime.credential_registry);
    // 通知订阅端随运行旁路投递；未配置渠道时为 None，行为与之前一致。
    let notifier = state.runtime.notifier.clone();
    tokio::spawn(async move {
        let credentials = CredentialStore::new(&db, registry, cipher);
        let events: Option<&dyn acmecast_pipeline::EventSink> =
            notifier.as_deref().map(|sink| sink as _);
        let result = crate::run::execute_run(
            &db,
            &steps,
            &credentials,
            &definition,
            history_id,
            source,
            events,
        )
        .await;
        match result {
            Ok(outcome) => tracing::info!(
                pipeline_id = id,
                history_id,
                success = outcome.is_success(),
                "手动触发的流水线运行结束"
            ),
            // 执行器或存储层出错：历史会停在 running，必须在日志里可见。
            Err(error) => tracing::error!(
                pipeline_id = id,
                history_id,
                %error,
                "手动触发的流水线运行未能写回终态"
            ),
        }
    });

    Ok(ApiResponse::new(RunResponse {
        history_id,
        status: "running",
    }))
}

async fn list_certificates(
    State(state): State<HttpState>,
    ApiQuery(query): ApiQuery<CertificateQuery>,
) -> Result<ApiResponse<PageResponse<CertificateResponse>>, ApiError> {
    let sort = match query
        .sort
        .as_deref()
        .unwrap_or("asc")
        .to_ascii_lowercase()
        .as_str()
    {
        "asc" | "ascending" => CertSort::Ascending,
        "desc" | "descending" => CertSort::Descending,
        other => {
            return Err(ApiError::validation(
                "sort",
                format!("排序方向 `{other}` 只支持 asc 或 desc"),
            ));
        }
    };
    let page = query.page.unwrap_or(1);
    let page_size = query.page_size.unwrap_or(DEFAULT_PAGE_SIZE);
    if page == 0 || !(1..=MAX_PAGE_SIZE).contains(&page_size) {
        return Err(ApiError::validation(
            "page",
            format!("page 从 1 开始，page_size 应在 1..={MAX_PAGE_SIZE} 之间"),
        ));
    }
    let result = CertRepository::new(&state.app.db)
        .list(CertQuery {
            domain: query.domain,
            sort,
            page,
            page_size,
        })
        .await
        .map_err(ApiError::from)?;
    Ok(ApiResponse::new(PageResponse {
        items: result
            .items
            .into_iter()
            .map(CertificateResponse::from)
            .collect(),
        total: result.total,
        page: result.page,
        page_size: result.page_size,
    }))
}

async fn create_certificate(
    State(state): State<HttpState>,
    ApiJson(request): ApiJson<CertificateRequest>,
) -> Result<ApiResponse<CertificateResponse>, ApiError> {
    let result = CertRepository::new(&state.app.db)
        .create(CertInput {
            domains: request.domains,
            cert_pem_path: request.cert_pem_path,
            key_pem_path: request.key_pem_path,
            fingerprint: request.fingerprint,
            issuer: request.issuer,
            not_before: request.not_before,
            not_after: request.not_after,
            acme_account_access_id: request.acme_account_access_id,
        })
        .await
        .map_err(ApiError::from)?;
    Ok(ApiResponse::new(result.into()))
}

async fn get_certificate(
    State(state): State<HttpState>,
    Path(id): Path<i64>,
) -> Result<ApiResponse<CertificateResponse>, ApiError> {
    let result = CertRepository::new(&state.app.db)
        .find(id)
        .await
        .map_err(ApiError::from)?
        .ok_or_else(|| {
            ApiError::new(
                axum::http::StatusCode::NOT_FOUND,
                "not_found",
                format!("证书不存在: {id}"),
            )
        })?;
    Ok(ApiResponse::new(result.into()))
}

async fn delete_certificate(
    State(state): State<HttpState>,
    Path(id): Path<i64>,
) -> Result<ApiResponse<serde_json::Value>, ApiError> {
    let deleted = CertRepository::new(&state.app.db)
        .delete(id)
        .await
        .map_err(ApiError::from)?;
    if !deleted {
        return Err(ApiError::new(
            axum::http::StatusCode::NOT_FOUND,
            "not_found",
            format!("证书不存在: {id}"),
        ));
    }
    Ok(ApiResponse::new(serde_json::json!({ "deleted": true })))
}

async fn list_credentials(
    State(state): State<HttpState>,
    ApiQuery(query): ApiQuery<CredentialQuery>,
) -> Result<ApiResponse<PageResponse<CredentialResponse>>, ApiError> {
    let page = query.page.unwrap_or(1);
    let page_size = query.page_size.unwrap_or(DEFAULT_PAGE_SIZE);
    validate_page("page", page, page_size)?;
    let mut rows = acmecast_store::entity::credential::Entity::find()
        .order_by_asc(acmecast_store::entity::credential::Column::Id)
        .all(&state.app.db)
        .await
        .map_err(|error| ApiError::from(acmecast_store::Error::from(error)))?;
    if let Some(name) = query
        .name
        .as_deref()
        .map(str::trim)
        .filter(|name| !name.is_empty())
    {
        let name = name.to_lowercase();
        rows.retain(|row| row.name.to_lowercase().contains(&name));
    }
    if let Some(type_id) = query
        .type_id
        .as_deref()
        .map(str::trim)
        .filter(|type_id| !type_id.is_empty())
    {
        rows.retain(|row| row.type_id == type_id);
    }
    let total = rows.len() as u64;
    let items = paginate(rows, page, page_size)
        .into_iter()
        .map(CredentialResponse::from)
        .collect();
    Ok(ApiResponse::new(PageResponse {
        items,
        total,
        page,
        page_size,
    }))
}

async fn get_credential(
    State(state): State<HttpState>,
    Path(id): Path<i64>,
) -> Result<ApiResponse<CredentialDetailResponse>, ApiError> {
    let row = acmecast_store::entity::credential::Entity::find_by_id(id)
        .one(&state.app.db)
        .await
        .map_err(|error| ApiError::from(acmecast_store::Error::from(error)))?
        .ok_or_else(|| {
            ApiError::new(
                axum::http::StatusCode::NOT_FOUND,
                "not_found",
                format!("凭据不存在: {id}"),
            )
        })?;
    // 编辑回填需要现值；没有加密密钥就连值都解不出来（也从来存不进去）。
    let cipher = state.runtime.cipher.as_ref().ok_or_else(|| {
        ApiError::new(
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            "configuration_error",
            "未配置凭据加密密钥，无法读取凭据字段",
        )
    })?;
    let plaintext = cipher
        .decrypt_string(&row.encrypted_fields)
        .map_err(ApiError::from)?;
    let fields: serde_json::Value = serde_json::from_str(&plaintext).map_err(|error| {
        ApiError::new(
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            "deserialization_error",
            error.to_string(),
        )
    })?;
    Ok(ApiResponse::new(CredentialDetailResponse {
        id: row.id,
        name: row.name,
        type_id: row.type_id,
        fields,
        created_at: row.created_at,
        updated_at: row.updated_at,
    }))
}

async fn create_credential(
    State(state): State<HttpState>,
    ApiJson(request): ApiJson<CredentialRequest>,
) -> Result<ApiResponse<CredentialResponse>, ApiError> {
    save_credential(&state, None, request)
        .await
        .map(ApiResponse::new)
}

async fn update_credential(
    State(state): State<HttpState>,
    Path(id): Path<i64>,
    ApiJson(request): ApiJson<CredentialRequest>,
) -> Result<ApiResponse<CredentialResponse>, ApiError> {
    save_credential(&state, Some(id), request)
        .await
        .map(ApiResponse::new)
}

async fn save_credential(
    state: &HttpState,
    id: Option<i64>,
    request: CredentialRequest,
) -> Result<CredentialResponse, ApiError> {
    let name = request.name.trim();
    if name.is_empty() {
        return Err(ApiError::validation("name", "凭据名称不能为空"));
    }
    let credential_type = state
        .runtime
        .credential_registry
        .require(request.type_id.trim())
        .map_err(ApiError::from)?;
    credential_type
        .validate(&request.fields)
        .map_err(ApiError::from)?;
    // 保存前统一去掉字符串字段的首尾空白：凭据通常从剪贴板粘贴，带上的
    // 换行/空格会让厂商 API 回「认证失败」，而且存进去就一直是脏的。
    let fields = trim_credential_strings(&request.fields);
    let cipher = state.runtime.cipher.as_ref().ok_or_else(|| {
        ApiError::new(
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            "configuration_error",
            "未配置凭据加密密钥，拒绝保存凭据",
        )
    })?;
    let encrypted_fields = cipher
        .encrypt_string(&serde_json::to_string(&fields).map_err(|error| {
            ApiError::new(
                axum::http::StatusCode::BAD_REQUEST,
                "serialization_error",
                error.to_string(),
            )
        })?)
        .map_err(ApiError::from)?;
    let now = Utc::now();
    let row = match id {
        Some(id) => {
            let existing = acmecast_store::entity::credential::Entity::find_by_id(id)
                .one(&state.app.db)
                .await
                .map_err(|error| ApiError::from(acmecast_store::Error::from(error)))?
                .ok_or_else(|| {
                    ApiError::new(
                        axum::http::StatusCode::NOT_FOUND,
                        "not_found",
                        format!("凭据不存在: {id}"),
                    )
                })?;
            acmecast_store::entity::credential::ActiveModel {
                id: Set(existing.id),
                name: Set(name.to_owned()),
                type_id: Set(request.type_id.trim().to_owned()),
                encrypted_fields: Set(encrypted_fields),
                updated_at: Set(now),
                ..Default::default()
            }
            .update(&state.app.db)
            .await
            .map_err(|error| ApiError::from(acmecast_store::Error::from(error)))?
        }
        None => acmecast_store::entity::credential::ActiveModel {
            name: Set(name.to_owned()),
            type_id: Set(request.type_id.trim().to_owned()),
            encrypted_fields: Set(encrypted_fields),
            created_at: Set(now),
            updated_at: Set(now),
            ..Default::default()
        }
        .insert(&state.app.db)
        .await
        .map_err(|error| ApiError::from(acmecast_store::Error::from(error)))?,
    };
    Ok(row.into())
}

/// 递归去掉凭据 JSON 里所有字符串的首尾空白。
///
/// 与 `parse_credentials` 侧的处理保持同一语义：读取时也 trim，这样
/// 在此修复之前已经存脏的凭据同样能被救回来。
fn trim_credential_strings(value: &serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::String(text) => serde_json::Value::String(text.trim().to_owned()),
        serde_json::Value::Array(items) => {
            serde_json::Value::Array(items.iter().map(trim_credential_strings).collect())
        }
        serde_json::Value::Object(map) => serde_json::Value::Object(
            map.iter()
                .map(|(key, value)| (key.clone(), trim_credential_strings(value)))
                .collect(),
        ),
        other => other.clone(),
    }
}

async fn delete_credential(
    State(state): State<HttpState>,
    Path(id): Path<i64>,
) -> Result<ApiResponse<serde_json::Value>, ApiError> {
    let cipher = state.runtime.cipher.as_ref().ok_or_else(|| {
        ApiError::new(
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            "configuration_error",
            "未配置凭据加密密钥，拒绝操作凭据",
        )
    })?;
    let store = CredentialStore::new(
        &state.app.db,
        Arc::clone(&state.runtime.credential_registry),
        Arc::clone(cipher),
    );
    store.delete(id).await.map_err(ApiError::from)?;
    Ok(ApiResponse::new(serde_json::json!({ "deleted": true })))
}

async fn test_credential(
    State(state): State<HttpState>,
    Path(id): Path<i64>,
) -> Result<ApiResponse<ConnectivityResponse>, ApiError> {
    let cipher = state.runtime.cipher.as_ref().ok_or_else(|| {
        ApiError::new(
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            "configuration_error",
            "未配置凭据加密密钥，拒绝测试凭据",
        )
    })?;
    let store = CredentialStore::new(
        &state.app.db,
        Arc::clone(&state.runtime.credential_registry),
        Arc::clone(cipher),
    );
    let resolved = store.resolve(id).await.map_err(ApiError::from)?;
    let kind = state
        .runtime
        .credential_registry
        .require(&resolved.type_id)
        .map_err(ApiError::from)?;
    let outcome = kind.test_connectivity(&resolved.fields).await;
    let response = match outcome {
        ConnectivityOutcome::Ok => ConnectivityResponse {
            status: "ok",
            reason: None,
        },
        ConnectivityOutcome::Unavailable { reason } => ConnectivityResponse {
            status: "unavailable",
            reason: Some(reason),
        },
        ConnectivityOutcome::NotTestable => ConnectivityResponse {
            status: "not_testable",
            reason: None,
        },
    };
    Ok(ApiResponse::new(response))
}

/// 向通知渠道发送测试消息，逐渠道返回投递结果。
///
/// 这是**交互式验证**：同步等待每个渠道的投递完成（与事件旁路的
/// spawn 投递不同），管理员配置 `[[notifications]]` 后先在这里确认
/// 连通性，再等真实事件。
async fn test_notifications(
    State(state): State<HttpState>,
    // 请求体可选：`axum::Json` 实现了 `OptionalFromRequest`；body 缺省或
    // 为空时按「测试全部启用渠道」处理。自定义 `ApiJson` 未实现该 trait，
    // 这里用不到它的错误包装——无效 JSON 与缺省同义。
    request: Option<axum::Json<NotificationTestRequest>>,
) -> Result<ApiResponse<NotificationTestResponse>, ApiError> {
    let channels = &state.app.config.notifications;
    let targets: Vec<&acmecast_notify::ChannelConfig> = match request
        .as_ref()
        .and_then(|request| request.0.name.as_deref())
    {
        // 指定渠道：不做启用过滤——启用前先测通是合理顺序。
        Some(name) => {
            let found: Vec<_> = channels.iter().filter(|c| c.name == name).collect();
            if found.is_empty() {
                return Err(ApiError::new(
                    StatusCode::NOT_FOUND,
                    "not_found",
                    format!("通知渠道不存在: {name}"),
                ));
            }
            found
        }
        // 缺省：全部启用渠道；一个都没有时要明说，而不是返回空成功。
        None => {
            let enabled: Vec<_> = channels.iter().filter(|c| c.enabled).collect();
            if enabled.is_empty() {
                return Err(ApiError::new(
                    StatusCode::CONFLICT,
                    "no_channels",
                    "没有可测试的通知渠道，请先在 config.toml 配置 [[notifications]] 并重启",
                ));
            }
            enabled
        }
    };

    let registry = Arc::new(acmecast_notify::default_registry());
    let client = acmecast_notify::http_client();
    let options = acmecast_notify::DeliveryOptions::default();
    let message = acmecast_notify::NotificationMessage {
        event: "test".to_owned(),
        title: "通知渠道测试".to_owned(),
        pipeline: "acmecast 服务".to_owned(),
        trigger: "manual".to_owned(),
        occurred_at: Utc::now(),
        domains: Vec::new(),
        target: None,
    };

    let mut results = Vec::with_capacity(targets.len());
    for channel in targets {
        let ok = acmecast_notify::deliver(&client, &registry, channel, &message, &options);
        let (ok, error) = match ok.await {
            Ok(()) => (true, None),
            Err(error) => (false, Some(error.to_string())),
        };
        results.push(NotificationTestResult {
            name: channel.name.clone(),
            provider: channel.provider.clone(),
            ok,
            error,
        });
    }

    Ok(ApiResponse::new(NotificationTestResponse { results }))
}

/// 单个通知渠道的测试投递结果。
#[derive(Debug, Serialize, ToSchema)]
pub(crate) struct NotificationTestResult {
    /// 渠道名。
    pub name: String,
    /// 适配器类型。
    pub provider: String,
    /// 投递是否成功。
    pub ok: bool,
    /// 失败原因。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// 通知渠道测试请求。
#[derive(Debug, Deserialize, ToSchema)]
pub(crate) struct NotificationTestRequest {
    /// 要测试的渠道名；缺省时测试全部启用渠道。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

/// 通知渠道测试响应：逐渠道给出投递结果。
#[derive(Debug, Serialize, ToSchema)]
pub(crate) struct NotificationTestResponse {
    /// 每个被测渠道一条结果，顺序与配置一致。
    pub results: Vec<NotificationTestResult>,
}

/// 调度配置响应。
#[derive(Debug, Serialize, ToSchema)]
pub(crate) struct ScheduleResponse {
    /// 所属流水线。
    pub pipeline_id: i64,
    /// cron 表达式原文。
    pub cron: Option<String>,
    /// 是否启用。
    pub enabled: bool,
    /// 停机补跑策略。
    pub catch_up: bool,
    /// 负责续期的域名集合。
    pub renewal_domains: Option<Vec<String>>,
    /// 上次触发时间。
    pub last_triggered_at: Option<DateTime<Utc>>,
    /// 下次预计触发时间。
    pub next_trigger_at: Option<DateTime<Utc>>,
    /// 更新时间。
    pub updated_at: DateTime<Utc>,
}

/// 新建调度请求。
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub(crate) struct ScheduleCreateRequest {
    /// 所属流水线。
    pub pipeline_id: i64,
    /// cron 表达式（5 或 6 段）；`None`/空表示不按 cron 触发。
    #[serde(default)]
    pub cron: Option<String>,
    /// 是否启用。
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// 停机补跑策略：追赶最近一次。
    #[serde(default)]
    pub catch_up: bool,
    /// 负责续期的域名集合。
    #[serde(default)]
    pub renewal_domains: Option<Vec<String>>,
}

fn default_true() -> bool {
    true
}

fn schedule_response(config: acmecast_store::repository::ScheduleConfig) -> ScheduleResponse {
    ScheduleResponse {
        pipeline_id: config.pipeline_id,
        cron: config.cron,
        enabled: config.enabled,
        catch_up: config.catch_up,
        renewal_domains: config.renewal_domains,
        last_triggered_at: config.last_triggered_at,
        next_trigger_at: config.next_trigger_at,
        updated_at: config.updated_at,
    }
}

/// 全部调度配置。
async fn list_schedules(
    State(state): State<HttpState>,
) -> Result<ApiResponse<Vec<ScheduleResponse>>, ApiError> {
    let configs = acmecast_store::repository::ScheduleRepository::new(&state.app.db)
        .list_all()
        .await
        .map_err(ApiError::from)?;
    Ok(ApiResponse::new(
        configs.into_iter().map(schedule_response).collect(),
    ))
}

/// 新建一条调度配置（含 cron 语法校验，非法时拒绝保存）。
async fn create_schedule(
    State(state): State<HttpState>,
    ApiJson(request): ApiJson<ScheduleCreateRequest>,
) -> Result<ApiResponse<ScheduleResponse>, ApiError> {
    let config = acmecast_store::repository::ScheduleRepository::new(&state.app.db)
        .save(
            request.pipeline_id,
            acmecast_store::repository::ScheduleInput {
                cron: request.cron,
                enabled: request.enabled,
                catch_up: request.catch_up,
                renewal_domains: request.renewal_domains,
            },
        )
        .await
        .map_err(ApiError::from)?;
    Ok(ApiResponse::new(schedule_response(config)))
}

/// 触发记录查询参数。
#[derive(Debug, Clone, Deserialize, Default, ToSchema)]
pub(crate) struct TriggerLogQueryParams {
    /// 按流水线过滤。
    pub pipeline_id: Option<i64>,
    /// 页码。
    pub page: Option<u64>,
    /// 每页条数。
    pub page_size: Option<u64>,
}

/// 触发记录响应。
#[derive(Debug, Clone, Serialize, ToSchema)]
pub(crate) struct TriggerLogResponse {
    /// 主键。
    pub id: i64,
    /// 被触发的流水线。
    pub pipeline_id: i64,
    /// 触发来源：`cron` / `renewal`。
    pub source: String,
    /// 触发说明（如命中的证书域名、失败原因）。
    pub detail: Option<String>,
    /// 触发时间。
    pub triggered_at: DateTime<Utc>,
}

/// 触发审计分页查询（9.7）：按触发时间倒序，可按流水线过滤。
///
/// 调度「到底有没有触发过、失败原因是什么」的排障入口——此前只能直查数据库。
async fn list_trigger_logs(
    State(state): State<HttpState>,
    ApiQuery(query): ApiQuery<TriggerLogQueryParams>,
) -> Result<ApiResponse<PageResponse<TriggerLogResponse>>, ApiError> {
    let page = query.page.unwrap_or(1).max(1);
    let page_size = query
        .page_size
        .unwrap_or(DEFAULT_PAGE_SIZE)
        .clamp(1, MAX_PAGE_SIZE);
    let result = acmecast_store::repository::TriggerLogRepository::new(&state.app.db)
        .list(query.pipeline_id, page, page_size)
        .await
        .map_err(ApiError::from)?;
    Ok(ApiResponse::new(PageResponse {
        items: result
            .items
            .into_iter()
            .map(|item| TriggerLogResponse {
                id: item.id,
                pipeline_id: item.pipeline_id,
                source: item.source.as_str().to_owned(),
                detail: item.detail,
                triggered_at: item.triggered_at,
            })
            .collect(),
        total: result.total,
        page: result.page,
        page_size: result.page_size,
    }))
}

async fn list_credential_types(
    State(state): State<HttpState>,
) -> Result<ApiResponse<Vec<CredentialTypeResponse>>, ApiError> {
    let types = state
        .runtime
        .credential_registry
        .list()
        .into_iter()
        .map(|credential_type| {
            Ok(CredentialTypeResponse {
                type_id: credential_type.type_id().to_owned(),
                display_name: credential_type.display_name().to_owned(),
                schema: serde_json::to_value(credential_type.fields_schema()).map_err(|error| {
                    ApiError::new(
                        axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                        "serialization_error",
                        error.to_string(),
                    )
                })?,
            })
        })
        .collect::<Result<Vec<_>, ApiError>>()?;
    Ok(ApiResponse::new(types))
}

async fn list_tasks(
    State(state): State<HttpState>,
) -> Result<ApiResponse<Vec<TaskTypeResponse>>, ApiError> {
    let tasks = state
        .runtime
        .step_registry
        .list()
        .into_iter()
        .map(|task| {
            let schema = task
                .input_schema()
                .map(serde_json::to_value)
                .transpose()
                .map_err(|error| {
                    ApiError::new(
                        axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                        "serialization_error",
                        error.to_string(),
                    )
                })?;
            Ok(TaskTypeResponse {
                type_id: task.type_id().to_owned(),
                display_name: task.type_id().to_owned(),
                schema,
            })
        })
        .collect::<Result<Vec<_>, ApiError>>()?;
    Ok(ApiResponse::new(tasks))
}

async fn task_schema(
    State(state): State<HttpState>,
    Path(type_id): Path<String>,
) -> Result<ApiResponse<serde_json::Value>, ApiError> {
    let task = state
        .runtime
        .step_registry
        .require(&type_id)
        .map_err(ApiError::from)?;
    let schema = task.input_schema().ok_or_else(|| {
        ApiError::new(
            axum::http::StatusCode::NOT_FOUND,
            "not_found",
            format!("任务类型 `{type_id}` 未声明输入 Schema"),
        )
    })?;
    Ok(ApiResponse::new(serde_json::to_value(schema).map_err(
        |error| {
            ApiError::new(
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                "serialization_error",
                error.to_string(),
            )
        },
    )?))
}

async fn list_histories(
    State(state): State<HttpState>,
    ApiQuery(query): ApiQuery<HistoryQueryParams>,
) -> Result<ApiResponse<PageResponse<HistoryResponse>>, ApiError> {
    list_histories_for(&state, query).await
}

async fn list_pipeline_histories(
    State(state): State<HttpState>,
    Path(id): Path<i64>,
    ApiQuery(mut query): ApiQuery<HistoryQueryParams>,
) -> Result<ApiResponse<PageResponse<HistoryResponse>>, ApiError> {
    query.pipeline_id = Some(id);
    list_histories_for(&state, query).await
}

async fn list_histories_for(
    state: &HttpState,
    query: HistoryQueryParams,
) -> Result<ApiResponse<PageResponse<HistoryResponse>>, ApiError> {
    let page = query.page.unwrap_or(1).max(1);
    let page_size = query
        .page_size
        .unwrap_or(DEFAULT_PAGE_SIZE)
        .clamp(1, MAX_PAGE_SIZE);
    let result = HistoryRepository::new(&state.app.db)
        .list(HistoryQuery {
            pipeline_id: query.pipeline_id,
            page,
            page_size,
        })
        .await
        .map_err(ApiError::from)?;
    Ok(ApiResponse::new(PageResponse {
        items: result
            .items
            .into_iter()
            .map(|item| HistoryResponse {
                id: item.id,
                pipeline_id: item.pipeline_id,
                trigger_source: item.trigger_source.as_str().to_owned(),
                status: item.status.as_str().to_owned(),
                started_at: item.started_at,
                finished_at: item.finished_at,
                error_message: item.error_message,
            })
            .collect(),
        total: result.total,
        page: result.page,
        page_size: result.page_size,
    }))
}

/// 单条运行历史（含终态与失败摘要），供详情页直接取用。
async fn get_history(
    State(state): State<HttpState>,
    Path(id): Path<i64>,
) -> Result<ApiResponse<HistoryResponse>, ApiError> {
    // 直接查实体：分页接口在长历史下可能取不到目标行，按主键取才可靠。
    let row = acmecast_store::entity::history::Entity::find_by_id(id)
        .one(&state.app.db)
        .await
        .map_err(|error| ApiError::from(acmecast_store::Error::from(error)))?
        .ok_or_else(|| {
            ApiError::new(
                StatusCode::NOT_FOUND,
                "not_found",
                format!("运行历史不存在: {id}"),
            )
        })?;
    let status =
        acmecast_store::entity::history::RunStatus::parse(&row.status).ok_or_else(|| {
            ApiError::from(acmecast_core::Error::Internal(format!(
                "运行历史 {id} 的状态 `{}` 无法识别",
                row.status
            )))
        })?;
    Ok(ApiResponse::new(HistoryResponse {
        id: row.id,
        pipeline_id: row.pipeline_id,
        trigger_source: row.trigger_source,
        status: status.as_str().to_owned(),
        started_at: row.started_at,
        finished_at: row.finished_at,
        error_message: row.error_message,
    }))
}

async fn history_logs(
    State(state): State<HttpState>,
    Path(id): Path<i64>,
) -> Result<ApiResponse<Vec<HistoryLogResponse>>, ApiError> {
    let logs = HistoryRepository::new(&state.app.db)
        .logs_of(id)
        .await
        .map_err(ApiError::from)?
        .into_iter()
        .map(|log| HistoryLogResponse {
            step_index: log.step_index,
            level: log.level.as_str().to_owned(),
            message: log.message,
            created_at: log.created_at,
        })
        .collect();
    Ok(ApiResponse::new(logs))
}

fn validate_page(field: &str, page: u64, page_size: u64) -> Result<(), ApiError> {
    if page == 0 {
        return Err(ApiError::validation(field, "页码从 1 开始"));
    }
    if !(1..=MAX_PAGE_SIZE).contains(&page_size) {
        return Err(ApiError::validation(
            "page_size",
            format!("每页条数应在 1..={MAX_PAGE_SIZE} 之间"),
        ));
    }
    Ok(())
}

fn paginate<T>(rows: Vec<T>, page: u64, page_size: u64) -> Vec<T> {
    let start = page
        .saturating_sub(1)
        .saturating_mul(page_size)
        .min(usize::MAX as u64) as usize;
    rows.into_iter()
        .skip(start)
        .take(page_size.min(usize::MAX as u64) as usize)
        .collect()
}

fn pipeline_input(request: PipelineRequest) -> PipelineInput {
    PipelineInput {
        name: request.name,
        description: request.description,
        enabled: request.enabled,
        steps: request
            .steps
            .into_iter()
            .map(|step| PipelineStepInput {
                type_id: step.type_id,
                input: step.input,
                enabled: step.enabled,
            })
            .collect(),
    }
}

fn empty_object() -> serde_json::Value {
    serde_json::json!({})
}

fn default_enabled() -> bool {
    true
}

impl From<acmecast_store::entity::cert::Model> for CertificateResponse {
    fn from(model: acmecast_store::entity::cert::Model) -> Self {
        Self {
            id: model.id,
            domains: model.domain_set(),
            cert_pem_path: model.cert_pem_path,
            key_pem_path: model.key_pem_path,
            fingerprint: model.fingerprint,
            issuer: model.issuer,
            not_before: model.not_before,
            not_after: model.not_after,
            acme_account_access_id: model.acme_account_access_id,
            revoked_at: model.revoked_at,
            created_at: model.created_at,
            updated_at: model.updated_at,
        }
    }
}

impl From<acmecast_store::entity::credential::Model> for CredentialResponse {
    fn from(model: acmecast_store::entity::credential::Model) -> Self {
        Self {
            id: model.id,
            name: model.name,
            type_id: model.type_id,
            created_at: model.created_at,
            updated_at: model.updated_at,
        }
    }
}

/// 构造默认凭据注册表，供没有自定义装配的服务使用。
#[must_use]
pub(crate) fn default_credential_registry() -> Arc<CredentialRegistry> {
    let mut registry = CredentialRegistry::new();
    registry
        .register(AcmeAccountType::new())
        .expect("默认 ACME 凭据类型不应重复注册");
    // DNS 提供商凭据：cert.apply 的 DNS-01 需要按类型创建它们，注册后
    // 前端凭据页与流水线的 dns_credential_id 下拉才有来源。
    registry
        .register(CloudflareCredentialType::default())
        .expect("内置 Cloudflare 凭据类型不应重复注册");
    registry
        .register(AliyunCredentialType::default())
        .expect("内置阿里云凭据类型不应重复注册");
    registry
        .register(TencentCredentialType::default())
        .expect("内置腾讯云凭据类型不应重复注册");
    registry
        .register(TencentEoCredentialType::default())
        .expect("内置腾讯云 EdgeOne 凭据类型不应重复注册");
    // SSH 主机档案：cert.deploy 的 SSH 配置引用它，注册后凭据页才能
    // 创建/测试档案，部署配置也才能从「每次手抄」变成「选一份档案」。
    registry
        .register(SshHostCredentialType)
        .expect("内置 SSH 主机凭据类型不应重复注册");
    Arc::new(registry)
}

/// 构造默认任务注册表。
#[must_use]
pub(crate) fn default_step_registry() -> Arc<StepRegistry> {
    Arc::new(StepRegistry::new())
}
