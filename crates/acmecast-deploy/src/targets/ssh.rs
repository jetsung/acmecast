//! SSH 远程部署。
//!
//! 通过 SSH 把证书写到远程主机，支持私钥与口令两种认证。
//!
//! **两种认证的凭据都来自凭据系统**，不放在部署输入里：明文一旦进了输入，
//! 就会跟着流水线配置与运行历史一起扩散。连接与命令通道被抽象成 [`SshTransport`]，
//! 这样「写到哪个路径、权限是什么、重载何时执行」这些逻辑可以在没有真实 SSH
//! 服务器的情况下被验证——CI 里起一台 sshd 既不现实也不稳定。

use std::sync::Arc;

use acmecast_access::{CredentialStore, ResolvedCredential};
use async_trait::async_trait;
use schemars::schema::RootSchema;
use schemars::schema_for;
use serde::Deserialize;
use serde_json::Value;

use crate::error::{Error, Result};
use crate::target::{CertMaterials, DeployMode, DeployOutcome, DeploymentTarget, parse_input};
use crate::targets::local::{default_cert_mode, default_key_mode};

/// 登录远程主机的方式。
///
/// 两种都只带**凭据标识**，真正的值在执行时从凭据系统取。
#[derive(Debug, Clone, Deserialize, schemars::JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SshAuth {
    /// 私钥认证：私钥 PEM 来自凭据系统。
    PrivateKey {
        /// 私钥凭据的标识。
        credential_id: i64,
    },
    /// 口令认证：口令来自凭据系统。
    Password {
        /// 口令凭据的标识。
        credential_id: i64,
    },
}

impl SshAuth {
    /// 要取的凭据标识。
    #[must_use]
    pub fn credential_id(&self) -> i64 {
        match self {
            Self::PrivateKey { credential_id } | Self::Password { credential_id } => *credential_id,
        }
    }
}

/// SSH 部署的输入。
#[derive(Debug, Clone, Deserialize, schemars::JsonSchema)]
pub struct SshInput {
    /// 目标主机。
    pub host: String,
    /// SSH 端口。
    #[serde(default = "default_port")]
    pub port: u16,
    /// 登录用户。
    pub user: String,
    /// 认证方式。
    pub auth: SshAuth,
    /// 远程的证书路径。
    pub cert_path: String,
    /// 远程的私钥路径。
    pub key_path: String,
    /// 远程证书文件的权限。
    #[serde(default = "default_cert_mode")]
    pub cert_mode: String,
    /// 远程私钥文件的权限。
    #[serde(default = "default_key_mode")]
    pub key_mode: String,
    /// 写入成功后在**远程**执行的重载命令。
    #[serde(default)]
    pub reload_command: Option<String>,
}

fn default_port() -> u16 {
    22
}

/// 一个已建立的远程连接。
#[async_trait]
pub trait SshTransport: Send + Sync {
    /// 把内容写到远程路径，并设成给定权限。
    async fn write_file(&self, path: &str, content: &str, mode: &str) -> Result<()>;

    /// 执行远程命令，返回（退出码、合并输出）。
    async fn exec(&self, command: &str) -> Result<(i32, String)>;
}

/// 建立远程连接的方式。
#[async_trait]
pub trait SshConnector: Send + Sync + std::fmt::Debug {
    /// 按输入连上远程主机并完成认证。
    ///
    /// 私钥／口令在**这里**才从凭据系统取出，因此它们既不经过部署输入，
    /// 也不需要被长时间持有。
    async fn connect(
        &self,
        input: &SshInput,
        credentials: &CredentialStore<'_>,
    ) -> Result<Box<dyn SshTransport>>;
}

/// SSH 远程部署。
pub struct SshTarget {
    connector: Arc<dyn SshConnector>,
}

impl SshTarget {
    /// 用给定的连接器装配。
    #[must_use]
    pub fn with_connector(connector: Arc<dyn SshConnector>) -> Self {
        Self { connector }
    }

    /// 走真实 SSH（用 russh）。
    #[must_use]
    pub fn live() -> Self {
        Self::with_connector(Arc::new(RusshConnector))
    }
}

impl std::fmt::Debug for SshTarget {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SshTarget")
            .field("connector", &self.connector)
            .finish()
    }
}

#[async_trait]
impl DeploymentTarget for SshTarget {
    fn type_id(&self) -> &'static str {
        "ssh"
    }

    fn display_name(&self) -> &'static str {
        "SSH 远程主机"
    }

    fn input_schema(&self) -> RootSchema {
        schema_for!(SshInput)
    }

    async fn deploy(
        &self,
        input: &Value,
        materials: &CertMaterials,
        credentials: &CredentialStore<'_>,
        mode: DeployMode,
    ) -> Result<DeployOutcome> {
        let input: SshInput = parse_input(input)?;

        let transport = self.connector.connect(&input, credentials).await?;

        let mut paths: Vec<String> = if mode == DeployMode::Write {
            Vec::with_capacity(2)
        } else {
            vec![input.cert_path.clone(), input.key_path.clone()]
        };

        for (path, content, file_mode) in [
            (&input.cert_path, &materials.chain_pem, &input.cert_mode),
            (&input.key_path, &materials.key_pem, &input.key_mode),
        ] {
            transport
                .write_file(path, content, file_mode)
                .await
                .map_err(|e| Error::Write {
                    path: format!("{}:{}", input.host, path),
                    reason: e.to_string(),
                })?;
            paths.push(path.clone());
        }

        let outcome = if mode == DeployMode::SkipWrite {
            DeployOutcome::skipped(paths)
        } else {
            DeployOutcome::written(paths)
        };

        match input.reload_command.as_deref() {
            Some(command) => {
                let (exit_code, output) = transport.exec(command).await?;
                if exit_code != 0 {
                    return Err(Error::Reload {
                        command: command.to_owned(),
                        exit_code: Some(exit_code),
                        output,
                    });
                }
                Ok(outcome.with_reload_output(output))
            }
            None => Ok(outcome),
        }
    }
}

/// 走 russh 的连接器。
#[derive(Debug, Default)]
pub struct RusshConnector;

#[async_trait]
impl SshConnector for RusshConnector {
    async fn connect(
        &self,
        input: &SshInput,
        credentials: &CredentialStore<'_>,
    ) -> Result<Box<dyn SshTransport>> {
        let credential = resolve_auth_credential(input, credentials).await?;

        let config = Arc::new(russh::client::Config::default());
        let mut session =
            russh::client::connect(config, (input.host.as_str(), input.port), ClientHandler)
                .await
                .map_err(|e| {
                    Error::Remote(format!("连接 {}:{} 失败: {e}", input.host, input.port))
                })?;

        // 私钥与口令的字段分别读；缺哪个就报哪个，比一句「认证失败」有用。
        match &input.auth {
            SshAuth::PrivateKey { .. } => {
                let pem = credential_field(&credential, "private_key")?;
                let key = parse_private_key(&pem)?;
                let result = session
                    .authenticate_publickey(&input.user, key)
                    .await
                    .map_err(|e| Error::Remote(format!("私钥认证失败: {e}")))?;

                ensure_authenticated(result)?;
            }
            SshAuth::Password { .. } => {
                let password = credential_field(&credential, "password")?;
                let result = session
                    .authenticate_password(&input.user, password)
                    .await
                    .map_err(|e| Error::Remote(format!("口令认证失败: {e}")))?;

                ensure_authenticated(result)?;
            }
        }

        Ok(Box::new(RusshTransport { session }))
    }
}

/// 取出本次认证要用的凭据。
async fn resolve_auth_credential(
    input: &SshInput,
    credentials: &CredentialStore<'_>,
) -> Result<ResolvedCredential> {
    let id = input.auth.credential_id();
    credentials.resolve(id).await.map_err(|e| {
        Error::Remote(format!(
            "取 {} 认证用的凭据 `{id}` 失败: {e}",
            match &input.auth {
                SshAuth::PrivateKey { .. } => "私钥",
                SshAuth::Password { .. } => "口令",
            }
        ))
    })
}

/// 从凭据里取一个字段值。
fn credential_field(credential: &ResolvedCredential, field: &str) -> Result<String> {
    credential
        .fields
        .get(field)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| {
            Error::invalid_input(
                "auth",
                format!("凭据 `{}` 里没有 `{field}` 字段", credential.id),
            )
        })
}

/// 把认证结果判成成功或失败。
fn ensure_authenticated(result: russh::client::AuthResult) -> Result<()> {
    if result.success() {
        Ok(())
    } else {
        Err(Error::Remote("认证被远程主机拒绝".to_owned()))
    }
}

/// 解析 OpenSSH 格式的私钥 PEM。
fn parse_private_key(pem: &str) -> Result<russh::keys::PrivateKeyWithHashAlg> {
    use russh::keys::{PrivateKey, PrivateKeyWithHashAlg};

    let key = PrivateKey::from_openssh(pem.as_bytes())
        .map_err(|e| Error::Remote(format!("私钥无法解析（应为 OpenSSH 或 PEM 格式）: {e}")))?;

    Ok(PrivateKeyWithHashAlg::new(Arc::new(key), None))
}

/// russh 的回调：这里只做主机密钥的处置决定。
#[derive(Debug)]
struct ClientHandler;

impl russh::client::Handler for ClientHandler {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        server_public_key: &russh::keys::PublicKeyOrCertificate,
    ) -> std::result::Result<bool, Self::Error> {
        // 首版尚未接入 known_hosts，因此接受任何主机密钥。
        // 这意味着存在中间人风险，留一条 warn 让它在日志里可被发现。
        tracing::warn!(
            fingerprint = %server_public_key
                .public_key()
                .fingerprint(russh::keys::HashAlg::Sha256),
            "未校验 SSH 主机指纹（known_hosts 支持尚未实现），接受该主机"
        );
        Ok(true)
    }
}

/// 已认证的 russh 会话。
struct RusshTransport {
    session: russh::client::Handle<ClientHandler>,
}

/// 把一段文本转成带引号的 shell 参数。
fn shell_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', "'\\''"))
}

#[async_trait]
impl SshTransport for RusshTransport {
    async fn write_file(&self, path: &str, content: &str, mode: &str) -> Result<()> {
        // 与本地部署同构：先写临时文件再原子替换。`trap` 保证任何一步失败时
        // 临时产物都被清掉，而目标文件保持原样——远端文件同样可能正被 web 服务器读着。
        //
        // 通过 stdin 灌进 `cat`，避免把整份证书塞进命令行参数——
        // 参数长度有限制，而且会出现在远端的 ps 输出里。
        let temp = format!("{path}.acmecast-{}.tmp", uuid::Uuid::new_v4().simple());
        let script = format!(
            "set -e\n\
             trap 'rm -f {temp}' 0\n\
             cat > {temp} <<'ACMECAST_EOF'\n{content}\nACMECAST_EOF\n\
             chmod {mode} {temp}\n\
             mv {temp} {path}\n\
             trap - 0\n",
            temp = shell_quote(&temp),
            path = shell_quote(path),
            content = content,
            mode = shell_quote(mode),
        );

        let (exit_code, output) = run_channel(&self.session, &script).await?;
        if exit_code != 0 {
            return Err(Error::Write {
                path: path.to_owned(),
                reason: format!("远端写入失败（退出码 {exit_code}）: {output}"),
            });
        }
        Ok(())
    }

    async fn exec(&self, command: &str) -> Result<(i32, String)> {
        run_channel(&self.session, command).await
    }
}

/// 开一个会话通道执行脚本，返回退出码与合并输出。
async fn run_channel(
    session: &russh::client::Handle<ClientHandler>,
    script: &str,
) -> Result<(i32, String)> {
    let mut channel = session
        .channel_open_session()
        .await
        .map_err(|e| Error::Remote(format!("打开会话通道失败: {e}")))?;

    channel
        .exec(true, script)
        .await
        .map_err(|e| Error::Remote(format!("执行命令失败: {e}")))?;

    let mut collected = Vec::new();
    let mut exit_code = 0_i32;

    while let Some(message) = channel.wait().await {
        match message {
            russh::ChannelMsg::Data { data, .. } | russh::ChannelMsg::ExtendedData { data, .. } => {
                collected.extend_from_slice(&data);
            }
            russh::ChannelMsg::ExitStatus { exit_status } => {
                exit_code = exit_status as i32;
                break;
            }
            russh::ChannelMsg::Eof => break,
            _ => {}
        }
    }

    channel
        .close()
        .await
        .map_err(|e| Error::Remote(format!("关闭通道失败: {e}")))?;

    let output = String::from_utf8_lossy(&collected).into_owned();
    Ok((
        exit_code,
        if output.trim().is_empty() {
            "(无输出)".to_owned()
        } else {
            output
        },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn both_auth_kinds_carry_only_a_credential_id() {
        let key = serde_json::from_value::<SshAuth>(serde_json::json!({
            "kind": "private_key", "credential_id": 7
        }))
        .expect("应能解析");
        assert_eq!(key.credential_id(), 7);

        let password = serde_json::from_value::<SshAuth>(serde_json::json!({
            "kind": "password", "credential_id": 9
        }))
        .expect("应能解析");
        assert_eq!(password.credential_id(), 9);
    }

    #[test]
    fn no_credential_material_lives_in_the_input_definition() {
        // 这一条是在防回归：一旦有人把 private_key / password 直接做成输入字段，
        // 明文就会随流水线配置与运行历史扩散。
        //
        // 只看属性名，不看整个 Schema 文本——`private_key`／`password` 会作为
        // 认证方式的 `kind` 枚举值出现，那是合法的，不能一概而论。
        let schema = schema_for!(SshInput);
        let properties = &schema.schema.object.as_ref().expect("应是对象").properties;

        assert!(
            !properties.contains_key("private_key"),
            "私钥不该成为输入字段: {:?}",
            properties.keys().collect::<Vec<_>>()
        );
        assert!(!properties.contains_key("password"), "口令不该成为输入字段");
        assert!(properties.contains_key("auth"), "认证方式应在");
    }

    #[test]
    fn shell_quoting_handles_quotes() {
        assert_eq!(shell_quote("plain"), "'plain'");
        // 内部的单引号按 POSIX 的方式闭合再转义。
        assert_eq!(shell_quote("it's"), "'it'\\''s'");
    }
}
