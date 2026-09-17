# 12. 运维、排障与已知限制

## 12.1 备份与迁移

持久化状态只有两处：

1. **数据目录**（`ACMECAST_DATA_DIR`）：数据库文件 + 证书/私钥文件；
2. **环境变量中的两把密钥**（`ACMECAST_JWT_SECRET`、`ACMECAST_CREDENTIAL_KEY`）。

迁移步骤：

```bash
# 1. 停服务（SIGTERM / Ctrl-C，等待优雅停机）
# 2. 备份数据目录（使用外部数据库时同时备份数据库）
tar czf acmecast-data-$(date +%F).tgz -C /path/to data
# 3. 在新主机还原数据目录，并**原样**配置两把密钥
```

注意：

- **必须同时保留凭据加密密钥**，否则数据库中的凭据密文永远无法解密；
- JWT 密钥可更换（代价是已签发令牌失效、需重新登录），
  但凭据加密密钥**不可更换**；
- 使用外部数据库时，数据目录中仍保存证书文件，二者要一起迁移。

## 12.2 升级

1. 备份数据目录；
2. 替换二进制/镜像；
3. 启动时会自动执行数据库迁移；迁移失败会报出失败版本并中止启动
   （不要强行跳过，先回滚到旧版本并检查）。

容器升级：

```bash
docker rm -f acmecast
docker run -d --name acmecast ... acmecast:<新版本>
```

## 12.3 排障

### 服务起不来

| 现象 | 可能原因 | 处理 |
| --- | --- | --- |
| 启动即退出，日志含 `缺少 ACMECAST_JWT_SECRET` | 未配置令牌签名密钥 | 设置该变量 |
| 迁移失败并报版本号 | 数据库结构不兼容/权限不足 | 检查数据库权限与版本，必要时回滚 |
| 端口占用 | 8080 已在使用 | 改 `ACMECAST_LISTEN` |
| 容器内数据目录不可写 | 以 uid 65532 运行 | `chmod 0777` 挂载目录或以正确属主挂载 |

### 无法登录

- 返回 500 `configuration_error`：未配置 `ACMECAST_ADMIN_PASSWORD_HASH`；
- 返回 429：连续失败 5 次，等待 900 秒或重启进程清空限流计数；
- 返回 400 `auth_disabled`：服务以 `ACMECAST_AUTH_DISABLED=1` 启动，无需登录；
- 令牌被拒（401）：令牌过期或 JWT 密钥变更，重新登录。

### 流水线没有运行

按顺序排查：

1. **是否配置了调度？** 没有调度就没有任何触发；
2. 调度是否 `enabled`？
3. cron 是否按 **UTC** 理解正确？
4. 流水线本身是否 `enabled`？（停用的流水线即使被触发也会跳过，日志有记录）
5. 是否已有 `running` 状态的运行历史卡住？运行中检查会阻止重复触发；
6. 查看服务日志中调度器相关记录（`触发点已消费`、`流水线仍在运行中` 等）。

### 流水线失败

查看 `GET /api/histories/{id}` 的 `error_message` 与 `/logs`：

| 日志/错误 | 可能原因 |
| --- | --- |
| `cert.apply 输入不合法` / `missing field` | 步骤输入字段缺失或拼写错误 |
| `未知的凭据类型` 或取不到凭据 | 引用的凭据 id 不存在，或该类型尚未注册 |
| `完成 DNS-01 挑战` 失败 | DNS 凭据/权限错误、zone 推导错误、TXT 记录未传播 |
| `等待订单就绪超时（120 秒）` | CA 验证迟迟未通过，检查 DNS 记录与 CA 端点 |
| `CA 拒绝了本次验证（订单状态 invalid）` | 挑战验证失败（记录值错、被旧记录干扰等） |
| `缺少所需产物` | 步骤顺序错误（如把 `cert.store` 放在 `cert.apply` 之前） |
| `部署 ... 失败` / `重载` 失败 | 路径权限、目标不可达、重载命令非 0 退出 |
| `凭据无法解密` | `ACMECAST_CREDENTIAL_KEY` 被更换或配置错误 |

### DNS-01 一直超时

- `wait_propagation=true` 但使用内网 DNS：公共解析器查不到记录，改为 `false`；
- `dns_zone` 推导错误（`example.co.uk` 这类公共后缀）：显式指定 `dns_zone`；
- DNS 凭据权限不足：确认 Token/AccessKey 对目标域名有编辑权限。

### 部署后服务仍未使用新证书

- 检查部署步骤日志中的 `skipped_write`：指纹未变时会跳过写入，但**重载仍会执行**；
- 确认 `reload_command` 正确且退出码为 0（重载失败会使步骤失败，但文件已写入）；
- 若确实需要强制重写，设置 `"force": true`。

## 12.4 当前实现状态与已知限制

> 本节描述 **dev 分支当前代码**的实际能力，与 README 的部分描述可能不一致，
> 请以此为准。

### 最重要：当前无法通过 API 创建 SSH 凭据

- 服务启动时注册了 **`acme.account`** 与 DNS 提供商（`cloudflare`、`aliyun`）
  三类凭据（见 `handlers::default_credential_registry()` 与
  `crates/acmecast-server/src/credentials.rs`）；
- SSH 目标的凭据字段 Schema **已在代码中定义**，但没有对应的 `CredentialType`
  注册，因此 `/api/credentials` 会以 `unknown_credential_type` 拒绝创建；
- 后果：`cert.deploy` 的 SSH 认证凭据在当前构建下**无法配置**，相关步骤会在运行时失败。

**影响**：DNS-01 端到端已经可用（创建 DNS 提供商凭据 → `cert.apply` 填
`dns_provider` / `dns_credential_id` → `cert.store` → 本地部署）；受影响的只剩
SSH 远程部署。要支持 SSH，需要在装配处注册对应凭据类型
（参考 `crates/acmecast-server/src/credentials.rs` 中 DNS 凭据类型的做法）。

### 手动触发

`POST /api/pipelines/{id}/run` 可立即触发一次运行：异步执行，返回 `history_id`
供轮询。前端控制台在**流水线列表页**每行提供「立即运行」按钮，触发成功后直接跳转到
该次运行的详情页。详见 [06-pipelines-and-tasks.md](06-pipelines-and-tasks.md) 6.7 与
[10-rest-api.md](10-rest-api.md) 10.3。

### 其他限制

| 限制 | 说明 |
| --- | --- |
| SSH 不校验主机指纹 | 未接入 `known_hosts`，接受任何主机密钥，仅 warn（中间人风险） |
| HTTP-01 无投放通道 | 仅有材料计算能力；申请步骤请用 DNS-01 |
| 登录限流是进程内的 | 多副本部署需在反向代理层另行防护 |
| cron 按 UTC 计算 | 前端预览按东八区显示，二者可能相差 8 小时（见 11.4） |
| 一条流水线一份调度 | 同一 `pipeline_id` 的调度是覆盖式 upsert |
| 触发审计无查询接口 | 审计记录落库，但需直接查数据库 |
| 删除证书不删文件 | `DELETE /api/certificates/{id}` 仅删记录 |
| 手动登记不校验文件 | 只登记相对路径，不检查文件是否存在 |
| YAML 配置文件未接入 | `acmecast-core` 的 `AppConfig` 未被服务端二进制使用，配置以环境变量为准 |
| 凭据引用冲突响应体不含明细 | `credential_in_use` 消息给出引用数量，但响应体不含引用它的流水线清单 |
| 域名集合唯一性由应用层保证 | 数据库未对域名集合加唯一约束，并发签发同一域名集合可能产生重复记录 |

### 文档与代码的差异提示

- README 的数据目录示意图列出 `certs/` 与 `keys/` 两个同级目录；实际实现为
  `certs/<fingerprint>/cert.pem` 与 `certs/<fingerprint>/key.pem`（**没有** `keys/` 目录）；
- README 提到「前端页面：仪表盘、证书、流水线…」均已实现，但凭据类型下拉目前
  只有 `acme.account`。

## 12.5 安全加固清单

- [ ] 生产使用 Let's Encrypt **生产**端点（`letsencrypt`），调试才用 staging；
- [ ] `ACMECAST_INSECURE_SKIP_VERIFY` 保持默认（关闭），仅自建测试 CA 场景开启；
- [ ] 服务监听绑定到内网/回环，由反向代理终止 TLS；
- [ ] 代理层增加登录限流与访问控制；
- [ ] 数据目录权限 `0700`、证书私钥 `0600`，属主为运行用户；
- [ ] 两把密钥与数据目录分开保管，纳入密钥管理/密管流程；
- [ ] 定期备份数据目录，并验证恢复流程（含凭据解密）；
- [ ] SSH 目标所在网络受控（因当前不校验主机指纹）；
- [ ] 关注证书到期情况，确认 `renewal_domains` 已覆盖所有需要自动续期的域名。
