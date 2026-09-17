# 5. 凭据管理

凭据是需要在调用外部服务时使用的秘密（ACME 账号、DNS API Token、SSH 私钥/口令）。
凭据字段**以密文存储在数据库中**，接口只返回元信息；流水线步骤按 `id` 引用凭据，
明文永远不会进入流水线配置或运行日志。

## 5.1 凭据端点

| 方法 | 路径 | 说明 |
| --- | --- | --- |
| GET | `/api/credentials` | 分页列表；支持 `page`、`page_size`，handler 另支持 `name`、`type_id` 过滤 |
| POST | `/api/credentials` | 新建 |
| GET | `/api/credentials/{id}` | 详情（不含字段明文） |
| PUT | `/api/credentials/{id}` | 更新（整体替换字段） |
| DELETE | `/api/credentials/{id}` | 删除；被流水线引用时返回 409 |
| POST | `/api/credentials/{id}/test` | 连通性测试 |
| GET | `/api/credential-types` | 全部已注册类型及字段 Schema |

创建/更新请求体：

```json
{ "name": "展示名称", "type_id": "acme.account", "fields": { "...": "..." } }
```

- `name` 不能为空；
- `type_id` 必须是已注册类型，否则返回 400 `unknown_credential_type`，错误信息会列出可用类型；
- `fields` 会按该类型的 `validate` 校验（失败返回 400 `validation_error` 并带 `field`）；
- 未配置 `ACMECAST_CREDENTIAL_KEY` 时保存/测试/删除凭据返回 500 `configuration_error`。

删除时若仍被流水线步骤引用，返回 409 `credential_in_use`，消息形如
「凭据 3 仍被 2 条流水线引用」。

## 5.2 已注册的凭据类型

### `acme.account` —— ACME 账号

| 字段 | 必填 | 说明 |
| --- | --- | --- |
| `ca` | 是 | CA 别名：`letsencrypt`、`letsencrypt-staging`、`zerossl`、`google`、`sslcom`、`custom` |
| `directory_url` | 条件 | 自定义 Directory URL。`ca=custom` 时必填；对内置 CA 填写会覆盖内置端点 |
| `eab_kid` | 否 | External Account Binding 的 Key ID |
| `eab_hmac_key` | 否 | base64url 编码的 EAB HMAC 密钥 |
| `credentials` | 否 | **由系统写回**的账号凭据（含 KID、私钥、服务端点）。首次使用时为空，签发成功后自动写入 |

内置 CA 的 Directory URL：

| `ca` | 端点 |
| --- | --- |
| `letsencrypt` | `https://acme-v02.api.letsencrypt.org/directory` |
| `letsencrypt-staging` | `https://acme-staging-v02.api.letsencrypt.org/directory` |
| `zerossl` | `https://acme.zerossl.com/v2/DV90` |
| `google` | `https://dv.acme-v02.api.pki.goog/directory` |
| `sslcom` | `https://acme.ssl.com/ssl/v2/DV` |
| `custom` | 无内置值，必须由 `directory_url` 提供 |

行为要点：

- **EAB 两半必须成对**：只填一半会被校验拒绝；
- 首次使用该凭据时，`cert.apply` 会注册新 ACME 账号并把凭据写回 `credentials` 字段；
  之后的运行**复用**同一账号，不再重复注册。多条流水线引用同一凭据也只会注册一次；
- 凭据写回失败只记 warn，不影响本次运行，但下次会重新注册；
- 连通性测试仅在已建立账号后可用（否则返回 `unavailable` 并说明「尚未在 CA 侧建立账号」），
  其本质是恢复会话时访问一次 Directory。

创建示例：

```bash
curl -sS -X POST http://127.0.0.1:8080/api/credentials \
  -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
  -d '{"name":"le-prod","type_id":"acme.account","fields":{"ca":"letsencrypt"}}'

# 使用 EAB（ZeroSSL / Google 等）
curl -sS -X POST http://127.0.0.1:8080/api/credentials \
  -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
  -d '{"name":"zerossl","type_id":"acme.account","fields":{
        "ca":"zerossl","eab_kid":"kid-xxx","eab_hmac_key":"base64url-key"}}'
```

## 5.3 DNS 提供商凭据

字段定义直接复用各 DNS 提供商的凭据结构体，字段名与 `cert.apply` 使用它们时的
解析一致；连通性测试为「无从测试」（一次有效探测需要具体 zone，不做猜测）。

| 提供商 `type_id` | 字段 | 说明 |
| --- | --- | --- |
| `cloudflare` | `api_token` | 需对目标域名具备 DNS 编辑权限的 API Token（推荐 Token 而非 Global API Key） |
| `aliyun` | `access_key_id`、`access_key_secret` | 阿里云 DNS 的 AccessKey |

```bash
curl -sS -X POST http://127.0.0.1:8080/api/credentials \
  -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
  -d '{"name":"cf-main","type_id":"cloudflare","fields":{"api_token":"cf-token"}}'
```

创建后，流水线编辑器里 `cert.apply` 的 `dns_credential_id` 会以下拉形式列出这些
凭据（按名称选择，提交数字 ID）。

## 5.4 系统已定义但**当前未注册**的凭据类型

### SSH 凭据（定义于 SSH 部署目标）

SSH 目标按认证方式从凭据里取字段：

| 认证方式 | 需要的字段 |
| --- | --- |
| `private_key` | `private_key`（OpenSSH 或 PEM 格式私钥） |
| `password` | `password`（登录口令） |

字段名与部署目标实现一致，但当前**没有为它们注册 `CredentialType`**，因此无法通过
`/api/credentials` 创建；引用它们的部署步骤会在运行时因取不到凭据而失败。

### 如何启用

注册是显式的：在服务启动装配处调用 `CredentialRegistry::register`，为每种类型提供
`type_id` / `display_name` / 字段 Schema / `validate` / 可选 `test_connectivity`。
DNS 提供商的做法见 `crates/acmecast-server/src/credentials.rs`；SSH 类型尚未注册，
详见 [12-operations-and-limitations.md](12-operations-and-limitations.md)。

## 5.5 凭据的引用与删除

- 流水线步骤通过 `account_credential_id`（`cert.apply`、`cert.store`）、
  `dns_credential_id`（`cert.apply`）或 SSH 认证的 `credential_id` 引用凭据；
- 删除被引用的凭据会被拒绝（409），请先改流水线或删除流水线；
- 凭据 `id` 一旦被证书记录引用（`acme_account_access_id`），吊销证书时仍需该账号，
  删除凭据会导致这些证书无法吊销。
