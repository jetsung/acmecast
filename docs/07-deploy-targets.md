# 部署目标

`cert.deploy` 步骤通过 `target` + `config` 选择部署目标。所有目标共享以下
语义：

- **原子写入**：先写临时文件再替换，任何时刻目标路径要么是旧证书要么是新
  证书，不存在写一半的中间态。
- **幂等**：目标键（输入的稳定 SHA-256 摘要）+ 证书指纹一致且未 `force` →
  跳过文件写入，但**重载命令仍执行**。部署记录只在成功后写入（失败的部署
  不算「已部署」，否则重试会被误判跳过）。
- **重载失败**：非 0 退出码视为失败（错误携带命令、退出码与合并输出），
  此时文件已写好。
- **文件类目标默认 3 文件**：`local` / `ssh` / `s3` / `webdav` 默认产出
  `<prefix>.fullchain.cer`（完整链原样）、`<prefix>.ca.cer`（中间链，链仅
  单张时为空文件）与 `<prefix>.key`（私钥），与主存储 `certs/ssl/` 的三文件
  布局一致；不再单独产出叶子 `<prefix>.cer`（需叶子时从 fullchain 拆首块）。
  存量字段 `cert_path` / `cert_filename` 视为 fullchain 的别名继续生效。
- **腾讯云 `tencent-ssl` 仅需 2 材料**：云端 API 上传 `CertificatePublicKey`
  （= fullchain 链 PEM）与 `CertificatePrivateKey`（= 私钥），不产生也不要求
  `ca.cer` 文件——与文件类目标的 3 文件语义显式区分。

## `local` — 本地文件系统

| 字段 | 必需 | 默认 | 说明 |
|---|---|---|---|
| `fullchain_path` | 是* | — | 完整链 PEM 目标路径（*存量 `cert_path` 视为其别名，二者需给其一） |
| `ca_path` | 否 | 按 `fullchain_path` 派生 | 中间链目标路径；未配置时同目录派生 `<stem>.ca.cer`，**显式留空**则跳过 `ca.cer` 产出 |
| `key_path` | 是 | — | 私钥 PEM 目标路径 |
| `cert_mode` | 否 | `0644` | 证书/中间链权限（八进制字符串） |
| `key_mode` | 否 | `0600` | 私钥权限（比证书严） |
| `uid` / `gid` | 否 | — | 文件属主；不填保持写入者所有（改他人所有需 root） |
| `reload_command` | 否 | — | 写完后执行的重载命令，走 shell（`sh -c`） |

细节：

- 权限在**创建时**用 `OpenOptions::mode` 设定，不留「先落盘后 chmod」的
  umask 窗口；写后再 `set_mode` 一次覆盖历史改动。
- 临时文件与目标同目录（保证 `rename` 原子性），失败时自动清理。
- 非 Unix 平台不支持设置属主（报 `uid` 字段错）。

## `ssh` — SSH 远程主机

连接方式两种可混用：**直接填**（认证材料经 `credential_id` 引用 `deploy.ssh` 私钥/口令

合并优先级：**输入显式值 > 主机档案值 > 系统缺省**（端口 22、证书 0644、
私钥 0600）。空白输入值不算显式，回退档案。合并后必填缺失报错并指出缺哪个
字段，绝不带半个配置去连主机。

| 字段 | 来源 | 说明 |
|---|---|---|
| `credential_id` | 输入 | 引用 `deploy.ssh` 类型主机档案 |
| `host` / `port` / `user` | 输入或档案 | 连接参数 |
| `auth` | 输入或档案 | `{"kind": "private_key", "credential_id": N}` 或 `{"kind": "password", "credential_id": N}` |
| `fullchain_path` / `key_path` | **仅输入** | 远端完整链与私钥路径（档案不含路径——一份档案可服务多条流水线；存量 `cert_path` 视为 `fullchain_path` 别名） |
| `ca_path` | **仅输入** | 远端中间链路径；未配置时按 fullchain 同目录派生 `<stem>.ca.cer`，显式留空则跳过写入 |
| `cert_mode` / `key_mode` | 输入或档案 | 文件权限（`cert_mode` 同时作用于 fullchain 与 ca） |
| `reload_command` | **仅输入** | 远端重载命令 |

细节：

- 远端写入与本地同构：临时文件 + `mv` 原子替换；证书内容经 **stdin 灌进
  `cat`**（不进命令行参数，避免出现在进程列表）；脚本 `set -e` +
  `trap ... EXIT` 保证失败清理。
- 私钥支持 OpenSSH 与 PEM 格式。
- ⚠️ **主机密钥未接入 known_hosts**：接受任何主机密钥，每次连接记 warn 并
  打出 SHA-256 指纹。有中间人风险的环境请配合网络层隔离使用。
- 凭据的「连通性测试」对 `deploy.ssh` 类型是真实探测（10 秒超时）。

## `tencent-ssl` — 腾讯云 SSL 证书库

把证书链与私钥上传到腾讯云 SSL 证书服务（API 3.0，服务 `ssl`），供 CLB /
CDN / WAF 等云资源引用。**国内站与国际站同构**：凭据的 `account_site` 字段
（`cn` / `intl`）决定请求端点（`ssl.tencentcloudapi.com` /
`ssl.intl.tencentcloudapi.com`），两侧接口的 Action 与参数一致。

| 字段 | 必需 | 默认 | 说明 |
|---|---|---|---|
| `credential_id` | 是 | — | 引用 `cloud.tencent` 类型凭据（`dns.tencent-eo` 仅服务 DNS，无 SSL 部署能力，不接受） |
| `alias` | 否 | — | 上传到云端的证书备注名；空白不提交 |
| `repeatable` | 否 | `false` | 上传请求的 `Repeatable`：`false` 时同指纹已存在返回已有证书 ID、不产生重复；`true` 时允许重复上传 |
| `replace_existing` | 否 | `true` | 云端已有同域名证书时是否替换 |
| `resource_types` | 否 | `[]` | 替换时需同步刷新的云资源类型（`ResourceTypes`），空时仅更新云端证书本身；多选，可选：`clb`、`cdn`、`waf`、`live`、`ddos`、`teo`、`apigateway`、`vod`、`tke`、`tcb`、`tse`、`cos`、`mqtt`、`scf`、`tdmq`、`gaap` |
| `resource_regions` | 否 | `[]` | `resource_types` 指定的云资源的地域列表，构造 `ResourceTypesRegions`（`{ Region, Type }`） |

细节：

- **`CertificateType` 固定为 `SVR`**：服务器证书，不暴露为可配置字段。
- **上传幂等**：`Repeatable=false`（默认）时同指纹重复上传天然幂等、不产生重复证书；`Repeatable=true` 时允许同指纹重复上传（用于需要多张同指纹证书的场景）。

替换语义（`replace_existing` 开启时）：

- **存在性判断**：部署前查证书库，云端服务器证书的主域名（`Domain`）与本次
  证书主域名**精确一致**即视为已存在（单域名匹配单域名、泛域名
  `*.example.com` 匹配同形式泛域名）。泛域名覆盖关系（云端 `*.example.com`
  而本次为 `a.example.com`）**不算**已存在——避免误删服务全部子域名的泛域名
  证书。查询遍历全部分页，证书多于单页上限也不漏判。
- **原证书 ID 不变、只更新内容**：查到存量后逐张调 `UploadUpdateCertificateInstance`（<https://cloud.tencent.com/document/api/400/119791>）原地更新，证书链与私钥刷新为新内容，原证书 ID 保持不变，引用该 ID 的云资源无需改绑。相比旧的「上传新 + 删除旧」，避免证书 ID 变化导致的云资源改绑成本。
- **资源同步可选**：`resource_types` 非空时，更新调用携带 `ResourceTypes`（所选类型列表）与 `ResourceTypesRegions`（`resource_types × resource_regions` 的笛卡尔积，每项 `{ Region, Type }`）。多选 `clb` 时与地域联动可同步刷新绑定的 CLB 集群；默认空时仅更新云端证书本身，已绑定资源不自动刷新。
- **多张匹配逐张更新**：云端存在多张同域名证书时，逐张调更新，单张失败记告警、不阻塞其他证书与整体部署——与原「删除失败不阻塞」语义对齐。
- **关闭替换仅上传**：`replace_existing` 为 `false` 时，仅按上传流程处理，不发起更新调用。

细节：

- **`paths` 记云端证书 ID**：本目标的「部署产物」是证书库里那张证书，部署
  记录的路径清单存证书 ID（`local`/`ssh` 目标存文件路径，语义随目标而异）。无存量时上传新证书、记录新 ID；有存量时更新原证书、记录原 ID（多张存量时记录全部原 ID）。
- **跳过写入时零云端调用**：指纹一致且未 `force` 时不查询、不上传、不更新；
  云端证书被人手动删除时，`force` 重跑一次即恢复。


## `s3` — S3 兼容存储

把证书链与私钥写入 S3 兼容对象存储（MinIO / R2 / OSS / COS 等）。

| 字段 | 必需 | 默认 | 说明 |
| `credential_id` | 是 | — | 引用 `deploy.s3` 类型凭据（含 `access_key`/`secret_key`，可选 `session_token`） |
| `endpoint` | 是 | — | S3 endpoint（含 scheme），如 `https://s3.example.com` |
| `region` | 是 | — | 区域，如 `us-east-1` |
| `bucket` | 是 | — | 桶名 |
| `prefix` | 否 | — | 对象键前缀，如 `certs/` |
| `fullchain_filename` | 否 | — | 完整链对象键模板，含 `{domain}` 占位，未填时为 `<prefix>/<domain>.fullchain.cer`（存量 `cert_filename` 视为其别名） |
| `ca_filename` | 否 | — | 中间链对象键模板，未填时为 `<prefix>/<domain>.ca.cer`（链仅单张时为空对象）；**显式留空**则跳过上传 |
| `key_filename` | 否 | — | 私钥对象键模板，未填时为 `<prefix>/<domain>.key` |
| `force_path_style` | 否 | `false` | 强制 path-style（MinIO 等自建存储） |
| `insecure_skip_verify` | 否 | `false` | 跳过 TLS 证书校验 |

细节：幂等同 `local`/`ssh`（`Deployer` 的 `config` 摘要 + `fingerprint`），失败不写部署记录、不污染主存储；凭据仅存 `credential_id`，日志不打密钥。

## `webdav` — WebDAV

把证书链与私钥 PUT 到 WebDAV 资源。

| 字段 | 必需 | 默认 | 说明 |
|---|---|---|---|
| `credential_id` | 是 | — | 引用 `deploy.webdav` 类型凭据（含 `username`/`password`） |
| `url` | 是 | — | WebDAV 基地址，如 `https://dav.example.com/` |
| `base_path` | 否 | — | 基路径，归一化后拼接 |
| `fullchain_path` | 否 | — | 完整链对象路径模板，含 `{domain}`，未填时派生 `<prefix>.fullchain.cer`（存量 `cert_path` 视为其别名） |
| `ca_path` | 否 | — | 中间链路径模板，未填时派生 `<prefix>.ca.cer`；**显式留空**则跳过上传 |
| `key_path` | 否 | — | 私钥对象路径模板 |
| `insecure_skip_verify` | 否 | `false` | 跳过 TLS 证书校验 |

细节：PUT 覆盖上传，幂等与失败语义同上。

## 部署记录

每次部署（含跳过写入的）成功后落一条记录：目标键、指纹、是否跳过写入、
路径清单、重载输出、时间。用于幂等判断与历史回溯（库表
`acmecast_deployment`，按时间倒序可查）。

## 示例

```json
{
  "target": "ssh",
  "config": {
    "credential_id": 5,
    "fullchain_path": "/etc/nginx/ssl/example.com.fullchain.cer",
    "key_path": "/etc/nginx/ssl/example.com.key",
    "reload_command": "systemctl reload nginx"
  }
}
```

`credential_id: 5` 是 `deploy.ssh` 类型主机档案；路径与重载命令随部署输入给出。
未配置 `ca_path` 时按 `fullchain_path` 同目录派生 `example.com.ca.cer`。

