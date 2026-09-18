# 14. Docker 镜像

容器镜像在仓库根的 `Dockerfile` 定义，构建上下文即仓库根（含 `crates/` 与
`Cargo.toml`），`docker build -t acmecast:dev .` 即可构建。运行方式见
[13-docker-deployment.md](13-docker-deployment.md)。

## 14.1 Dockerfile 概览

三个阶段：

| 阶段 | 基础镜像 | 做什么 |
| --- | --- | --- |
| `builder` | `rust:1.98-slim` + musl-tools | 以 `x86_64-unknown-linux-musl` 静态链接出后端二进制（项目全程 rustls，无 OpenSSL / glibc 依赖） |
| `frontend-builder` | `node:24-slim` | corepack 启用 pnpm，`pnpm build` 产出前端静态文件 |
| 运行阶段 | `gcr.io/distroless/static-debian12:nonroot` | 只装二进制与前端产物；非 root（uid 65532）运行 |

- distroless static 变体自带 ca-certificates 与 tzdata，ACME / DoH / DNS 提供商
  API 的出站 HTTPS 根证书无需额外安装；
- 前端构建产物复制到 `/static`，镜像默认 `ACMECAST_STATIC_DIR=/static`，即自带控制台；
- 默认环境变量：`ACMECAST_LISTEN=0.0.0.0:8080`、`ACMECAST_DATA_DIR=/data`；
  另有 `VOLUME /data` 与 `EXPOSE 8080`；
- Swagger UI 用仓库随附的 `docker/swagger-ui-v5.17.14.zip`
  （`SWAGGER_UI_DOWNLOAD_URL=file:///tmp/swagger-ui.zip`），构建完全不依赖外部
  网络，各环境（含离线 / CI）行为一致；
- cargo 的 registry 与 target 目录挂 cache 卷：源码不变时跨构建复用，
  改动时只重编译受影响的 crate。

## 14.2 构建镜像与 `USE_CN_MIRROR`

```bash
docker build -t acmecast:dev .
# 上线构建时关闭国内镜像源配置：
# docker build --build-arg USE_CN_MIRROR=false -t acmecast:dev .
```

`USE_CN_MIRROR` 默认 `true`，且只写入 builder 层：apt 换阿里云镜像、cargo/rustup
走 rsproxy、npm 走 npmmirror。上线构建传 `--build-arg USE_CN_MIRROR=false`，
最终运行镜像不含任何镜像源或代理配置。

compose 走同一份定义（`docker/compose.yaml` 的 `build.args`，默认同样为 `true`），
完整流程见 13.4。

## 14.3 `.dockerignore`

构建上下文排除了 `target/`、`data/`、`*.db*`（含 `-shm` / `-wal`）、
`frontend/node_modules/`、`frontend/dist/`、`site/`、`.cache/` 等：

- 上下文更小，构建更快；
- 本地数据库、证书与凭据密钥不会进镜像层。
