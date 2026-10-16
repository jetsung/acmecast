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
| 缓存连接串 | `server.redis_url` | `ACMECAST_REDIS_URL` | 未设置 | Redis / Valkey 缓存连接串（支持别名 `ACMECAST_VALKEY_URL`、`REDIS_URL`），加速执行历史与实时日志；未设置时直写数据库 |
| 关闭鉴权 | `server.auth_disabled` | `ACMECAST_AUTH_DISABLED` | `false` | `1/true/yes`（大小写不敏感）为真；仅限本地单机 |
| 请求体上限 | `server.body_limit_bytes` | `ACMECAST_BODY_LIMIT_BYTES` | `2097152`（2 MiB） | 超出返回 413 `payload_too_large`；env 须 > 0 |
| 跳过 CA 证书校验 | `server.accept_invalid_acme_certs` | `ACMECAST_INSECURE_SKIP_VERIFY` | `false` | 仅供自建测试 CA（pebble 等）使用 |
| 全局出网代理 | `proxy.url` | `ACMECAST_PROXY_URL` | 未设置（直连） | 凭据 `proxy_mode=inherit` 与 DoH 回退共用，见下文「全局代理」 |

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

config.toml 支持 `[[resolvers]]` 段追加自定义解析器（模板内附阿里云/腾讯
DNSPod/Cloudflare DoH 示例，`data/config.toml` 与 `acmecast-server config init`
生成的模板已包含可直接取消注释的片段）：

```toml
[[resolvers]]
type = "doh"
endpoint = "https://dns.alidns.com/dns-query"
name = "阿里 DNS"                  # 可选，缺省取主机名

[[resolvers]]
type = "doh"
endpoint = "https://doh.pub/dns-query"
name = "腾讯 DNSPod"               # 腾讯公共 DoH（域名形式）

[[resolvers]]
type = "doh"
endpoint = "https://1.12.12.12/dns-query"
name = "腾讯 DNSPod（IP 直连）"    # 同上，IP 直连避免 SNI 劫持时更稳
```

- `doh`：必须是 `https://`；腾讯推荐 `https://doh.pub/dns-query`（域名）与
  `https://1.12.12.12/dns-query` 二选一或并存，备用 `https://120.53.53.53/dns-query`（`dns/pod` 同源）；
- `dot`：仅校验并告警跳过（传输层未实现）；
- `dns`：必须是 IP 地址；腾讯传统 DNS `119.29.29.29` / `119.28.28.28`
  （`dnsdata/dns_servers.txt` 亦有收录：`腾讯 DNS (DNSPod)|119.28.28.28`、`DNSPod DNS+|119.29.29.29`）
  目前仅校验、传输层尚未实现，配置后会告警跳过、保留作清单备用。

也可用环境变量 `ACMECAST_DOH_RESOLVERS`（逗号分隔的 `名称=端点`，或仅端点，例如
`ACMECAST_DOH_RESOLVERS="腾讯 DoH=https://doh.pub/dns-query,https://1.12.12.12/dns-query"`）；
设为 `none` 表示跳过传播等待（不推荐）。合并顺序：内置 → config 条目 →
环境变量，按归一化端点去重。详见[DNS 提供商与传播检查](06-dns-providers.md)。

### 全局代理

`[proxy]` 段配置全局出网代理。DNS 提供商凭据（`proxy_mode=inherit`）与传播检查的
DoH 解析器共用同一份取值——改一次即全局生效，不必逐凭据重复填写：

```toml
[proxy]
url = "socks5://127.0.0.1:7890"   # 也支持 http:// 与 https://；留空或本段缺省即直连
```

也可用环境变量 `ACMECAST_PROXY_URL` 覆盖文件值（原 DoH 专用的
`ACMECAST_DOH_PROXY_URL` 已废弃合并到此，DoH 与凭据不再各配一遍）：

```bash
# .env 或部署环境
ACMECAST_PROXY_URL=socks5://127.0.0.1:7890
```

合并优先级与其它配置一致：

| 优先级 | 来源 | 说明 |
|---|---|---|
| 1（最低） | 内置默认 | 直连，不给任何渠道注入代理 |
| 2 | `config.toml` 的 `[proxy] url` | 非空覆盖内置 |
| 3（最高） | 环境变量 `ACMECAST_PROXY_URL` | 非空覆盖文件值 |

取值校验口径与凭据级代理一致：仅 `socks5://` / `http://` / `https://` 且可解析，
非法值在**启动时**被拒绝并提示支持的 scheme；未配置、空串或仅空白均维持直连，
也不会隐式读取 `HTTPS_PROXY` 等标准环境变量。

两个消费者：

- **凭据出网（`proxy_mode=inherit`）**：所有带代理字段的凭据类型——DNS 提供商
  （`cloud.tencent` / `dns.tencent-eo` / `cloud.cloudflare` / `cloud.aliyun`）、
  ACME 账号（`acme.account`）与 SSH 主机 / S3 / WebDAV 档案（`deploy.ssh` /
  `deploy.s3` / `deploy.webdav`）。继承生效的链路随类型不同：DNS 提供商 API
  调用与 ACME 申请/探测在运行链路上继承全局代理；SSH / S3 / WebDAV 仅凭据页
  连通性测试继承——部署执行链路没有全局代理来源，`inherit` 等同直连，只有
  凭据自带 `custom` URL 时部署出网才经代理。`direct` 不继承、`custom` 走
  凭据自己的 `proxy_url`，逐凭据可控，见[凭据体系](04-credentials.md)；
- **传播检查的 DoH 解析器**：DoH 端点（如 `cloudflare-dns.com`）所在网络不可直连时，
  传输层初始直连，直连查询失败后自动经该代理重试一次；重试成功则缓存「需经代理」
  偏好，后续轮次直接用代理传输层，不再每轮重复直连探测。未配置代理时不做回退，
  直连失败按既有熔断策略计次。

`proxy_mode=direct` 的凭据在任何链路都保持直连；通知投递按渠道凭据的
代理模式出网（`direct` 直连 / `inherit` 继承全局代理 / `custom` 走凭据自带 URL）。

## 通知

通知由「通知凭据 + 流水线通知字段」驱动，`config.toml` 不再声明渠道：

1. 在凭据页创建通知类凭据（`notify.feishu` / `notify.dingtalk` /
   `notify.generic` / `notify.email`，四类的字段、消息格式与签名方式见
   [凭据体系](04-credentials.md)）；
2. 在流水线编辑页的「通知」多选里选中要接收简讯的凭据（可多选，存的是
   凭据 ID 列表）。

运行结束（成功或失败）时，每个选中渠道各收到一条简讯。原
`[[notifications]]` 声明、`events` 订阅过滤、`enabled` 开关与
`notify.summary` 步骤都已移除：渠道是否接收通知完全由流水线的「通知」
字段决定，未选中任何渠道即不通知。

### 运行结束简讯

| 运行结果 | 标题 | 内容 |
|---|---|---|
| 成功 | 流水线运行成功 | 流水线名、触发来源、时间、域名列表、总耗时、各步骤状态（成功 / 跳过） |
| 失败 | 流水线运行失败 | 上述字段 + 失败步骤（`type_id` 与第几步）与失败原因；步骤状态含失败 / 未执行 / 跳过 |

- 域名列表取流水线里 `cert.apply` 步骤输入的 `domains`；
- 每次运行结束至多投递一条简讯；「通知」为空即不投递；
- 渠道逐个从凭据解密解析：凭据被删、字段非法或解密失败时该渠道跳过并记
  `warn`，其余渠道照常投递。

### 投递行为

- 异步旁路：事件受理即返回，投递在后台进行；单次请求 10 秒超时，失败按
  1s / 2s 退避最多 3 次尝试；投递成败不影响流水线运行结果。
- 通知投递按渠道凭据的代理模式出网（`direct` 直连 / `inherit` 继承全局代理 / `custom` 走凭据自带 URL）：webhook 三类经 `reqwest::Proxy`，邮件经 SOCKS5/HTTP-CONNECT 隧道后完成 SMTP 会话；代理不可达时错误含代理主机。
- IM 平台有频控（如钉钉约 20 条/分钟），大量流水线同时运行时可能触发限流。

### 连通性测试

凭据页每条通知凭据都可点「测试」向渠道发一条测试消息（等价于调用
`POST /api/credentials/{id}/test`），响应给出 `ok` 或失败原因。

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
