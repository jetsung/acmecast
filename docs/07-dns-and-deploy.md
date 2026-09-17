# 7. DNS 提供商与部署目标

## 7.1 DNS 提供商（DNS-01 挑战）

`cert.apply` 使用 `dns-01` 时，需要指定提供商标识与其凭据。系统内置两家：

| 提供商标识 | 展示名 | 凭据字段 | 说明 |
| --- | --- | --- | --- |
| `cloudflare` | Cloudflare | `api_token` | 使用 API Token（非 Global API Key），需对目标域名具备 DNS 编辑权限 |
| `aliyun` | 阿里云 DNS | `access_key_id`、`access_key_secret` | 走阿里云 DNS 的 RPC 风格 API（HMAC-SHA1 签名） |

### 挑战记录的处理

对每个需要验证的域名，系统会：

1. 解析出目标 zone（`dns_zone` 或由主域名去掉第一段推导）；
2. 计算挑战材料，得到 TXT 记录名（`_acme-challenge.<裸域名>`，通配符去掉 `*.`）
   与记录值；
3. 查询该记录名下的现有 TXT 记录，避免同名堆叠导致 CA 读到旧值；
4. 创建 TXT 记录（TTL 300）；
5. 若 `wait_propagation=true`，用公共 DoH 解析器轮询直到记录可见（最长 2 分钟）；
6. 通知 CA 验证；
7. **无论成功失败，退出时删除该 TXT 记录**（删除是幂等的）。

> 使用内网 DNS（公共解析器查不到记录）时，把 `wait_propagation` 设为 `false`，
> 否则会一直等到超时。

### 缺少内置提供商时

DNS 提供商注册表由 `default_dns_registry()` 装配，当前只注册上述两家。
新增提供商需要实现 `DnsProvider` trait（`type_id`、`display_name`、
`credential_fields` Schema、`find_txt` / `create_txt` / `delete_txt`）并注册进去。

### DNS 凭据的创建

DNS 提供商的凭据类型（`cloudflare` / `aliyun`）已注册，可直接用 `/api/credentials`
创建（字段见上表），流水线编辑器里的 `dns_credential_id` 也会以下拉形式列出它们。
详见 [05-credentials.md](05-credentials.md) 5.3。

## 7.2 部署目标（`cert.deploy` 的 `target`）

内置两种目标，由 `default_deploy_registry()` 注册：

| 标识 | 展示名 | 用途 |
| --- | --- | --- |
| `local` | 本地文件系统 | 写到服务所在主机的路径，可设权限/属主/重载命令 |
| `ssh` | SSH 远程主机 | 通过 SSH 写到远程主机，支持私钥与口令认证 |

### `local` 目标输入

| 字段 | 类型 | 必填 | 默认 | 说明 |
| --- | --- | --- | --- | --- |
| `cert_path` | string | 是 | — | 证书链写入路径 |
| `key_path` | string | 是 | — | 私钥写入路径 |
| `cert_mode` | string | 否 | `"0644"` | 证书文件权限（八进制字符串） |
| `key_mode` | string | 否 | `"0600"` | 私钥文件权限（默认比证书严） |
| `uid` | integer | 否 | — | 属主 uid；不填则保持写入者所有 |
| `gid` | integer | 否 | — | 属组 gid；不填则保持写入者所属 |
| `reload_command` | string | 否 | — | 写入成功后执行的重载命令，如 `nginx -s reload` |

要点：

- 会自动创建父目录；
- 权限在**创建文件时**设定（而不是落盘后再 chmod），避免私钥出现一段
  `umask` 默认权限的可读窗口；
- 属主设在临时文件上，目标文件一出现就是正确的属主；
- 重载命令走 shell（Unix `sh -c`，Windows `cmd /C`），stdout 与 stderr 合并返回，
  非 0 退出码视为部署失败（此时文件已写好）。

示例：

```json
{
  "target": "local",
  "config": {
    "cert_path": "/etc/nginx/ssl/example.com.crt",
    "key_path": "/etc/nginx/ssl/example.com.key",
    "cert_mode": "0644",
    "key_mode": "0600",
    "uid": 0,
    "gid": 0,
    "reload_command": "nginx -t && nginx -s reload"
  },
  "force": false
}
```

> 设置 `uid` / `gid` 改成他人所有通常需要以 root 运行服务。

### `ssh` 目标输入

| 字段 | 类型 | 必填 | 默认 | 说明 |
| --- | --- | --- | --- | --- |
| `host` | string | 是 | — | 目标主机 |
| `port` | integer | 否 | `22` | SSH 端口 |
| `user` | string | 是 | — | 登录用户 |
| `auth` | object | 是 | — | 认证方式（见下） |
| `cert_path` | string | 是 | — | 远程证书路径 |
| `key_path` | string | 是 | — | 远程私钥路径 |
| `cert_mode` | string | 否 | `"0644"` | 远程证书权限 |
| `key_mode` | string | 否 | `"0600"` | 远程私钥权限 |
| `reload_command` | string | 否 | — | 写入成功后在**远程**执行的重载命令 |

认证方式 `auth` 是带 `kind` 判别字段的对象，**只携带凭据 id，不携带明文**：

```json
{ "kind": "private_key", "credential_id": 4 }
```

```json
{ "kind": "password", "credential_id": 5 }
```

- `private_key`：从凭据的 `private_key` 字段取 OpenSSH/PEM 私钥；
- `password`：从凭据的 `password` 字段取口令。

远程写入同样采用「临时文件 + 原子替换」，并通过 stdin 灌入 `cat`，避免把整份证书
放进命令行参数（既受长度限制，也会出现在远端 `ps` 输出里）。

> **限制**：SSH 目标当前**不校验主机指纹**（未接入 `known_hosts`），接受任何主机密钥，
> 仅记录 warn。生产请确保目标主机与网络可信。
>
> **限制**：SSH 凭据类型未注册，当前无法通过 API 创建（见
> [05-credentials.md](05-credentials.md) 5.4）。
