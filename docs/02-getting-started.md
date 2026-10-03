# 快速开始

本页覆盖三种使用方式：Docker（推荐）、Docker Compose、从源码构建。
配置项的完整说明见[配置参考](03-configuration.md)。

## 1. Docker

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
```

镜像构建与分层细节见 [Docker 镜像](14-docker-image.md)，运维细节见
[Docker 部署](13-docker-deployment.md)。

## 2. Docker Compose

第 1、2 步的环境变量照旧（可写入 `docker/.env`），数据同样落在 `./data`：

```bash
docker compose -f docker/compose.yaml up -d      # 首次会按需构建 acmecast:dev
docker compose -f docker/compose.yaml logs -f
docker compose -f docker/compose.yaml down
```

compose 文件对三把密钥做了 `${VAR:?}` 强校验：任一未设置时**任何** compose
子命令（含 logs/build/down）都会报错退出。

## 3. 从源码构建

需要 Rust 1.98（项目用 mise 管理工具链，见[本地开发](12-development.md)）：

```bash
# 构建后端（前端静态资源需另行 pnpm build，见下）
cargo build --release -p acmecast-server

# 生成带注释的配置模板（可选；不存在时启动也会自动生成）
./target/release/acmecast-server init

# 必需的三件配置
export ACMECAST_JWT_SECRET="$(openssl rand -base64 48)"
export ACMECAST_CREDENTIAL_KEY="$(openssl rand -base64 32)"
export ACMECAST_ADMIN_PASSWORD_HASH="$(./target/release/acmecast-server hash-password --password '你的口令' | head -1)"

# 启动（默认监听 0.0.0.0:8080，数据目录 ./data）
./target/release/acmecast-server
```

托管前端（可选）：

```bash
cd frontend && pnpm install && pnpm build && cd ..
export ACMECAST_STATIC_DIR="$PWD/frontend/dist"
./target/release/acmecast-server
```

## 4. 登录与第一件事

浏览器打开 `http://127.0.0.1:8080/`（托管了前端时）进入控制台；API 文档在
`http://127.0.0.1:8080/swagger-ui`。

签发一张证书的最短路径：

1. **建 ACME 账号凭据**：类型 `acme.account`，选 CA（如 `letsencrypt`）。
   首次使用时自动注册，之后复用。详见[凭据体系](04-credentials.md)。
2. **建 DNS 凭据**：类型 `cloudflare` 或 `aliyun`，填 API Token / AccessKey。
3. **建流水线**：依次挂 `cert.apply` → `cert.store` → `cert.deploy` 三步，
   字段说明见[流水线](05-pipelines.md)。
4. **手动运行一次**：`POST /api/pipelines/{id}/run`，轮询
   `GET /api/histories/{id}` 与 `/logs` 看进度。
5. **挂调度**：cron 或续期域名集合，见[定时调度与自动续期](08-scheduling.md)。

## 5. CLI 子命令

| 命令 | 作用 |
|---|---|
| `acmecast-server` | 正常启动服务 |
| `acmecast-server init [--force]` | 生成带注释的 `config.toml` 模板（已存在需 `--force` 覆盖） |
| `acmecast-server hash-password [--password 口令]` | 生成 Argon2 PHC 口令哈希；省略口令时交互式输入 |

全局参数 `--config <path>` / `-c`（env `ACMECAST_CONFIG`）指定配置文件；
缺省解析顺序：显式 `--config` → `ACMECAST_CONFIG` → `$ACMECAST_DATA_DIR/config.toml`
→ `data/config.toml`。

## 6. 优雅停机

Ctrl-C 或 SIGTERM 触发优雅停机：停止接受新连接、已接受请求跑完；停机后给
调度器 5 秒跑完当前 tick，超时放弃。
