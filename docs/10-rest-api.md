# 10. REST API 参考

交互式文档：`GET /swagger-ui`；机器可读定义：`GET /api/openapi.json`。

## 10.1 通用约定

### 成功响应

所有成功响应包在 `data` 中：

```json
{ "data": { } }
```

分页响应统一为：

```json
{ "data": { "items": [ ], "total": 123, "page": 1, "page_size": 20 } }
```

### 错误响应

```json
{ "error": { "code": "validation_error", "message": "可读消息", "field": "domains" } }
```

`field` 仅在字段校验错误时出现。

### 鉴权

除 `/healthz`、`/api/login`、`/swagger-ui`、`/api/openapi.json` 与静态资源外，
所有 `/api/` 端点要求 `Authorization: Bearer <token>`。缺失或无效令牌返回
401 `unauthorized`。

### 通用请求头与限制

- `Content-Type: application/json`（带请求体的端点）；
- 请求体上限默认 2 MiB，超出返回 413 `payload_too_large`；
- 每个请求会生成或沿用 `x-request-id` 并在响应头回写，便于排查；
- JSON 解析失败返回 400 `invalid_json`；查询参数非法返回 400 `invalid_query`。

### 分页

`page` 从 1 开始；`page_size` 取值 1..=100（默认 20）。超范围的证书列表请求
返回 400 `validation_error`；历史列表会将越界值收敛到合法范围。

## 10.2 端点总览

| 方法 | 路径 | 说明 | 鉴权 |
| --- | --- | --- | --- |
| GET | `/healthz` | 探活，返回文本 `ok` | 否 |
| POST | `/api/login` | 登录，签发令牌 | 否 |
| GET | `/api/pipelines` | 流水线分页列表 | 是 |
| POST | `/api/pipelines` | 新建流水线 | 是 |
| GET | `/api/pipelines/{id}` | 流水线详情 | 是 |
| PUT | `/api/pipelines/{id}` | 更新流水线（整体替换） | 是 |
| DELETE | `/api/pipelines/{id}` | 删除流水线 | 是 |
| GET | `/api/pipelines/{id}/histories` | 该流水线运行历史 | 是 |
| POST | `/api/pipelines/{id}/run` | 手动触发一次运行（异步） | 是 |
| GET | `/api/certificates` | 证书分页列表 | 是 |
| POST | `/api/certificates` | 手动登记证书记录 | 是 |
| GET | `/api/certificates/{id}` | 证书详情 | 是 |
| DELETE | `/api/certificates/{id}` | 删除证书记录 | 是 |
| GET | `/api/certificates/{id}/download` | 按格式下载证书 | 是 |
| POST | `/api/certificates/{id}/revoke` | 吊销证书 | 是 |
| GET | `/api/credentials` | 凭据分页列表 | 是 |
| POST | `/api/credentials` | 新建凭据 | 是 |
| GET | `/api/credentials/{id}` | 凭据详情 | 是 |
| PUT | `/api/credentials/{id}` | 更新凭据 | 是 |
| DELETE | `/api/credentials/{id}` | 删除凭据 | 是 |
| POST | `/api/credentials/{id}/test` | 凭据连通性测试 | 是 |
| GET | `/api/credential-types` | 已注册凭据类型与 Schema | 是 |
| GET | `/api/schedules` | 调度配置列表 | 是 |
| POST | `/api/schedules` | 新建/更新调度 | 是 |
| GET | `/api/tasks` | 任务类型与输入 Schema | 是 |
| GET | `/api/tasks/{type_id}/schema` | 单任务输入 Schema | 是 |
| GET | `/api/histories` | 运行历史分页列表 | 是 |
| GET | `/api/histories/{id}` | 单条运行历史 | 是 |
| GET | `/api/histories/{id}/logs` | 运行日志（按步骤） | 是 |

## 10.3 端点明细

### POST /api/login

请求：`{ "username": "admin", "password": "..." }`
响应：`{ "data": { "token": "...", "token_type": "Bearer", "expires_in": 43200 } }`
错误：401 `invalid_credentials`、429 `too_many_attempts`、
500 `configuration_error`（未配置口令哈希）、400 `auth_disabled`。

### 流水线

**GET /api/pipelines** 查询参数：`page`、`page_size`、`name`（名称子串）、
`enabled`（true/false）、`sort`（`asc`/`desc`，按更新时间，默认 `desc`）。
响应 `items` 为 `PipelineSummaryResponse`：`id`、`name`、`description`、
`enabled`、`step_count`、`updated_at`。

**POST /api/pipelines** / **PUT /api/pipelines/{id}**

```json
{
  "name": "string（必填）",
  "description": "string | null",
  "enabled": true,
  "steps": [
    { "type_id": "cert.apply", "input": { }, "enabled": true }
  ]
}
```

响应 `data` 为完整流水线（含 `steps` 数组，每步有 `id`、`order_index`、
`type_id`、`input`、`enabled` 与 `created_at`/`updated_at`）。

**DELETE /api/pipelines/{id}** 响应 `{ "data": { "deleted": true } }`；不存在返回 404。

**GET /api/pipelines/{id}/histories** 查询 `page`、`page_size`，`pipeline_id` 被强制为该路径 id。

**POST /api/pipelines/{id}/run** 手动触发一次运行，响应
`{ "data": { "history_id": 12, "status": "running" } }`。

运行在**后台**进行，接口立即返回；用 `history_id` 轮询
`/api/histories/{id}`（终态）与 `/api/histories/{id}/logs`（步骤日志）。
运行来源记为 `manual`。

错误：404 `not_found`、400 `validation_error`（`field=enabled`，流水线已停用）、
409 `conflict`（该流水线正在运行中）、500 `configuration_error`（未配置凭据加密密钥）。

### 证书

**GET /api/certificates** 查询：`domain`、`sort`（`asc`/`desc`，按到期时间，默认 `asc`）、
`page`、`page_size`。

**POST /api/certificates** 请求字段见 [09-certificates.md](09-certificates.md) 9.4。

**GET /api/certificates/{id}/download** 查询：`format`（`pem`/`der`/`pfx`/`jks`/`p7b`，
默认 `pem`）、`password`（PFX/JKS 口令，默认 `changeit`）。成功返回附件字节流。

**POST /api/certificates/{id}/revoke** 请求 `{ "reason": 0 }`（可选），
响应 `RevokeResponse`（见 9.3）。

### 凭据

**GET /api/credentials** 查询：`page`、`page_size`（OpenAPI 未声明但 handler 支持
`name`、`type_id` 过滤）。响应项为 `CredentialResponse`：
`id`、`name`、`type_id`、`created_at`、`updated_at`（**不含字段明文**）。

**POST /api/credentials** / **PUT /api/credentials/{id}**

```json
{ "name": "string", "type_id": "acme.account", "fields": { } }
```

**POST /api/credentials/{id}/test** 响应：

```json
{ "data": { "status": "ok", "reason": null } }
```

`status` 为 `ok`、`unavailable`（附脱敏 `reason`）或 `not_testable`。

**GET /api/credential-types** 响应数组，每项 `{ type_id, display_name, schema }`。

### 调度

**GET /api/schedules** 响应数组，项为 `ScheduleResponse`：`pipeline_id`、`cron`、
`enabled`、`catch_up`、`renewal_domains`、`last_triggered_at`、`next_trigger_at`、
`updated_at`。

**POST /api/schedules** 请求见 [08-scheduling-renewal.md](08-scheduling-renewal.md) 8.1。
非法 cron 返回 400 `validation_error`（消息带原文）。

### 任务

**GET /api/tasks** 响应数组，项为 `{ type_id, display_name, schema }`
（当前 `display_name` 等于 `type_id`）。`schema` 为输入的 JSON Schema，可能为 `null`。

**GET /api/tasks/{type_id}/schema** 未知类型返回 400 `unknown_step_type`；
类型存在但未声明 Schema 返回 404。

### 运行历史

**GET /api/histories** 查询：`pipeline_id`、`page`、`page_size`。
响应项为 `HistoryResponse`：`id`、`pipeline_id`、`trigger_source`、`status`、
`started_at`、`finished_at`、`error_message`。

**GET /api/histories/{id}** 单条历史（按主键，长历史下也可靠）。

**GET /api/histories/{id}/logs** 响应数组，每项
`{ step_index, level, message, created_at }`。`level` 为 `info`/`warn`/`error`。

## 10.4 错误码与 HTTP 状态

| HTTP | 错误码 | 典型来源 |
| --- | --- | --- |
| 400 | `validation_error` | 字段校验失败（带 `field`）、非法 cron、非法 sort |
| 400 | `invalid_json` | 请求体不是合法 JSON |
| 400 | `invalid_query` | 查询参数反序列化失败 |
| 400 | `serialization_error` | 输入无法序列化 |
| 400 | `unknown_credential_type` | 凭据类型未注册（消息列出可用类型） |
| 400 | `unknown_step_type` | 任务类型未注册 |
| 400 | `auth_disabled` | 鉴权已关闭时调用登录 |
| 401 | `unauthorized` | 缺少/无效令牌 |
| 401 | `invalid_credentials` | 登录用户名或口令错误 |
| 404 | `not_found` | 资源不存在、路径不存在 |
| 409 | `conflict` | 通用冲突 |
| 409 | `credential_in_use` | 删除仍被引用的凭据 |
| 409 | `duplicate_credential_type` / `duplicate_step_type` | 注册冲突 |
| 413 | `payload_too_large` | 请求体超限 |
| 422 | `missing_field` | 存储层缺少必需字段 |
| 422 | `missing_artifact` | 步骤缺少必需产物（消息列出可用产物） |
| 422 | `certificate_error` | 证书解析/转换失败 |
| 422 | `missing_account` | 吊销时证书未绑定 ACME 账号 |
| 422 | `account_not_established` | 账号尚未在 CA 侧建立 |
| 429 | `too_many_attempts` | 登录失败次数过多 |
| 500 | `configuration_error` | 缺少必需配置（如凭据加密密钥、口令哈希） |
| 500 | `credential_decryption_error` | 凭据无法解密 |
| 500 | `cryptography_error` | 加解密失败 |
| 500 | `database_error` / `migration_error` / `io_error` | 存储/文件层失败 |
| 500 | `internal_error` | 未归类的内部错误 |
| 502 | `external_service_error` | 外部服务（CA、DNS、SSH）调用失败 |
| 504 | `timeout` | 外部调用超时 |
