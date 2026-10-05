# 构建上下文是本 workspace 根（含 crates/ 与 Cargo.toml）。
#
# 运行阶段用 distroless/static：项目全程 rustls（无 OpenSSL）、依赖均为纯 Rust，
# 因此 musl 静态链接后不依赖 glibc；static 变体自带 ca-certificates 与 tzdata，
# 出站 HTTPS（ACME、DoH、DNS 提供商 API）的根证书无需额外安装。

# ---- builder：完整工具链 + musl 静态目标 ----
FROM rust:1.98-slim AS builder

# 中国网络开发环境默认：apt 换阿里云镜像、cargo/rustup 走 rsproxy 镜像源；
# 上线构建传 `--build-arg USE_CN_MIRROR=false` 关闭。配置只写入 builder 层，
# 最终运行镜像不含任何镜像源或代理配置。
ARG USE_CN_MIRROR=true
RUN if [ "$USE_CN_MIRROR" = "true" ]; then \
        sed -i 's|deb.debian.org|mirrors.aliyun.com|g' \
            /etc/apt/sources.list.d/debian.sources /etc/apt/sources.list 2>/dev/null || true; \
        printf '[source.crates-io]\nreplace-with = "rsproxy"\n\n[source.rsproxy]\nregistry = "sparse+https://rsproxy.cn/index/"\n\n[net]\ngit-fetch-with-cli = true\n' \
            > /usr/local/cargo/config.toml; \
    fi
ENV RUSTUP_DIST_SERVER=https://rsproxy.cn \
    RUSTUP_UPDATE_ROOT=https://rsproxy.cn/rustup

# musl-gcc 供链接器使用；ring 的构建脚本还需要 make/perl（rust 镜像已含）。
RUN apt-get update \
    && apt-get install -y --no-install-recommends musl-tools pkg-config \
    && rm -rf /var/lib/apt/lists/*
RUN rustup target add x86_64-unknown-linux-musl

WORKDIR /app
COPY . .

# utoipa-swagger-ui 的 build.rs 构建时会从 github 下载 Swagger UI 压缩包；
# 用仓库内随附的本地副本（file: 协议），让构建完全不依赖外部网络，
# 各环境（含离线/CI）行为一致。
ENV SWAGGER_UI_DOWNLOAD_URL=file:///tmp/swagger-ui.zip
COPY docker/swagger-ui-v5.17.14.zip /tmp/swagger-ui.zip

ENV CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_LINKER=x86_64-linux-musl-gcc

# 依赖与产物目录挂 cache 卷：源码不变时跨构建复用，改动时只重编译受影响的 crate。
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/app/target \
    cargo build --release --target x86_64-unknown-linux-musl -p acmecast-server \
    && cp target/x86_64-unknown-linux-musl/release/acmecast-server /acmecast-server

# ---- 前端 builder：SPA 静态产物 ----
FROM node:24-slim AS frontend-builder

# 与后端共用同一镜像源开关：npmmirror 加速依赖安装（仅进 builder 层）。
ARG USE_CN_MIRROR=true
RUN corepack enable

WORKDIR /app/frontend
COPY frontend/package.json frontend/pnpm-lock.yaml frontend/pnpm-workspace.yaml ./
RUN if [ "$USE_CN_MIRROR" = "true" ]; then npm config set registry https://registry.npmmirror.com; fi \
    && pnpm install --frozen-lockfile

COPY frontend/ .
RUN pnpm build

# ---- 运行阶段：distroless static（非 root） ----
FROM gcr.io/distroless/static-debian12:nonroot

COPY --from=builder /acmecast-server /acmecast-server
COPY --from=frontend-builder /app/frontend/dist /static

# 数据目录（数据库、证书文件、凭据密钥）挂卷持久化；前端产物目录交给静态托管。
ENV ACMECAST_LISTEN=0.0.0.0:8080 \
    ACMECAST_DATA_DIR=/data \
    ACMECAST_STATIC_DIR=/static
EXPOSE 8080
VOLUME ["/data"]

ENTRYPOINT ["/acmecast-server"]
