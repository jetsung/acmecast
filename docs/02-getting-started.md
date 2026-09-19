# 2. 快速开始

两条部署路径：**Docker 部署**见 [13-docker-deployment.md](13-docker-deployment.md)；
**源码部署**从下文 2.2 开始（开发环境、构建与首个可用流水线），生产姿态
（systemd 常驻 + nginx 反代合并前后端）见
[17-backend-hosting-frontend.md](17-backend-hosting-frontend.md)。

> 只想最快在本机跑通「前端 + 后端 + 拿到一张证书」？见
> [15-local-development.md](15-local-development.md)，它不依赖 Docker、也不需要
> swagger-ui，是本文的精简实战版。

## 2.1 前置条件

- Docker 部署：Docker 20+（见 [13-docker-deployment.md](13-docker-deployment.md)）；
- 源码构建：Rust 1.98+（workspace edition 2024）；前端另需 Node 24 + pnpm。
  这些工具链可由仓库自带的 mise 配置一键提供，见 2.2；不用 mise 时需自行准备同版本；
- 一个可用的 ACME CA（默认 Let's Encrypt 生产环境）；
- 一个 DNS 服务商账号（DNS-01 挑战需要，见 [07-dns-and-deploy.md](07-dns-and-deploy.md)）；
- 部署目标（本机目录或一台可 SSH 登录的主机）。

> 生产请使用 Let's Encrypt 生产端点；调试建议先用 `letsencrypt-staging`，
> 避免触发生产环境的速率限制。

## 2.2 源码开发环境（mise）

仓库用 `mise.toml` 声明开发环境，工具链版本固定，各机器一致。
每台机器只需一次性安装 mise 并激活 shell hook，之后进入目录即自动就绪。

### 1）准备机器（每台一次）

**安装 mise**（一行命令，跨平台）：

```bash
curl https://mise.run | sh
```

国内网络可换清华镜像：

```bash
curl https://mirrors.tuna.tsinghua.edu.cn/mise/run | sh
```

**激活 mise**（让 shell 进入仓库目录时自动加载工具链与环境变量）：

```bash
echo 'eval "$(mise activate zsh)"' >> ~/.zshrc   # bash 换 ~/.bashrc，fish 换 ~/.config/fish/config.fish
source ~/.zshrc                                     # 让当前 shell 立刻生效
```

> 原来用 direnv 的项目应移除 direnv hook：mise 官方明确 `use mise`（direnv 集成）
> 已废弃且不受支持，mise 的 `[env]` 配置（含 `_.file`、`_.path`）能完全覆盖
> direnv 的能力。

**安装系统工具**（mise 只管理语言运行时，以下工具由 OS 包管理器提供）：

Linux（Debian/Ubuntu）：

```bash
sudo apt install -y build-essential pkg-config jq tree curl git tmux ca-certificates sqlite3
```

macOS（Xcode Command Line Tools 已含 clang/make/perl）：

```bash
xcode-select --install                    # 如未安装
brew install jq tree tmux pkg-config      # curl/git/sqlite3 系统自带
```

> 构建依赖说明：项目全程 rustls（无 OpenSSL），但 ring / aws-lc-sys 等 crate
> 的构建脚本需要 C 编译器（Linux 用 gcc，macOS 用系统 clang）、pkg-config、
> make 和 perl。这些由系统包管理器提供，不在 mise 管理范围内。

国内网络的镜像加速不写机器级配置，而是仓库根目录的 `.env`：mise.toml 的
`[env] _.file = ".env"` 会加载它，并在工具安装前把变量导出。模板是仓库里的
`.env.example`，复制步骤见下面的 2）克隆后。

模板内容：腾讯云的 rustup/npm/pnpm 镜像；另有 `IS_CHINA=1` 标记。
crates.io 镜像由仓库根的 `.cargo/config.toml` 无条件指向腾讯云（cargo 的源
替换不认环境变量，只能用配置文件）。海外网络不必创建 `.env`，留空即走官方源。

### 2）克隆后（每个 clone 都要做）

```bash
git clone <repo> && cd acmecast
cp .env.example .env   # 国内网络启用镜像加速；海外可跳过
mise install           # 按 mise.toml 安装 Rust/Node/pnpm/prek
prek install           # 写入 .git/hooks/；钩子不随仓库分发，每个 clone 都要装
```

### 3）环境提供什么

| 类别 | 内容 |
| --- | --- |
| Rust | rustc / cargo 1.98.0、clippy、rustfmt、rust-analyzer（mise 经 rustup 安装） |
| 前端 | Node 24、pnpm 11.20.0（对应 `frontend/package.json` 的 `packageManager`） |
| Git 钩子 | prek（mise 经 aqua 后端安装），配置见 `prek.toml`：提交前跑格式与文件卫生检查，推送前跑 clippy |
| 环境变量 | `SWAGGER_UI_DOWNLOAD_URL` 指向仓库内随附的 swagger-ui 压缩包（构建不依赖外网） |

生效范围：`mise.toml` 的 `[tools]` 决定工具链版本；`[env]` 的 `_.file = ".env"`
加载国内镜像变量（rustup/npm/pnpm），海外开发者不创建 `.env` 即走官方源；
`SWAGGER_UI_DOWNLOAD_URL` 由 `[env]` 直接设置，使用 `{{config_root}}` 模板
指向仓库根的 `docker/swagger-ui-v5.17.14.zip`。

crates.io 镜像由 `.cargo/config.toml` 无条件指向腾讯云（公开可访问，海外也
能用，仅略慢）。不想走镜像的海外开发者可删除该文件。cargo 的源替换不认
环境变量，只能用配置文件或 `--config`，因此放在 `.cargo/config.toml` 而非
`.env`。该文件已被 `.dockerignore` 排除，不影响 Docker 构建（Dockerfile 有
自己的镜像处理）。

mise 管理的是 **rustc/cargo 本体**（经 rustup），不在 PATH 暴露 `rustup` 命令
行工具（mise 内部调用 rustup）。直接用 `rustc` / `cargo` 即可。

系统包（gcc、pkg-config、make、perl、jq 等）需要自行安装，见第 1）步。

### 4）日常命令

```bash
mise install                         # 安装/更新 mise.toml 声明的工具
mise exec -- cargo test              # 在 mise 环境内执行单条命令
mise upgrade rust                     # 升级 Rust（会改 mise.lock，需提交）
prek run --all-files                 # 手动全量跑 Git 钩子
prek run cargo-clippy --all-files --stage pre-push
mise lock                            # 生成/更新 mise.lock（钉住精确版本）
```

> 钩子里的 cargo 取的是 PATH 上的版本（由 mise 提供），因此**提交要在 mise
> 激活的 shell 内进行**（进入仓库目录即生效）。IDE、GUI Git 客户端等不经
> shell 的场合不会加载该环境——配置 IDE 使用 mise shim 或 `mise exec`。

支持平台为 Linux（x86_64 / aarch64）与 Apple Silicon macOS；Intel Mac 与
Windows 不受支持（mise 能装工具链，但项目未在这些平台验证），Windows 请用 WSL。

## 2.3 从源码构建

以下命令都应在 2.2 的环境内执行（mise 已激活，即进入仓库目录后的 shell）：

```bash
# 后端
cargo build --release -p acmecast-server
# 生成管理员口令哈希（开发环境）
cargo run -p acmecast-server --example hash-password -- '你的口令'
# 也可以在容器内生成，见 [13-docker-deployment.md](13-docker-deployment.md) 13.3

# 前端（可选，产物由后端静态托管）
cd frontend && pnpm install && pnpm build     # 产物在 frontend/dist
```

运行本地实例（仅后端 API）：

```bash
export ACMECAST_JWT_SECRET="$(openssl rand -base64 32)"
export ACMECAST_CREDENTIAL_KEY="$(openssl rand -base64 32)"
export ACMECAST_ADMIN_PASSWORD_HASH="$(cargo run -q -p acmecast-server --example hash-password -- '你的口令')"
cargo run -p acmecast-server
# 默认监听 0.0.0.0:8080，数据目录 ./data
```

托管前端静态资源时额外设置 `ACMECAST_STATIC_DIR=/path/to/frontend/dist`；
未设置时服务只提供 API（`/swagger-ui` 仍可用）。

## 2.4 首个可用流程（按当前实现）

> 当前 dev 分支已能创建 `acme.account` 与 DNS 提供商（`cloudflare` / `aliyun`）凭据，
> 但**还没有创建 SSH 凭据的接口**。下面先走通「能走通的部分」，完整的 DNS-01
> 端到端配置见 [12-operations-and-limitations.md](12-operations-and-limitations.md)。

### 1）登录拿令牌

```bash
TOKEN=$(curl -sS -X POST http://127.0.0.1:8080/api/login \
  -H 'Content-Type: application/json' \
  -d '{"username":"admin","password":"你的口令"}' | sed 's/.*"token":"\([^"]*\)".*/\1/')
echo "$TOKEN"
```

后续所有请求带 `Authorization: Bearer $TOKEN`。

### 2）创建 ACME 账号凭据

```bash
curl -sS -X POST http://127.0.0.1:8080/api/credentials \
  -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
  -d '{
    "name": "letsencrypt-prod",
    "type_id": "acme.account",
    "fields": { "ca": "letsencrypt" }
  }'
# -> {"data":{"id":1,"name":"letsencrypt-prod","type_id":"acme.account",...}}
```

> 字段以 JSON Schema 为准，先查 `GET /api/credential-types` 得到
> `acme.account` 的字段定义。合法的字段是 `ca` / `directory_url` /
> `eab_kid` / `eab_hmac_key`（不知名键会被忽略或影响校验，按 Schema 填写）。
> 账号联系人 `contacts` 属于 `cert.apply` 步骤输入，不在凭据里。

记下返回的 `id`（例如 1），它是流水线里 `account_credential_id` 的取值。

### 3）查看任务与输入 Schema

```bash
curl -sS -H "Authorization: Bearer $TOKEN" http://127.0.0.1:8080/api/tasks
curl -sS -H "Authorization: Bearer $TOKEN" http://127.0.0.1:8080/api/tasks/cert.apply/schema
```

### 4）创建一条流水线

```bash
curl -sS -X POST http://127.0.0.1:8080/api/pipelines \
  -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
  -d '{
    "name": "example.com 签发",
    "description": "申请并部署 example.com",
    "enabled": true,
    "steps": [
      { "type_id": "cert.apply", "enabled": true, "input": {
          "domains": ["example.com"],
          "challenge": "dns-01",
          "account_credential_id": 1,
          "dns_provider": "cloudflare",
          "dns_credential_id": 2,
          "contacts": ["mailto:ops@example.com"]
      }},
      { "type_id": "cert.store", "enabled": true, "input": {
          "acme_account_credential_id": 1
      }},
      { "type_id": "cert.deploy", "enabled": true, "input": {
          "target": "local",
          "config": {
            "cert_path": "/etc/nginx/ssl/example.com.crt",
            "key_path": "/etc/nginx/ssl/example.com.key",
            "cert_mode": "0644",
            "key_mode": "0600",
            "reload_command": "nginx -s reload"
          },
          "force": false
      }}
    ]
  }'
```

### 5）触发方式（可选其一或同时使用）

#### 立即手动触发一次（调试首选）

```bash
curl -sS -X POST -H "Authorization: Bearer $TOKEN" \
  http://127.0.0.1:8080/api/pipelines/1/run
# -> {"data":{"history_id":1,"status":"running"}}
curl -sS -H "Authorization: Bearer $TOKEN" http://127.0.0.1:8080/api/histories/1
```

异步执行，用返回的 `history_id` 轮询终态与日志（见 6）。

#### 配置调度（自动触发）

```bash
curl -sS -X POST http://127.0.0.1:8080/api/schedules \
  -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
  -d '{
    "pipeline_id": 1,
    "cron": "0 3 * * *",
    "enabled": true,
    "catch_up": false,
    "renewal_domains": ["example.com"]
  }'
```

- `cron` 为 5 段（分 时 日 月 周）或 6/7 段（秒 分 时 日 月 周 [年]），
  **按 UTC 计算**；
- `renewal_domains` 让这条流水线同时参与到期扫描续期。

### 6）查看运行历史与日志

```bash
curl -sS -H "Authorization: Bearer $TOKEN" "http://127.0.0.1:8080/api/histories?page=1&page_size=10"
curl -sS -H "Authorization: Bearer $TOKEN" http://127.0.0.1:8080/api/histories/1/logs
```

也可以在前端「运行历史」页查看按步骤分组的日志。

## 2.5 调试建议

- 想尽快验证链路：直接 `POST /api/pipelines/{id}/run` 立即触发，或把 `cron` 设为
  `* * * * *`（下一分钟触发）；
- 首次接入建议用 `letsencrypt-staging` + 测试域名，成功后再切生产；
- 自建测试 CA（如 pebble）时，在 `cert.apply` 输入里
  `insecure_skip_verify: true`，或在服务端设置 `ACMECAST_INSECURE_SKIP_VERIFY=1`；
- DNS-01 传播等待若因内网 DNS 查不到而超时，把 `wait_propagation` 设为 `false`。
