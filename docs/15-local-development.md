# 15. 本地开发与调试（无 Docker）

面向希望在本机直接跑起来（**前端界面 + 后台服务**）的开发者。不需要 Docker、
不需要 swagger-ui；跑通之后如何「登录 → 建凭据 → 建流水线 → 签下第一张证书」
走控制台操作流程即可（见 [11-frontend.md](11-frontend.md) 11.10 的流程图，
取值细节与 [02-getting-started.md](02-getting-started.md) 2.4 一致）。

> 本文是 [02-getting-started.md](02-getting-started.md) 的精简实战版：
> 02 文档讲完整的 Nix + direnv 工具链、镜像加速与全部命令行用法，本文只关心
> 「把前端和后端在本机跑起来」。Docker 部署见
> [13-docker-deployment.md](13-docker-deployment.md)。

## 15.1 前置条件

- Rust 1.98+、Node 24 + pnpm（仓库自带 Nix 环境一键提供，见 02 文档 2.2）；
  root VPS / 容器 / WSL 走单用户安装、其余机器走多用户安装，两条路径与
  「是否需要手动 source nix.sh」「是否需要装 sudo」的说明见 02 文档 2.2 第 1）步；
- direnv（用来自动加载 Nix 开发环境，进入仓库目录即生效）：
  `curl -fsSL fx4.cn/direnv | bash`，详见 02 文档 2.2 第 1）步；
- 一个 ACME CA —— **调试请用 `letsencrypt-staging`**，避免触发生产速率限制；
- 一个 DNS 服务商账号（Cloudflare 或阿里云，DNS-01 挑战需要）；
- 一个你拥有 DNS 控制权的域名（可指向 staging 就能验证）。

> Staging 证书不被浏览器信任，但能完整跑通签发流程，足够验证本地开发链路。
> 成功后再切到 `letsencrypt` 生产端点即可。

## 15.2 准备密钥与口令

三个变量必须先在**运行后端的那个 shell** 里导出：

```bash
# 1) 两把必需密钥（妥善保存；更换 CREDENTIAL_KEY 会使已存凭据无法解密）
export ACMECAST_JWT_SECRET="$(openssl rand -base64 32)"
export ACMECAST_CREDENTIAL_KEY="$(openssl rand -base64 32)"

# 2) 管理员口令哈希（开发环境，直接在源码环境内生成）
export ACMECAST_ADMIN_PASSWORD_HASH="$(cargo run -q -p acmecast-server --example hash-password -- '你的口令')"
```

各变量含义见 [03-configuration.md](03-configuration.md) 3.1 与
[13-docker-deployment.md](13-docker-deployment.md) 13.2。

## 15.3 启动后端

```bash
# 不托管前端静态资源也不影响开发：后端只提供 API，前端走 Vite dev server
cargo run -p acmecast-server
# 默认监听 0.0.0.0:8080，数据目录 ./data
```

验证：

```bash
curl http://127.0.0.1:8080/healthz   # -> ok
```

## 15.4 启动前端

另开一个 shell（同样在开发环境内），后端保持运行：

```bash
cd frontend
pnpm install
pnpm dev          # http://localhost:5173
```

Vite dev server 会把 `/api` 代理到 `127.0.0.1:8080`（见 `vite.config.ts`），
因此前后端同源、无 CORS。打开 `http://localhost:5173` 即进入控制台，
**不需要 swagger-ui**——所有操作都能在界面上完成。

> 若后端接口有变更，记得 `pnpm gen:api` 重新生成前端类型；首次拉起无需此步。

## 15.4.1 打开调试日志（本地调试 / 测试环境）

日志由 `RUST_LOG` 控制（tracing 过滤指令，详见
[03-configuration.md](03-configuration.md) 3.1），默认 `info`。流水线各步骤的
执行日志（DNS-01 下发、传播等待、部署写入等）会原样落到运行历史里，前端
「运行历史详情」页可按步骤查看；把控制台日志调到 `debug` 能额外看到 HTTP
请求与调度器的细节，排查签发失败时首选。

**调试场景推荐配置**（另开终端或直接在启动命令前导出）：

```bash
# 本项目模块 debug，其余（hyper、sqlx 等依赖）保持 info，避免噪音
export RUST_LOG=acmecast_server=debug,acmecast_pipeline=debug,acmecast_dns=debug,acmecast_deploy=debug,info
cargo run -p acmecast-server
```

- 想更细（如 SQL 语句）：把 `sqlx=debug` 追加进过滤器；想安静些：`RUST_LOG=warn`；
- 只调某一次运行：`RUST_LOG=debug cargo run -p acmecast-server` 全开，排完即撤；
- 流水线运行历史是落库的（`GET /api/histories/{id}/logs`），调完重启不影响回看。

> 生产 / 长驻测试环境建议保持默认 `info`：`debug` 日志量大且含请求细节，
> 只在排查时临时打开。

## 15.5 下一步：在控制台走通一次签发

前后端都起来后，按 [11-frontend.md](11-frontend.md) 11.10 的控制台操作流程图
完成「登录 → 建 ACME 账号凭据 → 建 DNS 凭据 → 建流水线 → 立即运行 → 验证证书」。
本地开发场景的典型取值（staging 端点、Cloudflare DNS、本地部署）与逐字段说明见
[02-getting-started.md](02-getting-started.md) 2.4 的命令行版本，前端操作与其一一对应。

跑通 staging 后切生产端点：把 ACME 账号凭据的 `ca` 改为 `letsencrypt`
（或新建一条生产凭据），重新运行流水线即可。

> 生产端点有速率限制，同一域名集一周内只签几次；调试务必先在 staging 走通。

## 15.6 常见问题

- **登录返回 500 `configuration_error`**：`ACMECAST_ADMIN_PASSWORD_HASH` 未设置或非法；
  按 15.2 重新生成并确认已导出到**运行后端的 shell**。
- **`cert.apply` 报 `未知的凭据类型`**：凭据类型下拉只认 `acme.account` /
  `cloudflare` / `aliyun`；SSH 凭据当前无法通过 API 创建
  （见 [12-operations-and-limitations.md](12-operations-and-limitations.md) 12.4）。
- **DNS-01 一直超时**：内网 DNS 查不到 TXT 记录时把 `wait_propagation` 设为 `false`；
  确认 DNS Token/AccessKey 对目标域名有编辑权限；公共后缀域名显式指定 `dns_zone`
  （见 [07-dns-and-deploy.md](07-dns-and-deploy.md) 7.1）。
- **前端 `pnpm dev` 打不开**：确认后端已在 8080 端口运行（`curl /healthz`）；
  `vite.config.ts` 把 `/api` 代理到 `127.0.0.1:8080`，后端不在则接口全 404。
- **自建测试 CA（pebble 等）**：`cert.apply` 输入里设 `insecure_skip_verify: true`，
  或全局设 `ACMECAST_INSECURE_SKIP_VERIFY=1`。

完整排障表见 [12-operations-and-limitations.md](12-operations-and-limitations.md) 12.3。
