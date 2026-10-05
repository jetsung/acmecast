//! 本地文件系统部署。
//!
//! 把证书链与私钥写到服务所在主机的指定路径上，并按配置设置文件权限与属主。

use std::path::{Path, PathBuf};

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
    /// 证书链的写入路径。
    pub cert_path: PathBuf,
    /// 私钥的写入路径。
    pub key_path: PathBuf,
    /// 证书文件的权限，八进制写法，如 `"0644"`。
    #[serde(default = "default_cert_mode")]
    pub cert_mode: String,
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

pub(crate) fn default_key_mode() -> String {
    "0600".to_owned()
}

impl LocalInput {
    /// 要写入的两个文件：路径、内容、权限。
    fn files<'a>(&'a self, materials: &'a CertMaterials) -> [(FileSpec<'a>, &'a str); 2] {
        [
            (
                FileSpec {
                    path: &self.cert_path,
                    mode: &self.cert_mode,
                    field: "cert_mode",
                },
                &materials.chain_pem,
            ),
            (
                FileSpec {
                    path: &self.key_path,
                    mode: &self.key_mode,
                    field: "key_mode",
                },
                &materials.key_pem,
            ),
        ]
    }
}

/// 一个待写入的文件。
#[derive(Debug)]
struct FileSpec<'a> {
    path: &'a Path,
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
            "cert_path": "/etc/nginx/ssl/example.com.crt",
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
        let input: LocalInput = parse_input(input)?;

        let mut paths = Vec::with_capacity(2);
        if mode == DeployMode::Write {
            for (spec, content) in input.files(materials) {
                write_file(&spec, content, input.uid, input.gid).await?;
                paths.push(spec.path.display().to_string());
            }
        } else {
            // 跳过写入，但路径照实列出——调用方要知道这次碰的是哪些文件。
            for (spec, _) in input.files(materials) {
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

    let temp = temp_path_for(spec.path);
    match write_then_replace(&temp, spec.path, content, mode, uid, gid).await {
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
    }

    #[test]
    fn the_input_definition_carries_both_paths() {
        let rendered = serde_json::to_string(&LocalTarget.input_schema()).unwrap();
        assert!(rendered.contains("cert_path"), "{rendered}");
        assert!(rendered.contains("key_path"), "{rendered}");
        // 路径是必填，权限有默认值。
        assert!(rendered.contains("required"), "{rendered}");
    }
}
