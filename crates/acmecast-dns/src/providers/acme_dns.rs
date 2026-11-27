//! acme-dns 提供商（自建服务）。
//!
//! acme-dns 是专为 ACME DNS-01 挑战设计的轻量 DNS 服务：凭据只管一个
//! subdomain 的 TXT 更新，泄漏爆炸半径远小于整域 DNS API 密钥。它只有三个
//! 端点——`POST /register`（一次性注册）、`POST /update`（覆盖写 TXT）与
//! `GET /health`（无认证健康检查），**没有** TXT 查询与删除接口。
//!
//! 由此带来的语义映射（见 [`DnsProvider::supports_txt_query`]）：
//!
//! - `create_txt` → `POST /update`，**覆盖写**（同名记录被替换而非追加）；
//! - `find_txt` → 服务无读取接口，正常流程不会调用（挑战层与传播等待按
//!   `supports_txt_query() = false` 跳过），防御性返回空列表；
//! - `delete_txt` → 服务无删除接口，no-op 成功——旧值由下一次 update 覆盖，
//!   残留对 CA 无害（CA 经 CNAME 委派查到的就是 fulldomain 上最新的值）。
//!
//! # API 文档
//!
//! - 自建服务源码（本仓库 `acme-dns/`）与上游 <https://github.com/acme-dns/acme-dns>
//!
//! # 接入方式
//!
//! 流水线走 domain-alias 模式：`dns_alias` 填注册返回的 `fulldomain`
//! （如 `4104321c-….acme.example.edu`），`dns_zone` 填服务器 ACME zone
//! （如 `acme.example.edu`），CNAME 形如
//! `_acme-challenge.<授权域名> → <fulldomain>`。

use std::sync::Arc;

use async_trait::async_trait;
use schemars::schema::RootSchema;
use schemars::schema_for;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::error::{Error, Result};
use crate::http::{
    HttpRequest, HttpResponse, HttpTransport, ReqwestTransport, inject_proxy_field_annotations,
    transport_for_credentials,
};
use crate::provider::{DnsProvider, TxtRecord, parse_credentials};

/// acme-dns 的凭据字段。
///
/// 四个业务字段都取自 `POST /register` 响应，原样粘贴、零推导——
/// 不存 `fulldomain` 是为了不依赖「subdomain 是无点 UUID」的服务端实现细节
/// （运行时从 fulldomain 取第一段推导 subdomain 是隐含约定，靠不住）。
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct AcmeDnsCredentials {
    /// acme-dns 服务器基地址，如 `https://acmedns.example.net`（尾部斜杠可省）。
    pub api_url: String,
    /// 注册返回的 username（UUID）。
    pub username: String,
    /// 注册返回的 password。
    pub password: String,
    /// 注册返回的 subdomain（UUID）；TXT 恒写在该账号的 fulldomain
    /// （`<subdomain>.<服务器 ACME zone>`）上。
    ///
    /// 注意：凭据页「测试」会向该 subdomain 写入探针值并**覆盖当前 TXT**——
    /// acme-dns 没有查询接口，无法先读后写回；挑战进行中请勿测试，正常
    /// 情况下下次挑战写入会覆盖探针值。
    pub subdomain: String,
    /// 出网代理模式：`direct`（缺省，直连）/ `inherit`（继承全局代理，
    /// 见 `config.toml` 的 `[proxy] url` 或 `ACMECAST_PROXY_URL`）/
    /// `custom`（走下面 `proxy_url`，覆盖全局）。
    #[serde(default)]
    pub proxy_mode: Option<String>,
    /// 自定义出网代理（`proxy_mode=custom` 时生效）：支持 `socks5://`、
    /// `http://`、`https://`，留空直连；仅该提供商的 API 调用经此代理发送，
    /// 不影响 ACME 申请与通知渠道的出网路径。
    #[serde(default)]
    pub proxy_url: Option<String>,
}

/// acme-dns（自建）。
#[derive(Debug)]
pub struct AcmeDnsProvider {
    http: Arc<dyn HttpTransport>,
    /// 全局出网代理（`[proxy] url` / `ACMECAST_PROXY_URL`）；仅
    /// `proxy_mode=inherit` 的凭据读它。`None` 即直连。
    global_proxy_url: Option<String>,
    /// 网络操作超时（`[network] timeout_secs`）：`proxy_mode=custom` 建
    /// reqwest 客户端时施加请求级超时。
    network_timeout: std::time::Duration,
}

impl AcmeDnsProvider {
    /// 走真实网络。
    #[must_use]
    pub fn new() -> Self {
        Self::with_transport(Arc::new(ReqwestTransport::default()))
    }

    /// 注入传输层，供测试用。
    #[must_use]
    pub fn with_transport(http: Arc<dyn HttpTransport>) -> Self {
        Self {
            http,
            global_proxy_url: None,
            network_timeout: acmecast_core::NETWORK_TIMEOUT,
        }
    }

    /// 设置全局出网代理与网络超时，供 `proxy_mode=inherit` 的凭据继承代理。
    #[must_use]
    pub fn with_global_proxy_and_timeout(
        mut self,
        proxy_url: Option<impl Into<String>>,
        network_timeout: std::time::Duration,
    ) -> Self {
        self.global_proxy_url = proxy_url.map(Into::into);
        self.network_timeout = network_timeout;
        self
    }

    /// 全局代理 URL；`proxy_mode=inherit` 时读它（留空/未配即直连）。
    fn global_proxy_url(&self) -> Option<&str> {
        self.global_proxy_url.as_deref()
    }

    /// 按凭据选传输层：`proxy_mode` 三态决定直连 / 继承全局 / 走凭据自定义代理。
    fn http_for(
        &self,
        credentials: &Value,
        global_proxy_url: Option<&str>,
    ) -> Result<Arc<dyn HttpTransport>> {
        transport_for_credentials(
            credentials,
            &self.http,
            global_proxy_url,
            self.network_timeout,
        )
    }

    /// 归一化 API 基地址：去首尾空白与尾部斜杠，用户多敲一个 `/` 不至于拼出
    /// `//update`（服务端路由匹配不上）。
    fn api_base(credentials: &AcmeDnsCredentials) -> String {
        credentials.api_url.trim().trim_end_matches('/').to_owned()
    }

    /// `POST /update`：覆盖写该账号 subdomain 的 TXT 值。
    ///
    /// 记录名与 zone 不参与请求——acme-dns 恒更新账号自身的 fulldomain。
    async fn update_txt(
        &self,
        http: &Arc<dyn HttpTransport>,
        credentials: &AcmeDnsCredentials,
        txt: &str,
        what: &str,
    ) -> Result<()> {
        let request = HttpRequest::new("POST", format!("{}/update", Self::api_base(credentials)))
            .with_header("X-Api-User", &credentials.username)
            .with_header("X-Api-Key", &credentials.password)
            .with_body(json!({ "subdomain": credentials.subdomain, "txt": txt }));

        let response = http.send(request).await?;
        ensure_success(response, what)
    }

    /// `GET {api_url}/health`：按凭据的基地址做无认证健康检查。
    ///
    /// 连通性测试的第一段——先区分「服务不可达」（网络层）与「凭据无效」；
    /// health 不做认证，凭据有效性由探针 update 验证。
    pub async fn health_check_at(&self, credentials: &Value) -> Result<()> {
        let parsed: AcmeDnsCredentials = parse_credentials(credentials)?;
        let http = self.http_for(credentials, self.global_proxy_url())?;
        let request = HttpRequest::new("GET", format!("{}/health", Self::api_base(&parsed)));
        let response = http.send(request).await?;
        ensure_success(response, "健康检查")
    }

    /// 探测凭据是否可用：探针 `POST /update` 写一个随机的 43 字符 base64url 值。
    ///
    /// 这是凭据页「测试」按钮的探测动作——acme-dns 唯一能验证凭据的端点。
    /// 探针值必须形如 ACME key authorization 摘要（43 字符 base64url）：上游
    /// `validTXT` 只放行这个长度（LE auth key 尺寸），任意短标记会被 400
    /// `bad_txt` 拒掉。随机生成保证探针值不与任何真实挑战值相同——它没有
    /// 业务意义，只证明「凭据被服务端接受」。
    ///
    /// **副作用**：覆盖该账号当前的 TXT 值，且无查询接口无法先读后写回；
    /// TXT 值只在挑战进行中有意义，挑战期间勿点测试，正常情况下下次挑战
    /// 写入会覆盖探针值。
    pub async fn verify_credentials(&self, credentials: &Value) -> Result<()> {
        let http = self.http_for(credentials, self.global_proxy_url())?;
        let parsed: AcmeDnsCredentials = parse_credentials(credentials)?;
        self.update_txt(&http, &parsed, &probe_txt(), "凭据探针")
            .await
    }
}

/// 生成探针 TXT 值：32 字节随机数的 base64url（无填充）编码，恰 43 字符，
/// 满足 acme-dns `validTXT` 的长度要求且形似合法的挑战摘要。
fn probe_txt() -> String {
    use base64::Engine as _;
    let first = uuid::Uuid::new_v4();
    let second = uuid::Uuid::new_v4();
    let mut bytes = [0_u8; 32];
    bytes[..16].copy_from_slice(first.as_bytes());
    bytes[16..].copy_from_slice(second.as_bytes());
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

impl Default for AcmeDnsProvider {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl DnsProvider for AcmeDnsProvider {
    fn type_id(&self) -> &'static str {
        "dns.acme-dns"
    }

    fn display_name(&self) -> &'static str {
        "acme-dns（自建）"
    }

    fn credential_fields(&self) -> RootSchema {
        acme_dns_credentials_schema()
    }

    fn supports_txt_query(&self) -> bool {
        // 服务没有 TXT 读取接口，写入语义为覆盖写——挑战层跳过「同名异值
        // 残留」检查，传播等待跳过提供商侧确认直接进解析器阶段。
        false
    }

    async fn create_txt(&self, credentials: &Value, record: &TxtRecord) -> Result<()> {
        let http = self.http_for(credentials, self.global_proxy_url())?;
        let credentials: AcmeDnsCredentials = parse_credentials(credentials)?;
        self.update_txt(&http, &credentials, &record.value, "创建 TXT 记录")
            .await
    }

    async fn find_txt(&self, _credentials: &Value, _record: &TxtRecord) -> Result<Vec<String>> {
        // 服务没有查询端点；`supports_txt_query() = false` 保证正常流程不会
        // 走到这里，防御性返回空列表而不是报错——万一被调用，「没有记录」
        // 是比报错更接近事实的答案。
        Ok(Vec::new())
    }

    async fn delete_txt(&self, _credentials: &Value, _record: &TxtRecord) -> Result<()> {
        // 服务没有删除接口：no-op 成功。旧值由下一次 update 覆盖；对 CA 而
        // 言 fulldomain 上残留的旧值会在下一次挑战写入时被替换，无需清理。
        Ok(())
    }
}

/// acme-dns 凭据字段的 schema。
///
/// serde_json 的 Map 会把 schemars 产物的 `properties` 按字母序重排——
/// 用 `x-field-order` 钉住渲染顺序：`api_url` 与 `subdomain`（写入目标）、
/// `username` 与 `password`（账号密码对）各自两两同行，代理字段钉在最后。
/// `proxy_url` 挂 `x-visible-when` 与 `proxy_mode=custom` 联动显隐。
#[must_use]
pub fn acme_dns_credentials_schema() -> RootSchema {
    let mut schema = schema_for!(AcmeDnsCredentials);
    schema.schema.extensions.insert(
        "x-field-order".to_owned(),
        serde_json::json!([
            "api_url",
            "subdomain",
            "username",
            "password",
            "proxy_mode",
            "proxy_url"
        ]),
    );
    inject_proxy_field_annotations(&mut schema);
    schema
}

/// 校验响应成功。失败时摘出 acme-dns 的 `{"error": "..."}`（如认证失败的
/// `forbidden`），比一个光秃秃的状态码有用得多；报错不回显凭据字段值
/// （服务端错误信息只含错误码与原因，不回显请求内容）。
fn ensure_success(response: HttpResponse, what: &str) -> Result<()> {
    if response.is_success() {
        return Ok(());
    }
    let body = response
        .json()
        .unwrap_or_else(|_| json!({ "raw": &response.body }));
    let detail = body["error"]
        .as_str()
        .map(str::to_owned)
        .unwrap_or_else(|| body.to_string());

    Err(Error::provider(format!(
        "acme-dns {what}失败（HTTP {}）：{detail}",
        response.status
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_schema_pins_layout_and_proxy_annotations() {
        let rendered = serde_json::to_string(&acme_dns_credentials_schema()).unwrap();
        // Map 会把 properties 按字母序重排，布局全靠 x-field-order 钉住：
        // 四个业务字段在前，代理字段钉在最后。
        assert!(
            rendered.contains(
                r#""x-field-order":["api_url","subdomain","username","password","proxy_mode","proxy_url"]"#
            ),
            "应注入声明顺序: {rendered}"
        );
        // proxy_mode 是枚举下拉：三个候选 + 中文展示名。
        assert!(
            rendered.contains(r#""enum":["direct","inherit","custom"]"#),
            "proxy_mode 应注入枚举候选: {rendered}"
        );
        for label in ["直连", "继承全局", "自定义"] {
            assert!(
                rendered.contains(label),
                "proxy_mode 应标注中文展示名 {label}: {rendered}"
            );
        }
        // proxy_url 与 proxy_mode=custom 联动显隐。
        assert!(
            rendered.contains(
                r#""x-visible-when":{"Equals":{"field":"proxy_mode","values":["custom"]}}"#
            ),
            "proxy_url 应仅在 custom 模式显隐: {rendered}"
        );
        // 探针副作用提示随 schema 下发到表单（spec：测试会覆盖当前 TXT 值）。
        assert!(
            rendered.contains("覆盖当前 TXT"),
            "subdomain 描述应携带探针副作用提示: {rendered}"
        );
    }

    #[test]
    fn the_api_base_is_trimmed_and_slash_normalized() {
        let credentials = |api_url: &str| AcmeDnsCredentials {
            api_url: api_url.to_owned(),
            username: "u".to_owned(),
            password: "p".to_owned(),
            subdomain: "s".to_owned(),
            proxy_mode: None,
            proxy_url: None,
        };
        // 去首尾空白与尾部斜杠：`//update` 会让服务端路由匹配不上。
        assert_eq!(
            AcmeDnsProvider::api_base(&credentials("  https://acme.example.org/  ")),
            "https://acme.example.org"
        );
        assert_eq!(
            AcmeDnsProvider::api_base(&credentials("https://acme.example.org")),
            "https://acme.example.org"
        );
    }

    #[test]
    fn the_provider_declares_overwrite_semantics() {
        let provider = AcmeDnsProvider::new();
        assert_eq!(provider.type_id(), "dns.acme-dns");
        assert_eq!(provider.display_name(), "acme-dns（自建）");
        // 覆盖写语义的能力声明：挑战层与传播等待都按它跳过查询路径。
        assert!(!provider.supports_txt_query());
    }
}
