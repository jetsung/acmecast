# acmecast

证书自动签发与部署系统：以 ACME 协议向 Let's Encrypt（或自建 CA）申请证书，
按流水线自动完成 DNS 挑战、证书入库与部署到本地/远程目标，并通过 cron 或到期
扫描自动续期。提供带鉴权的 REST API 与 OpenAPI 文档，为前端 SPA 与第三方集成
提供统一入口。

> 完整使用文档（概念、配置、API、调度、部署、排障与已知限制）见
> [docs/index.md](docs/index.md)。

## 快速开始（Docker）

```bash
# 1. 生成两把必需密钥（务必妥善保存：更换加密密钥会使已存凭据无法解密）
export ACMECAST_JWT_SECRET="$(openssl rand -base64 32)"
export ACMECAST_CREDENTIAL_KEY="$(openssl rand -base64 32)"

# 2. 生成管理员口令哈希（镜像内子命令，运行环境无需 Rust 工具链）
#    输出第一行为哈希值（末尾另有提示行，不可捕获），复制第一行后设置：
docker run --rm acmecast:dev hash-password --password '你的口令'
export ACMECAST_ADMIN_PASSWORD_HASH='$argon2id$...<粘贴输出的第一行>'

# 3. 启动（数据目录挂载到宿主机；容器以 uid 65532 运行，目录需可写）
mkdir -p ./data && chmod 0777 ./data
docker run -d --name acmecast \
  -p 8080:8080 \
  -v "$(pwd)/data:/data" \
  -e ACMECAST_JWT_SECRET \
  -e ACMECAST_CREDENTIAL_KEY \
  -e ACMECAST_ADMIN_PASSWORD_HASH \
  acmecast:dev

# 4. 验证
curl http://127.0.0.1:8080/healthz          # -> ok
curl -sS -X POST http://127.0.0.1:8080/api/login \
  -H 'Content-Type: application/json' \
  -d '{"username":"admin","password":"你的口令"}'   # -> {"data":{"token":"..."}}

# 携带令牌访问受保护端点
TOKEN=...
curl -H "Authorization: Bearer $TOKEN" http://127.0.0.1:8080/api/pipelines
```

也可以走 Docker Compose：第 1、2 步的变量照旧，数据同样落在 `./data`，容器名同为
`acmecast`。

```bash
docker compose -f docker/compose.yaml up -d      # 首次会按需构建 acmecast:dev
docker compose -f docker/compose.yaml logs -f
```

完整 Docker 部署文档（密钥、口令哈希文件方式、升级、排障）见
[docs/13-docker-deployment.md](docs/13-docker-deployment.md)。

交互式 API 文档在 `http://127.0.0.1:8080/swagger-ui`（JSON：`/api/openapi.json`）。

## 从源码构建

```bash
cargo build --release -p acmecast-server
# 静态链接的 Docker 镜像：中国网络环境用 CN.Dockerfile（国内镜像源），
# 国际/上线构建用 Dockerfile（官方源），二者除镜像源配置外一致
docker build -f docker/CN.Dockerfile -t acmecast:dev .
```

镜像源配置只进 builder 层，运行镜像不含任何镜像源或代理配置。镜像分层与
`.dockerignore` 细节见 [docs/14-docker-image.md](docs/14-docker-image.md)。

## 配置

全部通过环境变量配置，均带默认值；除特别标注外为可选。

### 基础

| 变量 | 默认 | 说明 |
| --- | --- | --- |
| `ACMECAST_LISTEN` | `0.0.0.0:8080` | HTTP 监听地址 |
| `ACMECAST_DATA_DIR` | `./data`（容器内 `/data`） | 数据目录 |
| `ACMECAST_DATABASE_URL` | 数据目录下 SQLite | 连接串（见下「数据库」） |
| `ACMECAST_STATIC_DIR` | 未设置 | 前端静态资源目录；未设置时不托管（仅 API） |
| `ACMECAST_BODY_LIMIT_BYTES` | `2097152` | 请求体体积上限 |

### 安全（部署前必须核对）

| 变量 | 说明 |
| --- | --- |
| `ACMECAST_JWT_SECRET` | **必须配置**。访问令牌的 HS256 签名密钥（`openssl rand -base64 32`） |
| `ACMECAST_CREDENTIAL_KEY` | **必须配置**。凭据静态加密密钥 AES-256-GCM（`openssl rand -base64 32`）；**更换后已存凭据无法解密** |
| `ACMECAST_ADMIN_PASSWORD_HASH` | 管理员口令的 Argon2 PHC 哈希；未配置时服务可启动但无人能登录（启动日志告警） |
| `ACMECAST_ADMIN_PASSWORD_HASH_FILE` | 存放管理员口令哈希的**文件路径**；设置后优先读文件，文件不存在或内容为空白时回退到 `ACMECAST_ADMIN_PASSWORD_HASH` |
| `ACMECAST_ADMIN_USERNAME` | 管理员用户名，默认 `admin` |
| `ACMECAST_TOKEN_TTL_HOURS` | 访问令牌有效期（小时），默认 12 |
| `ACMECAST_AUTH_DISABLED` | `1`/`true`/`yes` 关闭鉴权——**仅限本地单机**，默认强制鉴权 |
| `ACMECAST_INSECURE_SKIP_VERIFY` | `1` 跳过 CA 的 TLS 证书校验，仅为自建测试 CA（pebble 等）准备 |

除登录、文档与静态资源外，全部 `/api/` 端点强制校验 `Authorization: Bearer` 令牌；
连续登录失败（默认 5 次）将锁定来源 15 分钟。

### 数据目录

```
data/
├── acmecast.db      # SQLite（使用外部数据库时无此文件）
├── certs/           # 证书链 PEM（按内容寻址）
└── keys/            # 私钥 PEM
```

> 数据目录就是全部持久化状态；备份它（或整卷）即可完整迁移。
> 凭据以密文存于数据库，加密密钥只存在于环境变量——**密钥与数据目录必须分开保管**。

### 数据库

默认使用数据目录下的 SQLite，零配置。切换外部数据库：

```bash
# PostgreSQL
export ACMECAST_DATABASE_URL='postgres://user:pass@db.example.com:5432/acmecast'
# MySQL
export ACMECAST_DATABASE_URL='mysql://user:pass@db.example.com:3306/acmecast'
```

启动时自动执行版本化迁移（SQLite / MySQL / PostgreSQL 三方言共用同一套定义，
行为由跨方言等价性测试保证）。迁移失败会明确报出失败版本并中止启动。

## 定时续期

在流水线「调度」中配置（二选一或同时）：

- **cron 表达式**（5 段，如 `0 3 * * *`）：到点自动执行流水线；
- **续期域名集合**：周期扫描证书仓库，剩余有效期小于阈值（默认 30 天）时
  自动触发负责该域名的流水线；已吊销证书自动排除。

停机错过 cron 触发点时默认跳过补跑（可配置为追赶最近一次）；同一时间窗口内
的重复续期触发会自动去重。每次自动触发都落审计记录，可按流水线与时间查询。

## 前端

控制台 SPA 位于 `frontend/`（React 19 + TypeScript + Ant Design 6），镜像构建
时自动打包，产物由后端静态托管（`ACMECAST_STATIC_DIR=/static`），与 API 同源。

```bash
# 开发：先起后端，再启动前端 dev server（/api 代理到 8080）
cd frontend && pnpm install && pnpm dev   # http://localhost:5173
# 后端接口变更后重新生成 API 类型
pnpm gen:api
```

页面：仪表盘、证书（列表/详情/多格式下载/吊销）、流水线编排（由任务
JSON Schema 驱动的动态表单）、运行历史（按步骤分组日志）、凭据管理、
调度配置。API 类型从 `/api/openapi.json` 生成，后端接口变更后执行
`pnpm gen:api` 保持同步。

## 已知限制

- SSH 部署目标未接入 known_hosts，主机指纹暂不校验（代码中留有告警）；
- HTTP-01 挑战仅有材料计算能力，无投放通道，申请步骤请使用 DNS-01；
- 登录失败限流为进程内计数，多副本部署时需在反向代理层另行防护。

## Docker 部署

### 使用预构建镜像

#### 可用镜像仓库

> **版本：** `latest`, `dev`(GHCR only), <`TAG`>

| Registry                                                                                   | Image                                                  |
| ------------------------------------------------------------------------------------------ | ------------------------------------------------------ |
| [**Docker Hub**](https://hub.docker.com/r/jetsung/acmecast/)                                | `jetsung/acmecast`                                    |
| [**GitHub Container Registry**](https://ghcr.io/jetsung/acmecast) | `ghcr.io/jetsung/acmecast`                            |
| **Tencent Cloud Container Registry（SG）**                                                       | `sgccr.ccs.tencentyun.com/jetsung/acmecast`             |
| **Aliyun Container Registry（GZ）**                                                              | `registry.cn-guangzhou.aliyuncs.com/jetsung/acmecast` |

### 使用 compose 启动

#### 默认：拉取预构建镜像

```bash
docker compose -f docker/compose.yaml up -d
```

#### 本地构建：使用 build 段

如需本地构建镜像运行（compose 默认使用 `docker/CN.Dockerfile`；国际/上线
构建把 build 段的 `dockerfile` 改为 `docker/Dockerfile`）：

```bash
docker compose -f docker/compose.yaml up -d --build
```

#### docker/compose.yaml

```yaml
# acmecast 的 Docker Compose 定义。在仓库根目录执行：
#
#   docker compose -f docker/compose.yaml up -d      # 首次会按需构建 acmecast:dev
#   docker compose -f docker/compose.yaml logs -f
#   docker compose -f docker/compose.yaml down
#
# 变量插值取自本文件所在目录的 docker/.env（Compose 按项目目录查找，不会去读仓库根的
# .env —— 那个文件由 mise 的 _.file 加载，是本地开发用的），也可以直接在当前 shell 里 export。
# 需要的三个变量（生成方式见 docs/13-docker-deployment.md 13.3）：
#   ACMECAST_JWT_SECRET  ACMECAST_CREDENTIAL_KEY  ACMECAST_ADMIN_PASSWORD_HASH
# 三者用 ${VAR:?} 声明：任何一个没设置，**任何** compose 子命令（含 logs/build/down）都会
# 直接报错退出，不会带着空密钥把服务拉起来。长期使用建议写进 docker/.env，否则每开一个
# 新 shell 都要重新 export，连 logs、down 都用不了。
#
# 数据落在仓库根的 data/（与 docker run 写法一致）。容器以 uid 65532 运行，目录需可写：
#   mkdir -p data && chmod 0777 data

name: acmecast

services:
  acmecast:
    image: ghcr.io/jetsung/acmecast:dev
    container_name: acmecast
    ports:
      - "8080:8080"
    volumes:
      - ./data:/data
    environment:
      ACMECAST_JWT_SECRET: "${ACMECAST_JWT_SECRET:?未设置：先 export，或写入 docker/.env}"
      ACMECAST_CREDENTIAL_KEY: "${ACMECAST_CREDENTIAL_KEY:?未设置：先 export，或写入 docker/.env}"
      ACMECAST_ADMIN_PASSWORD_HASH: "${ACMECAST_ADMIN_PASSWORD_HASH:?未设置：先 export，或写入 docker/.env}"
```

## 许可

Apache-2.0（见 [LICENSE](LICENSE)）
