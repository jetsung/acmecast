# 配置参考

acmecast-server 的配置合并优先级为 **内置默认 < config.toml < 环境变量**。
文件不存在或为空时回退内置默认；文件解析失败（字段类型不匹配等）启动中止。
`[server]` 段启用了 `deny_unknown_fields`：写错键名会直接报错。

## 配置文件位置

解析顺序：显式 `--config <path>` → 环境变量 `ACMECAST_CONFIG` →
`$ACMECAST_DATA_DIR/config.toml`（`ACMECAST_DATA_DIR` 未设时为 `data/config.toml`）。

首次启动若配置文件不存在，会自动生成带注释的模板（不覆盖已存在文件）；
也可用 `acmecast-server init [--force]` 手动生成。配置修改需重启生效。

## 配置项总表

| 配置项 | config.toml 键 | 环境变量 | 默认值 | 说明 |
|---|---|---|---|---|
| 监听地址 | `server.listen_addr` | `ACMECAST_LISTEN` | `0.0.0.0:8080` | `host:port` |
| 数据目录 | `server.data_dir` | `ACMECAST_DATA_DIR` | `./data` | 数据库、证书文件、凭据密文均在此 |
| 数据库连接串 | `server.database_url` | `ACMECAST_DATABASE_URL` | `sqlite://<data_dir>/acmecast.db?mode=rwc` | 支持 sqlite/mysql/postgres，见[数据库与数据目录](11-database.md) |
| 前端静态目录 | `server.static_dir` | `ACMECAST_STATIC_DIR` | 未设置 | 未设置时不托管前端（仅 API） |
| 关闭鉴权 | `server.auth_disabled` | `ACMECAST_AUTH_DISABLED` | `false` | `1/true/yes`（大小写不敏感）为真；仅限本地单机 |
| 请求体上限 | `server.body_limit_bytes` | `ACMECAST_BODY_LIMIT_BYTES` | `2097152`（2 MiB） | 超出返回 413 `payload_too_large`；env 须 > 0 |
| 跳过 CA 证书校验 | `server.accept_invalid_acme_certs` | `ACMECAST_INSECURE_SKIP_VERIFY` | `false` | 仅供自建测试 CA（pebble 等）使用 |

## 安全相关环境变量（仅环境变量，不入 config.toml）

| 变量 | 必需 | 说明 |
|---|---|---|
| `ACMECAST_JWT_SECRET` | **是** | HS256 JWT 签名密钥；缺失启动失败。建议 `openssl rand -base64 48` |
| `ACMECAST_CREDENTIAL_KEY` | **是** | 凭据静态加密密钥（AES-256-GCM，base64 编码的 32 字节）。也接受双下划线别名 `ACMECAST__SECURITY__CREDENTIAL_KEY`。**更换后已存凭据无法解密** |
| `ACMECAST_ADMIN_PASSWORD_HASH` | 否 | 管理员口令的 Argon2 PHC 哈希；未配置则服务可启动但无人能登录（启动日志告警，登录返回 500 `configuration_error`） |
| `ACMECAST_ADMIN_PASSWORD_HASH_FILE` | 否 | 哈希的文件路径；设置后优先读文件，文件不存在/空白时回退到 `ACMECAST_ADMIN_PASSWORD_HASH`；读取失败（如指向目录）视为配置错误且不回退 |
| `ACMECAST_ADMIN_USERNAME` | 否 | 管理员用户名，默认 `admin` |
| `ACMECAST_TOKEN_TTL_HOURS` | 否 | 访问令牌有效期（小时），默认 `12`；非法值回落 12 |

## 日志

- `RUST_LOG`：tracing 过滤器，缺省 `info`；支持 `info,acmecast_cert=debug`
  这类按模块调级的写法。
- 每个 HTTP 请求自动携带 `x-request-id`（UUID）并贯穿日志 span。

## DNS 解析器扩展

config.toml 支持 `[[resolvers]]` 段追加自定义解析器（模板内附阿里云/Cloudflare
DoH 示例）：

```toml
[[resolvers]]
type = "doh"                      # doh / dot / dns
endpoint = "https://dns.alidns.com/dns-query"
name = "alidns"                   # 可选，缺省取主机名
```

- `doh`：必须是 `https://`；
- `dot`：仅校验并告警跳过（传输层未实现）；
- `dns`：必须是 IP 地址。

也可用环境变量 `ACMECAST_DOH_RESOLVERS`（逗号分隔的 `名称=端点`，或仅端点）；
设为 `none` 表示跳过传播等待（不推荐）。合并顺序：内置 → config 条目 →
环境变量，按归一化端点去重。详见[DNS 提供商与传播检查](06-dns-providers.md)。

## webhook 通知

config.toml 支持 `[[notifications]]` 段声明 webhook 通知渠道：`cert.apply`
（证书申请成功）、`cert.deploy`（部署成功）后向 IM 群机器人推送文本消息。
内置三种适配器：

| provider | 平台 | 消息格式 | 签名 |
|---|---|---|---|
| `feishu` | 飞书（Lark）自定义机器人 | text | 配置 `secret` 时启用（`timestamp` + `sign`） |
| `dingtalk` | 钉钉自定义机器人 | text | 配置 `secret` 时启用官方加签（URL 查询参数） |
| `generic` | 通用 webhook（统一请求方案） | 可自定义模板 | 可指定 `sign`（feishu / dingtalk） |

```toml
[[notifications]]
name = "ops-feishu"          # 渠道名，须唯一（日志与测试端点用它定位）
provider = "feishu"          # feishu / dingtalk / generic
url = "https://open.feishu.cn/open-apis/bot/v2/hook/xxxxxxxx"
secret = "..."               # 可选；启用机器人签名校验时填写
events = ["cert.apply", "cert.deploy"]   # 订阅事件，不能为空
enabled = true               # 可选，缺省 true
```

### generic 统一请求方案

`generic` 渠道把 URL、签名方式、请求方法、请求头与请求体模板全部交给
配置；飞书与钉钉两种签名算法写死在代码中，只需用 `sign` 指定方式：

```toml
[[notifications]]
name = "ops-custom"
provider = "generic"
url = "https://ops.example.com/api/events"
secret = "sk-xxx"            # 签名密钥或 API-KEY
sign = "feishu"              # feishu / dingtalk，缺省不签名
method = "POST"              # POST / PUT / PATCH，缺省 POST
headers = { "X-API-Key" = "sk-xxx" }   # 额外静态请求头，可选
body_template = '{"text": "{{title}}｜{{pipeline}}｜{{domains}}"}'
events = ["cert.apply", "cert.deploy"]
```

- **签名方式**（算法内置，配置只选方式）：`feishu` 以
  `"{秒级时间戳}\n{secret}"` 为密钥对空串计算 HmacSHA256，签名与时间戳
  自动合并进请求体顶层字段；`dingtalk` 以 `secret` 为密钥对
  `"{毫秒级时间戳}\n{secret}"` 计算，经 URL 编码后自动追加到地址查询
  参数。两种方案都要求 `secret` 必填。

ntfy.sh 推送示例（ntfy 的 JSON 发布格式走根路径，`topic` 写在请求体里；
若直接 POST 到 `/<topic>`，JSON 会被当成纯文本消息）：

```toml
[[notifications]]
name = "ntfy"
provider = "generic"
url = "https://ntfy.sh/"      # 自建实例换成你的地址
method = "POST"
body_template = '{"topic": "acmecast-换成随机私有主题", "title": "{{title}}", "message": "{{pipeline}}（{{trigger_label}}）\n域名：{{domains}}", "tags": ["white_check_mark"], "priority": 4}'
# headers = { Authorization = "Bearer tk_xxx" }   # 实例开启访问控制时填写
events = ["cert.apply", "cert.deploy"]
```

- 公共实例上 topic 名就是订阅凭据（知道即可订阅与推送），请用随机
  主题名；部署事件想把目标也带进消息，在 `message` 里追加 `\n{{target}}`。
- ntfy JSON 格式的字段类型比 HTTP header 方式严格：`priority` 必须是
  `1`–`5` 的整数（1=min、3=default、4=high、5=urgent/max），写 `high`
  之类的字符串会被整体拒绝（报 40024 invalid JSON）。
- **请求体模板**：占位符 `{{var}}` 填充事件素材并做 JSON 转义（值放在
  引号内使用，如 `"{{domains}}"`）。可用变量：`event`、`title`、
  `pipeline`、`trigger`（原始标识）、`trigger_label`（中文）、
  `occurred_at`、`domains`（逗号连接）、`target`、`timestamp`（签名方
  案对应精度）、`sign`。模板里显式写了 `{{sign}}` 时签名以模板为准，
  不再自动合并。
- **缺省消息**：未配置模板时按签名方式选格式——`sign = feishu` /
  `dingtalk` 分别用对应平台的 text 格式（与专用适配器一致）；无签名时
  发统一事件负载 JSON（`event`/`title`/`pipeline`/`trigger`/
  `occurred_at`/`domains`/`target`），保持向后兼容。
- **专用适配器的兼容性**：`sign`、`method`、`headers`、`body_template`
  仅 `generic` 渠道可用——`feishu` / `dingtalk` 渠道配置这些字段会被
  视为非法条目跳过（它们的请求形态写死）。把渠道从专用适配器迁到
  `generic` 时，复制原有字段并加上 `sign` 即可，消息格式不变。
- **事件词汇**：`cert.apply`（证书申请成功）、`cert.deploy`（部署成功）。
  申请消息含域名列表；部署消息含部署目标（SSH 主机 / 本地路径）与域名。
- **平台接入**：在飞书/钉钉群里添加「自定义机器人」（webhook），钉钉建议
  选择「加签」安全设置并把密钥填入 `secret`；飞书同理可开启签名校验。
- **校验语义**：非法条目（`provider` 不在内置之列、`events` 含未知事件、
  `url` 非 http(s)、重名，以及 `sign`/`method`/`headers`/`body_template`
  不合法或专用适配器误配扩展字段）启动时跳过并 `warn`，其余渠道照常
  加载；`enabled = false` 的渠道保留配置但不投递。
- **投递行为**：事件触发后异步投递，10 秒超时，失败按 1s/2s 退避最多
  3 次尝试；投递成败不影响流水线运行结果。IM 平台有频控（如钉钉
  约 20 条/分钟），大量流水线同时运行时可能触发限流。
- **连通性测试**：配置后重启，调用测试端点验证（带鉴权）：

  ```bash
  curl -X POST http://127.0.0.1:8080/api/notifications/test \
    -H "Authorization: Bearer $TOKEN" \
    -H 'Content-Type: application/json' \
    -d '{"name": "ops-feishu"}'      # 缺省 body 测试全部启用渠道
  ```

  响应逐渠道给出 `ok` 与失败原因。

### 多渠道配置示例

一份脱敏自实际运行配置的三渠道示例，覆盖三种接入形态：飞书签名、
钉钉加签、ntfy 自建实例走 generic 统一请求方案：

```toml
[[notifications]]
name = "lark"
provider = "feishu"   # Lark（open.larksuite.com）与飞书同协议，适配器通用
url = "https://open.larksuite.com/open-apis/bot/v2/hook/xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx"
secret = "机器人签名密钥"    # 机器人开启「签名校验」时填写；不填即无签名
events = ["cert.apply", "cert.deploy"]

[[notifications]]
name = "dingtalk"
provider = "dingtalk"
url = "https://oapi.dingtalk.com/robot/send?access_token=xxxxxxxx"
secret = "SECxxxxxxxxxxxxxxxx"   # 钉钉「加签」安全设置给出的密钥（SEC 开头）
events = ["cert.apply", "cert.deploy"]

[[notifications]]
name = "ntfy"
provider = "generic"
url = "https://ntfy.example.com/"   # ntfy 自建实例（根路径，topic 在请求体里）
method = "POST"
body_template = '{"topic": "acme", "title": "{{title}}", "message": "{{pipeline}}（{{trigger_label}}）\n域名：{{domains}}", "tags": ["white_check_mark"], "priority": 4}'
headers = { "Authorization" = "Bearer tk_xxxx" }   # 实例开启访问控制时填写
events = ["cert.apply", "cert.deploy"]
```

三者的签名方式各不相同但都写死在服务端代码中：`feishu` 把签名放进
请求体、`dingtalk` 放进地址查询参数、`ntfy` 凭 `Authorization` 请求头
鉴权（generic 渠道不配 `sign` 即不签名）。

`secret` 明文存于 config.toml（数据目录权限受限 `0700`），请勿把该文件
提交进版本控制；泄露后果限于向对应群机器人发送消息，风险远低于 DNS /
SSH 凭据。

## 最小可用配置示例

```toml
# data/config.toml —— 只写与非默认不同的项
[server]
listen_addr = "0.0.0.0:8080"
static_dir = "/static"          # 托管前端时
# database_url = "postgres://acmecast:****@127.0.0.1:5432/acmecast"
```

配合环境变量：

```bash
export ACMECAST_JWT_SECRET='...'
export ACMECAST_CREDENTIAL_KEY='...'
export ACMECAST_ADMIN_PASSWORD_HASH='$argon2id$...'
```
