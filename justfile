# acmecast 本地开发任务。
#
# 工具链（rust / node / pnpm 等）已通过 `mise use -g` 全局安装，不再依赖仓库内
# mise.toml。但 .env 仍提供本地开发变量（ACMECAST_JWT_SECRET 等），后端启动依赖：
# 首次使用：cp .env.example .env（详见 docs/02-getting-started.md）。

# 默认：列出全部任务
default:
    @just --list

# ---- 后端（默认监听 0.0.0.0:8080，数据目录 ./data）----

# 启动后端开发服务（API：http://127.0.0.1:8080，接口文档：/swagger-ui）
# 可附带子命令与参数，例如：just backend hash-password '你的口令'
@backend *args:
    cargo run -p acmecast-server -- {{args}}

# ---- 前端（frontend/，React 19 + Vite）----

# 安装前端依赖（首次使用或 pnpm-lock.yaml 变更后）
@frontend-install:
    pnpm -C frontend install

# 启动前端 dev server（http://localhost:5173，/api 代理到 8080）
# exec 去掉一层 shell 包装，减少信号转发丢失的环节
@frontend:
    exec pnpm -C frontend dev

# 后端接口变更后重新生成前端 API 类型（需后端已在 8080 端口运行）
@gen-api:
    pnpm -C frontend gen:api

# 从一张图片（png/jpg/svg）重生成全套 favicon 到 frontend/public
# 用法：just favicon <图片路径> [输出目录，默认 frontend/public]
# 依赖：ImageMagick（magick）；生成后执行 pnpm -C frontend build 刷新构建产物
@favicon image out="frontend/public":
    ./scripts/generate-favicon.sh {{image}} {{out}}

# ---- 组合 ----

# 同时启动后端与前端本地调试（Ctrl-C 一并退出；任一退出则整体结束）
# 用 setsid 让每个子任务成为独立进程组长，退出时 kill -PID 整组击杀，
# 避免 pnpm/vite 被孤儿化后继续占用 517x 端口。
dev:
    #!/usr/bin/env bash
    set -euo pipefail
    pids=()
    cleanup() {
        for p in "${pids[@]}"; do
            kill -- -"$p" 2>/dev/null || true
        done
    }
    trap cleanup EXIT
    trap 'exit 130' INT
    trap 'exit 143' TERM
    setsid just backend & pids+=("$!")
    setsid just frontend & pids+=("$!")
    wait -n

# 停止所有本地开发服务（后端 8080 与前端 517x 的 vite/pnpm/cargo 进程）
@stop:
    pkill -TERM -f 'node .*[v]ite' 2>/dev/null || true
    pkill -TERM -f 'pnpm -C [f]rontend' 2>/dev/null || true
    pkill -TERM -f 'acmecast-serve[r]' 2>/dev/null || true
    pkill -TERM -f 'just (de[v]|fronten[d]|backen[d])' 2>/dev/null || true
    sleep 1
    @echo "本地开发服务已停止"
