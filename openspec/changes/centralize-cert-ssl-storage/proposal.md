# Proposal

## Why

当前有效证书分散在 `data/certs/<指纹>/` 的指纹子目录中，同一域名多次申请会产生多个指纹目录并存，运维难以确定哪一份为当前生效版本。需要将有效证书集中到与 `revoked` 同级的扁平活跃目录 `certs/ssl/`，扁平存放且仅保留最新一份；续期/重签时将旧版本按其指纹归档到 `certs/archived/<指纹>/`（与 `revoked` 同级、同为指纹隔离），从根源上防止同域名多证书并存被误用。

## What Changes

- **活跃目录扁平化**：有效证书统一落在 `data/certs/ssl/<主域名>.cert.pem` 与 `data/certs/ssl/<主域名>.key.pem`，不再按指纹分子目录；`ssl` 与 `revoked`、`archived` 在 `certs/` 下同级。文件名规则保持不变（`<主域名>.cert.pem` / `<主域名>.key.pem`，通配符 `*` → `_`），多域名证书以申请时的第一个域名为主域名命名。
- **同域名仅保留最新**：`certs/ssl/` 下同主域名的证书仅保留最新一份。当对同一主域名（或同一域名集合）再次申请并签发成功时，系统 MUST 先将 `certs/ssl/` 下已存在的旧证书链与私钥按**旧证书指纹**移动到 `certs/archived/<旧指纹>/` 下（文件名不变，仅换目录），再将新证书写入 `certs/ssl/`。数据库 `cert_pem_path` / `key_pem_path` 指向新活跃路径，旧证书的归档路径可追溯。
- **归档目录 `certs/archived/<指纹>/`**：与 `certs/revoked/<指纹>/` 完全对称——按指纹分子目录、文件名不变、整目录原子操作（如需）、权限 `0700`/`0600`。`archived` 存放因续期/重签被替换的旧有效证书，`revoked` 存放被吊销的证书，语义分离。
- **吊销路径更新**：吊销时源路径由 `certs/<指纹>/` 变更为 `certs/ssl/` 扁平路径，目标仍为 `certs/revoked/<指纹>/`，逐文件 `rename`（扁平目录不可整目录搬）。
- **FileStore 调整**：新增 `ACTIVE_CERT_DIR = "certs/ssl"`（扁平）、`ARCHIVED_CERT_DIR = "certs/archived"`，`cert_paths` 不再拼接指纹（仅 `certs/ssl/<前缀>.*`），新增 `archived_cert_dir(fingerprint)` 与 `archive_replaced_certificate` 等归档方法；`move_certificate` / `restore_certificate` 适配扁平源路径。
- **存量迁移**：一次性将既有 `certs/<指纹>/`（非 `revoked`/`ssl`/`archived`）下的有效证书迁移到 `certs/ssl/` 扁平目录；同主域名出现多指纹冲突时仅保留最新（按 `not_after` 或 `created_at` 最大），其余按指纹移入 `certs/archived/<指纹>/`，并同步更新数据库路径；幂等、可重入。
- **测试与文档同步更新**，权限模型保持不变。

## Capabilities

### New Capabilities

无新增能力域，均为既有能力的行为变更；如需可将 `archived` 归档视为 `certificate-storage` 能力内的新增需求。

### Modified Capabilities

- `certificate-storage` — 证书文件在数据目录中的落盘、续期归档、吊销归档、回退与权限约束：活跃目录由 `certs/<指纹>/` 变更为扁平 `certs/ssl/`，新增 `certs/archived/<指纹>/` 的续期归档语义，吊销源路径同步变更。

## Non-Goals

- 不改变证书文件名规则（`<主域名>.cert.pem` / `<主域名>.key.pem`、通配符 `*` → `_`）。
- 不改变吊销目录 `certs/revoked/<指纹>/` 的布局与权限模型。
- 不引入独立的版本历史保留策略（旧版本仅在 `archived/<指纹>/` 保留一份，不做无限历史堆叠的版本管理）。
- 不改变 `archived` 与 `revoked` 证书的下载/详情可用性之外的语义（归档后仅供审计追溯，不参与有效证书的部署与校验）。
