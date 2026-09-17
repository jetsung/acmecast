# .ide/Dockerfile —— CNB 云 IDE 的开发容器：code-server + SSH + 本仓库所需工具链。
FROM node:24

# 安装 code-server 及插件（单容器模式必须）
RUN curl -fsSL https://code-server.dev/install.sh | sh \
  && code-server --install-extension cnbcool.cnb-welcome \
  && echo done

# 系统依赖：
# - openssh-server：供 VSCode / Cursor / Zed 通过 Remote-SSH 连接；
# - build-essential / pkg-config / make / perl：Rust 侧 ring 等 crate 的构建脚本需要，
#   缺 cc 时 cargo build 会直接失败；
# - musl-tools：发布镜像走 musl 静态链接，装了才能在 IDE 内复现该构建；
# - default-jre-headless：提供 keytool。cert 的 JKS 用例会主动查找它并在缺失时失败
#   （不跳过），CI 同样是显式安装 JDK。
RUN apt-get update \
  && apt-get install -y --no-install-recommends \
    git wget unzip ca-certificates openssh-server \
    build-essential pkg-config make perl musl-tools \
    default-jre-headless \
  && rm -rf /var/lib/apt/lists/*

# Rust 工具链：与仓库 Dockerfile 的 rust:1.98 及 clippy.toml 的 msrv 对齐。
# profile minimal 不含 rustfmt/clippy，故显式补组件；musl 目标是发布镜像的构建目标。
ENV RUSTUP_HOME=/usr/local/rustup \
    CARGO_HOME=/usr/local/cargo \
    PATH=/usr/local/cargo/bin:$PATH \
    RUST_VERSION=1.98

RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
      | sh -s -- -y --no-modify-path --profile minimal --default-toolchain "${RUST_VERSION}" \
  && rustup component add rustfmt clippy \
  && rustup target add x86_64-unknown-linux-musl \
  && rustup show

# 用户级 CLI 的落点是 /root/.local/bin（pip --user / uv tool / pipx 都装在这里，
# 例如在 IDE 内装 zensical），纳入 PATH 后装完即可直接调用。
#
# 登录 shell 同样要能用：SSH / Remote-SSH 会话走的是登录 shell，而 Debian 的
# /etc/profile 会**重置** PATH（只留系统默认值），上面 ENV 设的路径会被丢掉。
# /etc/profile.d/*.sh 在重置之后被 source；两条路径写进同一个脚本，避免依赖
# 多个 profile.d 文件之间的 source 顺序。
ENV PATH=/root/.local/bin:$PATH
RUN printf '%s\n' \
      'export RUSTUP_HOME=/usr/local/rustup' \
      'export CARGO_HOME=/usr/local/cargo' \
      'export PATH=/usr/local/cargo/bin:/root/.local/bin:$PATH' \
      > /etc/profile.d/dev-path.sh

# musl 交叉链接器：与仓库 Dockerfile 保持一致，使
# cargo build --target x86_64-unknown-linux-musl 在 IDE 内也能链接通过。
ENV CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_LINKER=x86_64-linux-musl-gcc

# 前端包管理器：node 自带 corepack。把仓库锁定的 pnpm 版本预装进镜像，免得首次
# 在 IDE 里跑 pnpm 还要现场下载；版本与 frontend/package.json 的 packageManager 一致。
ENV PNPM_VERSION=11.20.0
RUN corepack enable \
  && corepack install -g "pnpm@${PNPM_VERSION}"

# 指定字符集支持命令行输入中文
ENV LANG=C.UTF-8
ENV LANGUAGE=C.UTF-8
