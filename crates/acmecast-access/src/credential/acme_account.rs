//! ACME 账号凭据类型。
//!
//! 字段里留一份 [`AccountCredentials`]——它同时装着 KID、账号私钥与服务端点。
//! 之所以不把「私钥」和「KID」拆成两个字段：那份凭据是 ACME 客户端的内部表示，
//! 拆开再拼装等于在两处维护同一套格式知识，容易在某一侧漂移。
//! 需要展示 KID 时用 [`AcmeAccountFields::kid`] 取即可。
//!
//! **共享账号靠的是复用而不是去重**：凭据里一旦有了 KID 与私钥，
//! 使用方拿到的就是 [`AcmeService::from_credentials`] 所需的输入，
//! 那条路径不与 CA 做任何注册交互。因此多条流水线引用同一份凭据，
//! 不会有第二次注册。
//!
//! [`AcmeService::from_credentials`]: acmecast_acme::AcmeService::from_credentials

use std::sync::Arc;

use acmecast_acme::{
    AccountCredentials, AccountMode, AcmeService, CaKind, EstablishAccountInput,
    ExternalAccountBinding, Transport, resolve_directory_url,
};
use schemars::JsonSchema;
use schemars::schema::RootSchema;
use schemars::schema_for;
use serde::{Deserialize, Serialize};

use super::{ConnectivityOutcome, CredentialType};

/// ACME 账号凭据的类型标识。
pub const TYPE_ID: &str = "acme.account";

/// ACME 账号凭据的字段定义。
///
/// `Debug` 手写：`credentials` 含账号私钥、`eab_hmac_key` 是 EAB 密钥，两者都得脱敏。
/// CA 与 Directory URL 不是秘密，保留它们才能从日志里看出「这是哪一份凭据」。
#[derive(Clone, Serialize, Deserialize, JsonSchema)]
pub struct AcmeAccountFields {
    /// CA 类型：内置别名（`letsencrypt`、`letsencrypt-staging`、`zerossl`、`google`、`google-test`、`freessl`）
    /// 或 `custom`。
    pub ca: String,

    /// 自定义 Directory URL。`ca = custom` 时必填；内置 CA 上填写它会覆盖内置端点。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub directory_url: Option<String>,

    /// EAB 密钥标识。与 `eab_hmac_key` 必须成对出现。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub eab_kid: Option<String>,

    /// base64url 编码的 EAB HMAC 密钥。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub eab_hmac_key: Option<String>,

    /// 账号凭据（含 KID、私钥与服务端点），注册或绑定成功后写入。
    ///
    /// 为空表示这个凭据还没在 CA 侧建立账号，首次使用时会走注册；
    /// 非空则所有使用方都走复用，不会再注册一次。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credentials: Option<String>,

    /// 出网代理模式：`direct` 直连 / `inherit` 继承全局 / `custom` 自定义。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proxy_mode: Option<String>,

    /// 自定义出网代理（`proxy_mode=custom` 时生效）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proxy_url: Option<String>,
}

impl std::fmt::Debug for AcmeAccountFields {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AcmeAccountFields")
            .field("ca", &self.ca)
            .field("directory_url", &self.directory_url)
            .field("eab_kid", &self.eab_kid)
            .field(
                "eab_hmac_key",
                &acmecast_core::redact_presence(&self.eab_hmac_key),
            )
            .field(
                "credentials",
                &acmecast_core::redact_presence(&self.credentials),
            )
            .finish()
    }
}

impl AcmeAccountFields {
    /// 该凭据是否已在 CA 侧建立账号。
    #[must_use]
    pub fn is_registered(&self) -> bool {
        self.credentials.is_some()
    }

    /// 账号 ID（KID），供展示；尚未建立账号或凭据形态坏掉时为 `None`。
    #[must_use]
    pub fn kid(&self) -> Option<String> {
        self.credentials().ok().flatten()?.kid()
    }

    /// 取出可在 CA 侧恢复会话的账号凭据。
    ///
    /// 拿到它交给 `AcmeService::from_credentials` 即为**复用**：
    /// 直接恢复会话，不产生任何注册请求。
    ///
    /// 字段为空表示尚未建立账号（`Ok(None)`）；字段有值但不是可用形态则报错。
    pub fn credentials(&self) -> acmecast_core::Result<Option<AccountCredentials>> {
        let Some(raw) = self.credentials.as_deref() else {
            return Ok(None);
        };
        AccountCredentials::from_json(raw)
            .map(Some)
            .map_err(|e| acmecast_core::Error::validation("credentials", e.to_string()))
    }

    /// 解析出本凭据对应的 Directory URL。
    pub fn resolve_directory(&self) -> acmecast_core::Result<String> {
        let ca = self.parse_ca()?;
        resolve_directory_url(ca, self.directory_url.as_deref())
            .map_err(|e| acmecast_core::Error::validation("directory_url", e.to_string()))
    }

    /// 构造交给 ACME 客户端的建立输入——用于**首次注册或绑定**已有账号。
    ///
    /// 已经注册过的凭据不该走这里，而应走 [`Self::credentials`] + 复用。
    pub fn establish_input(&self, mode: AccountMode) -> acmecast_core::Result<AcmeAccountInput> {
        Ok(AcmeAccountInput {
            directory_url: self.resolve_directory()?,
            mode,
            external_account: self.external_binding(),
        })
    }

    /// 校验字段之间是否自洽。
    fn check(&self) -> acmecast_core::Result<()> {
        // CA 别名必须认识；自定义 CA 必须给得出 Directory URL。
        self.resolve_directory()?;

        // EAB 仅需成对校验，不再与 credentials 关联校验（EAB 过期后 credentials 可复用，编辑时不应因 credentials 格式问题误报）
        match (&self.eab_kid, &self.eab_hmac_key) {
            (Some(_), Some(_)) | (None, None) => {}
            _ => {
                return Err(acmecast_core::Error::validation(
                    "eab_kid",
                    "EAB 的密钥标识与 HMAC 密钥必须成对提供",
                ));
            }
        }

        // credentials 若已填，需为可用形态；但失败不应阻塞编辑，申请阶段会再次校验
        if let Some(raw) = self.credentials.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            if serde_json::from_str::<serde_json::Value>(raw).is_err() && AccountCredentials::from_json(raw).is_err() {
                // 视为未注册，允许保存，申请时再探测
            }
        }
        // 代理字段与其他凭据类型同一口径：custom 必须给出合法 proxy_url。
        acmecast_core::validate_proxy_field(
            &serde_json::to_value(self).map_err(|e| acmecast_core::Error::Internal(e.to_string()))?,
        )
    }

    /// 解析 CA 别名。
    fn parse_ca(&self) -> acmecast_core::Result<CaKind> {
        CaKind::parse(&self.ca).ok_or_else(|| {
            acmecast_core::Error::validation(
                "ca",
                format!(
                    "未知的 CA 别名 `{}`，可选：{}",
                    self.ca,
                    CaKind::ALL.join("、")
                ),
            )
        })
    }

    /// 组出 EAB；两半不齐时返回 `None`（那份不齐的输入会在 [`Self::check`] 被拒）。
    fn external_binding(&self) -> Option<ExternalAccountBinding> {
        match (&self.eab_kid, &self.eab_hmac_key) {
            (Some(kid), Some(hmac_key)) => Some(ExternalAccountBinding {
                kid: kid.clone(),
                hmac_key: hmac_key.clone(),
            }),
            _ => None,
        }
    }
}

/// 由凭据字段翻译出的、可直接交给 `acmecast-acme` 的输入。
///
/// 单独成型是因为 [`EstablishAccountInput`] 借用外部字符串；
/// 先把它物化出来，调用方才能把借用交给 ACME 客户端。
#[derive(Debug, Clone)]
pub struct AcmeAccountInput {
    /// 解析后的 Directory URL。
    pub directory_url: String,
    /// 建立账号的方式。
    pub mode: AccountMode,
    /// 可选的外部账号绑定。
    pub external_account: Option<ExternalAccountBinding>,
}

impl AcmeAccountInput {
    /// 转成 ACME 客户端的建立输入。
    #[must_use]
    pub fn as_establish_input(&self) -> EstablishAccountInput<'_> {
        EstablishAccountInput {
            directory_url: &self.directory_url,
            mode: self.mode.clone(),
            external_account: self.external_account.as_ref(),
        }
    }
}

/// 造一个一次性的 HTTP 传输层。
///
/// [`Transport`] 会被 ACME 客户端消费掉，因此每次探测都得现造一个。
pub type TransportFactory = Arc<dyn Fn() -> Transport + Send + Sync>;

/// ACME 账号凭据类型。
///
/// 默认走真实 HTTP；测试可用 [`AcmeAccountType::with_transport`] 注入进程内替身，
/// 这样「能连上 CA」与「连不上」两条分支都能在无外网的情况下验证。
#[derive(Clone, Default)]
pub struct AcmeAccountType {
    transport: Option<TransportFactory>,
    /// 全局出网代理（`[proxy] url` / `ACMECAST_PROXY_URL`）：供
    /// `proxy_mode=inherit` 的凭据派生 `ProxyConfig`。
    proxy_url: Option<String>,
}

impl std::fmt::Debug for AcmeAccountType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // 只说明有没有注入替身，打印闭包本身没有意义。
        f.debug_struct("AcmeAccountType")
            .field("injected_transport", &self.transport.is_some())
            .finish()
    }
}

impl AcmeAccountType {
    /// 生产用构造：走真实 HTTP。
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// 由凭据与全局代理派生 ACME 客户端的 `ProxyConfig`：
    /// `direct`→空配置；`inherit`→全局代理；`custom`→凭据自带 URL。
    /// `socks5://` 填 `socks5`，`http://`/`https://` 同时填 `http`+`https`
    ///（代理本身对两种上行情境都适用）。非法 scheme 忽略走直连
    ///（保存时已被 `validate_proxy_url_field` 拦下）。
    #[must_use]
    pub fn acme_proxy_config(
        fields: &AcmeAccountFields,
        global_proxy_url: Option<&str>,
    ) -> acmecast_acme::ProxyConfig {
        let Some(url) =
            acmecast_core::effective_proxy_url(&serde_json::to_value(fields).unwrap_or_default(), global_proxy_url)
        else {
            return acmecast_acme::ProxyConfig::default();
        };
        if acmecast_core::validate_proxy_url(&url).is_err() {
            return acmecast_acme::ProxyConfig::default();
        }
        let (http, https, socks5) = if url.starts_with("socks5") {
            (None, None, Some(url))
        } else {
            (Some(url.clone()), Some(url), None)
        };
        acmecast_acme::ProxyConfig {
            http,
            https,
            socks5,
            accept_invalid_certs: false,
        }
    }

    /// 测试用构造：每次探测都用工厂现造一个传输层替身。
    #[must_use]
    pub fn with_transport(mut self, factory: TransportFactory) -> Self {
        self.transport = Some(factory);
        self
    }

    /// 设置全局出网代理（`[proxy] url` / `ACMECAST_PROXY_URL`）。
    #[must_use]
    pub fn with_global_proxy(mut self, proxy_url: Option<impl Into<String>>) -> Self {
        self.proxy_url = proxy_url.map(Into::into);
        self
    }
}

#[async_trait::async_trait]
impl CredentialType for AcmeAccountType {
    fn type_id(&self) -> &'static str {
        TYPE_ID
    }

    fn display_name(&self) -> &'static str {
        "ACME 账号"
    }

    fn fields_schema(&self) -> RootSchema {
        let mut schema = schema_for!(AcmeAccountFields);

        // serde_json 的 Map 会把 properties 按字母序重排：`eab_hmac_key` 会排到
        // `eab_kid` 前面，把成对的 EAB 密钥拆得阅读不顺。用 `x-field-order` 钉住
        // 渲染顺序——两个 EAB 字段相邻且 kid 在前，都是半宽标量，流式排成一行。
        schema.schema.extensions.insert(
            "x-field-order".to_owned(),
            serde_json::json!(["ca", "directory_url", "eab_kid", "eab_hmac_key", "credentials", "proxy_mode", "proxy_url"]),
        );

        // schemars 只认类型不认语义：`ca: String` 出来是自由文本框。
        // 这里把 CA 别名的合法取值补成 enum（前端据此渲染下拉），
        // 并给 `directory_url` 挂上「仅 ca=custom 时必填」的条件——
        // 内置 CA 上它仍是可选的端点覆盖项，所以不能直接进 required。
        let Some(object) = schema.schema.object.as_mut() else {
            return schema;
        };
        if let Some(schemars::schema::Schema::Object(ca)) = object.properties.get_mut("ca") {
            ca.enum_values = Some(CaKind::ALL.iter().map(|alias| (*alias).into()).collect());
            // 固定半宽，防止长名称被截断
            // EAB 管理/文档入口按 ca 值展示
            ca.extensions.insert(
                "x-help-links".to_owned(),
                serde_json::json!([
                    {"value": "freessl", "url": "https://freessl.cn/automation/eab-manager", "label": "申请 EAB"},
                    {"value": "zerossl", "url": "https://app.zerossl.com/developer", "label": "EAB 文档"},
                    {"value": "google", "url": "https://docs.cloud.google.com/certificate-manager/docs/public-ca-tutorial#request-key-hmac", "label": "申请 HMAC"},
                    {"value": "google-test", "url": "https://docs.cloud.google.com/certificate-manager/docs/public-ca-tutorial#request-key-hmac", "label": "申请 HMAC (测试)"}
                ]),
            );
            let mut labels = std::collections::BTreeMap::new();
            labels.insert("letsencrypt", "Let's Encrypt");
            labels.insert("letsencrypt-staging", "Let's Encrypt (测试)");
            labels.insert("zerossl", "ZeroSSL");
            labels.insert("google", "Google Trust Services");
            labels.insert("google-test", "Google Trust Services (测试)");
            labels.insert("freessl", "FreeSSL");
            labels.insert("custom", "自定义");
            ca.extensions.insert("x-enum-labels".to_owned(), serde_json::to_value(labels).unwrap());
            // 半宽 + 行尾空占位：CA 下拉半列，右侧 help-links 在同一行右侧
            ca.extensions.insert("x-end-row".to_owned(), serde_json::Value::Bool(true));
        }
        if let Some(schemars::schema::Schema::Object(url)) =
            object.properties.get_mut("directory_url")
        {
            url.extensions.insert(
                "x-required-when".to_owned(),
                serde_json::json!({ "Equals": { "field": "ca", "values": ["custom"] } }),
            );
            // URL 是长文本，单独占整行，不和短字段挤在一行。
            url.extensions
                .insert("x-full-width".to_owned(), serde_json::Value::Bool(true));
        }
        // 账号凭据是一段含私钥的 JSON，单行框既放不下也难审阅——
        // 渲染成多行文本域（与 SSH 私钥同机制）。
        if let Some(schemars::schema::Schema::Object(credentials)) =
            object.properties.get_mut("credentials")
        {
            credentials
                .extensions
                .insert("x-multiline".to_owned(), serde_json::Value::Bool(true));
        }
        // 代理字段独占一行：枚举下拉 + 联动输入框，前驱字段（credentials）
        // 自动挂 x-end-row。
        acmecast_core::inject_proxy_field_annotations(&mut schema);
        schema
    }

    fn validate(&self, fields: &serde_json::Value) -> acmecast_core::Result<()> {
        let parsed: AcmeAccountFields = serde_json::from_value(fields.clone())
            .map_err(|e| acmecast_core::Error::validation("fields", e.to_string()))?;
        parsed.check()
    }

    async fn test_connectivity(&self, fields: &serde_json::Value) -> ConnectivityOutcome {
        let parsed = match serde_json::from_value::<AcmeAccountFields>(fields.clone()) {
            Ok(fields) => fields,
            Err(e) => {
                return ConnectivityOutcome::Unavailable {
                    reason: format!("字段不合法: {e}"),
                };
            }
        };

        let credentials = match parsed.credentials() {
            Ok(Some(credentials)) => credentials,
            Ok(None) => {
                return ConnectivityOutcome::Unavailable {
                    reason: "该凭据尚未在 CA 侧建立账号，无从验证".to_owned(),
                };
            }
            Err(e) => {
                return ConnectivityOutcome::Unavailable {
                    reason: e.to_string(),
                };
            }
        };

        // 恢复会话会去取一次 Directory，所以这确实是对 CA 的连通性探测，
        // 而不是「构造了一个对象就算通过」。
        let probe = match &self.transport {
            Some(factory) => {
                AcmeService::from_credentials_with_transport(&credentials, factory()).await
            }
            None => {
                // 生产路径：按凭据 proxy_mode + 全局代理派生传输配置。
                let transport = Self::acme_proxy_config(&parsed, self.proxy_url.as_deref());
                AcmeService::from_credentials(&credentials, Some(&transport)).await
            }
        };

        match probe {
            Ok(_) => ConnectivityOutcome::Ok,
            Err(e) => ConnectivityOutcome::Unavailable {
                // 外部报错可能回显请求里的密钥，先脱敏再交给用户。
                reason: self.redact(fields, &e.to_string()),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 一份形态合法的账号凭据 JSON——与 ACME 客户端持久化的格式一致。
    fn stored_credentials(kid: &str) -> String {
        serde_json::json!({
            "id": kid,
            "key_pkcs8": "TUlJRXZnSUJBREFLQmdncWhrak9QUVFEQWc=",
            "directory": { "url": "https://acme-v02.api.letsencrypt.org/directory" },
        })
        .to_string()
    }

    fn letsencrypt_fields() -> AcmeAccountFields {
        AcmeAccountFields {
            ca: "letsencrypt".to_owned(),
            directory_url: None,
            eab_kid: None,
            eab_hmac_key: None,
            credentials: Some(stored_credentials(
                "https://acme-v02.api.letsencrypt.org/acme/acct/123456",
            )),
            proxy_mode: None,
            proxy_url: None,
        }
    }

    #[test]
    fn a_builtin_ca_resolves_to_its_directory() {
        let fields = letsencrypt_fields();
        assert_eq!(
            fields.resolve_directory().unwrap(),
            "https://acme-v02.api.letsencrypt.org/directory"
        );
    }

    #[test]
    fn a_registered_credential_exposes_its_kid() {
        let fields = letsencrypt_fields();
        assert!(fields.is_registered());
        assert_eq!(
            fields.kid().as_deref(),
            Some("https://acme-v02.api.letsencrypt.org/acme/acct/123456")
        );
    }

    #[test]
    fn an_unregistered_credential_has_no_account_yet() {
        let fields = AcmeAccountFields {
            credentials: None,
            ..letsencrypt_fields()
        };

        assert!(!fields.is_registered());
        assert!(fields.kid().is_none());
        assert!(
            fields.credentials().unwrap().is_none(),
            "尚未注册时不应凭空造出账号凭据"
        );
    }

    #[test]
    fn the_same_stored_credentials_yield_the_same_identity() {
        // 两条流水线引用同一份凭据：各自解析一次，拿到的 KID 与私钥必须完全一致，
        // 因而两条走的是同一个账号、都不会触发第二次注册。
        let fields = letsencrypt_fields();

        let first = fields.credentials().unwrap().expect("应能解出账号凭据");
        let second = fields.credentials().unwrap().expect("应能解出账号凭据");

        assert_eq!(first.kid(), second.kid());
        assert_eq!(first.as_json(), second.as_json());
        assert_eq!(first.kid(), fields.kid());
    }

    #[test]
    fn a_custom_ca_requires_a_directory_url() {
        let fields = AcmeAccountFields {
            ca: "custom".to_owned(),
            directory_url: None,
            ..letsencrypt_fields()
        };

        let err = fields.resolve_directory().unwrap_err();
        let text = err.to_string();
        assert!(text.contains("Directory URL"), "错误应说明缺什么: {text}");
    }

    #[test]
    fn a_custom_ca_uses_the_provided_directory_url() {
        let fields = AcmeAccountFields {
            ca: "custom".to_owned(),
            directory_url: Some("https://ca.internal/acme/directory".to_owned()),
            ..letsencrypt_fields()
        };

        assert_eq!(
            fields.resolve_directory().unwrap(),
            "https://ca.internal/acme/directory"
        );
    }

    #[test]
    fn an_unknown_ca_alias_is_rejected_and_lists_the_known_ones() {
        let fields = AcmeAccountFields {
            ca: "digicert".to_owned(),
            ..letsencrypt_fields()
        };

        let err = fields.resolve_directory().unwrap_err();
        let text = err.to_string();
        assert!(text.contains("digicert"), "{text}");
        assert!(text.contains("letsencrypt"), "应列出可用别名: {text}");
    }

    #[test]
    fn eab_must_be_provided_as_a_pair() {
        let fields = AcmeAccountFields {
            eab_kid: Some("eab-kid".to_owned()),
            eab_hmac_key: None,
            ..letsencrypt_fields()
        };

        let err = fields.check().unwrap_err();
        assert!(err.to_string().contains("成对"), "{err}");
    }

    #[test]
    fn a_complete_eab_pair_becomes_an_external_binding() {
        let fields = AcmeAccountFields {
            eab_kid: Some("eab-kid".to_owned()),
            eab_hmac_key: Some("c2VjcmV0".to_owned()),
            ..letsencrypt_fields()
        };

        fields.check().expect("成对的 EAB 应通过校验");
        let binding = fields.external_binding().expect("应组出 EAB");
        assert_eq!(binding.kid, "eab-kid");
        assert_eq!(binding.hmac_key, "c2VjcmV0");
    }

    #[test]
    fn a_malformed_stored_credential_is_tolerated_until_use() {
        // credentials 形态错误不再阻塞保存/编辑；视为未注册，申请时再探测。
        let fields = AcmeAccountFields {
            credentials: Some("不是 JSON".to_owned()),
            ..letsencrypt_fields()
        };

        fields.check().expect("非法的 credentials 应视为未注册而非校验失败");
        assert!(fields.credentials().is_err(), "credentials() 仍应报错以便调用方感知形态问题");
    }

    #[test]
    fn establish_input_carries_the_resolved_directory() {
        let fields = letsencrypt_fields();
        let input = fields
            .establish_input(AccountMode::Create {
                contacts: vec!["mailto:ops@example.com".to_owned()],
            })
            .unwrap();

        assert_eq!(
            input.directory_url,
            "https://acme-v02.api.letsencrypt.org/directory"
        );
        assert!(input.external_account.is_none());

        // 借用交给 ACME 客户端时，三者应保持一致。
        let establish = input.as_establish_input();
        assert_eq!(establish.directory_url, input.directory_url);
        assert!(matches!(establish.mode, AccountMode::Create { .. }));
    }

    // ---- CredentialType 实现 ----

    #[test]
    fn the_type_identifies_itself() {
        let credential_type = AcmeAccountType::new();
        assert_eq!(credential_type.type_id(), TYPE_ID);
        assert_eq!(credential_type.display_name(), "ACME 账号");
    }

    #[test]
    fn validate_accepts_a_well_formed_record() {
        let fields = serde_json::to_value(letsencrypt_fields()).unwrap();
        AcmeAccountType::new()
            .validate(&fields)
            .expect("合法记录应通过校验");
    }

    #[test]
    fn validate_reports_the_offending_field() {
        // 未知 CA 别名：错误应指向 `ca` 字段，前端才能高亮到具体输入框。
        let fields = serde_json::json!({ "ca": "nope" });
        let err = AcmeAccountType::new().validate(&fields).unwrap_err();
        match err {
            acmecast_core::Error::Validation { field, .. } => assert_eq!(field, "ca"),
            other => panic!("期望 Validation，实际 {other:?}"),
        }
    }

    #[test]
    fn the_schema_lists_every_field() {
        let rendered = serde_json::to_string(&AcmeAccountType::new().fields_schema()).unwrap();
        for field in [
            "ca",
            "directory_url",
            "eab_kid",
            "eab_hmac_key",
            "credentials",
        ] {
            assert!(
                rendered.contains(field),
                "Schema 缺少字段 {field}: {rendered}"
            );
        }
    }

    #[test]
    fn ca_is_a_dropdown_and_directory_url_is_conditionally_required() {
        let schema = AcmeAccountType::new().fields_schema();
        let object = schema.schema.object.as_ref().expect("应是对象 schema");

        let schemars::schema::Schema::Object(ca) = &object.properties["ca"] else {
            panic!("ca 应是对象型 schema");
        };
        let values = ca.enum_values.as_ref().expect("ca 应带枚举选项");
        for alias in ["letsencrypt", "custom"] {
            assert!(
                values.contains(&serde_json::Value::from(alias)),
                "枚举应含 {alias}: {values:?}"
            );
        }

        let schemars::schema::Schema::Object(url) = &object.properties["directory_url"] else {
            panic!("directory_url 应是对象型 schema");
        };
        let condition = url
            .extensions
            .get("x-required-when")
            .expect("directory_url 应带条件必填标记");
        assert_eq!(condition["Equals"]["field"], serde_json::Value::from("ca"));
        assert_eq!(
            condition["Equals"]["values"][0],
            serde_json::Value::from("custom")
        );
        // 不能进 required：内置 CA 上它是可选的端点覆盖项。
        assert!(
            !object.required.iter().any(|f| f == "directory_url"),
            "{:?}",
            object.required
        );
    }

    #[test]
    fn the_layout_extensions_mark_the_long_fields() {
        let schema = AcmeAccountType::new().fields_schema();
        let object = schema.schema.object.as_ref().expect("应是对象 schema");

        // serde_json Map 把 properties 重排成字母序（eab_hmac_key 会跑到
        // eab_kid 前面），渲染顺序靠 x-field-order 钉住：EAB 密钥对相邻、
        // kid 在前，两个半宽标量流式排成同一行。
        let order = schema
            .schema
            .extensions
            .get("x-field-order")
            .and_then(|value| value.as_array())
            .and_then(|items| {
                items
                    .iter()
                    .map(|item| item.as_str())
                    .collect::<Option<Vec<_>>>()
            })
            .expect("x-field-order 应是字符串数组");
        assert_eq!(
            order,
            [
                "ca",
                "directory_url",
                "eab_kid",
                "eab_hmac_key",
                "credentials",
                "proxy_mode",
                "proxy_url",
            ],
            "EAB 密钥对应按 kid 前、hmac 后相邻渲染，代理字段独占一行"
        );

        // ca 是长标签下拉：固定半宽独占一行（半宽 + 行尾空占位），长名称不被挤压隐藏。
        let schemars::schema::Schema::Object(ca) = &object.properties["ca"] else {
            panic!("ca 应是对象型 schema");
        };
        assert_eq!(
            ca.extensions.get("x-end-row"),
            Some(&serde_json::Value::Bool(true)),
            "ca 应标注 x-end-row 固定半宽独占一行: {:?}",
            ca.extensions
        );
        assert!(
            !ca.extensions.contains_key("x-full-width") && !ca.extensions.contains_key("x-multiline"),
            "ca 不应被标成整行: {:?}",
            ca.extensions
        );

        // credentials 是含私钥的 JSON：渲染成多行文本域。
        let schemars::schema::Schema::Object(credentials) = &object.properties["credentials"] else {
            panic!("credentials 应是对象型 schema");
        };
        assert_eq!(
            credentials.extensions.get("x-multiline"),
            Some(&serde_json::Value::Bool(true)),
            "credentials 应标注多行文本域: {:?}",
            credentials.extensions
        );

        // directory_url 是长 URL：单独占整行。
        let schemars::schema::Schema::Object(url) = &object.properties["directory_url"] else {
            panic!("directory_url 应是对象型 schema");
        };
        assert_eq!(
            url.extensions.get("x-full-width"),
            Some(&serde_json::Value::Bool(true)),
            "directory_url 应标注整行: {:?}",
            url.extensions
        );

        // 成对短字段不标注：保持半宽、流式排成同一行。
        for field in ["eab_kid", "eab_hmac_key"] {
            let schemars::schema::Schema::Object(node) = &object.properties[field] else {
                panic!("{field} 应是对象型 schema");
            };
            assert!(
                !node.extensions.contains_key("x-full-width")
                    && !node.extensions.contains_key("x-multiline"),
                "{field} 应保持半宽: {:?}",
                node.extensions
            );
        }
    }

    #[tokio::test]
    async fn probing_an_unregistered_account_reports_why() {
        // 还没在 CA 侧建立账号时探测无从谈起——但必须给出原因，
        // 而不是笼统的「不可用」或干脆报错。
        let fields = AcmeAccountFields {
            credentials: None,
            ..letsencrypt_fields()
        };
        let json = serde_json::to_value(&fields).unwrap();

        let outcome = AcmeAccountType::new().test_connectivity(&json).await;

        assert!(!outcome.is_ok());
        let reason = outcome.reason().expect("不可用时应给出原因");
        assert!(reason.contains("尚未在 CA 侧建立账号"), "{reason}");
    }

    /// 一份指向某个 ACME 端点的账号凭据。
    ///
    /// 私钥交给 acme 的 testing 辅助生成：底层库只接受满足它编码要求的 PKCS#8，
    /// 用 openssl 之类外部工具造的密钥会在恢复会话时才被拒。
    fn account_registered_at(base: &str) -> AcmeAccountFields {
        let directory = format!("{base}/directory");
        let kid = format!("{base}/acct/1");
        let credentials = acmecast_acme::testing::sample_credentials(&kid, &directory);

        AcmeAccountFields {
            ca: "custom".to_owned(),
            directory_url: Some(directory),
            eab_kid: None,
            eab_hmac_key: None,
            credentials: Some(credentials.as_json().to_owned()),
            proxy_mode: None,
            proxy_url: None,
        }
    }

    /// 取出字段里那份账号凭据的私钥片段，用于断言它没有被回显出去。
    fn private_key_of(fields: &AcmeAccountFields) -> String {
        let raw = fields.credentials.as_deref().expect("应有账号凭据");
        let parsed: serde_json::Value = serde_json::from_str(raw).expect("应是 JSON");
        parsed["key_pkcs8"]
            .as_str()
            .expect("应有 key_pkcs8")
            .to_owned()
    }

    /// 代理字段派生：`inherit` 继承全局、`custom` 自带 URL、`direct` 直连；
    /// scheme 映射 `socks5://`→socks5、`http(s)://`→http+https。
    #[test]
    fn acme_proxy_config_follows_proxy_mode() {
        let mut fields = letsencrypt_fields();
        let global = Some("socks5://1.2.3.4:7890");

        // 缺省：直连空配置。
        let none = AcmeAccountType::acme_proxy_config(&fields, global);
        assert!(!none.is_enabled(), "缺省应直连: {none:?}");

        // inherit：继承全局代理（socks5 映射到 socks5 槽位）。
        fields.proxy_mode = Some("inherit".into());
        let inherited = AcmeAccountType::acme_proxy_config(&fields, global);
        assert!(inherited.is_enabled(), "inherit 应填充代理: {inherited:?}");
        assert_eq!(inherited.socks5.as_deref(), Some("socks5://1.2.3.4:7890"));
        assert_eq!(inherited.http, None);

        // inherit 但全局未配：直连。
        assert!(
            !AcmeAccountType::acme_proxy_config(&fields, None).is_enabled(),
            "inherit 无全局应直连"
        );

        // custom：自带 URL 覆盖全局（http 代理填 http+https 两槽位）。
        fields.proxy_mode = Some("custom".into());
        fields.proxy_url = Some("http://10.0.0.1:3128".into());
        let custom = AcmeAccountType::acme_proxy_config(&fields, global);
        assert_eq!(custom.http.as_deref(), Some("http://10.0.0.1:3128"));
        assert_eq!(custom.https.as_deref(), Some("http://10.0.0.1:3128"));
        assert_eq!(custom.socks5, None);

        // direct：忽略一切。
        fields.proxy_mode = Some("direct".into());
        assert!(
            !AcmeAccountType::acme_proxy_config(&fields, global).is_enabled(),
            "direct 应直连"
        );

        // custom 非法 scheme：忽略走直连（保存时已被拦下）。
        fields.proxy_mode = Some("custom".into());
        fields.proxy_url = Some("ftp://127.0.0.1:21".into());
        assert!(
            !AcmeAccountType::acme_proxy_config(&fields, global).is_enabled(),
            "非法 scheme 应直连"
        );
    }

    /// schema 注入代理字段：枚举候选 + 中文标签 + custom 联动；
    /// credentials 为整行多行文本域已在新行首列，无需前驱 `x-end-row`。
    #[test]
    fn acme_schema_injects_proxy_annotations() {
        let rendered = serde_json::to_string(&AcmeAccountType::new().fields_schema()).unwrap();
        assert!(
            rendered.contains(r#""enum":["direct","inherit","custom"]"#),
            "proxy_mode 应注入枚举候选: {rendered}"
        );
        for label in ["直连", "继承全局", "自定义"] {
            assert!(rendered.contains(label), "应含中文展示名 {label}: {rendered}");
        }
        assert!(
            rendered.contains(r#""x-visible-when":{"Equals":{"field":"proxy_mode","values":["custom"]}}"#),
            "proxy_url 应联动 custom: {rendered}"
        );
        let value: serde_json::Value = serde_json::from_str(&rendered).unwrap();
        assert!(
            value["properties"]["credentials"]["x-end-row"].is_null(),
            "credentials 为整行多行文本域，proxy_mode 已在新行首列，不应再带 x-end-row: {rendered}"
        );
    }

    /// 把探测接到进程内的 mock 服务器上。
    fn probe_through_mock() -> AcmeAccountType {
        let mock = acmecast_acme::testing::MockAcme::new();
        AcmeAccountType::new().with_transport(Arc::new(move || Transport::new(mock.clone())))
    }

    #[tokio::test]
    async fn probing_a_reachable_ca_succeeds() {
        let fields = account_registered_at(acmecast_acme::testing::BASE);
        let outcome = probe_through_mock()
            .test_connectivity(&serde_json::to_value(&fields).unwrap())
            .await;

        assert!(
            outcome.is_ok(),
            "能取到 Directory 时应判定可用: {outcome:?}"
        );
    }

    #[tokio::test]
    async fn probing_an_unreachable_endpoint_reports_a_reason_without_the_key() {
        // mock 不认识这条路径，取 Directory 会失败。
        let fields = account_registered_at("https://ca.test/no-such-ca");
        let outcome = probe_through_mock()
            .test_connectivity(&serde_json::to_value(&fields).unwrap())
            .await;

        assert!(!outcome.is_ok(), "取不到 Directory 时不应判定可用");
        let reason = outcome.reason().expect("不可用必须给出原因");
        assert!(!reason.is_empty(), "原因不该是空的");
        assert!(
            !reason.contains(&private_key_of(&fields)),
            "给用户看的原因里不得带出私钥: {reason}"
        );
    }

    #[tokio::test]
    async fn probing_malformed_fields_reports_why() {
        let outcome = AcmeAccountType::new()
            .test_connectivity(&serde_json::json!({ "ca": 42 }))
            .await;

        assert!(!outcome.is_ok());
        assert!(
            outcome.reason().is_some_and(|r| r.contains("字段不合法")),
            "{outcome:?}"
        );
    }
}
