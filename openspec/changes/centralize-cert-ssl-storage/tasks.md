# Tasks

## 1. 路径常量与 FileStore 扁平化

- [ ] 1.1 调整 `crates/acmecast-store/src/filestore.rs`：新增 `ACTIVE_CERT_DIR = "certs/ssl"` 与 `ARCHIVED_CERT_DIR = "certs/archived"`，`cert_paths()` 改为扁平拼接 `certs/ssl/<前缀>.*.pem`（不再含指纹段），`REVOKED_CERT_DIR` 保持不变
- [ ] 1.2 新增 `archived_cert_dir(fingerprint)`、`ensure_archived_dir(fingerprint)`、`archive_replaced_certificate(...)` 等方法，参考 `revoked` 的实现（指纹隔离、逐层 `ensure_restricted_dir`、路径防逃逸、权限 `0700`/`0600`）
- [ ] 1.3 更新 `move_certificate` / `restore_certificate` 适配扁平源路径（`certs/ssl/` → `certs/revoked/<指纹>/` 逐文件搬），`try_move_whole_dir` 对扁平源返回 `false` 退化为逐文件分支
- [ ] 1.4 全局搜索并更新硬编码路径字符串（`certs/<指纹>/` 旧前缀）涉及的常量、日志、错误文案与文档，`certs/revoked/` 保持不变

## 2. 续期/重签旧证书归档

- [ ] 2.1 在证书写入编排层（`CertRepository` 或 `FileStore::write_certificate` 上层）实现“写入前检测”：若 `certs/ssl/<前缀>.*` 已存在且指纹不同，先按旧指纹调用 `archive_replaced_certificate` 将旧证书移入 `certs/archived/<旧指纹>/`
- [ ] 2.2 归档失败不阻断新证书写入：记录错误日志后仍覆盖写入 `certs/ssl/`，DB 路径指向新活跃文件
- [ ] 2.3 归档目标已存在时覆盖，不报错；`archived` 目录与文件权限逐层收紧

## 3. 存量迁移

- [ ] 3.1 实现幂等迁移例程：扫描 `data/certs/` 下一级子目录（跳过 `revoked`/`ssl`/`archived`），按主域名分组，同主域名多指纹冲突时仅保留最新（按 `not_after` 或 `created_at` 最大），其余按指纹移入 `certs/archived/<指纹>/`，并同步更新 `cert` 表的 `cert_pem_path` / `key_pem_path`
- [ ] 3.2 确定并实现迁移触发时机（启动钩子或独立 `migrate` 命令），保证并发幂等与失败回退（DB 更新失败则文件回退）
- [ ] 3.3 补充迁移的运维文档与回滚指引（`certs/ssl/` 扁平与 `certs/archived/<指纹>/` 的反向操作说明）

## 4. 权限与一致性

- [ ] 4.1 确保 `certs/ssl`、`certs/archived`、`certs/archived/<指纹>/` 逐层以 `0700` 创建/收紧，文件以 `0600` 落盘，复用 `ensure_restricted_dir` / `write_restricted` 现有机制
- [ ] 4.2 吊销归档失败降级保持不变：文件移动失败不回滚吊销结果，仅记日志、DB 路径保持 `certs/ssl/` 不变

## 5. 测试与回归

- [ ] 5.1 更新 `crates/acmecast-store/tests/file_store.rs`：证书落盘、归档、覆盖、权限等用例的路径断言由 `certs/<fp>/` 改为 `certs/ssl/` 扁平或 `certs/archived/<fp>/`，`revoked` 断言保持不变
- [ ] 5.2 更新 `crates/acmecast-store/tests/cert_repository.rs` 与 `crates/acmecast-server/tests/pipeline_e2e.rs` 等涉及 `cert_pem_path` / `key_pem_path` 的断言与种子数据
- [ ] 5.3 新增用例：扁平覆盖仅保留最新、续期旧证书归档到 `archived/<旧指纹>/`、归档失败不阻断写入、迁移同主域名冲突归档、幂等重入、吊销证书不被迁移
- [ ] 5.4 全量相关测试回归（`cargo test -p acmecast-store -p acmecast-server` 涉及 filestore / cert 链路）

## 6. 文档与发布

- [ ] 6.1 更新面向运维的数据目录结构说明（`certs/ssl/` 扁平、`certs/archived/<指纹>/` 与 `certs/revoked/<指纹>/` 同级指纹隔离的布局、`ls`/`find` 指引）
- [ ] 6.2 CHANGELOG 中记录路径变更、归档语义与迁移说明，明确旧 `certs/<指纹>/` 路径不再使用
