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
  这些工具链可由仓库自带的 Nix 环境一键提供，见 2.2；不用 Nix 时需自行准备同版本；
- 一个可用的 ACME CA（默认 Let's Encrypt 生产环境）；
- 一个 DNS 服务商账号（DNS-01 挑战需要，见 [07-dns-and-deploy.md](07-dns-and-deploy.md)）；
- 部署目标（本机目录或一台可 SSH 登录的主机）。

> 生产请使用 Let's Encrypt 生产端点；调试建议先用 `letsencrypt-staging`，
> 避免触发生产环境的速率限制。

## 2.2 源码开发环境（Nix + direnv）

仓库用 `flake.nix` + `flake.lock` 声明开发环境，用 `.envrc` 交给 direnv 自动加载。
工具链版本固定，各机器一致；每台机器只需一次性准备 Nix 与 direnv，之后进入目录即自动就绪。

### 1）准备机器（每台一次）

常规 Linux（有 systemd）与 Apple Silicon macOS：

```bash
sh <(curl -L https://nixos.org/nix/install) --daemon --yes   # 安装 Nix
```

国内网络可改用清华镜像，避免拉取官方源超时：

```bash
sh <(curl -L https://mirrors.tuna.tsinghua.edu.cn/nix/latest/install) --daemon
```

容器 / WSL 没有 systemd，走单用户安装；下面两步前置动作不能省，否则安装器会失败：

```bash
sudo mkdir -m 0755 /nix                                       # 安装器建 /nix 依赖 sudo，容器里常没有
sudo mkdir -p /etc/nix
printf 'build-users-group =\n' | sudo tee -a /etc/nix/nix.conf # 单用户没有 nixbld 组，不置空则最后一步报错
sh <(curl -L https://nixos.org/nix/install) --no-daemon --yes --no-channel-add
```

国内网络同样可换清华镜像：

```bash
sh <(curl -L https://mirrors.tuna.tsinghua.edu.cn/nix/latest/install) --no-daemon
```

两种安装方式都还要再开一次实验特性 —— 官方安装器默认关闭新 CLI 与 flakes，
不开启的话 `nix develop`、`nix profile install`、`nix flake update` 都会报
`experimental Nix feature 'nix-command' is disabled`：

```bash
mkdir -p ~/.config/nix
printf 'experimental-features = nix-command flakes\n' >> ~/.config/nix/nix.conf
nix profile install nixpkgs#direnv                             # 装完立刻可用，无需重启
echo 'eval "$(direnv hook zsh)"' >> ~/.zshrc                   # 让 direnv 接管 shell（bash 换 ~/.bashrc）
```

> direnv 的 `use flake` 自带 `--extra-experimental-features` 参数，所以只有手敲 nix
> 命令才需要上面那行。若 `--daemon` 安装后仍报同样错误，说明 nix-daemon 读的是自己的
> 配置：把该行追加到 `/etc/nix/nix.conf` 并重启守护进程
> （Linux `sudo systemctl restart nix-daemon`；macOS
> `sudo launchctl kickstart -k system/org.nixos.nix-daemon`）。

国内网络的镜像加速不写机器级配置，而是仓库根目录的 `.env`：`.envrc` 的
`dotenv_if_exists` 会加载它，并在 `use flake` 之前把变量导出。模板是仓库里的
`.env.example`，复制步骤见下面的 2）克隆后。

模板内容：腾讯云的 rustup/npm/pnpm 镜像、清华的 Nix 二进制缓存；另有 `IS_CHINA=1`，
devShell 的 cargo 包装按它在运行时给会访问 registry 的子命令注入 crates.io 镜像。
海外网络不必创建 `.env`，留空即走官方源。

### 2）克隆后（每个 clone 都要做）

```bash
git clone <repo> && cd acmecast
cp .env.example .env   # 国内网络启用镜像加速；海外可跳过
direnv allow           # 授权 .envrc（按内容哈希）；改过 .envrc 后需重新执行
prek install           # 写入 .git/hooks/；钩子不随仓库分发，每个 clone 都要装
```

### 3）环境提供什么

| 类别 | 内容 |
| --- | --- |
| Rust | rustc / cargo 1.98.0、clippy、rustfmt、rust-analyzer |
| 前端 | Node 24、pnpm（对应 `frontend/package.json` 的 `packageManager`） |
| 构建依赖 | gcc、pkg-config、gnumake、perl（Linux；darwin 走系统 clang） |
| 排查工具 | jq、tree、curl、git、sqlite、cacert |
| Git 钩子 | prek，配置见 `prek.toml`：提交前跑格式与文件卫生检查，推送前跑 clippy |

生效范围：`flake.nix` 决定工具链；镜像环境变量来自 `.env`（由 `.envrc` 的
`dotenv_if_exists` 加载，仅 direnv 目录内）；
crates.io 镜像由 devShell 提供的 cargo 包装在运行时按 `IS_CHINA` 注入 `--config`，
且只对会访问 registry 的子命令（`fetch`/`build`/`clippy`/`test`/`install` 等）生效，
`--version`、`fmt`、`new`、`clean` 之类原样直通（cargo 的源替换不认环境变量，这样做
就不必在仓库或 `$CARGO_HOME` 里放 `config.toml`；包装排在 PATH 最前，
`nix develop -c cargo …` 与 prek 钩子同样覆盖）。
系统包不需要额外安装，链接器与 make/perl 都由环境提供。

### 4）日常命令

```bash
direnv allow                        # 授权 .envrc（按内容哈希）
direnv reload                       # 改过 .envrc 或 .env 后重新求值
nix develop                         # 不用 direnv 时的等价入口（需按 1）开启 flakes）
                                    # 旧命令 nix-shell 无效：仓库只有 flake.nix，没有 shell.nix / default.nix
nix develop -c cargo test           # 一次性执行某条命令
prek run --all-files                # 手动全量跑 Git 钩子
prek run cargo-clippy --all-files --stage pre-push
nix flake update nixpkgs            # 升级 nixpkgs（会改写 flake.lock，需一并提交）
```

> 钩子里的 cargo 取的是 PATH 上的版本，因此**提交要在环境内进行**（direnv 目录内或
> `nix develop` 里）；IDE、GUI Git 客户端等不经 shell 的场合不会加载该环境。

支持平台为 Linux（x86_64 / aarch64）与 Apple Silicon macOS；Intel Mac 不受支持
（`flake.lock` 钉住的 nixpkgs 已移除 `x86_64-darwin`），Windows 请用 WSL。
另外 `rust-overlay` 输入仍取自 GitHub，国内首次拉取会走 GitHub，之后进本地缓存。

## 2.3 从源码构建

以下命令都应在 2.2 的环境内执行（`direnv` 已加载或 `nix develop`）：

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
