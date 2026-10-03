//! 部署目标抽象。
//!
//! 各部署目标的差异（本地写文件、走 SSH 写远程、调云厂商 API）全部收敛在
//! 这个 Trait 背后；调用方只管「把这份证书交给它」。

use acmecast_access::CredentialStore;
use async_trait::async_trait;
use schemars::schema::RootSchema;
use serde_json::Value;

use crate::error::{Error, Result};

/// 待部署的证书材料。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CertMaterials {
    /// 证书链 PEM（含中间证书）。
    pub chain_pem: String,
    /// 私钥 PEM。
    pub key_pem: String,
    /// 证书指纹，用于幂等判断。
    ///
    /// 由上游（证书仓库）给出而不是在这里算：同一份证书在签发时就已算过指纹，
    /// 这里再算一遍只会多出一处可能不一致的地方，而「两处算出的指纹不同」
    /// 会让幂等判断悄悄失效。
    pub fingerprint: String,
}

impl CertMaterials {
    /// 建一份材料。
    #[must_use]
    pub fn new(
        chain_pem: impl Into<String>,
        key_pem: impl Into<String>,
        fingerprint: impl Into<String>,
    ) -> Self {
        Self {
            chain_pem: chain_pem.into(),
            key_pem: key_pem.into(),
            fingerprint: fingerprint.into(),
        }
    }
}

/// 一次部署的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeployOutcome {
    /// 是否跳过了写入。
    ///
    /// 为 `true` 表示目标上已是同样指纹的证书，写操作被省掉——但**重载仍然要跑**，
    /// 否则「换了证书但服务没重新加载」这种状态会一直持续到下次部署。
    pub skipped_write: bool,
    /// 本次涉及的路径（本地路径或远程路径）。
    pub paths: Vec<String>,
    /// 重载命令的输出；未配置重载时为 `None`。
    pub reload_output: Option<String>,
}

impl DeployOutcome {
    /// 真的写入了一次。
    #[must_use]
    pub fn written(paths: Vec<String>) -> Self {
        Self {
            skipped_write: false,
            paths,
            reload_output: None,
        }
    }

    /// 指纹一致，跳过写入。
    #[must_use]
    pub fn skipped(paths: Vec<String>) -> Self {
        Self {
            skipped_write: true,
            paths,
            reload_output: None,
        }
    }

    /// 附上重载命令的输出。
    #[must_use]
    pub fn with_reload_output(mut self, output: impl Into<String>) -> Self {
        self.reload_output = Some(output.into());
        self
    }
}

/// 本次部署要不要真的写文件。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeployMode {
    /// 正常写入。
    Write,
    /// 目标上已经是同一份证书，省掉写入——但**重载流程仍要走过**，
    /// 否则「证书换了、服务没重新加载」会一直持续到下次部署。
    SkipWrite,
}

/// 一个部署目标。
///
/// 实现者只管「怎么把这份证书放过去」，不关心证书是怎么签出来的，
/// 也不接触凭据的存储与解密——拿到的凭据已经是解密后的字段值。
#[async_trait]
pub trait DeploymentTarget: Send + Sync + std::fmt::Debug + 'static {
    /// 类型标识，如 `local`。在同一个注册表内必须唯一。
    fn type_id(&self) -> &'static str;

    /// 展示名称。
    fn display_name(&self) -> &'static str;

    /// 本目标的输入字段定义，供前端渲染表单。
    fn input_schema(&self) -> RootSchema;

    /// 本目标输入的示例，供前端在 `config` 输入框的占位提示中展示。
    ///
    /// 与 [`DeploymentTarget::input_schema`] 一起构成「结构 + 样例」：
    /// 字段含义看 schema 的 description，长什么样看这里。示例只含必填
    /// 项与最常用的可选项，避免占位提示长得读不完。
    fn example_input(&self) -> Value;

    /// 把证书投放到目标。
    ///
    /// `input` 是本目标的配置（目标路径、主机地址、重载命令等）。
    /// 需要凭据的目标（比如 SSH 私钥）经由 `credentials` **按标识**取用——
    /// 凭据不放进 `input`，那会把明文留在流水线配置与运行历史里。
    async fn deploy(
        &self,
        input: &Value,
        materials: &CertMaterials,
        credentials: &CredentialStore<'_>,
        mode: DeployMode,
    ) -> Result<DeployOutcome>;
}

/// 把输入反序列化成目标自己的结构体。
///
/// 做成自由函数而非 trait 方法：带泛型参数的方法会让 [`DeploymentTarget`]
/// 失去 dyn 兼容性，而注册表恰恰需要 `Box<dyn DeploymentTarget>`。
///
/// 失败时描述里会带上 serde 的原始信息——它形如
/// ``missing field `cert_path` ``，字段名就在其中。
pub fn parse_input<T>(input: &Value) -> Result<T>
where
    T: serde::de::DeserializeOwned,
{
    serde_json::from_value(input.clone())
        .map_err(|e| Error::invalid_input("(部署输入)", format!("不符合本目标的输入定义: {e}")))
}
