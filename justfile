# acmecast 本地开发任务。
#
# 所有命令统一通过 `mise exec` 运行：mise.toml 提供工具链版本（rust / node / pnpm），
# 其 [env] 与 .env 提供本地开发环境变量（SWAGGER_UI_DOWNLOAD_URL、ACMECAST_JWT_SECRET、
# ACMECAST_ADMIN_PASSWORD_HASH 等）。绕过 mise 直接跑 cargo/pnpm 时这些变量不会加载，
# 后端会因缺 JWT_SECRET 启动失败、首次构建也会因 swagger-ui 走外网下载而变慢。
#
# 首次使用：cp .env.example .env（详见 docs/02-getting-started.md）。

# 默认：列出全部任务
default:
    @just --list

# ---- 后端（默认监听 0.0.0.0:8080，数据目录 ./data）----

# 启动后端开发服务（API：http://127.0.0.1:8080，接口文档：/swagger-ui）
# 可附带子命令与参数，例如：just backend hash-password '你的口令'
@backend *args:
    mise exec -- cargo run -p acmecast-server -- {{args}}

# ---- 前端（frontend/，React 19 + Vite）----

# 安装前端依赖（首次使用或 pnpm-lock.yaml 变更后）
@frontend-install:
    mise exec -- pnpm -C frontend install

# 启动前端 dev server（http://localhost:5173，/api 代理到 8080）
@frontend:
    mise exec -- pnpm -C frontend dev

# 后端接口变更后重新生成前端 API 类型（需后端已在 8080 端口运行）
@gen-api:
    mise exec -- pnpm -C frontend gen:api

# ---- 组合 ----

# 同时启动后端与前端本地调试（Ctrl-C 一并退出；任一退出则整体结束）
dev:
    #!/usr/bin/env bash
    set -euo pipefail
    trap 'kill "$(jobs -p)" 2>/dev/null || true' EXIT
    just backend &
    just frontend &
    wait -n
