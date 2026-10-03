# 排障与已知限制

## 排障

### 启动失败

| 错误 | 原因与处理 |
|---|---|
| `ACMECAST_JWT_SECRET` 相关配置错误 | 必需，缺失拒绝启动 |
| credential key 相关配置错误 | 必需；须为 base64 编码的 32 字节（`openssl rand -base64 32`） |
| 口令哈希非法 PHC | `ACMECAST_ADMIN_PASSWORD_HASH` 必须形如 `$argon2id$...`，用 `hash-password` 子命令重新生成 |
| 迁移失败（报版本号） | 数据库 schema 与迁移版本冲突；看日志中失败版本，切勿手工改已应用的迁移 |
| 不支持的数据库方言 | 连接串 scheme 仅支持 sqlite/mysql(mariadb)/postgres(postgresql) |

### 登录

- **401 `invalid_credentials`**：用户名或口令错误（统一文案，不区分哪种错）。
  注意 `ACMECAST_ADMIN_USERNAME` 是否被改过。
- **429 `too_many_attempts`**：连续失败 5 次锁定 15 分钟。锁定是进程内计数，
  重启进程即清零。
- **500 `configuration_error`**：未配置管理员口令哈希——服务能启动但无人能
  登录，启动日志有对应告警。

### 证书申请

- **挑战超时，错误提示「提供商侧没认」**：DNS 凭据权限不足或 zone 填错——
  检查 `dns_zone`（缺省从主域名去第一段推导，多级后缀如 `a.b.co.uk` 需显式
  给出 `b.co.uk`）。
- **挑战超时，错误提示「解析器没看到」**：传播慢或用了内网 DNS。内网 zone
  请在 `cert.apply` 输入里设 `wait_propagation=false`。
- **报「异值残留」**：`_acme-challenge` 下有上次失败留下的旧 TXT 记录。
  这是刻意的保护（防 CA 读到旧值）；手工删掉残留记录后重跑。
- **`externalAccountRequired`**：该 CA（ZeroSSL/Google 等）要求 EAB，在
  `acme.account` 凭据里成对补 `eab_kid` / `eab_hmac_key`。
- **通配符失败**：通配符只能 DNS-01，确认 `challenge` 为 `dns-01`。

### 部署

- **权限/属主报错**：改他人所有的文件需 root；容器内 uid 65532 对宿主机
  目录的写权限要单独处理。
- **重载命令失败但文件已写好**：`Reload` 错误携带命令、退出码与合并输出；
  文件不会回滚，修好命令后重跑（同指纹会跳过写入但仍执行重载）。
- **SSH 连接失败**：先用凭据的「测试」按钮（10 秒超时的真实探测）定位是
  网络、认证还是权限问题。

### 凭据

- **500 `credential_decryption_error`**：`ACMECAST_CREDENTIAL_KEY` 被更换或
  数据目录来自另一套密钥的环境。密钥无法找回时只能逐个重建凭据。
- **删除被拒（409）**：错误里列出了引用该凭据的流水线与步骤序号，先改流水线。

### 日常诊断

- 日志：`RUST_LOG=debug` 或按模块 `RUST_LOG=info,acmecast_dns=debug`。
- 每个请求带 `x-request-id`，报错时连同该值检索日志。
- 运行历史与逐步骤日志：`GET /api/histories/{id}/logs`。
- 触发审计（为什么没触发/触发了几次）：`GET /api/schedules/trigger-logs`。

## 已知限制

1. **SSH 部署未接入 known_hosts**：接受任何主机密钥（每次连接记 warn 并打出
   SHA-256 指纹），有中间人风险的环境请配合网络层隔离。
2. **HTTP-01 只有材料计算能力**：没有投放通道，申请步骤请用 DNS-01。
3. **登录限流是进程内计数**：多副本部署下需在反向代理层做防护。
4. **DNS 提供商仅内置 Cloudflare 与阿里云**；其它厂商需扩展
   `acmecast-dns` 的 provider。
5. **换数据库不迁移数据**：SQLite → MySQL/PG 需自行导出导入或重新申请。
6. **同一流水线同时只允许一个运行**：重复触发被跳过（不排队），需要并发
   跑同一批域名时请拆成多条流水线。
