# 凭据体系

凭据是流水线访问外部系统（CA、DNS 提供商、SSH 主机）或投递通知
（IM 机器人 / 通用 webhook / SMTP）的认证材料。
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
| `ca` | 是 | CA 别名：`letsencrypt`、`letsencrypt-staging`、`zerossl`、`google`、`google-test`、`freessl`、`custom` |
| `directory_url` | 条件 | `ca=custom` 时必填；在内置 CA 上填写会**覆盖内置端点**（可走自建反代） |
| `eab_kid` / `eab_hmac_key` | 条件 | `zerossl`/`google`/`google-test`/`freessl` 在 `credentials` 为空时必填且必须成对；已有有效 `credentials` 时由其复用，EAB 不再必填。`hmac_key` 为 base64url（兼容标准 base64） |
| `credentials` | 否 | ACME 账号凭据 JSON（KID、账号私钥、目录端点）。**不要手填**：首次使用时自动注册并写回；有值时后续签发复用，不再使用 EAB 重新注册 |
| `proxy_mode` | `direct` | 出网代理模式：`direct` / `inherit` / `custom`（见下文「出网代理」） |
| `proxy_url` | 空 | `proxy_mode=custom` 时的自定义代理（见下文「出网代理」） |

内置 CA 端点：

| 别名 | 目录 URL |
|---|---|
| `letsencrypt` / `le` | `https://acme-v02.api.letsencrypt.org/directory` |
| `letsencrypt-staging` / `le-staging` | `https://acme-staging-v02.api.letsencrypt.org/directory` |
| `zerossl` | `https://acme.zerossl.com/v2/DV90`（要求 EAB，管理入口：https://app.zerossl.com/developer ） |
| `google` / `gts` | `https://dv.acme-v02.api.pki.goog/directory`（要求 EAB，HMAC 文档：https://docs.cloud.google.com/certificate-manager/docs/public-ca-tutorial#request-key-hmac ） |
| `google-test` / `gts-test` | `https://dv.acme-v02.test-api.pki.goog/directory`（要求 EAB，同上文档） |
| `freessl` / `free-ssl` | `https://acme.litessl.com/acme/v2/directory`（要求 EAB，管理入口：https://freessl.cn/automation/eab-manager ） |

账号生命周期：凭据中 `credentials` 为空 = 尚未在 CA 侧建立账号，首次被
`cert.apply` 使用时注册（`terms_of_service_agreed=true`，有 EAB 时一并携带），签发的账号凭据
写回记录；之后的运行一律复用同一账号（不再使用 EAB）。多条流水线引用同一份凭据时共享同一
CA 账号，不会重复注册（也避免撞上 CA 的账号配额）。

连通性测试：已注册账号会恢复会话并取一次 Directory（真实探测 CA 连通性）；
未注册账号返回 `unavailable`（「尚未在 CA 侧建立账号，无从验证」）。

### `cloud.tencent` — 腾讯云（DNS、部署）

腾讯云账号 API 密钥，同一套 `SecretId` / `SecretKey` 同时服务 DNS 提供商
（云解析 DNSPod）与 SSL 证书部署（`cert.deploy` 的 `tencent-ssl` 目标）。
字段与 `dns.tencent-eo` 一致，仅类型标识独立。

| 字段 | 默认 | 说明 |
|---|---|---|
| `secret_id` | — | 必填，API 密钥 ID |
| `secret_key` | — | 必填，API 密钥 Key（TC3 签名派生密钥来源） |
| `account_site` | `cn` | 账号站点：`cn`（国内站）/ `intl`（国际站）；决定请求端点 |
| `proxy_mode` | `direct` | 出网代理模式：`direct` / `inherit` / `custom`（见下文「出网代理」） |
| `proxy_url` | 空 | `proxy_mode=custom` 时的自定义代理（见下文「出网代理」） |

`account_site` 选错站点的表现是执行期的 `AuthFailure.*`——同一对密钥只能
登录其中一边。SSL 证书 API 国内版与国际版同构（Action 与参数一致）。

### `dns.tencent-eo` — 腾讯云 EdgeOne（DNS）

EdgeOne（TEO）DNS 提供商凭据。字段与 `cloud.tencent` 完全一致（同为腾讯云账号
密钥），类型标识独立以便区分用途。EdgeOne 仅服务 DNS、无 SSL 证书部署能力，
`cert.deploy` 的 `tencent-ssl` 目标**不接受** `dns.tencent-eo` 凭据（需使用
`cloud.tencent` 类型）。

| 字段 | 默认 | 说明 |
|---|---|---|
| `secret_id` | — | 必填，API 密钥 ID |
| `secret_key` | — | 必填，API 密钥 Key |
| `account_site` | `cn` | 账号站点：`cn` / `intl` |
| `proxy_mode` | `direct` | 出网代理模式：`direct` / `inherit` / `custom`（见下文「出网代理」） |
| `proxy_url` | 空 | `proxy_mode=custom` 时的自定义代理（见下文「出网代理」） |

### `cloud.cloudflare` — Cloudflare DNS

| 字段 | 说明 |
|---|---|
| `api_token` | API Token，需目标域名的 DNS 编辑权限 |
| `proxy_mode` | 出网代理模式：`direct` / `inherit` / `custom`（见下文「出网代理」） |
| `proxy_url` | `proxy_mode=custom` 时的自定义代理（见下文「出网代理」） |

### `cloud.aliyun` — 阿里云 DNS

| 字段 | 说明 |
|---|---|
| `access_key_id` | AccessKey ID |
| `access_key_secret` | AccessKey Secret |
| `proxy_mode` | 出网代理模式：`direct` / `inherit` / `custom`（见下文「出网代理」） |
| `proxy_url` | `proxy_mode=custom` 时的自定义代理（见下文「出网代理」） |

### 出网代理（`proxy_mode`）

八类凭据（DNS 提供商四类 `cloud.tencent` / `dns.tencent-eo` / `cloud.cloudflare` /
`cloud.aliyun`、ACME 账号 `acme.account`、部署档案 `deploy.ssh` / `deploy.s3` /
`deploy.webdav`）都用 `proxy_mode` 三态控制出网代理，用于提供商 API 不可直连的
网络环境（如国内访问 `api.cloudflare.com` 超时）。模式语义：

| `proxy_mode` | 行为 |
|---|---|
| `direct`（缺省） | 直连，不继承全局代理——行为与引入代理字段之前完全一致 |
| `inherit` | 继承全局代理（`config.toml` 的 `[proxy] url` 或 `ACMECAST_PROXY_URL`，env 优先）；全局未配则直连 |
| `custom` | 走本凭据的 `proxy_url`，覆盖全局代理 |

行为要点：

- `proxy_url` 仅在 `proxy_mode=custom` 时显示并生效（表单联动显隐）；支持
  `socks5://`、`http://`、`https://` 三种 URL，允许 `socks5://user:pass@host:port`
  形态（内嵌凭据会被脱敏，不出现在错误信息里）；
- **存量凭据无需迁移**：变更前保存的凭据没有 `proxy_mode`——旧 `proxy_url` 有值
  视为 `custom` + 该值，两字段都缺视为 `direct`，读取时归一化，不报错；
- **代理只覆盖持有该凭据的出网调用**，链路随类型不同：
  - DNS 提供商（四类）：提供商 API 调用（查询/写入/删除）与凭据页连通性测试；
  - `acme.account`：ACME 目录/账号/下单请求与凭据页连通性测试（含吊销）；
  - `deploy.ssh` / `deploy.s3` / `deploy.webdav`：凭据页连通性测试，以及
    `cert.deploy` 部署执行（SSH 走 SOCKS5/HTTP CONNECT 隧道，S3/WebDAV 走
    HTTP 代理）——但**部署执行链路没有全局代理来源**，这三类的 `inherit`
    在部署执行时等同直连，仅凭据自带 `custom` URL 才经代理（探测与测试
    链路有全局来源，`inherit` 正常继承）；
  - `notify.feishu` / `notify.dingtalk` / `notify.generic`（运行结束简讯投递与凭据页连通性测试，webhook 经 `reqwest::Proxy`）与 `notify.email`（运行结束简讯投递与连通性测试，SMTP 经 SOCKS5/HTTP-CONNECT 隧道）；同一流水线里 `direct` 模式的凭据也保持直连；
- `custom` 模式的非法 URL（不支持的 scheme 或无法解析）在凭据保存时被拒绝并
  定位到 `proxy_url` 字段；代理不可达时错误信息会指向代理地址，凭据页「测试」
  可提前暴露；
- 全局代理的配置方式与合并优先级（`builtin < config.toml < env`）见
  [配置](03-configuration.md)的「全局代理」——DoH 解析器的代理自动回退也读同一份取值。

### `deploy.ssh` — SSH 主机档案

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
| `proxy_mode` | `direct` | 出网代理模式：`direct` / `inherit` / `custom`（见下文「出网代理」） |
| `proxy_url` | 空 | `proxy_mode=custom` 时的自定义代理（见下文「出网代理」） |

`private_key` 与 `password` 必须**恰好其一**（皆缺或皆有都报错）。

凭据字段字符串在保存/读取时递归 trim 首尾空白——粘贴带换行的密钥不会让
厂商认证在莫名其妙的地方失败。

### `notify.*` — 通知渠道（四类）

通知渠道以凭据形式管理，供流水线的「通知」字段引用，运行结束（成功或
失败）时向渠道投递简讯（见[配置](03-configuration.md)的「通知」）。四类
适配器：

| 类型标识 | 渠道 | 消息格式 | 签名/认证 |
|---|---|---|---|
| `notify.feishu` | 飞书（Lark）自定义机器人 | text | 配置 `secret` 时启用（`timestamp` + `sign`） |
| `notify.dingtalk` | 钉钉自定义机器人 | text | 配置 `secret` 时启用官方加签（URL 查询参数） |
| `notify.generic` | 通用 webhook（统一请求方案） | 可自定义模板 | 无内置签名，认证头自行放 `headers` |
| `notify.email` | SMTP 邮件 | 纯文本邮件 | `username` + `password` 认证（可选） |

`notify.feishu` / `notify.dingtalk` 字段：

| 字段 | 必需 | 说明 |
|---|---|---|
| `name` | 是 | 渠道名（简讯与日志中标识渠道） |
| `url` | 是 | 机器人 webhook 地址（http/https） |
| `secret` | 否 | 机器人签名密钥：填写即启用签名校验——飞书签名合并进请求体顶层字段，钉钉经 URL 查询参数加签 |
| `proxy_mode` / `proxy_url` | 否 | 出网代理（见下文「出网代理」） |

`notify.generic` 字段：

| 字段 | 默认 | 说明 |
|---|---|---|
| `name` | — | 必填，渠道名 |
| `url` | — | 必填，请求地址 |
| `method` | `POST` | `POST` / `PUT` / `PATCH` |
| `headers` | 空 | 额外静态请求头（JSON 对象），如自建端点的 `X-API-Key` |
| `body_template` | 空 | 请求体模板，见下 |
| `proxy_mode` / `proxy_url` | 否 | 出网代理（见下文「出网代理」） |

`notify.email` 字段：

| 字段 | 默认 | 说明 |
|---|---|---|
| `name` | — | 必填，渠道名 |
| `host` | — | 必填，SMTP 服务器（不含端口与协议前缀） |
| `port` | 随 `tls` | `implicit`→465、`starttls`→587、`none`→25 |
| `tls` | `implicit` | `implicit`（连接即 TLS）/ `starttls`（先明文连接后升级）/ `none`（不加密，仅限内网调试） |
| `from` | — | 必填，发件人地址 |
| `to` | — | 必填，收件人列表（表单每行一个），一次 SMTP 会话送达全部收件人 |
| `username` / `password` | 空 | 同时填写才启用 SMTP 认证，都缺省时匿名连接（内网 relay 场景） |
| `proxy_mode` / `proxy_url` | 否 | 出网代理（见下文「出网代理」） |

**请求体模板（generic）**：占位符 `{{var}}` 填充简讯素材并做 JSON 转义
（值放在引号内使用，如 `"{{domains}}"`）。可用变量：`event`、`title`、
`pipeline`、`trigger`（原始标识）、`trigger_label`（中文）、`occurred_at`、
`domains`（逗号连接）、`timestamp`（秒级时间戳）、`sign`（未签名时为空串）。
未配置模板时发统一简讯负载 JSON：恒有 `event` / `title` / `pipeline` /
`trigger` / `occurred_at`；域名非空时带 `domains` 数组；失败简讯另有
`failure`（`step_type` / `step` / `reason`）；成功与失败简讯都带
`summary`（`result` / `duration_secs` / `steps`）。

ntfy.sh 推送示例（ntfy 的 JSON 发布格式走根路径，`topic` 写在请求体里；
若直接 POST 到 `/<topic>`，JSON 会被当成纯文本消息）：

```json
{
  "name": "ntfy",
  "url": "https://ntfy.sh/",
  "method": "POST",
  "body_template": "{\"topic\": \"acmecast-换成随机私有主题\", \"title\": \"{{title}}\", \"message\": \"{{pipeline}}（{{trigger_label}}）\\n域名：{{domains}}\", \"tags\": [\"white_check_mark\"], \"priority\": 4}",
  "headers": { "Authorization": "Bearer tk_xxx" }
}
```

- 公共实例上 topic 名就是订阅凭据（知道即可订阅与推送），请用随机主题名；
- ntfy JSON 格式的字段类型比 HTTP header 方式严格：`priority` 必须是
  `1`–`5` 的整数（1=min、3=default、4=high、5=urgent/max），写 `high`
  之类的字符串会被整体拒绝（报 40024 invalid JSON）。

**平台接入**：在飞书/钉钉群里添加「自定义机器人」（webhook），钉钉建议
选择「加签」安全设置并把密钥填入 `secret`；飞书同理可开启签名校验。

**连通性测试**：四类都向渠道发一条测试消息（飞书 / 钉钉按平台 text 格式，
generic 按上述负载或渲染模板，email 发一封测试邮件），且按凭据的
代理模式出网：`direct` 直连 / `inherit` 继承全局代理 / `custom` 走凭据自带
URL（webhook 经 `reqwest::Proxy`，邮件经 SOCKS5/HTTP-CONNECT 隧道）；代理
不可达时失败原因含代理主机。`notify.generic` 保存时还会校验 `headers` 与
`body_template` 的合法性，非法值定位到对应字段。

## API 速查

- `GET /api/credential-types`：列出已注册类型（含 JSON Schema，前端渲染表单用）
- `GET/POST /api/credentials`、`GET/PUT/DELETE /api/credentials/{id}`
- `POST /api/credentials/{id}/test`

详见 [REST API](09-api.md)。
