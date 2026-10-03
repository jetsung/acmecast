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
