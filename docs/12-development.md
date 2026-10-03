# 本地开发

## 环境要求（mise 管理）

项目用 [mise](https://mise.jdx.dev) 钉住全部工具链版本（`mise.toml`），进入
仓库目录自动加载：

| 工具 | 版本 | 说明 |
|---|---|---|
| rust | 1.98.0 | 含 clippy / rustfmt / rust-analyzer 组件 |
| node | 24 | 前端 |
| pnpm | 11.20.0 | 与 `frontend/package.json` 的 `packageManager` 一致 |
| prek | latest | Git 钩子（要求 ≥ 0.5.0） |

```bash
mise install        # 安装全部工具链
```

mise 还会自动加载仓库根 `.env`（从 `.env.example` 复制；国内网络加速镜像、
本地开发密钥，已被 gitignore），并设置 `SWAGGER_UI_DOWNLOAD_URL` 指向仓库内
随附的 `docker/swagger-ui-v5.17.14.zip`——构建不依赖外网下载 Swagger UI。

!!! note "国内网络"
    `.env.example` 提供腾讯云 rustup/npm 镜像与
    `CARGO_NET_GIT_FETCH_WITH_CLI=true`；`.cargo/config.toml` 已无条件指向
    腾讯云 crates 镜像。`IS_CHINA=1` 仅为标记。

## 后端开发

```bash
cargo build -p acmecast-server          # 开发构建
cargo build --release -p acmecast-server
cargo fmt --all -- --check              # 格式（rustfmt.toml）
cargo clippy --workspace --all-targets -- -D warnings
cargo test -p acmecast-server           # 本地常用（见下「测试」）
```

本地起服务（另开终端跑前端）：

```bash
export ACMECAST_JWT_SECRET=dev-only-jwt-secret
export ACMECAST_CREDENTIAL_KEY=Y2ktb25seS1rZXktZm9yLXRlc3RpbmctMzItYnl0ZXM=  # 32 字节 base64
export ACMECAST_ADMIN_PASSWORD_HASH="$(cargo run -p acmecast-server -- hash-password -p dev | head -1)"
cargo run -p acmecast-server            # 默认 0.0.0.0:8080，数据 ./data
```

## 前端开发

```bash
cd frontend
pnpm install
pnpm dev              # http://localhost:5173，/api 代理到 127.0.0.1:8080（需先起后端）
pnpm build            # tsc -b && vite build（含类型检查）
pnpm typecheck        # tsc -b --noEmit
pnpm test             # vitest
pnpm test:e2e         # playwright（首次需 pnpm exec playwright install --with-deps chromium）
pnpm gen:api          # 从 http://127.0.0.1:8080/api/openapi.json 重新生成 API 类型
```

技术栈：React 19 + TypeScript + Vite 7 + Ant Design 6 + react-router 7 +
zustand（登录态持久化到 localStorage）+ @tanstack/react-query +
openapi-fetch。

## 测试

| 命令 | 说明 |
|---|---|
| `cargo test -p acmecast-server` | 本地常用全集：pipeline e2e 会自起 **pebble** 容器（需 Docker）；方言等价测试自起 MySQL/Postgres 容器，起不来则跳过 |
| `cargo test --workspace` | 全量；**需要 keytool**（acmecast-cert 的 JKS 用例显式依赖，装 JDK 或设 `ACMECAST_KEYTOOL`） |
| `cargo test -p acmecast-server --test dialect_equivalence -- mysql_` | MySQL 方言等价（需 `ACMECAST_TEST_MYSQL_URL` 或 Docker） |
| `cargo test -p acmecast-server --test dialect_equivalence -- postgres_` | PostgreSQL 方言等价（`ACMECAST_TEST_POSTGRES_URL`） |

## Git 钩子（prek）

```bash
prek install                     # 装 pre-commit + pre-push 钩子
prek run --all-files             # 手动全量跑
```

- pre-commit：文件卫生（trailing-whitespace、end-of-file、LF、yaml/toml/json
  校验、大文件检查）+ `cargo fmt --check`。
- pre-push：`cargo clippy --workspace --all-targets -- -D warnings` +
  `cargo test -p acmecast-server`。
- 刻意不含 detect-private-key：源码中有合法的 PEM 字符串常量，会全线误报。

## 文档开发

```bash
pip install zensical==0.0.62     # 与 CI 同版本
zensical serve                   # 本地预览
zensical build --clean --strict  # 严格构建（告警即失败），产物在 site/
```

配置在仓库根 `zensical.toml`，文档源 `docs/`，产物 `site/`（已 gitignore）。

## CI（GitHub Actions）

push 到 `main` 与全部 PR 触发，七个 job：

1. **lint**：rustfmt + clippy（参数与 prek 逐字对齐）
2. **frontend**：pnpm install/test/build（含类型检查）+ playwright e2e
3. **docs**：`zensical build --clean --strict`
4. **build**：`cargo build --workspace --all-targets`
5. **test**：`cargo test --workspace`（带 JDK 21 提供 keytool）
6. **license**：`cargo deny check`（许可白名单见 `deny.toml`；明确拒绝
   GPL-2.0/LGPL-2.1）
7. **integration-sqlite / mysql / postgres**：三方言集成测试

## 依赖纪律

- 全程 rustls，**不引入 OpenSSL**（musl 静态构建的前提）。
- SSH 用纯 Rust 的 `russh`，不依赖系统 libssh2。
- 内部 crate 依赖统一在 workspace 根钉版本（`publish = false`）。
- 提交前 `cargo deny check` 不过不要合入。
