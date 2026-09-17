# 1. 架构与核心概念

## 1.1 一句话定位

acmecast 把「证书从申请到落地」这件事拆成一条**可编排的流水线**：
向 ACME CA 申请 → 完成域名挑战 → 证书与私钥落盘入库 → 部署到目标主机/目录，
并由**调度器**在 cron 到点或证书临近到期时自动重跑。

## 1.2 进程边界

只有一个可执行文件 `acmecast-server`，它承担三件事：

1. **HTTP API 服务**：REST 管理接口 + OpenAPI/Swagger UI + 可选的前端静态托管；
2. **调度器**：进程内常驻后台任务，处理 cron 触发与到期扫描触发；
3. **流水线执行器**：被调度触发时按顺序执行流水线步骤。

持久化状态集中在**数据目录**（SQLite + 证书文件），因此备份数据目录即可整体迁移。

## 1.3 代码结构（crate 划分）

本仓库是 Cargo workspace，所有 crate 均为内部实现（`publish = false`）：

| crate | 职责 |
| --- | --- |
| `acmecast-core` | 错误类型、配置装载、凭据加密原语（AES-256-GCM） |
| `acmecast-store` | 数据模型、仓储、数据库迁移、数据目录文件存储 |
| `acmecast-access` | 凭据类型注册表、凭据加解密与在流水线中的取用 |
| `acmecast-acme` | ACME 客户端：账号、订单、挑战、签发、吊销、Directory URL 解析 |
| `acmecast-dns` | DNS 提供商抽象与内置实现（Cloudflare、阿里云）、DNS-01 挑战辅助 |
| `acmecast-cert` | PEM/DER 编解码、X.509 解析、格式转换（PFX/JKS/P7B）、到期判定 |
| `acmecast-deploy` | 部署目标抽象与内置实现（本地文件系统、SSH 远程）、幂等与部署记录 |
| `acmecast-pipeline` | 流水线执行器、步骤契约、产物集合、运行历史、步骤注册表 |
| `acmecast-scheduler` | cron 触发与到期扫描触发的判定引擎（不亲自执行流水线） |
| `acmecast-server` | HTTP 装配、鉴权、handler、内置步骤装配、进程入口 |

依赖方向自下而上：`core` < `store` < `access/dns/deploy/cert/pipeline` < `scheduler` < `server`。
领域 crate 之间彼此独立，由 `acmecast-server` 在启动时组装。

## 1.4 核心概念

### 证书（Certificate）

一份证书记录包含：覆盖的域名集合、证书链 PEM 与私钥 PEM 的**相对路径**、
SHA-256 指纹、签发者、生效/到期时间、签发所用的 ACME 账号凭据标识、吊销时间。
证书与私钥本身存放在数据目录中，数据库只记相对路径。

- **去重维度是「域名集合」**：同一组域名重复签发时是**更新**同一条记录，而不是新增。
  这使补发/续期不会让列表膨胀。
- 证书文件按内容寻址：`certs/<fingerprint>/cert.pem` 与 `certs/<fingerprint>/key.pem`。

### 凭据（Credential）

需要在调用外部服务时使用的秘密（ACME 账号、DNS API Token、SSH 私钥/口令）。
凭据**以密文存储在数据库中**，解密密钥只存在于环境变量；接口永不返回字段明文。

### 流水线（Pipeline）与步骤（Step）

一条流水线是一串**有序、可启停**的步骤。每个步骤由 `type_id` 引用一种任务实现，
并携带一份 JSON 输入。内置三种任务：

| type_id | 作用 | 是否必需产物（上游） |
| --- | --- | --- |
| `cert.apply` | 向 ACME CA 申请证书 | 无 |
| `cert.store` | 把证书写入数据目录并入库 | `cert_pem` `key_pem` `domains` `fingerprint` |
| `cert.deploy` | 部署到指定目标 | `cert_pem` `key_pem` `fingerprint` |

步骤之间通过**产物（Artifact）**传递数据，而不是读磁盘重算。同名产物会被后序步骤
覆盖并记录一条 warn 日志——这是多域名流水线的正常语义。

### 运行历史（History）

每次流水线运行产生一条历史记录（`running` / `success` / `failed`），并保存各步骤的
日志。失败停止在当前步骤，后续步骤不会执行。

### 调度（Schedule）

一条流水线最多挂**一份**调度，包含两个独立的触发来源：

- **cron 表达式**：到点触发；
- **续期域名集合**：周期扫描证书仓库，剩余有效期低于阈值时触发负责该域名的流水线。

### 触发来源（trigger_source）

运行历史与触发审计里记录三种来源：`manual`（`POST /api/pipelines/{id}/run` 手动
触发）、`cron`、`renewal`。

## 1.5 一次签发流水线的生命周期

```
调度器判定到点
      │
      ▼
创建运行历史（running）
      │
      ▼
cert.apply ── 取 ACME 账号凭据 ─→ 复用/注册账号 ─→ 下单
      │            └─ DNS-01：取 DNS 凭据 → 写 TXT → 等传播 → 通知 CA → 清理 TXT
      │            └─ 生成密钥对与 CSR → finalize → 下载证书链
      │  产物: cert_pem / key_pem / domains / fingerprint
      ▼
cert.store ── 写 certs/<fp>/cert.pem、key.pem ─→ 按域名集合入库/更新
      │  产物: cert_id / fingerprint
      ▼
cert.deploy ── 指纹一致则跳过写入（但仍执行重载） ─→ 原子替换文件 → 重载
      │  产物: deployed_paths / skipped_write
      ▼
写回运行历史（success / failed + 各步骤日志）
```

## 1.6 与外部系统的交互

- **ACME CA**：Let's Encrypt、ZeroSSL、Google Trust Services、SSL.com 或自定义
  Directory URL（内网 CA、pebble 等）。
- **DNS 服务商 API**：Cloudflare、阿里云 DNS（用于 DNS-01 挑战记录的增删查）。
- **部署目标**：本地文件系统、SSH 远程主机。
- **DoH 解析器**：DNS-01 传播等待时用公共解析器确认 TXT 记录已可见。
