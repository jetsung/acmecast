# acmecast 使用文档

acmecast 是一个**证书自动签发与部署系统**：以 ACME 协议向 Let's Encrypt（或自建 CA）
申请证书，按流水线自动完成 DNS 挑战、证书入库与部署到本地/远程目标，并通过 cron
或到期扫描自动续期。对外提供带鉴权的 REST API 与 OpenAPI 文档。

本文档面向**使用者与运维人员**，按「概念 → 上手 → 配置 → 功能 → 参考」组织。

## 目录

| 文档 | 内容 |
| --- | --- |
| [01-architecture.md](01-architecture.md) | 系统架构、crate 划分与核心概念 |
| [02-getting-started.md](02-getting-started.md) | Docker / 源码部署（Nix + direnv 开发环境），首个可用的签发流水线 |
| [03-configuration.md](03-configuration.md) | 环境变量、数据目录、数据库与日志 |
| [04-authentication-security.md](04-authentication-security.md) | 鉴权、令牌、密钥管理与安全注意事项 |
| [05-credentials.md](05-credentials.md) | 凭据管理与各类型字段定义 |
| [06-pipelines-and-tasks.md](06-pipelines-and-tasks.md) | 流水线模型与任务（步骤）输入/输出参考 |
| [07-dns-and-deploy.md](07-dns-and-deploy.md) | DNS 提供商（DNS-01）与部署目标（local / ssh） |
| [08-scheduling-renewal.md](08-scheduling-renewal.md) | cron 调度、到期扫描与自动续期 |
| [09-certificates.md](09-certificates.md) | 证书列表、下载格式、吊销与手动登记 |
| [10-rest-api.md](10-rest-api.md) | REST API 完整参考与错误码 |
| [11-frontend.md](11-frontend.md) | 前端控制台的使用与开发 |
| [12-operations-and-limitations.md](12-operations-and-limitations.md) | 备份迁移、排障、已知限制与当前实现状态 |

## 一分钟速览

```bash
# 1. 生成两把必需密钥（务必妥善保存：更换加密密钥会使已存凭据无法解密）
export ACMECAST_JWT_SECRET="$(openssl rand -base64 32)"
export ACMECAST_CREDENTIAL_KEY="$(openssl rand -base64 32)"

# 2. 生成管理员口令哈希
export ACMECAST_ADMIN_PASSWORD_HASH="$(docker run --rm acmecast:dev hash-password '你的口令')"

# 3. 启动
mkdir -p ./data && chmod 0777 ./data
docker run -d --name acmecast -p 8080:8080 -v "$(pwd)/data:/data" \
  -e ACMECAST_JWT_SECRET -e ACMECAST_CREDENTIAL_KEY -e ACMECAST_ADMIN_PASSWORD_HASH \
  acmecast:dev

# 4. 验证
curl http://127.0.0.1:8080/healthz          # -> ok
curl -sS -X POST http://127.0.0.1:8080/api/login \
  -H 'Content-Type: application/json' \
  -d '{"username":"admin","password":"你的口令"}'   # -> {"data":{"token":"..."}}
```

交互式 API 文档在 `http://127.0.0.1:8080/swagger-ui`（JSON：`/api/openapi.json`）。

> **重要**：请先阅读 [12-operations-and-limitations.md](12-operations-and-limitations.md)
> 的「当前实现状态」。当前 **dev** 分支已注册 `acme.account` 与 DNS 提供商
> （`cloudflare` / `aliyun`）凭据类型；**SSH 凭据仍无法通过 API 创建**，
> 这会影响你如何配置第一套流程。
> 触发流水线可用 `POST /api/pipelines/{id}/run` 手动发起，或配置调度自动触发。
