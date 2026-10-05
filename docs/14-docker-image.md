# Docker 镜像

## 构建

两个构建文件除镜像源配置外完全一致，二选一：

```bash
docker build -f docker/CN.Dockerfile -t acmecast:dev .   # 中国网络：阿里云 apt / rsproxy / npmmirror
docker build -f docker/Dockerfile    -t acmecast:dev .   # 国际/上线：官方源
```

- 构建上下文 = 仓库根；
- 镜像源配置只写入 builder 层，最终运行镜像不含任何镜像源或代理配置；
- 目标平台由 buildx 的 `TARGETPLATFORM` 决定（`linux/amd64` →
  `x86_64-unknown-linux-musl`，`linux/arm64` → `aarch64-unknown-linux-musl`），
  镜像与目标平台同架构，不做交叉编译。

## 三段式结构

| 阶段 | 基础镜像 | 产物 |
|---|---|---|
| `builder` | `rust:1-slim` + musl target | 静态链接的 `/acmecast-server` |
| `frontend-builder` | `node:24-slim` + corepack/pnpm | `frontend/dist` |
| 运行阶段 | `gcr.io/distroless/static-debian13:nonroot` | 最终镜像 |

选择依据：

- 全程 rustls（无 OpenSSL）、SSH 用纯 Rust 的 russh、无系统库依赖 → 可以
  musl 静态链接，运行阶段用 distroless/static（无 shell、无包管理器）。
- 容器以 **uid 65532（nonroot）** 运行；`/data` 声明为 VOLUME。
- Swagger UI 资源用仓库内随附的 `docker/swagger-ui-v5.17.14.zip`
  （`SWAGGER_UI_DOWNLOAD_URL=file:///tmp/swagger-ui.zip`），构建不访问外网。
- builder 阶段用 cache mount 复用 cargo registry 与 target，增量构建快。

镜像内固定环境变量：

```
ACMECAST_LISTEN=0.0.0.0:8080
ACMECAST_DATA_DIR=/data
ACMECAST_STATIC_DIR=/static
```

`ENTRYPOINT ["acmecast-server"]`（二进制位于 `/usr/bin`）——子命令直接作为
参数：`docker run --rm acmecast:dev hash-password --password 'xxx'`。

## .dockerignore 要点

构建上下文排除：`target/`、`data/`、`*.db*`、编辑器文件、
`frontend/node_modules`、`frontend/dist`、`site/`、`.cache/`、`.cargo/`、
`mise.toml`/`mise.lock`、`openspec/`，以及 **`DNSTOKEN.md`**（本地 DNS 凭据
笔记，绝不进镜像与构建上下文）。

## 发布流水线

- GitHub Actions 只跑 CI（lint/test/build/集成），不发布镜像。
- CNB（`.cnb.yml`）：push 到 `main` 构建并推送
  `<registry>/<repo>:latest`，push 到 `dev` 推送 `:dev`；同流水线还把
  本文档站点（`zensical build`）部署到 EdgeOne Pages（main → 生产，
  dev → 预览）。
