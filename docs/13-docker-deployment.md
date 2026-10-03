# Docker 部署

## 最小部署

```bash
# 1. 三件套（一次性，妥善保存）
export ACMECAST_JWT_SECRET="$(openssl rand -base64 32)"
export ACMECAST_CREDENTIAL_KEY="$(openssl rand -base64 32)"   # 更换后已存凭据无法解密！
export ACMECAST_ADMIN_PASSWORD_HASH="$(docker run --rm acmecast:dev hash-password --password '你的口令' | head -1)"

# 2. 数据目录（容器以 uid 65532 运行，需对其可写）
mkdir -p ./data && chmod 0777 ./data

# 3. 运行
docker run -d --name acmecast --restart unless-stopped \
  -p 8080:8080 \
  -v "$(pwd)/data:/data" \
  -e ACMECAST_JWT_SECRET \
  -e ACMECAST_CREDENTIAL_KEY \
  -e ACMECAST_ADMIN_PASSWORD_HASH \
  acmecast:dev
```

容器内固定环境：`ACMECAST_LISTEN=0.0.0.0:8080`、`ACMECAST_DATA_DIR=/data`、
`ACMECAST_STATIC_DIR=/static`（镜像已内置前端产物）。

## Compose

```bash
docker compose -f docker/compose.yaml up -d
docker compose -f docker/compose.yaml logs -f
```

- 三把密钥用 `${VAR:?}` 强校验：任一未设置时**任何** compose 子命令（含
  logs/build/down）都报错退出。
- 变量插值取自 `docker/.env`（Compose 按项目目录找，不读仓库根 `.env`）。
- 数据落在仓库根 `data/`（`../data:/data`）。
- `USE_CN_MIRROR` 构建参数默认 `true`（apt 换阿里云、cargo 走 rsproxy、
  npm 走 npmmirror）；上线构建传 `--build-arg USE_CN_MIRROR=false`，产物不含
  任何镜像源配置。

## 切换数据库

```bash
docker run -d --name acmecast \
  -e ACMECAST_DATABASE_URL='postgres://acmecast:****@db:5432/acmecast' \
  -e ACMECAST_JWT_SECRET -e ACMECAST_CREDENTIAL_KEY -e ACMECAST_ADMIN_PASSWORD_HASH \
  acmecast:dev
```

首次启动自动跑迁移；从 SQLite 换到 MySQL/PG 不迁移旧数据（自行导出导入或
重新申请）。

## 密钥管理要点

| 密钥 | 丢了/换了的后果 |
|---|---|
| `ACMECAST_JWT_SECRET` | 已签发令牌全部失效，重新登录即可 |
| `ACMECAST_CREDENTIAL_KEY` | **已存凭据全部无法解密**（500 `credential_decryption_error`），需逐个重建凭据 |
| `ACMECAST_ADMIN_PASSWORD_HASH` | 管理员无法登录；重新生成哈希重启即可 |

密钥与数据目录必须**分开保管**：备份数据目录的同时备份密钥，但不要把密钥
写进数据目录。

## 升级

1. `docker compose -f docker/compose.yaml build`（或 `docker build`）出新镜像；
2. `up -d` 重建容器；
3. 启动时自动跑增量迁移——迁移失败会报出版本号并拒绝启动，旧容器不受影响，
   先 `logs` 确认再回滚镜像。

## 排障速查

| 现象 | 先看 |
|---|---|
| 启动即退出 | `docker logs acmecast`：缺 `ACMECAST_JWT_SECRET`/`ACMECAST_CREDENTIAL_KEY` 会直接报错 |
| 能启动但登录 500 | 未配 `ACMECAST_ADMIN_PASSWORD_HASH`（启动日志有告警） |
| 登录 429 | 连续失败 5 次锁定 15 分钟，等锁定过期或重启进程 |
| 数据写不进去 | 宿主机目录权限：容器以 uid 65532 运行 |
| API 正常但页面 404 | 镜像内置前端在 `/static`，确认未覆盖 `ACMECAST_STATIC_DIR` 指向空目录 |

更多见[排障与已知限制](15-troubleshooting.md)。
