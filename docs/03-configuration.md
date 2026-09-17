# 3. 配置参考

服务端**全部通过环境变量配置**，均带默认值（除标注「必需」外为可选）。
配置随进程启动读取一次，修改后需重启。

## 3.1 环境变量总表

### 基础

| 变量 | 默认 | 说明 |
| --- | --- | --- |
| `ACMECAST_LISTEN` | `0.0.0.0:8080` | HTTP 监听地址（`host:port`） |
| `ACMECAST_DATA_DIR` | `./data`（容器内 `/data`） | 数据目录 |
| `ACMECAST_DATABASE_URL` | 数据目录下的 SQLite | 数据库连接串（见 3.3） |
| `ACMECAST_STATIC_DIR` | 未设置 | 前端静态资源目录；未设置时不托管静态资源（仅 API） |
| `ACMECAST_BODY_LIMIT_BYTES` | `2097152`（2 MiB） | 请求体体积上限，超出返回 `413 payload_too_large` |

### 安全（部署前必须核对）

| 变量 | 必需 | 说明 |
| --- | --- | --- |
| `ACMECAST_JWT_SECRET` | 是 | 访问令牌 HS256 签名密钥。缺失时服务**拒绝启动** |
| `ACMECAST_CREDENTIAL_KEY` | 是（写凭据需要） | 凭据静态加密密钥，AES-256-GCM，base64 编码 32 字节。`openssl rand -base64 32`。**更换后已存凭据无法解密** |
| `ACMECAST_ADMIN_USERNAME` | 否 | 管理员用户名，默认 `admin` |
| `ACMECAST_ADMIN_PASSWORD_HASH` | 否 | 管理员口令的 Argon2 PHC 哈希。未配置时服务可启动但**无人能登录**（启动日志会告警） |
| `ACMECAST_TOKEN_TTL_HOURS` | 否 | 访问令牌有效期（小时），默认 `12` |
| `ACMECAST_AUTH_DISABLED` | 否 | `1`/`true`/`yes` 关闭鉴权——**仅限本地单机**，默认强制鉴权 |
| `ACMECAST_INSECURE_SKIP_VERIFY` | 否 | `1` 跳过 ACME CA 的 TLS 证书校验，仅为自建测试 CA（pebble 等）准备 |

`ACMECAST_CREDENTIAL_KEY` 还有一个等价的双下划线写法
`ACMECAST__SECURITY__CREDENTIAL_KEY`（两种都读，后者优先）。

### 日志

| 变量 | 默认 | 说明 |
| --- | --- | --- |
| `RUST_LOG` | `info` | tracing 过滤指令，如 `acmecast_server=debug,info`、`warn` |

日志默认以文本格式输出到标准输出。

### 固定策略（不可配置）

- 登录失败限流：连续失败 **5** 次后锁定来源 **900 秒**；
- 续期阈值：剩余有效期 ≤ **30 天** 进入续期窗口；
- 到期扫描周期：**1 小时**；cron 检查周期：**30 秒**；续期去重窗口：**1 小时**。

> 这些参数在 `acmecast-scheduler` 的 `SchedulerConfig` 中定义，当前以默认值
> 启动，未通过环境变量暴露。

## 3.2 数据目录

```
data/
├── acmecast.db                 # SQLite（使用外部数据库时无此文件）
└── certs/
    └── <fingerprint>/
        ├── cert.pem            # 证书链 PEM（含中间证书）
        └── key.pem             # 私钥 PEM
```

- 证书按 **SHA-256 指纹内容寻址**，同一份证书重复签发指向同一目录；
- 目录权限在创建时设为 `0700`、文件为 `0600`；已存在的目录若权限过宽会被收紧并告警；
- 数据目录内所有相对路径都做逃逸校验，`..`、绝对路径一律拒绝。

> 数据目录就是全部持久化状态：备份它（或整个卷）即可完整迁移。
> 凭据的密文在数据库中，加密密钥只存在于环境变量——**密钥与数据目录必须分开保管**。

## 3.3 数据库

默认使用数据目录下的 SQLite，零配置。切换外部数据库只需设置连接串：

```bash
# PostgreSQL
export ACMECAST_DATABASE_URL='postgres://user:pass@db.example.com:5432/acmecast'
# MySQL / MariaDB
export ACMECAST_DATABASE_URL='mysql://user:pass@db.example.com:3306/acmecast'
```

- 支持 SQLite / MySQL / PostgreSQL 三方言；
- 启动时自动执行**版本化迁移**，三方言共用同一套定义，行为由跨方言等价性测试保证；
- 迁移失败会明确报出失败版本并**中止启动**；
- 连接池上限等参数当前由默认值决定（10 连接）。

## 3.4 静态资源与前端托管

设置 `ACMECAST_STATIC_DIR` 后：

- 未匹配的**非 `/api/` 路径**会先尝试静态文件，未命中时回退到该目录下的
  `index.html`（SPA 深链路由由前端接管）；
- `/api/` 前缀下未匹配的路径返回结构化 JSON 404；
- 未设置时，非 API 路径返回 JSON 404。

容器镜像默认 `ACMECAST_STATIC_DIR=/static`，即自带控制台。

## 3.5 启动、健康检查与停机

- `GET /healthz` 返回纯文本 `ok`，**不做鉴权、不计入业务端点**，供编排系统探活；
- 支持 Ctrl-C 与 SIGTERM 优雅停机：停止接受新连接，已接受的请求执行完，
  随后给调度器最多 5 秒收尾。

## 3.6 口令哈希子命令

```bash
acmecast-server hash-password '<口令>'          # 二进制内
docker run --rm acmecast:dev hash-password '<口令>'  # 容器内
cargo run -p acmecast-server --example hash-password -- '<口令>'  # 开发环境
```

输出为 Argon2 PHC 字符串，直接赋给 `ACMECAST_ADMIN_PASSWORD_HASH`。

## 3.7 关于 YAML 配置文件（当前未接入运行路径）

`acmecast-core` 中实现了一套 YAML 配置装载（`ACMECAST_CONFIG` 指定路径，
`ACMECAST__<SECTION>__<KEY>` 形式的环境变量覆盖，默认查找 `./config.yaml`、
`./config.yml`、`/etc/acmecast/config.yaml`），但**当前服务端二进制并未调用它**：
`main.rs` 使用 `ServerConfig::from_env()` 与各模块各自读环境变量。

因此，**部署时请只依赖 3.1 的环境变量**；不要指望 `config.yaml` 生效。
详见 [12-operations-and-limitations.md](12-operations-and-limitations.md)。
