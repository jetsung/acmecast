# 6. 流水线与任务参考

## 6.1 流水线模型

一条流水线由**名称、描述、启用开关**和一组**有序步骤**组成。步骤由 `type_id`
引用任务实现，并携带 JSON 输入。流水线是**整体替换**语义：`PUT` 会用请求体
完整覆盖原有步骤。

### 流水线端点

| 方法 | 路径 | 说明 |
| --- | --- | --- |
| GET | `/api/pipelines` | 分页列表；`name` 名称模糊、`enabled` 过滤、`sort`=`asc`/`desc`（按更新时间）、`page`、`page_size` |
| POST | `/api/pipelines` | 新建 |
| GET | `/api/pipelines/{id}` | 详情（含有序步骤） |
| PUT | `/api/pipelines/{id}` | 整体更新 |
| DELETE | `/api/pipelines/{id}` | 删除（连同步骤与调度） |
| GET | `/api/pipelines/{id}/histories` | 该流水线的运行历史分页 |

创建/更新请求体：

```json
{
  "name": "string，必填",
  "description": "string，可选",
  "enabled": true,
  "steps": [
    { "type_id": "cert.apply", "input": { }, "enabled": true }
  ]
}
```

- `enabled` 默认 `true`；`input` 省略时为空对象；
- 步骤顺序由数组顺序决定（服务端按序编号，执行按序进行）；
- 引用未知 `type_id` 不会在保存时报错（当前实现只在**执行时**才由注册表判定）。

### 任务类型与 Schema 端点

| 方法 | 路径 | 说明 |
| --- | --- | --- |
| GET | `/api/tasks` | 全部已注册任务类型与输入 Schema |
| GET | `/api/tasks/{type_id}/schema` | 单个任务的输入 Schema（未声明 Schema 时 404） |

内置任务：`cert.apply`、`cert.store`、`cert.deploy`。

## 6.2 执行语义

- 按步骤顺序依次执行，**某步失败即中止**，后续步骤不执行；
- 停用（`enabled=false`）的步骤被跳过，不算失败；
- 步骤之间通过**产物**传递结果，后序步骤按名称读取前序产出；
- 同名产物会被后序步骤**覆盖**并记一条 warn（多域名流水线的正常语义）；
- 缺少必需产物时失败，错误码 `missing_artifact`，消息会列出当前可用产物；
- 运行结果落到运行历史：`success` 或 `failed`（含失败步骤与原因），各步骤日志可查。

### 内置任务的产物约定

| 任务 | 需要（上游产物） | 产出（供下游） |
| --- | --- | --- |
| `cert.apply` | 无 | `cert_pem`、`key_pem`、`domains`、`fingerprint` |
| `cert.store` | `cert_pem`、`key_pem`、`domains`、`fingerprint` | `cert_id`、`fingerprint` |
| `cert.deploy` | `cert_pem`、`key_pem`、`fingerprint` | `deployed_paths`、`skipped_write` |

因此典型顺序是 `cert.apply` → `cert.store` → `cert.deploy`（可重复出现，用于多域名）。

## 6.3 `cert.apply` —— 向 ACME CA 申请证书

一次执行完成「建账号（或复用）→ 下单 → 完成挑战 → 提交 CSR → 拿到证书链」。

| 字段 | 类型 | 必填 | 默认 | 说明 |
| --- | --- | --- | --- | --- |
| `domains` | string[] | 是 | — | 要申请的域名；通配符 `*.example.com` 会被强制走 DNS-01 |
| `challenge` | string | 是 | — | `dns-01` 或 `http-01`。**当前只有 DNS-01 可用**（HTTP-01 仅能计算材料，无投放通道） |
| `account_credential_id` | integer | 是 | — | `acme.account` 类型凭据的 id |
| `dns_provider` | string | DNS-01 必填 | — | 提供商标识，如 `cloudflare`、`aliyun` |
| `dns_credential_id` | integer | DNS-01 必填 | — | DNS 凭据 id |
| `dns_zone` | string | 否 | 由主域名去掉第一段推导（`a.example.com` → `example.com`） | 目标 DNS zone。公共后缀域名建议显式指定 |
| `wait_propagation` | boolean | 否 | `true` | 写 TXT 后等待其在公共解析器可见再让 CA 验证。内网 DNS 环境请设为 `false` |
| `contacts` | string[] | 否 | `[]` | 账号联系人（`mailto:` 开头）；**仅首次注册账号时使用** |
| `insecure_skip_verify` | boolean | 否 | `false` | 跳过 CA 的 TLS 校验，仅用于自签测试 CA（pebble）。生产保持 `false` |

行为要点：

- 账号凭据**首次使用时注册**，签发成功后把账号凭据写回该凭据记录，之后复用；
- DNS-01 流程：读取挑战材料 → 写入 `_acme-challenge.<domain>` TXT 记录 →
  （可选）等传播 → 通知 CA 验证 → **无论成败都清理该 TXT 记录**；
- 挑战记录 TTL 为 300 秒，传播等待最长 2 分钟、每 5 秒轮询一次；
- 生成密钥对与 CSR 在本次运行内完成，**私钥不落盘、不进日志**，只作为产物传给下游；
- 订单就绪最长等待 120 秒，超时或订单 `invalid` 即失败。

## 6.4 `cert.store` —— 证书入库

把申请到的证书写入数据目录（`certs/<fingerprint>/cert.pem` 与 `key.pem`）并入库。

| 字段 | 类型 | 必填 | 说明 |
| --- | --- | --- | --- |
| `acme_account_credential_id` | integer | 否 | 签发该证书的 ACME 账号凭据 id。**记录它之后吊销才知道用哪个账号**；手动登记场景可不填 |

- 按**域名集合去重**：同域名集合再次入库是**更新**（主键不变），日志会区分
  「新证书已入库 #id」与「同域名集合的证书已更新 #id」；
- 生效时间、到期时间、签发者由服务端解析证书得到，不取输入。

## 6.5 `cert.deploy` —— 部署证书

| 字段 | 类型 | 必填 | 说明 |
| --- | --- | --- | --- |
| `target` | string | 是 | 部署目标类型：`local`、`ssh` |
| `config` | object | 是 | 目标输入，结构与所选目标的 Schema 一致（见 [07-dns-and-deploy.md](07-dns-and-deploy.md)） |
| `force` | boolean | 否 | 强制重写，跳过「指纹一致即跳过写入」的判断（默认 `false`） |

行为要点：

- **幂等**：目标上已是同一指纹时跳过写入，但**重载命令仍会执行**
  （否则会出现「证书换了、服务没重新加载」且一直持续到下次部署）；
- 写入采用「先写临时文件 → 原子替换」，避免运行中的服务读到半截文件；
- 重载命令以 shell 执行（Unix `sh -c`，Windows `cmd /C`），非 0 退出码视为失败。

## 6.6 配置示例

### 单域名：申请 → 入库 → 本地部署并重载

```json
{
  "name": "example.com",
  "enabled": true,
  "steps": [
    { "type_id": "cert.apply", "input": {
        "domains": ["example.com", "www.example.com"],
        "challenge": "dns-01",
        "account_credential_id": 1,
        "dns_provider": "cloudflare",
        "dns_credential_id": 2,
        "wait_propagation": true,
        "contacts": ["mailto:ops@example.com"]
    }},
    { "type_id": "cert.store", "input": { "acme_account_credential_id": 1 } },
    { "type_id": "cert.deploy", "input": {
        "target": "local",
        "config": {
          "cert_path": "/etc/nginx/ssl/example.com.crt",
          "key_path": "/etc/nginx/ssl/example.com.key",
          "reload_command": "nginx -s reload"
        }
    }}
  ]
}
```

### 多域名 + 远程部署（同一份证书部署到两台主机）

```json
{
  "name": "multi-domain",
  "steps": [
    { "type_id": "cert.apply", "input": {
        "domains": ["api.example.com", "app.example.com"],
        "challenge": "dns-01",
        "account_credential_id": 1,
        "dns_provider": "aliyun",
        "dns_credential_id": 3
    }},
    { "type_id": "cert.store", "input": { "acme_account_credential_id": 1 } },
    { "type_id": "cert.deploy", "input": {
        "target": "ssh",
        "config": {
          "host": "10.0.0.11", "user": "deploy",
          "auth": { "kind": "private_key", "credential_id": 4 },
          "cert_path": "/etc/ssl/api.crt", "key_path": "/etc/ssl/api.key",
          "reload_command": "systemctl reload nginx"
        }
    }},
    { "type_id": "cert.deploy", "input": {
        "target": "ssh",
        "config": {
          "host": "10.0.0.12", "user": "deploy",
          "auth": { "kind": "password", "credential_id": 5 },
          "cert_path": "/etc/ssl/api.crt", "key_path": "/etc/ssl/api.key"
        },
        "force": true
    }}
  ]
}
```

> 注意：当前构建中 `dns_provider` / SSH 所需凭据类型未注册，上面的示例在
> 「创建凭据」这一步会失败。完整的可用性说明见
> [12-operations-and-limitations.md](12-operations-and-limitations.md)。

## 6.7 触发流水线

有两种触发方式：

### 手动触发（`POST /api/pipelines/{id}/run`）

```bash
curl -sS -X POST -H "Authorization: Bearer $TOKEN" \
  http://127.0.0.1:8080/api/pipelines/1/run
# -> {"data":{"history_id":12,"status":"running"}}
```

- **异步执行**：接口先落一条 `running` 的运行历史再立即返回，一次签发可能耗时
  几分钟（挑战传播、CA 验证），同步等待只会把请求挂在超时边缘；
- 用返回的 `history_id` 轮询结果：
  `GET /api/histories/{history_id}`（终态）与
  `GET /api/histories/{history_id}/logs`（步骤日志）；
- 运行来源记为 `manual`，与调度触发（`cron`/`renewal`）在历史里可区分；
- 前端等价入口：**流水线列表页**每行的「立即运行」按钮，成功后自动跳转到该次运行的详情页。

拒绝的情况：

| 情况 | HTTP | 错误码 |
| --- | --- | --- |
| 流水线不存在 | 404 | `not_found` |
| 流水线已停用 | 400 | `validation_error`（`field = enabled`） |
| 该流水线正在运行中 | 409 | `conflict` |
| 未配置凭据加密密钥 | 500 | `configuration_error` |

### 调度触发

由调度器按 cron 或续期扫描自动触发：

- 配置了 `cron`：到点触发；
- 配置了 `renewal_domains`：证书进入续期窗口时触发。

没有调度配置且不手动触发的流水线不会有任何运行。详见
[08-scheduling-renewal.md](08-scheduling-renewal.md)。
