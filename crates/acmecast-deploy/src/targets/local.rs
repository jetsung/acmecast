//! 本地文件系统部署。
//!
//! 把证书链与私钥写到服务所在主机的指定路径上，并按配置设置文件权限与属主。

use std::path::{Path, PathBuf};

use acmecast_cert::ca_pem_of;
use async_trait::async_trait;
use schemars::schema::RootSchema;
use schemars::schema_for;
use serde::Deserialize;
use serde_json::Value;

use crate::error::{Error, Result};
use crate::target::{CertMaterials, DeployMode, DeployOutcome, DeploymentTarget, parse_input};

/// 本地文件系统部署。
#[derive(Debug, Default)]
pub struct LocalTarget;

/// 本地部署的输入。
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct LocalInput {
    /// 完整证书链的写入路径。
    ///
    /// 以 `/` 结尾视为目录，保存时按当前证书主域补为 `<prefix>.fullchain.cer`
    /// 等（通配 `*.example.com` → `_wildcard.example.com.*`；无主域时以
    /// `{domain}` 占位，部署时展开），未重保存的目录写法部署时同样兜底。
    #[serde(alias = "cert_path")]
    pub fullchain_path: PathBuf,
    /// 中间链的写入路径。
    ///
    /// 未配置时按 `fullchain_path` 同目录派生 `<stem>.ca.cer`（链仅含单张
    /// 证书时没有可写内容，自动跳过）；显式留空（空串）时跳过；以 `/` 结尾
    /// 视为目录，保存时按当前证书主域补为 `<prefix>.ca.cer` 等（无主域时以
    /// `{domain}` 占位，部署时展开）。
    #[serde(default)]
    pub ca_path: Option<PathBuf>,
    /// 私钥的写入路径。
    ///
    /// 以 `/` 结尾视为目录，保存时按当前证书主域补为 `<prefix>.key` 等
    /// （无主域时以 `{domain}` 占位，部署时展开），未重保存的目录写法部署时
    /// 同样兜底。
    pub key_path: PathBuf,
    /// 完整链文件的权限，八进制写法，如 `"0644"`。
    #[serde(default = "default_cert_mode")]
    pub cert_mode: String,
    /// 中间链文件的权限，八进制写法，如 `"0644"`。
    #[serde(default = "default_ca_mode")]
    pub ca_mode: String,
    /// 私钥文件的权限。
    ///
    /// 默认比证书严：私钥泄漏等同于身份被冒用，而证书本身是公开信息。
    #[serde(default = "default_key_mode")]
    pub key_mode: String,
    /// 属主 uid；不填则保持写入者所有。
    #[serde(default)]
    pub uid: Option<u32>,
    /// 属组 gid；不填则保持写入者所属。
    #[serde(default)]
    pub gid: Option<u32>,
    /// 写入成功后执行的重载命令，如 `nginx -s reload`。
    ///
    /// 走 shell（`sh -c`／`cmd /C`）而非直接 exec：配置里写管道、重定向
    /// 之类很自然，逐个拆参数反而不实用。
    #[serde(default)]
    pub reload_command: Option<String>,
}

pub(crate) fn default_cert_mode() -> String {
    "0644".to_owned()
}

pub(crate) fn default_ca_mode() -> String {
    "0644".to_owned()
}

pub(crate) fn default_key_mode() -> String {
    "0600".to_owned()
}

impl LocalInput {
    /// 要写入的文件：路径、内容、权限。
    ///
    /// 3 文件语义：fullchain（`chain_pem` 原样）、ca（拆链剩余，显式留空或
    /// 链仅单张时跳过）、key（`key_pem` 原样）。路径为目录时按默认文件名
    /// （`fullchain.cer`／`ca.cer`／`key`）写入该目录。
    fn files(&self, materials: &CertMaterials) -> Vec<(FileSpec<'_>, String)> {
        let fullchain_path = resolve_target_path(&self.fullchain_path, "fullchain.cer");
        let ca_path = match self.ca_path.as_deref() {
            // 显式留空（空串）：跳过 ca 文件的写入。
            Some(path) if path.as_os_str().is_empty() => None,
            Some(path) => Some(resolve_target_path(path, "ca.cer")),
            // 未配置：按 fullchain 同目录派生 `<stem>.ca.cer`。
            None => Some(derive_ca_path(&fullchain_path)),
        };

        let mut files = Vec::with_capacity(3);
        files.push((
            FileSpec {
                path: fullchain_path,
                mode: &self.cert_mode,
                field: "cert_mode",
            },
            materials.chain_pem.clone(),
        ));
        if let Some(path) = ca_path {
            let content = ca_pem_of(&materials.chain_pem);
            // 链里只有叶子证书时没有可写的中间链，跳过（与显式留空一致）。
            if !content.is_empty() {
                files.push((
                    FileSpec {
                        path,
                        mode: &self.ca_mode,
                        field: "ca_mode",
                    },
                    content,
                ));
            }
        }
        files.push((
            FileSpec {
                path: resolve_target_path(&self.key_path, "key"),
                mode: &self.key_mode,
                field: "key_mode",
            },
            materials.key_pem.clone(),
        ));
        files
    }
}

/// 目标路径是目录时按默认文件名写入该目录。
///
/// 目录的判定：`is_dir()` 为真，或路径以 `/` 结尾——后者用于目录尚未
/// 创建、但用户明确按目录写的场景。
fn resolve_target_path(path: &Path, default_name: &str) -> PathBuf {
    let ends_with_separator = path.as_os_str().to_string_lossy().ends_with('/');
    if path.is_dir() || ends_with_separator {
        path.join(default_name)
    } else {
        path.to_path_buf()
    }
}

/// 未配置 `ca_path` 时按 fullchain 同目录派生 `<stem>.ca.cer`。
///
/// `example.com.fullchain.cer` → `example.com.ca.cer`；按默认文件名落盘的
/// `fullchain.cer` → 同目录 `ca.cer`。
fn derive_ca_path(fullchain_path: &Path) -> PathBuf {
    let stem = fullchain_path
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_default();
    let name = if stem == "fullchain" {
        "ca.cer".to_owned()
    } else {
        let prefix = stem.strip_suffix(".fullchain").unwrap_or(&stem);
        format!("{prefix}.ca.cer")
    };
    fullchain_path.with_file_name(name)
}

/// 补全本地路径字段的目录写法。
///
/// 仅以 `/` 结尾判定目录（不做 `is_dir()` 探测，与 SSH/S3/WebDAV 同口径）；
/// 保存时由仓储层做同一补全，这里作为未重新保存的历史配置的执行期兜底。
/// 主域前缀已知（部署时从证书材料解析）时补 `<prefix>.fullchain.cer` 等
/// 带前缀文件名，未知时回落静态名。`cert_path` 是 `fullchain_path` 的旧
/// 别名，存量单键配置同样处理。
pub(crate) fn normalize_local_config(value: &mut Value, prefix: Option<&str>) {
    let Some(object) = value.as_object_mut() else {
        return;
    };
    let [fullchain_name, ca_name, key_name] = super::default_file_names(prefix);
    for (field, default_name) in [
        ("fullchain_path", fullchain_name.as_str()),
        ("cert_path", fullchain_name.as_str()),
        ("ca_path", ca_name.as_str()),
        ("key_path", key_name.as_str()),
    ] {
        super::fill_directory_field(object, field, default_name);
    }
}

/// 兼容存量输入：`fullchain_path` 与旧别名 `cert_path` 同时出现时丢弃旧键。
///
/// serde 的 `alias` 把两者视作同一个字段，双键同交会报 `duplicate field`；
/// 存量步骤经前端别名回显后保存时两个键都会带上，这里兜底去重（新键优先）。
fn without_legacy_alias(input: &Value) -> Value {
    let mut value = input.clone();
    if let Some(object) = value.as_object_mut()
        && object
            .get("fullchain_path")
            .is_some_and(|value| !value.is_null())
    {
        object.remove("cert_path");
    }
    value
}

/// 一个待写入的文件。
#[derive(Debug)]
struct FileSpec<'a> {
    path: PathBuf,
    mode: &'a str,
    /// 出错的权限该报哪个字段名。
    field: &'static str,
}

#[async_trait]
impl DeploymentTarget for LocalTarget {
    fn type_id(&self) -> &'static str {
        "local"
    }

    fn display_name(&self) -> &'static str {
        "本地文件系统"
    }

    fn input_schema(&self) -> RootSchema {
        schema_for!(LocalInput)
    }

    fn example_input(&self) -> Value {
        serde_json::json!({
            "fullchain_path": "/etc/nginx/ssl/example.com.fullchain.cer",
            "ca_path": "/etc/nginx/ssl/example.com.ca.cer",
            "key_path": "/etc/nginx/ssl/example.com.key",
            "reload_command": "nginx -s reload"
        })
    }

    async fn deploy(
        &self,
        input: &Value,
        materials: &CertMaterials,
        _credentials: &acmecast_access::CredentialStore<'_>,
        mode: DeployMode,
    ) -> Result<DeployOutcome> {
        // 目录写法在保存时已归一化；这里再补一次，覆盖未重新保存的历史配置。
        // 主域取本次部署的证书材料，与保存时按 `cert.apply` 主域补全同源。
        let mut normalized = input.clone();
        normalize_local_config(
            &mut normalized,
            super::primary_domain_prefix(materials).as_deref(),
        );
        let input: LocalInput = parse_input(&without_legacy_alias(&normalized))?;
        let files = input.files(materials);

        let mut paths = Vec::with_capacity(files.len());
        if mode == DeployMode::Write {
            for (spec, content) in &files {
                write_file(spec, content, input.uid, input.gid).await?;
                paths.push(spec.path.display().to_string());
            }
        } else {
            // 跳过写入，但路径照实列出——调用方要知道这次碰的是哪些文件。
            for (spec, _) in &files {
                paths.push(spec.path.display().to_string());
            }
        }

        let outcome = if mode == DeployMode::SkipWrite {
            DeployOutcome::skipped(paths)
        } else {
            DeployOutcome::written(paths)
        };

        // 无论是否跳过写入，重载都要走过——否则「证书换了、服务没重新加载」会一直持续到下次部署。
        match input.reload_command.as_deref() {
            Some(command) => Ok(outcome.with_reload_output(run_reload(command).await?)),
            None => Ok(outcome),
        }
    }
}

/// 执行重载命令，返回其合并输出。
///
/// 非 0 退出码视为失败。注意此时文件**已经写好了**——spec 只要求把整个
/// 部署步骤标记为失败，回滚（8.6 的原子替换）另论。
async fn run_reload(command: &str) -> Result<String> {
    let child = if cfg!(windows) {
        tokio::process::Command::new("cmd")
            .arg("/C")
            .arg(command)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
    } else {
        tokio::process::Command::new("sh")
            .arg("-c")
            .arg(command)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
    }
    .map_err(|e| Error::Write {
        path: "(重载命令)".to_owned(),
        reason: format!("无法启动 `{command}`: {e}"),
    })?;

    let output = child.wait_with_output().await.map_err(|e| Error::Write {
        path: "(重载命令)".to_owned(),
        reason: format!("等待 `{command}` 结束失败: {e}"),
    })?;

    let combined = combined_output(&output);

    if !output.status.success() {
        return Err(Error::Reload {
            command: command.to_owned(),
            exit_code: output.status.code(),
            output: combined,
        });
    }

    Ok(combined)
}

/// 把 stdout 与 stderr 合并成一段文本。
///
/// 不区分来源：重载脚本往往把「配置里哪一行有问题」打进 stderr，
/// 而 stdout 可能什么都没有——只留 stdout 会把最有用的信息丢掉。
fn combined_output(output: &std::process::Output) -> String {
    let mut combined = String::new();
    combined.push_str(&String::from_utf8_lossy(&output.stdout));
    if !output.stderr.is_empty() {
        if !combined.is_empty() && !combined.ends_with('\n') {
            combined.push('\n');
        }
        combined.push_str(&String::from_utf8_lossy(&output.stderr));
    }

    if combined.trim().is_empty() {
        return "(无输出)".to_owned();
    }
    combined
}

/// 写一个文件：建父目录、**先写临时文件再原子替换**。
///
/// 不直接往目标路径写，是因为中途出错时目标文件会只剩半截内容——而它可能
/// 正被一个运行中的 web 服务器读取。临时文件放在**同一目录**下：`rename` 只有
/// 在同一文件系统内才是原子的，跨文件系统会退化成「复制 + 删除」，
/// 那份中途状态恰恰是要避免的。
async fn write_file(
    spec: &FileSpec<'_>,
    content: &str,
    uid: Option<u32>,
    gid: Option<u32>,
) -> Result<()> {
    let mode = parse_mode(spec.mode, spec.field)?;

    if let Some(parent) = spec.path.parent()
        && !parent.as_os_str().is_empty()
    {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|e| Error::Write {
                path: parent.display().to_string(),
                reason: format!("无法创建父目录: {e}"),
            })?;
    }

    let temp = temp_path_for(&spec.path);
    match write_then_replace(&temp, &spec.path, content, mode, uid, gid).await {
        Ok(()) => Ok(()),
        Err(err) => {
            // 失败时把临时产物清掉：留着它既占地方，也会让下一次部署与排查更乱。
            remove_quietly(&temp).await;
            Err(err)
        }
    }
}

/// 写临时文件、设好权限与属主，然后原子替换到目标。
async fn write_then_replace(
    temp: &Path,
    target: &Path,
    content: &str,
    mode: u32,
    uid: Option<u32>,
    gid: Option<u32>,
) -> Result<()> {
    write_with_mode(temp, content, mode).await?;

    // 属主也设在临时文件上：这样目标文件一出现就是对的，不会有一个
    // 「内容已是新证书、属主还是写入者」的窗口。
    set_owner(temp, uid, gid).await?;

    tokio::fs::rename(temp, target)
        .await
        .map_err(|e| Error::Write {
            path: target.display().to_string(),
            reason: format!("原子替换失败: {e}"),
        })
}

/// 为原子替换准备临时文件路径。
///
/// 名字里带随机串：两个并发部署写同一个目标时，各自的临时文件不会互相踩。
#[must_use]
pub fn temp_path_for(target: &Path) -> PathBuf {
    let name = target.file_name().map_or_else(
        || "out".to_owned(),
        |name| name.to_string_lossy().into_owned(),
    );

    target.with_file_name(format!(
        ".{name}.acmecast-{}.tmp",
        uuid::Uuid::new_v4().simple()
    ))
}

/// 删掉一个文件，删不掉时只记 warn。
///
/// 清理发生在失败路径上，此时真正要紧的是原始错误；为「临时文件没删掉」
/// 覆盖掉它，只会让排查更困难。
async fn remove_quietly(path: &Path) {
    match tokio::fs::remove_file(path).await {
        Ok(()) => {}
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => {
            tracing::warn!(path = %path.display(), error = %err, "临时文件清理失败");
        }
    }
}

/// 以指定权限创建并写入。
///
/// 权限在**创建时**设定，而不是落盘后再 chmod：后者存在一个短暂窗口，
/// 文件已是 umask 默认权限（通常 0644）——对私钥来说那是真正的泄漏窗口。
async fn write_with_mode(path: &Path, content: &str, mode: u32) -> Result<()> {
    let mut options = tokio::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    restrict_new_file_mode(&mut options, mode);

    let mut file = options.open(path).await.map_err(|e| Error::Write {
        path: path.display().to_string(),
        reason: e.to_string(),
    })?;

    tokio::io::AsyncWriteExt::write_all(&mut file, content.as_bytes())
        .await
        .map_err(|e| Error::Write {
            path: path.display().to_string(),
            reason: e.to_string(),
        })?;

    // 已存在的文件不会因 `mode` 改名权限，因此再显式设一次——
    // 用同一个值，覆盖「上次权限被改过」的情况。
    set_mode(path, mode).await
}

#[cfg(unix)]
fn restrict_new_file_mode(options: &mut tokio::fs::OpenOptions, mode: u32) {
    options.mode(mode);
}

#[cfg(not(unix))]
fn restrict_new_file_mode(_options: &mut tokio::fs::OpenOptions, _mode: u32) {}

/// 设置文件权限。
#[cfg(unix)]
async fn set_mode(path: &Path, mode: u32) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
        .await
        .map_err(|e| Error::Write {
            path: path.display().to_string(),
            reason: format!("无法设置权限: {e}"),
        })
}

/// 非 Unix 平台没有 POSIX 权限位。
#[cfg(not(unix))]
async fn set_mode(_path: &Path, _mode: u32) -> Result<()> {
    Ok(())
}

/// 设置属主。
///
/// 只在配置了 uid/gid 时才调用——不改属主时连系统调用都不必发。
#[cfg(unix)]
async fn set_owner(path: &Path, uid: Option<u32>, gid: Option<u32>) -> Result<()> {
    if uid.is_none() && gid.is_none() {
        return Ok(());
    }

    nix::unistd::chown(
        path,
        uid.map(nix::unistd::Uid::from_raw),
        gid.map(nix::unistd::Gid::from_raw),
    )
    .map_err(|e| Error::Write {
        path: path.display().to_string(),
        // 这条提示很实用：chown 到自己以外的用户需要特权，
        // 而失败现场只有一个光秃秃的 EPERM。
        reason: format!("无法设置属主（改成他人所有通常需要以 root 运行）: {e}"),
    })
}

#[cfg(not(unix))]
async fn set_owner(_path: &Path, _uid: Option<u32>, _gid: Option<u32>) -> Result<()> {
    Err(Error::invalid_input("uid", "当前平台不支持设置文件属主"))
}

/// 把八进制权限字符串解析成数值。
fn parse_mode(text: &str, field: &str) -> Result<u32> {
    let trimmed = text.trim();
    u32::from_str_radix(trimmed, 8).map_err(|e| {
        Error::invalid_input(
            field,
            format!("`{trimmed}` 不是合法的八进制权限（应形如 0644）: {e}"),
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn octal_modes_parse() {
        assert_eq!(parse_mode("0644", "cert_mode").unwrap(), 0o644);
        assert_eq!(parse_mode(" 600 ", "key_mode").unwrap(), 0o600);
        // 不写前导零也能认。
        assert_eq!(parse_mode("755", "cert_mode").unwrap(), 0o755);
    }

    #[test]
    fn an_invalid_mode_names_its_field() {
        let err = parse_mode("rw-r--r--", "cert_mode").expect_err("非八进制应被拒绝");
        assert!(err.to_string().contains("cert_mode"), "{err}");

        // 八进制里没有 8 和 9。
        let err = parse_mode("0899", "key_mode").expect_err("非法八进制应被拒绝");
        assert!(err.to_string().contains("key_mode"), "{err}");
    }

    #[test]
    fn the_defaults_are_conservative() {
        // 私钥默认比证书严。
        assert_eq!(default_key_mode(), "0600");
        assert_eq!(default_cert_mode(), "0644");
        assert_eq!(default_ca_mode(), "0644");
    }

    #[test]
    fn the_input_definition_carries_three_paths() {
        let rendered = serde_json::to_string(&LocalTarget.input_schema()).unwrap();
        assert!(rendered.contains("fullchain_path"), "{rendered}");
        assert!(rendered.contains("ca_path"), "{rendered}");
        assert!(rendered.contains("key_path"), "{rendered}");
        // 旧字段名经 serde alias 兼容，但不再是 schema 的一部分。
        assert!(!rendered.contains("cert_path"), "{rendered}");
        // 路径是必填，权限有默认值。
        assert!(rendered.contains("required"), "{rendered}");
    }

    // ---- ca 路径派生 ----

    #[test]
    fn the_derived_ca_path_replaces_the_fullchain_marker() {
        assert_eq!(
            derive_ca_path(Path::new("/etc/nginx/ssl/example.com.fullchain.cer")),
            PathBuf::from("/etc/nginx/ssl/example.com.ca.cer")
        );
    }

    #[test]
    fn the_default_fullchain_name_derives_the_default_ca_name() {
        assert_eq!(
            derive_ca_path(Path::new("/etc/nginx/ssl/fullchain.cer")),
            PathBuf::from("/etc/nginx/ssl/ca.cer")
        );
    }

    #[test]
    fn a_name_without_the_fullchain_marker_still_derives_a_ca_name() {
        assert_eq!(
            derive_ca_path(Path::new("/etc/ssl/cert.pem")),
            PathBuf::from("/etc/ssl/cert.ca.cer")
        );
    }

    // ---- 目录路径 ----

    #[test]
    fn a_trailing_separator_marks_a_directory() {
        assert_eq!(
            resolve_target_path(Path::new("/etc/nginx/ssl/"), "fullchain.cer"),
            PathBuf::from("/etc/nginx/ssl/fullchain.cer")
        );
    }

    #[test]
    fn an_existing_directory_gets_the_default_file_name() {
        let dir = std::env::temp_dir().join(format!("acmecast-resolve-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();

        assert_eq!(resolve_target_path(&dir, "key"), dir.join("key"));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn directory_fields_are_filled_before_parsing() {
        let mut input = serde_json::json!({
            "fullchain_path": "/data/certs/ssl/",
            "ca_path": "/data/certs/ssl/",
            "key_path": "/data/certs/ssl/",
            "reload_command": "nginx -s reload"
        });

        normalize_local_config(&mut input, None);

        assert_eq!(input["fullchain_path"], "/data/certs/ssl/fullchain.cer");
        assert_eq!(input["ca_path"], "/data/certs/ssl/ca.cer");
        assert_eq!(input["key_path"], "/data/certs/ssl/key");
        assert_eq!(
            input["reload_command"], "nginx -s reload",
            "非路径字段不受影响"
        );
    }

    #[test]
    fn a_known_primary_domain_prefills_prefixed_names() {
        let mut input = serde_json::json!({
            "fullchain_path": "/tmp/t1/",
            "ca_path": "/tmp/t1/",
            "key_path": "/tmp/t1/"
        });

        normalize_local_config(&mut input, Some("_wildcard.example.com"));

        assert_eq!(
            input["fullchain_path"],
            "/tmp/t1/_wildcard.example.com.fullchain.cer"
        );
        assert_eq!(input["ca_path"], "/tmp/t1/_wildcard.example.com.ca.cer");
        assert_eq!(input["key_path"], "/tmp/t1/_wildcard.example.com.key");
    }

    #[test]
    fn non_directory_and_blank_paths_are_left_alone() {
        let mut input = serde_json::json!({
            "fullchain_path": "/data/certs/fullchain.pem",
            "ca_path": "  ",
            "key_path": "/data/certs/key.pem"
        });

        normalize_local_config(&mut input, None);

        assert_eq!(input["fullchain_path"], "/data/certs/fullchain.pem");
        assert_eq!(input["ca_path"], "  ");
        assert_eq!(input["key_path"], "/data/certs/key.pem");
    }

    #[test]
    fn the_schema_explains_the_directory_convention() {
        let rendered = serde_json::to_string(&LocalTarget.input_schema()).unwrap();
        assert!(rendered.contains("以 `/` 结尾视为目录"), "{rendered}");
        assert!(rendered.contains("fullchain.cer"), "{rendered}");
    }

    #[test]
    fn a_plain_path_is_taken_as_a_file() {
        assert_eq!(
            resolve_target_path(Path::new("/etc/nginx/ssl/fullchain.cer"), "fullchain.cer"),
            PathBuf::from("/etc/nginx/ssl/fullchain.cer")
        );
    }

    // ---- 旧别名兼容 ----

    #[test]
    fn the_legacy_cert_path_alias_still_deserializes() {
        let input: LocalInput = serde_json::from_value(serde_json::json!({
            "cert_path": "/etc/nginx/ssl/example.com.fullchain.cer",
            "key_path": "/etc/nginx/ssl/example.com.key",
        }))
        .expect("旧别名应仍可解析");

        assert_eq!(
            input.fullchain_path,
            PathBuf::from("/etc/nginx/ssl/example.com.fullchain.cer")
        );
        assert_eq!(input.ca_path, None, "未配置 ca_path 时保持 None");
    }

    #[test]
    fn a_stale_duplicate_cert_path_is_dropped_before_parsing() {
        // 前端回显旧值（`cert_path`）又写入新值（`fullchain_path`）后，保存的配置
        // 里两个键同在；不去重的话 serde alias 会报 `duplicate field`。
        let input = serde_json::json!({
            "fullchain_path": "/new/fullchain.cer",
            "cert_path": "/old/cert.pem",
            "key_path": "/new/key.pem",
        });
        let cleaned = without_legacy_alias(&input);
        assert!(cleaned.get("cert_path").is_none(), "旧键应被丢弃");

        let parsed: LocalInput = parse_input(&cleaned).expect("去重后应能解析");
        assert_eq!(parsed.fullchain_path, PathBuf::from("/new/fullchain.cer"));
    }

    #[test]
    fn a_lone_legacy_cert_path_survives_deduplication() {
        // 只有旧键的存量配置不能被当成重复键误删。
        let input = serde_json::json!({
            "cert_path": "/old/cert.pem",
            "key_path": "/old/key.pem",
        });
        let cleaned = without_legacy_alias(&input);

        let parsed: LocalInput = parse_input(&cleaned).expect("旧配置应能解析");
        assert_eq!(parsed.fullchain_path, PathBuf::from("/old/cert.pem"));
    }

    // ---- 文件清单 ----

    /// 叶子 + 中间两段证书的链：ca 文件的写入需要它。
    fn chained_materials() -> CertMaterials {
        CertMaterials::new(
            concat!(
                "-----BEGIN CERTIFICATE-----\n叶子\n-----END CERTIFICATE-----\n",
                "-----BEGIN CERTIFICATE-----\n中间\n-----END CERTIFICATE-----\n"
            ),
            "-----BEGIN PRIVATE KEY-----\n钥\n-----END PRIVATE KEY-----\n",
            "sha256:abc",
        )
    }

    fn single_cert_materials() -> CertMaterials {
        CertMaterials::new(
            "-----BEGIN CERTIFICATE-----\n单张\n-----END CERTIFICATE-----\n",
            "-----BEGIN PRIVATE KEY-----\n钥\n-----END PRIVATE KEY-----\n",
            "sha256:abc",
        )
    }

    #[test]
    fn files_derive_the_ca_path_when_it_is_absent() {
        let input: LocalInput = parse_input(&serde_json::json!({
            "fullchain_path": "/etc/nginx/ssl/example.com.fullchain.cer",
            "key_path": "/etc/nginx/ssl/example.com.key",
        }))
        .unwrap();
        let materials = chained_materials();

        let files = input.files(&materials);
        let paths: Vec<String> = files
            .iter()
            .map(|(spec, _)| spec.path.display().to_string())
            .collect();
        assert_eq!(
            paths,
            vec![
                "/etc/nginx/ssl/example.com.fullchain.cer",
                "/etc/nginx/ssl/example.com.ca.cer",
                "/etc/nginx/ssl/example.com.key",
            ],
            "未配置 ca_path 时应按 fullchain 同目录派生"
        );

        // 内容：fullchain 为原链，ca 为拆链剩余，key 原样。
        assert_eq!(files[0].1, materials.chain_pem);
        assert_eq!(files[1].1, ca_pem_of(&materials.chain_pem));
        assert!(files[1].1.contains("中间"), "ca 文件应含中间证书");
        assert_eq!(files[2].1, materials.key_pem);
    }

    #[test]
    fn an_explicitly_blank_ca_path_skips_the_ca_file() {
        let input: LocalInput = parse_input(&serde_json::json!({
            "fullchain_path": "/etc/nginx/ssl/example.com.fullchain.cer",
            "ca_path": "",
            "key_path": "/etc/nginx/ssl/example.com.key",
        }))
        .unwrap();

        let files = input.files(&chained_materials());
        assert_eq!(files.len(), 2, "显式留空时只写 fullchain 与 key");
    }

    #[test]
    fn a_single_cert_chain_has_no_ca_file_to_write() {
        let input: LocalInput = parse_input(&serde_json::json!({
            "fullchain_path": "/etc/nginx/ssl/example.com.fullchain.cer",
            "key_path": "/etc/nginx/ssl/example.com.key",
        }))
        .unwrap();

        let files = input.files(&single_cert_materials());
        assert_eq!(files.len(), 2, "链里没有中间证书时跳过 ca");
        let paths: Vec<String> = files
            .iter()
            .map(|(spec, _)| spec.path.display().to_string())
            .collect();
        assert_eq!(
            paths,
            vec![
                "/etc/nginx/ssl/example.com.fullchain.cer",
                "/etc/nginx/ssl/example.com.key",
            ]
        );
    }
}
