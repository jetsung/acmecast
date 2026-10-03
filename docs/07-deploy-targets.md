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

## `local` — 本地文件系统

| 字段 | 必需 | 默认 | 说明 |
|---|---|---|---|
| `cert_path` | 是 | — | 证书链 PEM 目标路径 |
| `key_path` | 是 | — | 私钥 PEM 目标路径 |
| `cert_mode` | 否 | `0644` | 证书权限（八进制字符串） |
| `key_mode` | 否 | `0600` | 私钥权限（比证书严） |
| `uid` / `gid` | 否 | — | 文件属主；不填保持写入者所有（改他人所有需 root） |
| `reload_command` | 否 | — | 写完后执行的重载命令，走 shell（`sh -c`） |

细节：

- 权限在**创建时**用 `OpenOptions::mode` 设定，不留「先落盘后 chmod」的
  umask 窗口；写后再 `set_mode` 一次覆盖历史改动。
- 临时文件与目标同目录（保证 `rename` 原子性），失败时自动清理。
- 非 Unix 平台不支持设置属主（报 `uid` 字段错）。

## `ssh` — SSH 远程主机

连接方式两种可混用：**直接填**（认证材料经 `credential_id` 引用 SSH 私钥/口令
凭据）或**引用 SSH 主机档案**（顶层 `credential_id` 指向 `ssh` 类型凭据）。

合并优先级：**输入显式值 > 主机档案值 > 系统缺省**（端口 22、证书 0644、
私钥 0600）。空白输入值不算显式，回退档案。合并后必填缺失报错并指出缺哪个
字段，绝不带半个配置去连主机。

| 字段 | 来源 | 说明 |
|---|---|---|
| `credential_id` | 输入 | 引用 `ssh` 类型主机档案 |
| `host` / `port` / `user` | 输入或档案 | 连接参数 |
| `auth` | 输入或档案 | `{"kind": "private_key", "credential_id": N}` 或 `{"kind": "password", "credential_id": N}` |
| `cert_path` / `key_path` | **仅输入** | 远端目标路径（档案不含路径——一份档案可服务多条流水线） |
| `cert_mode` / `key_mode` | 输入或档案 | 文件权限 |
| `reload_command` | **仅输入** | 远端重载命令 |

细节：

- 远端写入与本地同构：临时文件 + `mv` 原子替换；证书内容经 **stdin 灌进
  `cat`**（不进命令行参数，避免出现在进程列表）；脚本 `set -e` +
  `trap ... EXIT` 保证失败清理。
- 私钥支持 OpenSSH 与 PEM 格式。
- ⚠️ **主机密钥未接入 known_hosts**：接受任何主机密钥，每次连接记 warn 并
  打出 SHA-256 指纹。有中间人风险的环境请配合网络层隔离使用。
- 凭据的「连通性测试」对 `ssh` 类型是真实探测（10 秒超时）。

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
    "cert_path": "/etc/nginx/ssl/example.com.crt",
    "key_path": "/etc/nginx/ssl/example.com.key",
    "reload_command": "systemctl reload nginx"
  }
}
```

`credential_id: 5` 是 `ssh` 类型主机档案；路径与重载命令随部署输入给出。
