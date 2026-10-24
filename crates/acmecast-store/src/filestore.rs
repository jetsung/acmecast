//! 数据目录文件存储。
//!
//! 证书链与私钥这类大对象不塞进数据库，而是落在数据目录里，库里只记**相对路径**。
//! 这样数据库备份体积小、私钥也不会随查询结果被意外带出。
//!
//! 两条硬约束：
//!
//! - **路径不得逃逸**：所有相对路径逐段校验，`..`、`.`、绝对路径一律拒绝。
//!   否则调用方——乃至经由它传入的上游数据——能把文件写到数据目录之外。
//! - **权限受限**：目录 `0700`、文件 `0600`，且在**创建时**就设好，
//!   而不是先落盘再 chmod。后者存在一个短暂窗口，文件已是 umask 默认权限
//!   （通常 `0644`，同机其他用户可读）。

use std::path::{Component, Path, PathBuf};

use tokio::io::AsyncWriteExt;

use crate::error::{Error, Result};

/// 活跃证书的扁平目录：`certs/ssl/`，扁平存放，不按指纹分子目录。
const ACTIVE_CERT_DIR: &str = "certs/ssl";
/// 已归档（被续期/重签替换）证书的目录：`certs/archived/<指纹>/`。
const ARCHIVED_CERT_DIR: &str = "certs/archived";
/// 已吊销证书的归档目录名：`certs/revoked/<指纹>/`。
const REVOKED_CERT_DIR: &str = "certs/revoked";
/// 叶子证书文件的后缀：`<主域名>.cer`，对齐 `acme.sh $CERT_PATH`。
const CERT_SUFFIX: &str = "cer";
/// 中间链文件的后缀：`<主域名>.ca.cer`，对齐 `acme.sh $CA_CERT_PATH`（带前缀以支持扁平多域）。
const CA_SUFFIX: &str = "ca.cer";
/// 完整链文件的后缀：`<主域名>.fullchain.cer`，对齐 `acme.sh $CERT_FULLCHAIN_PATH`。
const FULLCHAIN_SUFFIX: &str = "fullchain.cer";
/// 私钥文件的后缀：`<主域名>.key`，对齐 `acme.sh $CERT_KEY_PATH`。
const KEY_SUFFIX: &str = "key";
/// 证书链文件的固定后缀（旧布局，兼容读取）：`<主域名>.cert.pem`。
const CERT_PEM_SUFFIX: &str = "cert.pem";
/// 私钥文件的固定后缀（旧布局，兼容读取）：`<主域名>.key.pem`。
const KEY_PEM_SUFFIX: &str = "key.pem";

/// 文件名前缀的清洗规则：字母数字与 `.`、`-`、`_` 原样保留，其余替换为 `_`。
fn sanitize_file_prefix(normalized: String) -> String {
    let cleaned: String = normalized
        .chars()
        .map(|c| match c {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '.' | '-' | '_' => c,
            _ => '_',
        })
        .collect();
    if cleaned.is_empty() {
        "certificate".to_owned()
    } else {
        cleaned
    }
}

/// 主域名 → 文件名前缀。
///
/// `*` 显式映射为 `_wildcard`（`*.hello.example.com` → `_wildcard.hello.example.com`），
/// 而非模糊的 `_`；字母数字与 `.`、`-`、`_` 原样保留。空域名兜底为 `certificate`。
pub(crate) fn domain_file_prefix(domain: &str) -> String {
    let normalized = if let Some(stripped) = domain.strip_prefix("*.") {
        format!("_wildcard.{stripped}")
    } else if domain == "*" {
        "_wildcard".to_owned()
    } else {
        domain.to_owned()
    };
    sanitize_file_prefix(normalized)
}

/// 旧布局文件名前缀：通配 `*.` → `_`（历史约定，`*.example.com` → `_.example.com`）；
/// 其余清洗规则与 [`domain_file_prefix`] 一致。供兼容回退读取推导旧布局候选路径。
fn legacy_domain_file_prefix(domain: &str) -> String {
    let normalized = if let Some(stripped) = domain.strip_prefix("*.") {
        format!("_.{stripped}")
    } else if domain == "*" {
        "_".to_owned()
    } else {
        domain.to_owned()
    };
    sanitize_file_prefix(normalized)
}

/// 复刻 `acme.sh _split_cert_chain` 的拆链：首个 `END CERTIFICATE` 为界。
///
/// 返回 `(leaf_pem, ca_pem)`：`leaf` 为首块（含头尾），`ca` 为剩余；单块时 `ca` 为空。
pub fn split_pem_chain(chain_pem: &str) -> (String, String) {
    const END_MARKER: &str = "-----END CERTIFICATE-----";
    if let Some(idx) = chain_pem.find(END_MARKER) {
        let end = idx + END_MARKER.len();
        let leaf = chain_pem[..end].trim().to_owned() + "\n";
        let rest = chain_pem[end..].trim();
        if rest.is_empty() {
            (leaf, String::new())
        } else {
            (leaf, format!("{rest}\n"))
        }
    } else {
        (chain_pem.to_owned(), String::new())
    }
}

/// 相对路径的文件名部分；库中路径总有文件名，兜底原样返回仅是防御。
fn base_name_of(relative: &str) -> &str {
    Path::new(relative)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(relative)
}

/// 一条证书在数据目录中的位置（均为相对路径）。
///
/// 对齐 `acme.sh` 的三文件布局：`cer`（叶子）/ `ca.cer`（中间链）/ `fullchain.cer`（完整链）+ `key`（私钥）。
/// 旧布局的 `cert_pem`/`key_pem` 字段保留作兼容读取，新布局以 `cert`/`ca`/`fullchain`/`key` 为准。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CertFilePaths {
    /// 叶子证书相对路径（`<prefix>.cer`）。
    pub cert: String,
    /// 中间链相对路径（`<prefix>.ca.cer`），单证书时为空文件。
    pub ca: String,
    /// 完整链相对路径（`<prefix>.fullchain.cer`）。
    pub fullchain: String,
    /// 私钥相对路径（`<prefix>.key`）。
    pub key: String,
    /// 兼容：旧布局证书链路径（`<prefix>.cert.pem`）。
    pub cert_pem: String,
    /// 兼容：旧布局私钥路径（`<prefix>.key.pem`）。
    pub key_pem: String,
}

/// 数据目录中的文件存储。
#[derive(Debug, Clone)]
pub struct FileStore {
    root: PathBuf,
}

impl FileStore {
    /// 绑定数据目录。
    ///
    /// 目录不存在时以 `0700` 创建；已存在时若权限过宽（对属主之外有任何权限）
    /// 会被收紧为 `0700` 并记一条 warn——里面要放私钥，权限过宽不该被当作运维疏忽放过，
    /// 但「原本就是错的」这件事本身值得让人知道。
    pub async fn open(root: impl Into<PathBuf>) -> Result<Self> {
        let root = root.into();
        ensure_restricted_dir(&root).await?;
        Ok(Self { root })
    }

    /// 数据目录的根路径。
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// 某条证书在数据目录中的位置（扁平活跃目录 `certs/ssl/`）。
    ///
    /// 路径规则集中定义在这里，避免各处自行拼接导致不一致。文件名带主域名：
    /// - 多域名取申请时的第一个（`sub1.example.com.cert.pem`）；
    /// - 通配符 `*` 换成 `_`（`*.hello.example.com` → `_.hello.example.com.cert.pem`）；
    /// - 其余字符原样（`sub.example.com.cert.pem`）。
    ///
    /// 扁平存放，不再按指纹分子目录；`fingerprint` 参数保留以兼容调用方与归档推导，
    /// 但不影响活跃路径本身。`fingerprint` 的逃逸校验由 `archived_cert_dir` / `revoked_cert_dir` 承担。
    #[must_use]
    pub fn cert_paths(_fingerprint: &str, primary_domain: &str) -> CertFilePaths {
        let prefix = domain_file_prefix(primary_domain);
        CertFilePaths {
            cert: format!("{ACTIVE_CERT_DIR}/{prefix}.{CERT_SUFFIX}"),
            ca: format!("{ACTIVE_CERT_DIR}/{prefix}.{CA_SUFFIX}"),
            fullchain: format!("{ACTIVE_CERT_DIR}/{prefix}.{FULLCHAIN_SUFFIX}"),
            key: format!("{ACTIVE_CERT_DIR}/{prefix}.{KEY_SUFFIX}"),
            cert_pem: format!("{ACTIVE_CERT_DIR}/{prefix}.{CERT_PEM_SUFFIX}"),
            key_pem: format!("{ACTIVE_CERT_DIR}/{prefix}.{KEY_PEM_SUFFIX}"),
        }
    }

    /// 某条**已归档**（被续期/重签替换）证书的归档目录：`certs/archived/<指纹>/`。
    ///
    /// 与 `revoked_cert_dir` 对称，按指纹分子目录、文件名不变。
    #[must_use]
    pub fn archived_cert_dir(fingerprint: &str) -> String {
        format!("{ARCHIVED_CERT_DIR}/{fingerprint}")
    }

    /// 某条**已吊销**证书的归档目录：`certs/revoked/<指纹>/`。
    ///
    /// 归档只换目录、不改文件名——文件名跟随库中现有记录，旧证书的
    /// `cert.pem` 也原样搬入。`fingerprint` 的逃逸校验由 `resolve` 承担。
    #[must_use]
    pub fn revoked_cert_dir(fingerprint: &str) -> String {
        format!("{REVOKED_CERT_DIR}/{fingerprint}")
    }

    /// 把单条证书的材料移动到归档目录 `certs/archived/<指纹>/`，返回归档后路径。
    ///
    /// 供**手动归档**与续期归档复用：目标按指纹分子目录、文件名不变，权限受限、防逃逸。
    /// - 已归档过（源即目标）直接返回目标（幂等）；
    /// - 任一源文件缺失返回 `None`；
    /// - 归档目标目录逐层以受限权限创建。
    pub async fn move_to_archived(
        &self,
        cert_pem_relative: &str,
        key_pem_relative: &str,
        fingerprint: &str,
    ) -> Result<Option<CertFilePaths>> {
        let archived_dir = Self::archived_cert_dir(fingerprint);
        // 从旧路径反推 prefix，构造新布局 archived 目标
        let prefix = base_name_of(cert_pem_relative)
            .strip_suffix(&format!(".{CERT_PEM_SUFFIX}"))
            .or_else(|| base_name_of(cert_pem_relative).strip_suffix(&format!(".{CERT_SUFFIX}")))
            .or_else(|| {
                base_name_of(cert_pem_relative).strip_suffix(&format!(".{FULLCHAIN_SUFFIX}"))
            })
            .unwrap_or(base_name_of(cert_pem_relative))
            .to_owned();
        let target = CertFilePaths {
            cert: format!("{archived_dir}/{prefix}.{CERT_SUFFIX}"),
            ca: format!("{archived_dir}/{prefix}.{CA_SUFFIX}"),
            fullchain: format!("{archived_dir}/{prefix}.{FULLCHAIN_SUFFIX}"),
            key: format!("{archived_dir}/{prefix}.{KEY_SUFFIX}"),
            cert_pem: format!("{archived_dir}/{}", base_name_of(cert_pem_relative)),
            key_pem: format!("{archived_dir}/{}", base_name_of(key_pem_relative)),
        };
        if cert_pem_relative == target.cert_pem && key_pem_relative == target.key_pem {
            return Ok(Some(target));
        }
        // 兼容：若活跃目录已有新布局文件，一并归档
        let active_prefix = prefix.clone();
        let active_candidates = [
            format!("{ACTIVE_CERT_DIR}/{active_prefix}.{CERT_SUFFIX}"),
            format!("{ACTIVE_CERT_DIR}/{active_prefix}.{CA_SUFFIX}"),
            format!("{ACTIVE_CERT_DIR}/{active_prefix}.{FULLCHAIN_SUFFIX}"),
            format!("{ACTIVE_CERT_DIR}/{active_prefix}.{KEY_SUFFIX}"),
        ];
        let mut any = false;
        for c in [cert_pem_relative, key_pem_relative]
            .iter()
            .copied()
            .chain(active_candidates.iter().map(|s| s.as_str()))
        {
            if tokio::fs::metadata(self.resolve(c)?).await.is_ok() {
                any = true;
                break;
            }
        }
        if !any {
            return Ok(None);
        }
        self.ensure_archived_dir(fingerprint).await?;
        // 新布局 4 文件
        for src in active_candidates.iter() {
            if tokio::fs::metadata(self.resolve(src)?).await.is_ok() {
                let dst = format!("{archived_dir}/{}", base_name_of(src));
                if src != &dst {
                    let _ = self.move_within(src, &dst).await;
                }
            }
        }
        // 旧布局 2 文件
        for (src, dst) in [
            (cert_pem_relative, &target.cert_pem),
            (key_pem_relative, &target.key_pem),
        ] {
            if src == dst {
                continue;
            }
            if tokio::fs::metadata(self.resolve(src)?).await.is_ok() {
                let _ = self.move_within(src, dst).await;
            }
        }
        Ok(Some(target))
    }

    /// 把被续期/重签替换的旧证书材料移动到归档目录 `certs/archived/<旧指纹>/`，返回归档后路径。
    ///
    /// 活跃目录扁平，无法整目录搬，只能逐文件 `rename`。目标指纹为**旧证书**的指纹。
    /// - 已归档过（源即目标）直接返回目标；
    /// - 任一源文件缺失返回 `None`；
    /// - 归档目标目录逐层以受限权限创建。
    pub async fn archive_replaced_certificate(
        &self,
        cert_pem_relative: &str,
        key_pem_relative: &str,
        old_fingerprint: &str,
    ) -> Result<Option<CertFilePaths>> {
        self.move_to_archived(cert_pem_relative, key_pem_relative, old_fingerprint)
            .await
    }

    /// 把一条证书的材料移动到吊销目录，返回归档后的相对路径。
    ///
    /// 源路径以调用方传入的库中记录为准——手动上传的证书可能不在标准布局；
    /// 目标是 [`Self::revoked_cert_dir`] 下**同名**文件（只换目录不改名）。
    ///
    /// - 已归档过（源即目标，重复吊销的幂等路径）直接返回目标；
    /// - 任一源文件缺失返回 `None`：没有材料可归档，调用方应保持库中路径不变；
    /// - 两个源文件同在一个目录时**整个目录一次 `rename`**——原子，且目录里
    ///   可能混放的其他文件（手动上传）也跟着走，不在原地留孤儿；
    /// - 整搬不可行（源分处不同目录等）时退回逐文件 `rename`，逐文件路径
    ///   覆盖 `sources` 的全部 6 个路径——归档布局的 `.cer`/`.ca.cer` 等
    ///   文件必须一并搬入吊销目录，不在源目录留遗留；搬空后的源目录一并
    ///   清掉，避免 `certs/<指纹>/` 留下空壳。
    pub async fn move_certificate(
        &self,
        sources: &CertFilePaths,
        fingerprint: &str,
    ) -> Result<Option<CertFilePaths>> {
        let cert_pem_relative = sources.cert_pem.as_str();
        let key_pem_relative = sources.key_pem.as_str();
        let revoked_dir = Self::revoked_cert_dir(fingerprint);
        let prefix = base_name_of(cert_pem_relative)
            .strip_suffix(&format!(".{CERT_PEM_SUFFIX}"))
            .or_else(|| base_name_of(cert_pem_relative).strip_suffix(&format!(".{CERT_SUFFIX}")))
            .unwrap_or(base_name_of(cert_pem_relative))
            .to_owned();
        let target = CertFilePaths {
            cert: format!("{revoked_dir}/{prefix}.{CERT_SUFFIX}"),
            ca: format!("{revoked_dir}/{prefix}.{CA_SUFFIX}"),
            fullchain: format!("{revoked_dir}/{prefix}.{FULLCHAIN_SUFFIX}"),
            key: format!("{revoked_dir}/{prefix}.{KEY_SUFFIX}"),
            cert_pem: format!("{revoked_dir}/{}", base_name_of(cert_pem_relative)),
            key_pem: format!("{revoked_dir}/{}", base_name_of(key_pem_relative)),
        };
        if cert_pem_relative == target.cert_pem && key_pem_relative == target.key_pem {
            return Ok(Some(target));
        }

        // 6 个源路径 + 活跃目录的 4 个新布局候选：任一存在即视为有材料。
        // 空路径（库中旧记录缺列）跳过，避免 resolve 报错中断归档。
        let active_candidates = [
            format!("{ACTIVE_CERT_DIR}/{prefix}.{CERT_SUFFIX}"),
            format!("{ACTIVE_CERT_DIR}/{prefix}.{CA_SUFFIX}"),
            format!("{ACTIVE_CERT_DIR}/{prefix}.{FULLCHAIN_SUFFIX}"),
            format!("{ACTIVE_CERT_DIR}/{prefix}.{KEY_SUFFIX}"),
        ];
        let source_candidates: Vec<&str> = [
            sources.cert.as_str(),
            sources.ca.as_str(),
            sources.fullchain.as_str(),
            sources.key.as_str(),
            cert_pem_relative,
            key_pem_relative,
        ]
        .into_iter()
        .filter(|s| !s.is_empty())
        .collect();
        let mut any = false;
        for c in source_candidates
            .iter()
            .copied()
            .chain(active_candidates.iter().map(|s| s.as_str()))
        {
            if tokio::fs::metadata(self.resolve(c)?).await.is_ok() {
                any = true;
                break;
            }
        }
        if !any {
            return Ok(None);
        }

        if self
            .try_move_whole_dir(cert_pem_relative, key_pem_relative, &revoked_dir)
            .await?
        {
            return Ok(Some(target));
        }

        self.ensure_revoked_dir(fingerprint).await?;
        for src in active_candidates.iter() {
            if tokio::fs::metadata(self.resolve(src)?).await.is_ok() {
                let dst = format!("{revoked_dir}/{}", base_name_of(src));
                if src != &dst {
                    let _ = self.move_within(src, &dst).await;
                }
            }
        }
        for src in source_candidates.iter() {
            let dst = format!("{revoked_dir}/{}", base_name_of(src));
            if src == &dst {
                continue;
            }
            if tokio::fs::metadata(self.resolve(src)?).await.is_ok() {
                let _ = self.move_within(src, &dst).await;
            }
        }
        // 扁平活跃目录为共享目录，不应因单条证书归档而尝试删除。
        let active_dir = self.resolve(ACTIVE_CERT_DIR)?;
        for source in source_candidates.iter() {
            let Ok(parent) = self
                .resolve(source)
                .map(|p| p.parent().unwrap().to_path_buf())
            else {
                continue;
            };
            if parent == active_dir {
                continue;
            }
            let _ = self.remove_parent_dir_if_empty(source).await;
        }
        Ok(Some(target))
    }

    /// 把归档在吊销目录的材料移回原位（库路径更新失败时的回退）。
    ///
    /// 优先整个目录搬回——整目录归档时目录里可能还带着混放的其他文件，
    /// 逐文件只搬 cert/key 会把它们遗落在吊销目录。原目录仍在（逐文件
    /// 归档后的回退）等整搬不可行的情况退回逐文件，并把搬空的吊销目录清掉。
    pub async fn restore_certificate(
        &self,
        archived: &CertFilePaths,
        original: &CertFilePaths,
    ) -> Result<()> {
        if self.try_restore_whole_dir(archived, original).await? {
            return Ok(());
        }
        for (src, dst) in [
            (&archived.cert, &original.cert),
            (&archived.ca, &original.ca),
            (&archived.fullchain, &original.fullchain),
            (&archived.key, &original.key),
            (&archived.cert_pem, &original.cert_pem),
            (&archived.key_pem, &original.key_pem),
        ] {
            if tokio::fs::metadata(self.resolve(src)?).await.is_ok() {
                if src != dst {
                    let _ = self.move_within(src, dst).await;
                }
            }
        }
        for file in [
            &archived.cert_pem,
            &archived.key_pem,
            &archived.cert,
            &archived.key,
        ] {
            let _ = self.remove_parent_dir_if_empty(file).await;
        }
        Ok(())
    }

    /// 两个源文件同在一个（非数据目录根的）目录、目标又不存在时，把整个
    /// 目录一次 `rename` 进吊销目录：不会出现「证书链已移走、私钥还没动」
    /// 的中间态。不可行时返回 `false`，交回逐文件路径。
    async fn try_move_whole_dir(
        &self,
        cert_pem_relative: &str,
        key_pem_relative: &str,
        revoked_dir: &str,
    ) -> Result<bool> {
        let Some(source_dir) = self.shared_parent_dir(cert_pem_relative, key_pem_relative)? else {
            return Ok(false);
        };
        // 扁平活跃目录共享，不可整目录搬；否则会把其他证书一并移走。
        if source_dir == self.resolve(ACTIVE_CERT_DIR)? {
            return Ok(false);
        }
        let target_dir = self.resolve(revoked_dir)?;
        if source_dir == target_dir || tokio::fs::metadata(&target_dir).await.is_ok() {
            return Ok(false);
        }
        // 吊销根目录先建好且受限；目标叶子由 rename 产生，权限随源目录，
        // 落地后再收紧一次——手动上传的目录权限未必干净。
        ensure_restricted_dir(&self.resolve(REVOKED_CERT_DIR)?).await?;
        tokio::fs::rename(&source_dir, &target_dir).await?;
        ensure_restricted_dir(&target_dir).await?;
        Ok(true)
    }

    /// [`Self::restore_certificate`] 的整目录搬回：归档目录整个 rename 回
    /// 原目录。原目录已存在等不可行情况返回 `false`，交回逐文件路径。
    async fn try_restore_whole_dir(
        &self,
        archived: &CertFilePaths,
        original: &CertFilePaths,
    ) -> Result<bool> {
        let Some(source_dir) = self.shared_parent_dir(&archived.cert_pem, &archived.key_pem)?
        else {
            return Ok(false);
        };
        let Some(target_dir) = self.shared_parent_dir(&original.cert_pem, &original.key_pem)?
        else {
            return Ok(false);
        };
        if source_dir == target_dir || tokio::fs::metadata(&target_dir).await.is_ok() {
            return Ok(false);
        }
        tokio::fs::rename(&source_dir, &target_dir).await?;
        Ok(true)
    }

    /// 两个相对路径的共同父目录；分处不同目录、或就在数据目录根部时返回
    /// `None`——根部没有可搬的目录。
    fn shared_parent_dir(&self, a: &str, b: &str) -> Result<Option<PathBuf>> {
        let dir_a = target_dir_of(self.resolve(a)?);
        let dir_b = target_dir_of(self.resolve(b)?);
        if dir_a != dir_b || dir_a == self.root {
            return Ok(None);
        }
        Ok(Some(dir_a))
    }

    /// 删除包含 `relative_file` 的目录，**仅当该目录已为空**。
    ///
    /// 供逐文件归档与回退清掉搬空后的空壳目录：`remove_dir` 天然只删空目录，
    /// 目录里还有别的文件、目录已不存在等情况都会报错——那是调用方该容忍的
    /// 「无需清理」，不是故障。
    async fn remove_parent_dir_if_empty(&self, relative_file: &str) -> Result<()> {
        let file_path = self.resolve(relative_file)?;
        if let Some(parent) = file_path.parent() {
            tokio::fs::remove_dir(parent).await?;
        }
        Ok(())
    }

    /// 在数据目录内移动一个文件（源、目标均为相对路径）。
    ///
    /// 供逐文件归档与其回退使用：目标父目录先以受限权限建好，`rename`
    /// 保留文件权限位。源文件缺失会让 `rename` 报错——归档前应先确认材料存在。
    async fn move_within(&self, source: &str, target: &str) -> Result<()> {
        let from = self.resolve(source)?;
        let to = self.resolve(target)?;
        if let Some(parent) = to.parent() {
            ensure_restricted_dir(parent).await?;
        }
        tokio::fs::rename(from, to).await?;
        Ok(())
    }

    /// 确保归档目录整条链存在且受限，返回 `certs/archived/<指纹>/` 的绝对路径。
    async fn ensure_archived_dir(&self, fingerprint: &str) -> Result<PathBuf> {
        let leaf = self.resolve(&Self::archived_cert_dir(fingerprint))?;
        if let Some(archived_root) = leaf.parent() {
            ensure_restricted_dir(archived_root).await?;
        }
        ensure_restricted_dir(&leaf).await?;
        Ok(leaf)
    }

    /// 确保吊销目录整条链存在且受限，返回 `certs/revoked/<指纹>/` 的绝对路径。
    ///
    /// `DirBuilder::recursive` 只把 mode 应用到最深一层，中间目录会落成
    /// umask 默认权限；这条链里要放私钥，所以每一层都显式收紧。
    async fn ensure_revoked_dir(&self, fingerprint: &str) -> Result<PathBuf> {
        let leaf = self.resolve(&Self::revoked_cert_dir(fingerprint))?;
        if let Some(revoked_root) = leaf.parent() {
            ensure_restricted_dir(revoked_root).await?;
        }
        ensure_restricted_dir(&leaf).await?;
        Ok(leaf)
    }

    /// 把证书链与私钥写入数据目录，返回它们的相对路径——库里存的就是这两个值。
    ///
    /// `primary_domain` 决定文件名前缀，规则见 [`Self::cert_paths`]。
    pub async fn write_certificate(
        &self,
        fingerprint: &str,
        primary_domain: &str,
        cert_chain_pem: &str,
        key_pem: &str,
    ) -> Result<CertFilePaths> {
        let paths = Self::cert_paths(fingerprint, primary_domain);
        let (_leaf, ca) = split_pem_chain(cert_chain_pem);
        // B 方案：仅三文件（ca.cer / fullchain.cer / key），不再落叶子 cer 与兼容 pem
        self.write(&paths.ca, &ca).await?;
        self.write(&paths.fullchain, cert_chain_pem).await?;
        self.write(&paths.key, key_pem).await?;
        // 清理可能残留的叶子 cer 与旧 pem（幂等、无害）
        let _ = self.delete(&paths.cert).await;
        let _ = self.delete(&paths.cert_pem).await;
        let _ = self.delete(&paths.key_pem).await;
        Ok(paths)
    }

    /// 读取一个文件。
    pub async fn read(&self, relative: &str) -> Result<String> {
        let path = self.resolve(relative)?;
        Ok(tokio::fs::read_to_string(path).await?)
    }

    /// 兼容回退读取的候选相对路径（有序、去重）。
    ///
    /// 顺序：库中记录的路径（跳过空值）→ 活跃目录 `certs/ssl/` 下新旧前缀
    /// × 新旧后缀的推导路径 → `certs/archived/<指纹>/` 下与前序候选同名的
    /// 归档副本。候选只作尝试清单，逐一经 [`Self::read`] 的 `resolve`
    /// 防逃逸校验，非法路径计入尝试清单，不会逃出数据目录。
    #[must_use]
    pub fn cert_read_candidates(
        record_paths: &[&str],
        primary_domain: &str,
        fingerprint: &str,
        suffixes: &[&str],
    ) -> Vec<String> {
        let mut candidates: Vec<String> = record_paths
            .iter()
            .filter(|p| !p.is_empty())
            .map(|p| (*p).to_owned())
            .collect();
        let mut prefixes = vec![domain_file_prefix(primary_domain)];
        let legacy = legacy_domain_file_prefix(primary_domain);
        if legacy != prefixes[0] {
            prefixes.push(legacy);
        }
        for prefix in &prefixes {
            for suffix in suffixes {
                candidates.push(format!("{ACTIVE_CERT_DIR}/{prefix}.{suffix}"));
            }
        }
        let archived_dir = Self::archived_cert_dir(fingerprint);
        candidates.extend(
            candidates
                .iter()
                .map(|c| format!("{archived_dir}/{}", base_name_of(c)))
                .collect::<Vec<_>>(),
        );
        let mut seen = std::collections::HashSet::new();
        candidates.retain(|c| seen.insert(c.clone()));
        candidates
    }

    /// 兼容回退读取：按候选顺序尝试，返回 `(内容, 命中路径)`；
    /// 全部失败时返回按序的 `(路径, 错误)` 尝试清单，由调用方决定如何呈现。
    pub async fn read_with_fallback(
        &self,
        candidates: &[String],
    ) -> std::result::Result<(String, String), Vec<(String, Error)>> {
        let mut attempts = Vec::new();
        for candidate in candidates {
            match self.read(candidate).await {
                Ok(content) => return Ok((content, candidate.clone())),
                Err(error) => attempts.push((candidate.clone(), error)),
            }
        }
        Err(attempts)
    }

    /// 写入一个文件；缺失的中间目录会被自动创建，权限同样受限。
    pub async fn write(&self, relative: &str, contents: &str) -> Result<()> {
        let path = self.resolve(relative)?;
        if let Some(parent) = path.parent() {
            ensure_restricted_dir(parent).await?;
        }
        write_restricted(&path, contents).await
    }

    /// 删除一个文件；文件本就不存在时返回 `false`。
    pub async fn delete(&self, relative: &str) -> Result<bool> {
        let path = self.resolve(relative)?;
        match tokio::fs::remove_file(&path).await {
            Ok(()) => Ok(true),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(err) => Err(err.into()),
        }
    }

    /// 彻底清理一条证书的材料：指纹专属目录整删，共享活跃目录退回逐文件。
    ///
    /// - 两个文件同在一个**指纹专属**目录（`certs/archived/<指纹>/`、
    ///   `certs/revoked/<指纹>/`）时，整个目录一次 `remove_dir_all`——目录只为
    ///   这条证书存在，文件与目录一步清掉，无需先删文件再删目录。
    /// - 共享扁平活跃目录 `certs/ssl/` **不可整删**（手动上传的证书可能混放在
    ///   同一目录），退回逐文件删除且不动目录本身；与 [`Self::try_move_whole_dir`]
    ///   的整搬守卫同一口径。
    /// - 两文件分处不同目录（手动上传的非常规布局）同样退回逐文件。
    ///
    /// 路径均经 `resolve` 防逃逸校验；文件/目录不存在视为幂等成功。
    pub async fn remove_material(
        &self,
        cert_pem_relative: &str,
        key_pem_relative: &str,
    ) -> Result<()> {
        // 同时清理新布局的 4 文件（若存在）
        let prefix = base_name_of(cert_pem_relative)
            .strip_suffix(&format!(".{CERT_PEM_SUFFIX}"))
            .or_else(|| base_name_of(cert_pem_relative).strip_suffix(&format!(".{CERT_SUFFIX}")))
            .unwrap_or(base_name_of(cert_pem_relative))
            .to_owned();
        let parent_dir = std::path::Path::new(cert_pem_relative)
            .parent()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default();
        let extra = if parent_dir.is_empty() {
            vec![]
        } else {
            vec![
                format!("{parent_dir}/{prefix}.{CERT_SUFFIX}"),
                format!("{parent_dir}/{prefix}.{CA_SUFFIX}"),
                format!("{parent_dir}/{prefix}.{FULLCHAIN_SUFFIX}"),
                format!("{parent_dir}/{prefix}.{KEY_SUFFIX}"),
            ]
        };
        let delete_files_individually = async {
            for rel in [cert_pem_relative, key_pem_relative]
                .iter()
                .copied()
                .chain(extra.iter().map(|s| s.as_str()))
            {
                let _ = self.delete(rel).await;
            }
        };

        let Some(dir) = self.shared_parent_dir(cert_pem_relative, key_pem_relative)? else {
            delete_files_individually.await;
            return Ok(());
        };
        if dir == self.resolve(ACTIVE_CERT_DIR)? {
            delete_files_individually.await;
            return Ok(());
        }
        match tokio::fs::remove_dir_all(&dir).await {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(err) => Err(err.into()),
        }
    }

    /// 把相对路径解析为数据目录内的绝对路径，并拒绝一切逃逸可能。
    ///
    /// 只接受由「普通路径段」组成的相对路径：`..`、`.`、根前缀、Windows 盘符
    /// 都会在这里被挡下。
    fn resolve(&self, relative: &str) -> Result<PathBuf> {
        if relative.is_empty() {
            return Err(Error::Validation("文件路径不能为空".to_owned()));
        }

        let path = Path::new(relative);
        if path.is_absolute() {
            return Err(Error::Validation(format!(
                "文件路径必须是数据目录内的相对路径: {relative}"
            )));
        }
        for component in path.components() {
            if !matches!(component, Component::Normal(_)) {
                return Err(Error::Validation(format!(
                    "文件路径不得包含 `..`、`.` 或路径前缀: {relative}"
                )));
            }
        }

        Ok(self.root.join(path))
    }
}

/// 确保目录存在，且权限对属主之外没有任何放行。
/// 数据目录内部某条路径的所在目录。
fn target_dir_of(path: PathBuf) -> PathBuf {
    path.parent()
        .expect("数据目录内部的路径必有父目录")
        .to_path_buf()
}

async fn ensure_restricted_dir(path: &Path) -> Result<()> {
    if tokio::fs::metadata(path).await.is_ok() {
        restrict_existing_dir(path).await?;
        return Ok(());
    }

    let mut builder = tokio::fs::DirBuilder::new();
    builder.recursive(true);
    restrict_new_dir_mode(&mut builder);
    builder.create(path).await?;
    Ok(())
}

/// 已有的目录若对属主之外开放，收紧为 `0700`。
#[cfg(unix)]
async fn restrict_existing_dir(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let mode = tokio::fs::metadata(path).await?.permissions().mode() & 0o777;
    if mode & 0o077 == 0 {
        return Ok(());
    }

    tracing::warn!(
        path = %path.display(),
        current_mode = format!("{mode:o}"),
        "数据目录权限过宽，已收紧为 0700"
    );
    tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).await?;
    Ok(())
}

/// 非 Unix 平台没有 POSIX 权限位，这里不做处理。
#[cfg(not(unix))]
async fn restrict_existing_dir(_path: &Path) -> Result<()> {
    Ok(())
}

/// 以受限权限写文件：权限在**创建时**设定，不留「先落盘后 chmod」的窗口。
async fn write_restricted(path: &Path, contents: &str) -> Result<()> {
    let mut options = tokio::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    restrict_new_file_mode(&mut options);

    let mut file = options.open(path).await?;
    file.write_all(contents.as_bytes()).await?;
    file.flush().await?;
    Ok(())
}

/// 让新建的文件仅属主可读写。
fn restrict_new_file_mode(options: &mut tokio::fs::OpenOptions) {
    #[cfg(unix)]
    options.mode(0o600);
    #[cfg(not(unix))]
    let _ = options;
}

/// 让新建的目录仅属主可进入。
fn restrict_new_dir_mode(builder: &mut tokio::fs::DirBuilder) {
    #[cfg(unix)]
    builder.mode(0o700);
    #[cfg(not(unix))]
    let _ = builder;
}
