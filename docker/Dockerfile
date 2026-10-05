# 构建上下文是本 workspace 根（含 crates/ 与 Cargo.toml）。
# 本文件全部使用官方源（apt/cargo/rustup/npm 均为国际默认），供 CI 与上线
# 构建；中国网络环境请用 docker/CN.Dockerfile（全量国内镜像源），二者除
# 镜像源配置外完全一致。
#
# 运行阶段用 distroless/static：项目全程 rustls（无 OpenSSL）、依赖均为纯 Rust，
# 因此 musl 静态链接后不依赖 glibc；static 变体自带 ca-certificates 与 tzdata，
# 出站 HTTPS（ACME、DoH、DNS 提供商 API）的根证书无需额外安装。

# ---- builder：完整工具链 + musl 静态目标 ----
FROM rust:1-slim AS builder

# buildx 会按目标平台注入 TARGETPLATFORM（linux/amd64、linux/arm64）；
# 预定义 ARG 必须显式声明才能在 RUN 中引用。
ARG TARGETPLATFORM

# musl-gcc 供链接器使用；ring 的构建脚本还需要 make/perl（rust 镜像已含）。
RUN apt-get update \
    && apt-get install -y --no-install-recommends musl-tools pkg-config \
    && rm -rf /var/lib/apt/lists/*

# buildx 会按目标平台注入 TARGETPLATFORM（linux/amd64、linux/arm64）。
# Rust musl target 必须与之一一对应：musl-tools 的 wrapper 只支持宿主架构，
# 本镜像不做交叉编译（CI 中每台 runner 各构建一个平台），在 aarch64 容器里
# 为 x86_64 编译会因找不到交叉工具链而失败。选出的 target 与 linker 变量
# 写入文件，供后面的构建步骤 source/读取。
RUN case "$TARGETPLATFORM" in \
        linux/amd64) RUST_TARGET=x86_64-unknown-linux-musl ;; \
        linux/arm64) RUST_TARGET=aarch64-unknown-linux-musl ;; \
        *) echo "不支持的构建平台：$TARGETPLATFORM" >&2; exit 1 ;; \
    esac \
    && rustup target add "$RUST_TARGET" \
    && printf '%s\n' "$RUST_TARGET" > /.rust-target \
    && printf 'export CARGO_TARGET_%s_LINKER=musl-gcc\n' \
        "$(printf '%s' "$RUST_TARGET" | tr 'a-z-' 'A-Z_')" > /.rust-env

WORKDIR /app
COPY . .

# utoipa-swagger-ui 的 build.rs 构建时会从 github 下载 Swagger UI 压缩包；
# 用仓库内随附的本地副本（file: 协议），让构建完全不依赖外部网络，
# 各环境（含离线/CI）行为一致。
ENV SWAGGER_UI_DOWNLOAD_URL=file:///tmp/swagger-ui.zip
COPY docker/swagger-ui-v5.17.14.zip /tmp/swagger-ui.zip

# 依赖与产物目录挂 cache 卷：源码不变时跨构建复用，改动时只重编译受影响的 crate。
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/app/target \
    . /.rust-env \
    && cargo build --release --target "$(cat /.rust-target)" -p acmecast-server \
    && cp "target/$(cat /.rust-target)/release/acmecast-server" /acmecast-server

# ---- 前端 builder：SPA 静态产物 ----
FROM node:24-slim AS frontend-builder

RUN corepack enable

WORKDIR /app/frontend
COPY frontend/package.json frontend/pnpm-lock.yaml frontend/pnpm-workspace.yaml ./
RUN pnpm install --frozen-lockfile

COPY frontend/ .
RUN pnpm build

# ---- 运行阶段：distroless static（非 root） ----
FROM gcr.io/distroless/static-debian13:nonroot

COPY --from=builder /acmecast-server /usr/bin/acmecast-server
COPY --from=frontend-builder /app/frontend/dist /static

# 数据目录（数据库、证书文件、凭据密钥）挂卷持久化；前端产物目录交给静态托管。
ENV ACMECAST_LISTEN=0.0.0.0:8080 \
    ACMECAST_DATA_DIR=/data \
    ACMECAST_STATIC_DIR=/static
EXPOSE 8080
VOLUME ["/data"]

ENTRYPOINT ["acmecast-server"]
