# 13. Docker 部署

acmecast 有两条部署路径：**Docker**（本文）与**源码**（见
[02-getting-started.md](02-getting-started.md) 2.2 起）。镜像怎么来、里面有什么，
见 [14-docker-image.md](14-docker-image.md)。

## 13.1 前置条件

- Docker 20+；
- 数据目录挂载点需可被容器内的 uid 65532 写入（见 13.4）；
- 一个可用的 ACME CA（默认 Let's Encrypt 生产环境）与 DNS 服务商账号（DNS-01
  挑战需要，见 [07-dns-and-deploy.md](07-dns-and-deploy.md)）；
- 部署目标（本机目录或一台可 SSH 登录的主机）。

> 生产请使用 Let's Encrypt 生产端点；调试建议先用 `letsencrypt-staging`，
> 避免触发生产环境的速率限制。

## 13.2 生成必需密钥

```bash
export ACMECAST_JWT_SECRET="$(openssl rand -base64 32)"
export ACMECAST_CREDENTIAL_KEY="$(openssl rand -base64 32)"
```

| 密钥 | 用途 | 丢失/更换后果 |
| --- | --- | --- |
| `ACMECAST_JWT_SECRET` | 访问令牌的 HS256 签名密钥 | 已签发的令牌全部失效（重新登录即可） |
| `ACMECAST_CREDENTIAL_KEY` | 凭据静态加密密钥（AES-256-GCM） | **已存凭据无法解密**，必须重新录入 |

两者都请与数据目录**分开保管**。

## 13.3 生成管理员口令哈希

口令以 Argon2 PHC 字符串配置，明文口令不出现在配置、日志或响应中。
用镜像内的 `hash-password` 子命令生成（运行环境无需 Rust 工具链）：

```bash
export ACMECAST_ADMIN_PASSWORD_HASH="$(docker run --rm acmecast:dev hash-password '你的口令')"
```

### 1）四种等价的运行方式

```bash
acmecast-server hash-password '<口令>'          # 二进制内
docker run --rm acmecast:dev hash-password '<口令>'  # 容器内
docker compose -f docker/compose.yaml run --rm acmecast hash-password '<口令>'  # 容器内（compose）
cargo run -p acmecast-server --example hash-password -- '<口令>'  # 开发环境
```

> compose 那条要求 `ACMECAST_JWT_SECRET`、`ACMECAST_CREDENTIAL_KEY`、
> `ACMECAST_ADMIN_PASSWORD_HASH` 三个变量都已设置（`docker/compose.yaml` 用 `${VAR:?}`
> 强制，见 13.4），因此不适用于「还没有口令哈希」的首次生成——首次请用前两条。

### 2）以文件方式提供（`ACMECAST_ADMIN_PASSWORD_HASH_FILE`）

口令哈希也可以不放在环境变量里，而是写进一个文件，把路径交给
`ACMECAST_ADMIN_PASSWORD_HASH_FILE`：

```bash
export ACMECAST_ADMIN_PASSWORD_HASH_FILE=/path/to/hash.txt
```

- 文件内容为一行 PHC 字符串，首尾空白会被忽略；
- 文件不存在或内容全为空白时，回退到 `ACMECAST_ADMIN_PASSWORD_HASH`；
- 文件存在却读不了（权限不足、指向目录、非 UTF-8）则**启动即失败**；
- 容器以 uid 65532 运行，挂载进容器的文件需该 uid 可读；
- `docker/compose.yaml` 仍用 `${VAR:?}` 强制要求 `ACMECAST_ADMIN_PASSWORD_HASH`
  本身（见 13.4），用仓库自带的 compose 时仍需提供该变量。

## 13.4 启动容器

容器以 uid 65532 运行，数据目录需可写：

```bash
mkdir -p ./data && chmod 0777 ./data
```

**方式 A：`docker run`**

```bash
docker run -d --name acmecast \
  -p 8080:8080 \
  -v "$(pwd)/data:/data" \
  -e ACMECAST_JWT_SECRET \
  -e ACMECAST_CREDENTIAL_KEY \
  -e ACMECAST_ADMIN_PASSWORD_HASH \
  acmecast:dev
```

**方式 B：Docker Compose**（用仓库自带的 `docker/compose.yaml`，在仓库根目录执行）

```bash
docker compose -f docker/compose.yaml up -d      # 首次会按需构建 acmecast:dev
docker compose -f docker/compose.yaml logs -f
```

`docker/compose.yaml` 的完整内容如下（如有出入，以仓库里的文件为准）：

```yaml
# acmecast 的 Docker Compose 定义。在仓库根目录执行：
#
#   docker compose -f docker/compose.yaml up -d      # 首次会按需构建 acmecast:dev
#   docker compose -f docker/compose.yaml logs -f
#   docker compose -f docker/compose.yaml down
#
# 变量插值取自本文件所在目录的 docker/.env（Compose 按项目目录查找，不会去读仓库根的
# .env —— 那个文件是 direnv 的镜像开关），也可以直接在当前 shell 里 export。
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
    image: acmecast:dev
    container_name: acmecast
    build:
      context: ..
      args:
        # 上线构建改为 false，关闭镜像内的国内源配置
        USE_CN_MIRROR: "${USE_CN_MIRROR:-true}"
    ports:
      - "8080:8080"
    volumes:
      - ../data:/data
    environment:
      ACMECAST_JWT_SECRET: "${ACMECAST_JWT_SECRET:?未设置：先 export，或写入 docker/.env}"
      ACMECAST_CREDENTIAL_KEY: "${ACMECAST_CREDENTIAL_KEY:?未设置：先 export，或写入 docker/.env}"
      ACMECAST_ADMIN_PASSWORD_HASH: "${ACMECAST_ADMIN_PASSWORD_HASH:?未设置：先 export，或写入 docker/.env}"
```

要点（与文件头注释一致）：

- 变量插值取自 `docker/.env`（compose 按项目目录查找，不会去读仓库根的 `.env`——
  那是 direnv 的镜像开关），也可以直接在当前 shell 里 `export`；
- 三个密钥变量任何一个没设置，**任何** compose 子命令（含 `logs`、`down`）都会直接
  报错退出，不会带着空密钥把服务拉起来；
- 数据落在仓库根的 `data/`，与方式 A 的 `docker run` 一致。

两种方式等价：容器名同为 `acmecast`，数据同样落在 `./data`，下面 13.5 的验证命令通用。
差别只在变量来源——compose 取本文件所在目录的 `docker/.env` 或当前 shell 环境
（仓库根的 `.env` 是 direnv 的镜像开关，compose 不会读它）。compose 定义里这三个变量用
`${VAR:?}` 声明，任何一个没设置都会让**任何** compose 子命令（含 `logs`）直接报错退出，
不会带着空密钥把服务拉起来——所以上面 13.2、13.3 的 `export` 必须先执行。

容器默认环境变量为 `ACMECAST_LISTEN=0.0.0.0:8080`、`ACMECAST_DATA_DIR=/data`、
`ACMECAST_STATIC_DIR=/static`。

## 13.5 验证

```bash
curl http://127.0.0.1:8080/healthz            # -> ok
docker logs -f acmecast                        # 观察启动日志（数据库就绪 / 开始服务）
```

浏览器打开 `http://127.0.0.1:8080/` 进入控制台，或 `/swagger-ui` 查看接口文档。

## 13.6 升级

通用升级步骤（备份数据目录、数据库迁移的注意事项）见
[12-operations-and-limitations.md](12-operations-and-limitations.md) 12.2。容器方式：

```bash
docker rm -f acmecast
docker run -d --name acmecast ... acmecast:<新版本>
```

用 compose 时同一个文件即可完成——先构建新镜像，`up -d` 会因镜像变化重建容器
（容器名不变，数据目录照旧；三个密钥变量需已设置，见 13.2、13.3）：

```bash
docker build -t acmecast:dev .                   # 或直接 up -d --build 让 compose 先构建
docker compose -f docker/compose.yaml up -d
docker compose -f docker/compose.yaml ps         # 确认新容器已替换
```

## 13.7 排障

- 容器起来又退出：数据目录不可写（uid 65532），`chmod 0777 ./data` 或以正确属主挂载；
- compose 直接报 `variable is not set`：`${VAR:?}` 生效，三个密钥变量有缺失（见 13.4）；
- 设置了 `ACMECAST_ADMIN_PASSWORD_HASH_FILE` 却仍无人能登录：文件不存在或内容为空会
  回退到 `ACMECAST_ADMIN_PASSWORD_HASH`；文件存在但读不了则启动即失败，日志有明确报错。

完整排障表见 [12-operations-and-limitations.md](12-operations-and-limitations.md) 12.3。
