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
/// 证书链的文件名。
const CERT_FILE: &str = "cert.pem";
/// 私钥的文件名。
const KEY_FILE: &str = "key.pem";

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
    /// 路径规则集中定义在这里，避免各处自行拼接导致不一致。
    /// `fingerprint` 会随其余路径一起做逃逸校验，因此含 `/` 或 `..` 的值会被拒绝。
    #[must_use]
    pub fn cert_paths(fingerprint: &str) -> CertFilePaths {
        CertFilePaths {
            cert_pem: format!("{CERT_DIR}/{fingerprint}/{CERT_FILE}"),
            key_pem: format!("{CERT_DIR}/{fingerprint}/{KEY_FILE}"),
        }
    }

    /// 把证书链与私钥写入数据目录，返回它们的相对路径——库里存的就是这两个值。
    pub async fn write_certificate(
        &self,
        fingerprint: &str,
        cert_chain_pem: &str,
        key_pem: &str,
    ) -> Result<CertFilePaths> {
        let paths = Self::cert_paths(fingerprint);
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
