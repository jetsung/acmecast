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

/// 证书文件在数据目录中的一级目录名。
const CERT_DIR: &str = "certs";
/// 已吊销证书的归档目录名：`certs/revoked/<指纹>/`。
const REVOKED_CERT_DIR: &str = "certs/revoked";
/// 证书链文件的固定后缀：`<主域名>.cert.pem`。
const CERT_PEM_SUFFIX: &str = "cert.pem";
/// 私钥文件的固定后缀：`<主域名>.key.pem`。
const KEY_PEM_SUFFIX: &str = "key.pem";

/// 主域名 → 文件名前缀。
///
/// 通配符的 `*` 与其他文件名不安全字符一律换成 `_`（`*.hello.example.com`
/// → `_.hello.example.com`）；字母数字与 `.`、`-`、`_` 原样保留。空域名
/// 兜底为 `certificate`，保证路径始终可用。
fn domain_file_prefix(domain: &str) -> String {
    let cleaned: String = domain
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

/// 相对路径的文件名部分；库中路径总有文件名，兜底原样返回仅是防御。
fn base_name_of(relative: &str) -> &str {
    Path::new(relative)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(relative)
}

/// 一条证书在数据目录中的位置（均为相对路径）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CertFilePaths {
    /// 证书链 PEM 的相对路径。
    pub cert_pem: String,
    /// 私钥 PEM 的相对路径。
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

    /// 某条证书在数据目录中的位置。
    ///
    /// 路径规则集中定义在这里，避免各处自行拼接导致不一致。文件名带主域名：
    /// - 多域名取申请时的第一个（`sub1.example.com.cert.pem`）；
    /// - 通配符 `*` 换成 `_`（`*.hello.example.com` → `_.hello.example.com.cert.pem`）；
    /// - 其余字符原样（`sub.example.com.cert.pem`）。
    ///
    /// `fingerprint` 会随其余路径一起做逃逸校验，因此含 `/` 或 `..` 的值会被拒绝。
    #[must_use]
    pub fn cert_paths(fingerprint: &str, primary_domain: &str) -> CertFilePaths {
        let prefix = domain_file_prefix(primary_domain);
        CertFilePaths {
            cert_pem: format!("{CERT_DIR}/{fingerprint}/{prefix}.{CERT_PEM_SUFFIX}"),
            key_pem: format!("{CERT_DIR}/{fingerprint}/{prefix}.{KEY_PEM_SUFFIX}"),
        }
    }

    /// 某条**已吊销**证书的归档目录：`certs/revoked/<指纹>/`。
    ///
    /// 归档只换目录、不改文件名——文件名跟随库中现有记录，旧证书的
    /// `cert.pem` 也原样搬入。`fingerprint` 的逃逸校验由 `resolve` 承担。
    #[must_use]
    pub fn revoked_cert_dir(fingerprint: &str) -> String {
        format!("{REVOKED_CERT_DIR}/{fingerprint}")
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
    /// - 整搬不可行（源分处不同目录等）时退回逐文件 `rename`，搬空后的
    ///   源目录一并清掉，避免 `certs/<指纹>/` 留下空壳。
    pub async fn move_certificate(
        &self,
        cert_pem_relative: &str,
        key_pem_relative: &str,
        fingerprint: &str,
    ) -> Result<Option<CertFilePaths>> {
        let revoked_dir = Self::revoked_cert_dir(fingerprint);
        let target = CertFilePaths {
            cert_pem: format!("{revoked_dir}/{}", base_name_of(cert_pem_relative)),
            key_pem: format!("{revoked_dir}/{}", base_name_of(key_pem_relative)),
        };
        if cert_pem_relative == target.cert_pem && key_pem_relative == target.key_pem {
            return Ok(Some(target));
        }

        let sources = [cert_pem_relative, key_pem_relative];
        for source in sources {
            if tokio::fs::metadata(self.resolve(source)?).await.is_err() {
                return Ok(None);
            }
        }

        if self
            .try_move_whole_dir(cert_pem_relative, key_pem_relative, &revoked_dir)
            .await?
        {
            return Ok(Some(target));
        }

        self.ensure_revoked_dir(fingerprint).await?;
        for (source, target) in sources.iter().zip([&target.cert_pem, &target.key_pem]) {
            self.move_within(source, target).await?;
        }
        for source in sources {
            if let Err(error) = self.remove_parent_dir_if_empty(source).await {
                tracing::debug!(source, %error, "归档后清理源目录失败，忽略");
            }
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
        self.move_within(&archived.cert_pem, &original.cert_pem)
            .await?;
        self.move_within(&archived.key_pem, &original.key_pem)
            .await?;
        for file in [&archived.cert_pem, &archived.key_pem] {
            if let Err(error) = self.remove_parent_dir_if_empty(file).await {
                tracing::debug!(file, %error, "回退后清理吊销目录失败，忽略");
            }
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
        self.write(&paths.cert_pem, cert_chain_pem).await?;
        self.write(&paths.key_pem, key_pem).await?;
        Ok(paths)
    }

    /// 读取一个文件。
    pub async fn read(&self, relative: &str) -> Result<String> {
        let path = self.resolve(relative)?;
        Ok(tokio::fs::read_to_string(path).await?)
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
