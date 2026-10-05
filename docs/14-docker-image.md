# Docker 镜像

## 构建

```bash
docker build -t acmecast:dev .                          # 构建上下文 = 仓库根
docker build -t acmecast:dev --build-arg USE_CN_MIRROR=false .   # 上线构建（不用镜像源）
```

## 三段式结构

| 阶段 | 基础镜像 | 产物 |
|---|---|---|
| `builder` | `rust:1.98-slim` + musl target | 静态链接的 `/acmecast-server` |
| `frontend-builder` | `node:24-slim` + corepack/pnpm | `frontend/dist` |
| 运行阶段 | `gcr.io/distroless/static-debian12:nonroot` | 最终镜像 |

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

`ENTRYPOINT ["/acmecast-server"]`——子命令直接作为参数：
`docker run --rm acmecast:dev hash-password --password 'xxx'`。

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
