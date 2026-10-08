# Design

## Context

当前 `FileStore` 将有效证书写入 `certs/<指纹>/<主域名>.*.pem`，吊销时原子移动到 `certs/revoked/<指纹>/`。用户新需求：有效证书扁平集中到 `certs/ssl/`（不再按指纹分子目录），同主域名仅保留最新一份；续期/重签成功时将旧证书按其指纹移动到 `certs/archived/<旧指纹>/`（与 `revoked` 同级、同为指纹隔离，参考 `revoked` 实现）。需在保持路径防逃逸、权限受限等不变量的前提下完成布局切换、续期归档与存量迁移。详见 `proposal.md`。

## Goals / Non-Goals

**Goals:**

- 将活跃证书路径由 `certs/<指纹>/` 切换为扁平 `certs/ssl/<主域名>.*.pem`，`certs/ssl`、`certs/archived/<指纹>/`、`certs/revoked/<指纹>/` 在 `certs/` 下同级。
- 续期/重签时旧证书按旧指纹归档到 `certs/archived/<旧指纹>/`，参考 `revoked` 的实现（指纹隔离、权限受限、防逃逸），新证书覆盖扁平活跃路径。
- 吊销源路径适配扁平布局，提供幂等的存量迁移（旧指纹目录 → 扁平 + 冲突归档）。

**Non-Goals:**

- 不改变文件名规则（`<主域名>.cert.pem` / `<主域名>.key.pem`，`*` → `_`）。
- 不改变吊销目录 `certs/revoked/<指纹>/` 的布局与权限模型。
- 不引入无限版本历史的版本管理（`archived/<指纹>/` 仅保留被替换的那一份旧证书）。

## Decisions

### 1. 扁平活跃目录 `certs/ssl/`，按主域名定文件名

**决策**：活跃路径为 `certs/ssl/<主域名>.cert.pem` 与 `certs/ssl/<主域名>.key.pem`，常量 `ACTIVE_CERT_DIR = "certs/ssl"`，`cert_paths` 不再拼接指纹，仅拼接 `ACTIVE_CERT_DIR` 与文件前缀。

**理由**：满足“防止同域名多证书并存、仅保留最新”的核心诉求；`ls certs/ssl/` 即可直观看到当前生效的全部证书；与 `revoked`/`archived` 的指纹隔离归档形成“扁平活跃 + 指纹归档”的清晰分层。

**影响**：`FileStore::cert_paths(fingerprint, primary_domain)` 的 `fingerprint` 参数对活跃路径不再影响路径本身，但仍用于推导 `archived/<指纹>/` 的归档目标；可保留参数以兼容调用方，或新增 `active_cert_paths(primary_domain)` 并保留旧方法作兼容。

### 2. 续期归档 `certs/archived/<旧指纹>/`，参考 revoked 实现

**决策**：新增 `ARCHIVED_CERT_DIR = "certs/archived"`，新增方法：

- `archived_cert_dir(fingerprint) -> String`：`format!("{ARCHIVED_CERT_DIR}/{fingerprint}")`，与 `revoked_cert_dir` 对称。
- `archive_replaced_certificate(cert_pem_relative, key_pem_relative, old_fingerprint) -> Result<Option<CertFilePaths>>`：将 `certs/ssl/` 下的旧证书逐文件 `rename` 到 `certs/archived/<旧指纹>/` 下同名文件，目标目录 `ensure_restricted_dir` 逐层收紧，参考 `move_certificate` 的逐文件分支与 `ensure_revoked_dir` 的逐层收紧逻辑。
- `ensure_archived_dir(fingerprint)`：与 `ensure_revoked_dir` 对称，`DirBuilder::recursive` 后逐层 `restrict_existing_dir`。

**关键点**：

- 活跃目录扁平，无法整目录 `rename`，归档只能逐文件；需先 `ensure_archived_dir` 再 `move_within`，与吊销的扁平源路径一致。
- 旧证书的指纹由调用方（证书入库/续期逻辑）传入——从被替换的数据库记录或旧证书 PEM 解析得到；若无法确定指纹则退化为直接覆盖（不归档）并记 warn，避免阻塞新证书写入。
- 归档失败不阻断新证书写入：`archive_replaced_certificate` 返回 `None` 或 `Err` 时记录日志，仍继续 `write_certificate` 覆盖活跃文件。

**调用时机**：证书存储层（`CertRepository` 或 `FileStore::write_certificate` 的上层编排）在写入新证书前检测 `certs/ssl/<前缀>.*` 是否已存在且指纹不同，若是则先归档旧证书。

### 3. 吊销路径适配扁平源

**变更**：`move_certificate` 的源路径由 `certs/<指纹>/...` 变为 `certs/ssl/...` 扁平路径，目标仍为 `certs/revoked/<指纹>/`。扁平源无法整目录搬，`try_move_whole_dir` 对此类源返回 `false`，退化为逐文件 `move_within` + `ensure_revoked_dir`，行为与现有逐文件分支一致。

### 4. 存量迁移策略

**决策**：提供一次性、幂等的迁移例程 `migrate_legacy_certs_to_ssl_flat()`（命名示意）：

- 扫描 `data/certs/` 下一级子目录，跳过 `revoked`、`ssl`、`archived` 三个保留名，其余视为旧指纹目录。
- 对每个旧指纹目录下的证书文件：解析主域名前缀（文件名去后缀），按主域名分组；组内按 `not_after`（或 `created_at`）取最新者作为保留项，其余为待归档项。
- 保留项：移动到 `certs/ssl/<前缀>.*.pem`（若 `certs/ssl/` 下已存在同名且更新则覆盖，否则保留现有），DB 路径更新为 `certs/ssl/...`。
- 待归档项：移动到 `certs/archived/<指纹>/<前缀>.*.pem`，DB 路径更新为 `certs/archived/...`。
- 已在 `certs/ssl/` 或 `certs/archived/` / `certs/revoked/` 的记录跳过；重复执行幂等。
- 执行时机：`FileStore::open` 之后、首次业务写入之前，或独立 `migrate` 子命令；并发下以文件存在性检查 + `rename` 原子性保证幂等。

**替代**：惰性迁移（读写时按需搬）—— 分支多、易遗漏，不推荐。

### 5. 数据库与文件系统一致性

- 续期归档：文件先归档旧证书、再写入新证书、最后更新 DB；归档失败仅记日志不阻断写入。
- 吊销归档：沿用既有“文件先动、DB 后更新、失败降级”顺序。
- 迁移：DB 更新失败则回退文件移动（逐文件 `restore`）。

## Risks / Trade-offs

| 风险 | 缓解 |
|---|---|
| 扁平下无法整目录原子搬，归档为逐文件 | 活跃文件仅两份（cert+key），逐文件风险可控；归档失败不阻断新证书 |
| 同主域名冲突判定依赖文件名前缀 | 前缀规则与现有 `domain_file_prefix` 完全一致，单点定义 |
| 存量多指纹同域名冲突时“最新”判定 | 优先 `not_after`，缺失时回退 `created_at`，并在迁移日志中记录决策 |
| 旧路径硬编码测试大面积失效 | 批量替换断言中的 `certs/<fp>/` → `certs/ssl/` 扁平或 `certs/archived/<fp>/` |
| 运维脚本依赖旧路径 | CHANGELOG 明确新布局，提供 `ls certs/ssl/` 与 `find certs/archived -type f` 指引 |

## Migration

1. 发布前：无 DB schema 变更，仅路径字符串前缀变化。
2. 发布后首次启动：执行存量迁移（或运维手动触发 `migrate` 命令）。
3. 回滚：需将 `certs/ssl/` 扁平文件按指纹搬回 `certs/<指纹>/` 并回写 DB，同时将 `certs/archived/<指纹>/` 的归档文件一并处理；建议保留反向迁移脚本。

## Open Questions

- 迁移时“最新”判定以 `not_after` 还是 `created_at` 为主？倾向 `not_after`，需确认证书解析在迁移上下文中是否可用。
- 是否需要在 `archived` 证书的 DB 记录中保留状态标记（如 `archived`）以区分于有效与吊销？当前 DB 无 `archived` 状态，归档后旧证书行可保持原状态但路径指向 `archived`，或新增状态——需与现有查询过滤逻辑确认。

## Implementation Notes

- 权限：`ensure_restricted_dir` 对 `certs/ssl`、`certs/archived`、`certs/archived/<指纹>/` 逐层收紧，与 `revoked` 链路一致。
- 测试：`file_store.rs`、`cert_repository.rs`、`pipeline_e2e.rs` 中所有 `certs/<fp>/` 相关断言需更新；新增“续期归档”“扁平覆盖”“迁移冲突归档”“幂等”用例。
