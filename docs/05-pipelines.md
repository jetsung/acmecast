# 流水线

流水线把证书生命周期串成一串有序步骤。每条流水线由若干**步骤**组成，按
`order_index` 升序执行；每步声明 `type_id`（步骤类型）、`input`（JSON 输入）、
`enabled`（可停用，停用的步骤跳过并记日志）。

## 执行语义

- **产物传递**：步骤产出具名产物（`cert_pem`、`fingerprint` 等），后续步骤按
  名取用；同名产物**覆盖**而非报错（续期重跑依赖「读最近产出」语义），覆盖
  会记一条 warn 日志。取用缺失产物报错并列出当前可用产物名。
- **失败即中止**：单步失败立即中止后续所有步骤；失败步骤的日志完整保留。
  流水线没有自动重试——重试由调度层（下一轮触发）承担。
- **输入校验**：保存与执行前按步骤声明的 JSON Schema 校验
  `required`/`type`/`enum`，错误定位到具体字段（如 `dns.provider`、
  `domains[1]`）。
- **并发闸门**：同一流水线同时只允许一个运行（重复触发直接跳过，不排队）；
  全局并发名额用满时新运行挂起等待。
- **历史**：每次运行先落 `running` 历史行，跑完原地改写终态；崩溃遗留的
  running 视为仍在运行（宁可不自动触发）。日志带步骤序号与逐条时间戳。

## 内置步骤

### `cert.apply` — ACME 申请

一次执行完成「建账号（或复用）→ 下单 → 完成挑战 → 提交 CSR → 拿到证书链」。

输入字段：

| 字段 | 类型 | 必需 | 说明 |
|---|---|---|---|
| `domains` | string[] | 是 | 域名集合；通配符（`*.example.com`）强制走 DNS-01 |
| `challenge` | string | 是 | 挑战类型，当前只支持 `dns-01`（HTTP-01 尚无投放通道） |
| `key_algorithm` | string | 否（默认 `ecdsa_p256`） | 申请密钥与证书的算法：`ecdsa_p256` / `ecdsa_p384` / `ed25519` / `rsa2048`；密钥类型在签发时即固定，CA 是否接受所选算法以 CA 支持为准 |
| `account_credential_id` | int | 是 | ACME 账号凭据标识（`acme.account` 类型） |
| `dns_provider` | string | DNS-01 必填 | DNS 提供商标识：`cloudflare` / `aliyun` |
| `dns_credential_id` | int | DNS-01 必填 | DNS 提供商凭据标识 |
| `dns_zone` | string | 否 | DNS zone；缺省从主域名去掉第一段推导（`a.example.com` → `example.com`） |
| `wait_propagation` | bool | 否（默认 `true`） | 等 TXT 记录可见后再让 CA 验证；内网 DNS（解析器查不到）应关闭 |
| `contacts` | string[] | 否 | 账号联系人（裸邮箱或 `mailto:` URI），仅首次注册时用 |
| `insecure_skip_verify` | bool | 否（默认 `false`） | 跳过 CA 的 TLS 校验，仅供自签测试 CA（pebble） |

产物：`cert_pem`（证书链 PEM）、`key_pem`（私钥 PEM）、`domains`、
`fingerprint`（SHA-256 指纹）。

行为细节：

- ACME 交互重试为指数退避（初始 500ms、倍率 2.0、总上限 120s）；badNonce
  自动重试（上限 3 次）。
- `key_algorithm` 决定本次申请的密钥对与 CSR 签名算法，CA 据此签发同类型
  证书；CA 不支持所选算法时按 CA 返回的错误失败，不回退重试。
- 通配符下单保留 `*.` 前缀（剥前缀会被 CA 在 finalize 时拒绝）。
- DNS-01 写 TXT 记录前先查同值（幂等跳过）；发现**异值残留直接报错**——
  宁可失败也不让 CA 读到旧值。挑战结束无论成败都清理记录。
- 传播等待细节见[DNS 提供商与传播检查](06-dns-providers.md)。

### `cert.store` — 证书入库

把申请到的证书写进数据目录（受限权限）并入库。同一域名集合再次入库是更新
而非新增（续期重跑不产生重复记录）。

| 字段 | 类型 | 必需 | 说明 |
|---|---|---|---|
| `acme_account_credential_id` | int | 否 | 签发账号凭据标识；记录后吊销才知道用哪个账号。手动上传场景可不填 |

前置产物：`cert_pem`、`key_pem`、`domains`、`fingerprint`。
产物：`cert_id`、`fingerprint`。

### `cert.deploy` — 部署

| 字段 | 类型 | 必需 | 说明 |
|---|---|---|---|
| `target` | string | 是 | 部署目标类型：`local` / `ssh` |
| `config` | object | 是 | 目标输入，结构随 `target` 而定，见[部署目标](07-deploy-targets.md) |
| `force` | bool | 否（默认 `false`） | 强制重写（绕过同指纹跳过） |

前置产物：`cert_pem`、`key_pem`、`fingerprint`。
产物：`deployed_paths`、`skipped_write`。

幂等：目标键（输入的稳定摘要）+ 指纹一致且未 `force` → 跳过文件写入，
但**重载命令仍执行**；部署记录只在成功后写入。

## 调度

流水线可挂一份调度（cron 和/或续期域名集合），见
[定时调度与自动续期](08-scheduling.md)。

## API 速查

- `GET/POST /api/pipelines`、`GET/PUT/DELETE /api/pipelines/{id}`
- `POST /api/pipelines/{id}/run`：手动触发；停用 → 400，运行中 → 409
- `GET /api/pipelines/{id}/histories`、`GET /api/histories/{id}/logs`
- `GET /api/tasks`、`GET /api/tasks/{type_id}/schema`：步骤类型清单与输入
  Schema（前端表单用；Schema 带 `x-visible-when`/`x-hidden` 显隐联动标注）

## 典型流水线示例

```json
{
  "name": "example.com 证书",
  "steps": [
    {
      "type_id": "cert.apply",
      "input": {
        "domains": ["example.com", "*.example.com"],
        "challenge": "dns-01",
        "account_credential_id": 1,
        "dns_provider": "cloudflare",
        "dns_credential_id": 2,
        "dns_zone": "example.com"
      }
    },
    {
      "type_id": "cert.store",
      "input": { "acme_account_credential_id": 1 }
    },
    {
      "type_id": "cert.deploy",
      "input": {
        "target": "local",
        "config": {
          "cert_path": "/etc/nginx/ssl/example.com.crt",
          "key_path": "/etc/nginx/ssl/example.com.key",
          "reload_command": "nginx -s reload"
        }
      }
    }
  ]
}
```
