# 2. 快速开始

## 2.1 前置条件

- Docker 部署：Docker 20+；
- 源码构建：Rust 1.98+（workspace edition 2024）；前端另需 Node 24 + pnpm。
  这些工具链可由仓库自带的 Nix 环境一键提供，见 2.3；不用 Nix 时需自行准备同版本；
- 一个可用的 ACME CA（默认 Let's Encrypt 生产环境）；
- 一个 DNS 服务商账号（DNS-01 挑战需要，见 [07-dns-and-deploy.md](07-dns-and-deploy.md)）；
- 部署目标（本机目录或一台可 SSH 登录的主机）。

> 生产请使用 Let's Encrypt 生产端点；调试建议先用 `letsencrypt-staging`，
> 避免触发生产环境的速率限制。

## 2.2 用 Docker 启动

### 1）生成必需密钥

```bash
export ACMECAST_JWT_SECRET="$(openssl rand -base64 32)"
export ACMECAST_CREDENTIAL_KEY="$(openssl rand -base64 32)"
```

| 密钥 | 用途 | 丢失/更换后果 |
| --- | --- | --- |
| `ACMECAST_JWT_SECRET` | 访问令牌的 HS256 签名密钥 | 已签发的令牌全部失效（重新登录即可） |
| `ACMECAST_CREDENTIAL_KEY` | 凭据静态加密密钥（AES-256-GCM） | **已存凭据无法解密**，必须重新录入 |

两者都请与数据目录**分开保管**。

### 2）生成管理员口令哈希

口令以 Argon2 PHC 字符串配置，明文口令不出现在配置、日志或响应中。
用镜像内的 `hash-password` 子命令生成（运行环境无需 Rust 工具链）：

```bash
export ACMECAST_ADMIN_PASSWORD_HASH="$(docker run --rm acmecast:dev hash-password '你的口令')"
```

### 3）构建镜像（若尚未构建）

```bash
docker build -t acmecast:dev .
# 上线构建时关闭国内镜像源配置：
# docker build --build-arg USE_CN_MIRROR=false -t acmecast:dev .
```

镜像内已包含前端构建产物，并以非 root（uid 65532）运行。

### 4）启动容器

```bash
mkdir -p ./data && chmod 0777 ./data    # 容器以 uid 65532 运行，目录需可写

docker run -d --name acmecast \
  -p 8080:8080 \
  -v "$(pwd)/data:/data" \
  -e ACMECAST_JWT_SECRET \
  -e ACMECAST_CREDENTIAL_KEY \
  -e ACMECAST_ADMIN_PASSWORD_HASH \
  acmecast:dev
```

容器默认环境变量为 `ACMECAST_LISTEN=0.0.0.0:8080`、`ACMECAST_DATA_DIR=/data`、
`ACMECAST_STATIC_DIR=/static`。

### 5）验证

```bash
curl http://127.0.0.1:8080/healthz            # -> ok
docker logs -f acmecast                        # 观察启动日志（数据库就绪 / 开始服务）
```

浏览器打开 `http://127.0.0.1:8080/` 进入控制台，或 `/swagger-ui` 查看接口文档。

## 2.3 源码开发环境（Nix + direnv）

仓库用 `flake.nix` + `flake.lock` 声明开发环境，用 `.envrc` 交给 direnv 自动加载。
工具链版本固定，各机器一致；每台机器只需一次性准备 Nix 与 direnv，之后进入目录即自动就绪。

### 1）准备机器（每台一次）

常规 Linux（有 systemd）与 Apple Silicon macOS：

```bash
sh <(curl -L https://nixos.org/nix/install) --daemon --yes   # 安装 Nix
nix profile install nixpkgs#direnv                           # 安装 direnv
echo 'eval "$(direnv hook zsh)"' >> ~/.zshrc                 # 让 direnv 接管 shell（bash 换 ~/.bashrc）
```

容器 / WSL 没有 systemd，走单用户安装；下面两步前置动作不能省，否则安装器会失败：

```bash
sudo mkdir -m 0755 /nix                                       # 安装器建 /nix 依赖 sudo，容器里常没有
sudo mkdir -p /etc/nix
printf 'build-users-group =\n' | sudo tee -a /etc/nix/nix.conf # 单用户没有 nixbld 组，不置空则最后一步报错
sh <(curl -L https://nixos.org/nix/install) --no-daemon --yes --no-channel-add
```

国内网络另加一行（`.envrc` 通过环境变量判断，写进机器级配置才生效）：

```bash
mkdir -p ~/.config/direnv
echo 'export IS_CHINA=1' >> ~/.config/direnv/direnvrc
```

`IS_CHINA=1` 会同时启用腾讯云的 rustup/npm/pnpm 镜像、清华的 Nix 二进制缓存，
并把 nixpkgs 输入换成清华的 channel tarball。最后一项有取舍：channel 的内容是
Hydra 测过的 revision，未必等于 `flake.lock` 记录的 commit，因此国内与外网/CI 侧的
pnpm、prek 等次要工具版本可能有小差异；要严格对齐 lock 的 rev，按 `.envrc` 里的
注释换成 atomgit 镜像（需保留 `shallow=1`，否则会全量克隆 nixpkgs）。

### 2）克隆后（每个 clone 两次操作）

```bash
git clone <repo> && cd acmecast
direnv allow      # 授权 .envrc（按内容哈希）；改过 .envrc 后需重新执行
prek install      # 写入 .git/hooks/；钩子不随仓库分发，每个 clone 都要装
```

### 3）环境提供什么

| 类别 | 内容 |
| --- | --- |
| Rust | rustc / cargo 1.98.0、clippy、rustfmt、rust-analyzer |
| 前端 | Node 24、pnpm（对应 `frontend/package.json` 的 `packageManager`） |
| 构建依赖 | gcc、pkg-config、gnumake、perl（Linux；darwin 走系统 clang） |
| 排查工具 | jq、tree、curl、git、sqlite、cacert |
| Git 钩子 | prek，配置见 `prek.toml`：提交前跑格式与文件卫生检查，推送前跑 clippy |

生效范围：`flake.nix` 决定工具链；镜像环境变量来自 `.envrc`（仅 direnv 目录内）；
crates.io 镜像由 devShell 提供的 cargo 包装在运行时按 `IS_CHINA` 注入 `--config`，
且只对会访问 registry 的子命令（`fetch`/`build`/`clippy`/`test`/`install` 等）生效，
`--version`、`fmt`、`new`、`clean` 之类原样直通（cargo 的源替换不认环境变量，这样做
就不必在仓库或 `$CARGO_HOME` 里放 `config.toml`；包装排在 PATH 最前，
`nix develop -c cargo …` 与 prek 钩子同样覆盖）。
系统包不需要额外安装，链接器与 make/perl 都由环境提供。

### 4）日常命令

```bash
direnv allow                        # 授权 .envrc（按内容哈希）
direnv reload                       # 改过 .envrc 后重新求值
nix develop                         # 不用 direnv 时的等价入口（需启用 flakes）
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

## 2.4 从源码构建

以下命令都应在 2.3 的环境内执行（`direnv` 已加载或 `nix develop`）：

```bash
# 后端
cargo build --release -p acmecast-server
# 生成管理员口令哈希（开发环境）
cargo run -p acmecast-server --example hash-password -- '你的口令'

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

## 2.5 首个可用流程（按当前实现）

> 当前 dev 分支只注册了 `acme.account` 一种凭据类型，**没有创建 DNS / SSH 凭据的
> 接口**，也没有「立即运行一次流水线」的接口。下面先走通「能走通的部分」，
> 完整的 DNS-01 端到端配置见 [12-operations-and-limitations.md](12-operations-and-limitations.md)。

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

## 2.6 调试建议

- 想尽快验证链路：直接 `POST /api/pipelines/{id}/run` 立即触发，或把 `cron` 设为
  `* * * * *`（下一分钟触发）；
- 首次接入建议用 `letsencrypt-staging` + 测试域名，成功后再切生产；
- 自建测试 CA（如 pebble）时，在 `cert.apply` 输入里
  `insecure_skip_verify: true`，或在服务端设置 `ACMECAST_INSECURE_SKIP_VERIFY=1`；
- DNS-01 传播等待若因内网 DNS 查不到而超时，把 `wait_propagation` 设为 `false`。
