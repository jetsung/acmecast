# 4. 鉴权与安全

## 4.1 鉴权模型

服务默认**强制鉴权**。除以下端点外，所有 `/api/` 请求都必须携带
`Authorization: Bearer <token>`：

- `GET /healthz`（探活，不鉴权）；
- `POST /api/login`（登录）；
- `/swagger-ui`、`/api/openapi.json`（文档）；
- 静态资源（设置 `ACMECAST_STATIC_DIR` 时）。

访问令牌是 **HS256 签名的 JWT**，载荷含 `sub`（管理员用户名）、`iat`、`exp`。
有效期由 `ACMECAST_TOKEN_TTL_HOURS` 控制（默认 12 小时）。

## 4.2 登录

```bash
curl -sS -X POST http://127.0.0.1:8080/api/login \
  -H 'Content-Type: application/json' \
  -d '{"username":"admin","password":"你的口令"}'
```

成功响应：

```json
{ "data": { "token": "<jwt>", "token_type": "Bearer", "expires_in": 43200 } }
```

失败与限流：

| 情况 | HTTP | 错误码 |
| --- | --- | --- |
| 用户名或密码错误 | 401 | `invalid_credentials`（统一为「用户名或密码错误」，不泄露用户名是否存在） |
| 连续失败达到 5 次后的来源 | 429 | `too_many_attempts` |
| 未配置管理员口令哈希（`ACMECAST_ADMIN_PASSWORD_HASH` 与 `ACMECAST_ADMIN_PASSWORD_HASH_FILE` 均未提供） | 500 | `configuration_error`（无法登录） |
| 鉴权被显式关闭时调用登录 | 400 | `auth_disabled` |

限流为**进程内计数**，按来源 IP 记录，连续失败 5 次锁定 900 秒；多副本部署时
需在反向代理/网关层另行防护。

## 4.3 关闭鉴权

```bash
export ACMECAST_AUTH_DISABLED=1
```

**仅限本地单机调试**。关闭后所有 `/api/` 端点不再校验令牌，不会有人能登录，
启动日志会输出 warn。

## 4.4 密钥与口令

| 秘密 | 形态 | 存放 | 生成 |
| --- | --- | --- | --- |
| 管理员口令 | Argon2 PHC 哈希 | 环境变量 `ACMECAST_ADMIN_PASSWORD_HASH`，或文件 + `ACMECAST_ADMIN_PASSWORD_HASH_FILE` | `hash-password` 子命令（见 [13-docker-deployment.md](13-docker-deployment.md) 13.3） |
| JWT 签名密钥 | 任意字符串 | 环境变量 `ACMECAST_JWT_SECRET` | `openssl rand -base64 32` |
| 凭据加密密钥 | base64 的 32 字节 | 环境变量 `ACMECAST_CREDENTIAL_KEY` | `openssl rand -base64 32` |
| 凭据字段 | AES-256-GCM 密文 | 数据库 `credential.encrypted_fields` | 由加密密钥派生 |

约定：

- 口令明文、JWT 密钥、凭据字段**不会出现在日志、响应或 Debug 输出**中
  （配置对象的 `Debug` 已脱敏，仅输出「是否已设置」）；
- 凭据加解密在进程内完成，字段明文仅短暂存在于内存；
- 凭据接口**永远不返回加密字段**，只返回 `id` / `name` / `type_id` 与时间戳；
- 凭据连通性测试返回的错误信息会先做**脱敏**（把与字段值相同的片段替换为 `***`），
  避免外部服务回显密钥。

> 凭据加密密钥与数据目录必须分开保管。只备份数据目录而不备份密钥，凭据不可恢复；
> 密钥泄漏则等同于所有凭据泄漏。

## 4.5 传输安全

服务本身以 **HTTP 明文**监听。生产部署建议置于反向代理（Nginx / Caddy / 网关）
之后，由代理终止 TLS 并提供公网入口：

- 将 `ACMECAST_LISTEN` 绑定到回环或内网地址（如 `127.0.0.1:8080`）；
- 在代理层配置 HTTPS、访问控制与额外的登录限流。

## 4.6 已知的安全注意事项

- **SSH 部署不校验主机指纹**：当前未接入 `known_hosts`，接受任何主机密钥，
  仅记录一条 warn。存在中间人风险，生产请确保目标主机地址可信、网络受控。
- **HTTP-01 挑战无投放通道**：仅有材料计算能力，申请步骤请使用 DNS-01。
- **登录限流是进程内的**：多副本部署时需在代理层补充防护。
- **手动登记的证书**：`POST /api/certificates` 只登记记录与相对路径，不校验
  路径下文件是否真实存在；请确保内容与元数据一致。
