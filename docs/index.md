# acmecast

证书自动签发与部署系统：以 ACME 协议向 Let's Encrypt（或自建 CA）申请证书，
按流水线自动完成 DNS 挑战、证书入库与部署到本地/远程目标，并通过 cron 或到期
扫描自动续期。提供带鉴权的 REST API 与 OpenAPI 文档，为前端 SPA 与第三方集成
提供统一入口。

## 能力一览

- **ACME 申请**：内置 Let's Encrypt（含 Staging）、ZeroSSL、Google、SSL.com 五家
  CA 端点，支持自定义目录 URL 与 EAB（外部账号绑定）；账号凭据首次使用时注册、
  之后自动复用。
- **DNS-01 挑战**：内置 Cloudflare 与阿里云 DNS 提供商，写入后等待权威 NS
  可见再通知 CA 验证，支持通配符证书。
- **流水线编排**：`cert.apply`（申请）→ `cert.store`（入库）→ `cert.deploy`
  （部署）三步串成流水线，步骤可停用、产物逐段传递。
- **部署目标**：本地文件系统（原子替换 + 权限/属主控制 + 重载命令）与 SSH
  远程主机（临时文件 + 原子 mv + 重载命令）；同指纹重复部署自动跳过写入。
- **自动续期**：cron 表达式定时触发，或按域名集合做到期扫描（默认阈值 30 天），
  同窗口重复触发自动去重，全部触发落审计记录。
- **证书管理**：证书入库去重、到期状态判定（健康/临期/已过期）、下载
  PEM/DER/PFX/P7B/JKS 五种格式、向 CA 吊销并归档。
- **webhook 通知**：证书申请、部署成功后向飞书（Lark）/钉钉等 IM 群机器人
  推送文本消息，支持签名校验与按事件订阅，投递不阻塞流水线。
- **安全**：全部 `/api/` 端点（除登录与文档）强制 Bearer JWT；管理员口令以
  Argon2 PHC 存储；登录失败限流锁定；凭据以 AES-256-GCM 加密落库，密钥只存在
  于环境变量。

## 阅读路径

| 我想…… | 看这里 |
|---|---|
| 尽快跑起来 | [快速开始](02-getting-started.md) / [Docker 部署](13-docker-deployment.md) |
| 了解整体结构 | [架构设计](01-architecture.md) |
| 配置服务 | [配置参考](03-configuration.md) |
| 配 DNS 与账号凭据 | [凭据体系](04-credentials.md) / [DNS 提供商](06-dns-providers.md) |
| 建一条签发流水线 | [流水线](05-pipelines.md) / [部署目标](07-deploy-targets.md) |
| 申请/部署成功后推送 IM | [webhook 通知](03-configuration.md#webhook)（[配置参考](03-configuration.md)） |
| 自动续期 | [定时调度与自动续期](08-scheduling.md) |
| 对接 API | [REST API](09-api.md) |
| 换数据库 / 备份 | [数据库与数据目录](11-database.md) |
| 参与开发 | [本地开发](12-development.md) |
| 出了问题 | [排障与已知限制](15-troubleshooting.md) |

## 许可

Apache-2.0，见仓库根 `LICENSE`。
