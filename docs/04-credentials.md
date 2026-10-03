# 凭据体系

凭据是流水线步骤访问外部系统（CA、DNS 提供商、SSH 主机）的认证材料。
所有凭据字段经 AES-256-GCM 加密后落库（密钥为 `ACMECAST_CREDENTIAL_KEY`，
只存在于环境变量），API 与日志中永不出现明文——读取详情的接口解密返回，
其余列表接口不含字段。

## 通用机制

- **引用方式**：步骤输入中任何层级名为 `credential_id` 的整数字段都是凭据
  引用（约定固定名而非「`_id` 结尾」模糊匹配）。多凭据靠嵌套：
  `{"dns": {"credential_id": 3}, "acme": {"credential_id": 7}}`。
- **取用时校验**：执行时按标识现取、解密并**当场校验**（防旧类型定义下写入
  的失效内容被注入）；凭据不存在会在执行前明确报 `not_found`，绝不静默放过。
- **整体替换**：更新凭据是整体覆盖字段，不做局部合并。
- **删除保护**：仍被流水线引用的凭据拒绝删除，错误中列出引用者
  （流水线名 + 第几步）。可通过 `GET /api/credentials/{id}` 之外的
  删除报错信息确认引用位置。
- **连通性测试**：`POST /api/credentials/{id}/test`，返回
  `ok / unavailable / not_testable`；`not_testable` 表示该类型无从测试，
  不算故障。失败原因回显前会做 `redact` 脱敏（与秘密值相同的片段替换为
  `***`，仅抹长度 ≥ 4 的片段）。

## 内建凭据类型

### `acme.account` — ACME 账号

| 字段 | 必需 | 说明 |
|---|---|---|
| `ca` | 是 | CA 别名：`letsencrypt`、`letsencrypt-staging`、`zerossl`、`google`、`sslcom`、`custom` |
| `directory_url` | 条件 | `ca=custom` 时必填；在内置 CA 上填写会**覆盖内置端点**（可走自建反代） |
| `eab_kid` / `eab_hmac_key` | 条件 | EAB（外部账号绑定）必须**成对**提供，只给一半会报错。`hmac_key` 为 base64url（兼容标准 base64） |
| `credentials` | 否 | ACME 账号凭据 JSON（KID、账号私钥、目录端点）。**不要手填**：首次使用时自动注册并写回 |

内置 CA 端点：

| 别名 | 目录 URL |
|---|---|
| `letsencrypt` / `le` | `https://acme-v02.api.letsencrypt.org/directory` |
| `letsencrypt-staging` / `le-staging` | `https://acme-staging-v02.api.letsencrypt.org/directory` |
| `zerossl` | `https://acme.zerossl.com/v2/DV90`（要求 EAB） |
| `google` / `gts` | `https://dv.acme-v02.api.pki.goog/directory`（要求 EAB） |
| `sslcom` / `ssl.com` | `https://acme.ssl.com/ssl/v2/DV` |

账号生命周期：凭据中 `credentials` 为空 = 尚未在 CA 侧建立账号，首次被
`cert.apply` 使用时注册（`terms_of_service_agreed=true`），签发的账号凭据
写回记录；之后的运行一律复用同一账号。多条流水线引用同一份凭据时共享同一
CA 账号，不会重复注册（也避免撞上 CA 的账号配额）。

连通性测试：已注册账号会恢复会话并取一次 Directory（真实探测 CA 连通性）；
未注册账号返回 `unavailable`（「尚未在 CA 侧建立账号，无从验证」）。

### `cloudflare` — Cloudflare DNS

| 字段 | 说明 |
|---|---|
| `api_token` | API Token，需目标域名的 DNS 编辑权限 |

### `aliyun` — 阿里云 DNS

| 字段 | 说明 |
|---|---|
| `access_key_id` | AccessKey ID |
| `access_key_secret` | AccessKey Secret |

### `ssh` — SSH 主机档案

一份档案 = 一台主机的连接方式 + 文件权限缺省值；远端路径与重载命令
**不属于档案**（在部署步骤输入里给）。

| 字段 | 默认 | 说明 |
|---|---|---|
| `host` | — | 必填，主机地址 |
| `port` | `22` | SSH 端口 |
| `user` | `root` | 登录用户 |
| `private_key` | — | OpenSSH/PEM 私钥（表单为多行输入） |
| `password` | — | 口令 |
| `cert_mode` | `0644` | 证书文件权限缺省 |
| `key_mode` | `0600` | 私钥文件权限缺省 |

`private_key` 与 `password` 必须**恰好其一**（皆缺或皆有都报错）。

凭据字段字符串在保存/读取时递归 trim 首尾空白——粘贴带换行的密钥不会让
厂商认证在莫名其妙的地方失败。

## API 速查

- `GET /api/credential-types`：列出已注册类型（含 JSON Schema，前端渲染表单用）
- `GET/POST /api/credentials`、`GET/PUT/DELETE /api/credentials/{id}`
- `POST /api/credentials/{id}/test`

详见 [REST API](09-api.md)。
