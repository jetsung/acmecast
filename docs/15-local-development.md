# 15. 本地开发（无 Docker）

面向希望在本机直接跑起来（**前端界面 + 后台服务**）并以最短路径拿到一张证书的
开发者。不需要 Docker、不需要 swagger-ui；全程在源码开发环境内完成。

> 本文是 [02-getting-started.md](02-getting-started.md) 的精简实战版：
> 02 文档讲完整的 Nix + direnv 工具链与全部命令行用法，本文只关心
> 「前端 + 后端 + 能签下证书」这一条路径。Docker 部署见
> [13-docker-deployment.md](13-docker-deployment.md)。

## 15.1 前置条件

- Rust 1.98+、Node 24 + pnpm（仓库自带 Nix 环境一键提供，见 02 文档 2.2）；
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

## 15.5 界面走通一次签发

以下步骤全部在 `http://localhost:5173` 的控制台里完成。

### 1）登录

用 15.2 设置的口令（用户名默认 `admin`）登录。令牌存入浏览器 `localStorage`，
刷新可恢复。

### 2）创建 ACME 账号凭据

进入「凭据管理」页 → 新建：

| 字段 | 填写 |
| --- | --- |
| 名称 | `le-staging`（自选） |
| 类型 | `acme.account`（下拉显示为「ACME 账号」） |
| `ca` | `letsencrypt-staging`（调试首选） |

保存后记下凭据 **id**（例如 1）。字段语义见 [05-credentials.md](05-credentials.md) 5.2。

### 3）创建 DNS 提供商凭据

同页再新建一条，类型选 `cloudflare`（下拉显示为「Cloudflare（DNS）」）或 `aliyun`（「阿里云（DNS）」），填入对应 Token/AccessKey。
记下其凭据 id（例如 2）。字段见 [05-credentials.md](05-credentials.md) 5.3。

### 4）新建流水线

进入「流水线」页 → 新建，添加三个步骤：

**步骤 1：`cert.apply`**

| 字段 | 取值 |
| --- | --- |
| `domains` | `["你的域名"]`（通配符形如 `["*.example.com", "example.com"]`） |
| `challenge` | `dns-01` |
| `account_credential_id` | 选 2）里那条 ACME 账号凭据（下拉） |
| `dns_provider` | 选 `cloudflare` / `aliyun`（下拉） |
| `dns_credential_id` | 选 3）里那条 DNS 凭据（下拉） |
| `contacts` | `["mailto:你的邮箱"]` |
| `wait_propagation` | 内网 DNS 查不到记录时设为 `false`；否则保持默认 |

**步骤 2：`cert.store`**

| 字段 | 取值 |
| --- | --- |
| `acme_account_credential_id` | 选 2）里那条 ACME 账号凭据 |

**步骤 3：`cert.deploy`（本地部署，可选）**

| 字段 | 取值 |
| --- | --- |
| `target` | `local` |
| `config` → `cert_path` | 绝对路径，如 `/tmp/acmecast/你的域名.crt` |
| `config` → `key_path` | 绝对路径，如 `/tmp/acmecast/你的域名.key` |
| `config` → `reload_command` | 可留空（本地开发无需重载） |
| `force` | `false` |

> `cert.deploy` 的 `config` 结构随 `target` 变化，前端按 JSON 文本编辑。
> 完整字段见 [07-dns-and-deploy.md](07-dns-and-deploy.md) 7.2。

保存流水线。

### 5）立即运行

在流水线列表页点击该行的「**立即运行**」按钮，前端会跳转到运行历史详情页，
每 5 秒自动轮询状态与日志。`running` → `success` 即签发成功。

也可以用命令行触发与查看：

```bash
TOKEN=...   # 登录拿到的令牌
curl -sS -X POST -H "Authorization: Bearer $TOKEN" \
  http://127.0.0.1:8080/api/pipelines/1/run
curl -sS -H "Authorization: Bearer $TOKEN" http://127.0.0.1:8080/api/histories/1/logs
```

### 6）验证证书

- 控制台「证书」页能看到新签发的证书；staging 端点签发的证书不被浏览器/系统信任（ issuer 含 `(STAGING)` 字样），这是正常现象，切到生产端点后即受信任；
- 数据目录下 `data/certs/<fingerprint>/cert.pem` 与 `key.pem` 已落盘；
- 若配了 `cert.deploy`，对应路径的文件也已写入。

## 15.6 切换到生产端点

staging 跑通后，把 ACME 账号凭据的 `ca` 改为 `letsencrypt`（或新建一条生产
凭据），重新运行流水线即可拿到受信任的证书。

> 生产端点有速率限制，同一域名集一周内只签几次；调试务必先在 staging 走通。

## 15.7 常见问题

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
