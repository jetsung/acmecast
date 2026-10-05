# REST API

- 交互式文档：`http://<host>:8080/swagger-ui`（免鉴权）
- OpenAPI JSON：`http://<host>:8080/api/openapi.json`（免鉴权）

## 通用约定

- **鉴权**：除 `/api/login`、`/api/openapi.json` 外，全部 `/api/` 端点要求
  `Authorization: Bearer <token>`。缺少/无效令牌返回 401 并带
  `WWW-Authenticate: Bearer`。
- **成功响应**：统一信封 `{"data": ...}`。
- **错误响应**：统一信封 `{"error": {"code", "message", "field?"}}`。
- **分页**：`page`（1 起）、`page_size`（默认 20，最大 100）。响应为
  `{items, total, page, page_size}`。
- **请求体上限**：默认 2 MiB，超出返回 413 `payload_too_large`；JSON 解析
  失败返回 400 `invalid_json`；查询参数解析失败返回 400 `invalid_query`。
- 每个请求带 `x-request-id`（UUID），错误排查时连同该值一起提供。

## 认证

| 方法 | 路径 | 鉴权 | 说明 |
|---|---|---|---|
| GET | `/healthz` | 否 | 存活探针，返回 `ok` |
| POST | `/api/login` | 否 | 入参 `{username, password}`；返回 `{token, token_type: "Bearer", expires_in}` |

- 令牌为 HS256 JWT，默认有效期 12 小时（`ACMECAST_TOKEN_TTL_HOURS`）。
- 登录失败统一 401 `invalid_credentials`（不区分用户不存在/口令错，防枚举）。
- 限流：同一来源连续失败 5 次锁定 15 分钟，锁定期返回 429
  `too_many_attempts`。来源取 `x-forwarded-for`/`x-real-ip` 首值，否则连接
  地址。**限流为进程内计数**，多副本部署需反向代理层防护。

## 流水线

| 方法 | 路径 | 说明 |
|---|---|---|
| GET | `/api/pipelines` | 列表；query：`page` `page_size` `name` `enabled` `sort`(asc/desc) |
| POST | `/api/pipelines` | 创建；body `{name, description?, enabled=true, steps: [{type_id, input={}, enabled=true}]}` |
| GET | `/api/pipelines/{id}` | 详情 |
| PUT | `/api/pipelines/{id}` | 整体更新 |
| DELETE | `/api/pipelines/{id}` | 删除 → `{"deleted": true}` |
| GET | `/api/pipelines/{id}/histories` | 该流水线的运行历史 |
| POST | `/api/pipelines/{id}/run` | 手动触发 → `{history_id, status: "running"}`；停用 → 400，运行中 → 409 |

步骤类型清单：`GET /api/tasks`；单个步骤输入 Schema：
`GET /api/tasks/{type_id}/schema`（前端据此渲染表单，含 `x-visible-when`
显隐联动标注）。

## 证书

| 方法 | 路径 | 说明 |
|---|---|---|
| GET | `/api/certificates` | 列表；query：`domain` `sort`(asc/desc) `page` `page_size` |
| POST | `/api/certificates` | 手动上传入库（`domains`、`cert_pem_path`、`key_pem_path`、`fingerprint`、`not_before`、`not_after` 等） |
| GET | `/api/certificates/{id}` | 详情 |
| DELETE | `/api/certificates/{id}` | 删除 |
| GET | `/api/certificates/{id}/download` | 下载；query：`format`（`pem` 默认 / `der` / `pfx` / `jks` / `p7b`，也认 `crt`/`p12`）、`password`（keystore 口令，默认 `changeit`） |
| POST | `/api/certificates/{id}/revoke` | 吊销；body `{reason?: 0..=10}`（RFC 5280 原因码） |

吊销语义：先 CA 后本地；CA 报已吊销 → 只同步本地（`revoked_now=false`）；
手动上传（无签发账号）的记录 → 422 `missing_account`；重复吊销幂等。

## 凭据

| 方法 | 路径 | 说明 |
|---|---|---|
| GET | `/api/credentials` | 列表（**不含字段**）；query：`page` `page_size` `name` `type_id` |
| POST | `/api/credentials` | 创建；body `{name, type_id, fields}` |
| GET | `/api/credentials/{id}` | 详情（含解密后的 fields） |
| PUT | `/api/credentials/{id}` | 整体替换字段 |
| DELETE | `/api/credentials/{id}` | 删除；仍被流水线引用 → 409 并列出引用者 |
| POST | `/api/credentials/{id}/test` | 连通性测试 → `{status: ok/unavailable/not_testable, reason?}` |
| GET | `/api/credential-types` | 已注册类型清单（含字段 JSON Schema） |

## 历史与日志

| 方法 | 路径 | 说明 |
|---|---|---|
| GET | `/api/histories` | 全部历史；query：`pipeline_id` `page` `page_size` |
| GET | `/api/histories/{id}` | 单条历史（`finished_at` 为空 = 运行中） |
| GET | `/api/histories/{id}/logs` | 日志列表 `{step_index, level, message, created_at}` |

历史 `status`：`running` / `success` / `failed`；`trigger_source`：
`manual` / `cron` / `renewal`。

## 调度

| 方法 | 路径 | 说明 |
|---|---|---|
| GET | `/api/schedules` | 全部调度（含上次/下次触发时间） |
| POST | `/api/schedules` | 创建；`{pipeline_id, cron?, enabled=true, catch_up?, renewal_domains?}`，cron 非法保存即拒 |
| GET | `/api/schedules/trigger-logs` | 触发审计；query：`pipeline_id` `page` `page_size` |

## 错误码映射

| HTTP | code | 触发场景 |
|---|---|---|
| 400 | `validation_error` | 输入校验失败（带 `field`） |
| 400 | `invalid_json` / `invalid_query` | 请求体/查询串解析失败 |
| 400 | `unknown_credential_type` | 未注册的凭据类型（message 列出已注册） |
| 401 | `invalid_credentials` / `unauthorized` | 登录失败 / 令牌缺失或无效 |
| 404 | `not_found` | 资源不存在 |
| 409 | `conflict` | 流水线运行中重复触发、凭据仍被引用等 |
| 413 | `payload_too_large` | 请求体超限 |
| 422 | `missing_field` / `missing_account` | 缺字段 / 手动上传证书无法吊销 |
| 429 | `too_many_attempts` | 登录限流锁定 |
| 500 | `configuration_error` | 凭据密钥缺失等配置问题 |
| 500 | `credential_decryption_error` | 凭据解密失败（多为更换了加密密钥） |
| 500 | `database_error` / `migration_error` / `io_error` | 存储层故障 |
| 502 | `external_service_error` | CA/DNS 等外部服务错误 |
| 504 | `timeout` | 外部调用超时 |
