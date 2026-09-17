//! ACME 账号：注册、绑定、复用，以及 KID 的持久化。
//!
//! 对外暴露不透明的 [`AccountCredentials`]（一段 JSON 文本），
//! 内部承载 `instant-acme` 的账号凭据（账号 ID、私钥、服务端点）。
//! 上层只管存取这段文本，不接触底层库类型。

use base64::Engine;
use instant_acme::{Account, ExternalAccountKey, Key, NewAccount};
use rustls_pki_types::{PrivateKeyDer, PrivatePkcs8KeyDer};

use crate::error::{AcmeError, Result};

/// PKCS#8 私钥的 PEM 头尾标记。
const PKCS8_PEM_HEADER: &str = "-----BEGIN PRIVATE KEY-----";
const PKCS8_PEM_FOOTER: &str = "-----END PRIVATE KEY-----";

/// 可持久化的账号凭据。
///
/// 内容是一段不透明 JSON（`instant-acme` 的 `AccountCredentials` 序列化结果），
/// 包含账号 ID（KID）、私钥与服务端点。**必须加密后落库**——
/// 加解密由 `acmecast-core::CredentialCipher` 在上层完成，本模块不接触明文存储。
///
/// `Debug` 是手写的：内容含账号私钥，绝不原样打印。KID 本身不是秘密，
/// 保留它能让排障时仍看得出「是哪一份凭据」。
#[derive(Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct AccountCredentials(String);

impl std::fmt::Debug for AccountCredentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AccountCredentials")
            .field("kid", &self.kid())
            .field("secret", &acmecast_core::REDACTED)
            .finish()
    }
}

impl AccountCredentials {
    /// 从序列化文本还原。
    ///
    /// 校验形态是否像账号凭据，避免把无关字符串当作凭据传给底层库
    /// 后得到难以定位的错误。
    pub fn from_json(json: &str) -> Result<Self> {
        let trimmed = json.trim();
        if !trimmed.starts_with('{') || !trimmed.contains("\"id\"") {
            return Err(AcmeError::Account(format!(
                "账号凭据格式不正确：期望含 `id` 字段的 JSON，实际前 32 字符为 `{}`",
                &trimmed[..trimmed.len().min(32)]
            )));
        }
        Ok(Self(trimmed.to_owned()))
    }

    /// 序列化为文本，供上层加密后落库。
    #[must_use]
    pub fn as_json(&self) -> &str {
        &self.0
    }

    /// 反序列化为底层凭据实例（仅供本 crate 内部使用）。
    pub(crate) fn to_inner(&self) -> Result<instant_acme::AccountCredentials> {
        serde_json::from_str(&self.0)
            .map_err(|e| AcmeError::Account(format!("账号凭据无法反序列化: {e}")))
    }

    /// 从底层凭据实例构造。
    pub(crate) fn from_inner(inner: &instant_acme::AccountCredentials) -> Result<Self> {
        let json = serde_json::to_string(inner)
            .map_err(|e| AcmeError::Account(format!("账号凭据无法序列化: {e}")))?;
        Ok(Self(json))
    }
}

/// 建立账号的三种方式，对应 spec 3.4。
///
/// `Debug` 手写：其中两种方式带有账号私钥。联系方式与 KID 照常显示。
#[derive(Clone)]
pub enum AccountMode {
    /// 注册全新账号：生成新密钥并向 CA 注册。
    Create {
        /// 联系方式，通常为 `mailto:xxx` 形式的 URI。
        contacts: Vec<String>,
    },
    /// 绑定已有账号：用用户提供的私钥去 CA 换取账号 ID。
    ///
    /// 若该密钥从未在 CA 注册过，CA 会报错。
    Bind {
        /// 用户提供的 PKCS#8 私钥（PEM 或 DER 皆可，PEM 更常见）。
        key_pkcs8_pem: String,
    },
    /// 复用：已有账号 ID 与私钥，直接恢复会话，不产生任何网络注册请求。
    Reuse {
        /// PKCS#8 私钥 PEM。
        key_pkcs8_pem: String,
        /// CA 返回过的账号 ID（KID）。
        kid: String,
    },
}

impl std::fmt::Debug for AccountMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Create { contacts } => f
                .debug_struct("Create")
                .field("contacts", contacts)
                .finish(),
            Self::Bind { .. } => f
                .debug_struct("Bind")
                .field("key_pkcs8_pem", &acmecast_core::REDACTED)
                .finish(),
            Self::Reuse { kid, .. } => f
                .debug_struct("Reuse")
                .field("kid", kid)
                .field("key_pkcs8_pem", &acmecast_core::REDACTED)
                .finish(),
        }
    }
}

/// 可选的 External Account Binding 凭据。
///
/// `Debug` 手写：`hmac_key` 是秘密，`kid` 不是。
#[derive(Clone)]
pub struct ExternalAccountBinding {
    /// EAB 密钥标识（KID）。
    pub kid: String,
    /// base64url 编码的 HMAC 密钥。
    pub hmac_key: String,
}

impl std::fmt::Debug for ExternalAccountBinding {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExternalAccountBinding")
            .field("kid", &self.kid)
            .field("hmac_key", &acmecast_core::REDACTED)
            .finish()
    }
}

/// 建立账号所需的全部输入。
#[derive(Debug, Clone)]
pub struct EstablishAccountInput<'a> {
    /// Directory URL，由 [`crate::directory::resolve_directory_url`] 解析得到。
    pub directory_url: &'a str,
    /// 建立方式。
    pub mode: AccountMode,
    /// 可选的 EAB 凭据；为 `None` 时不携带 externalAccountBinding 字段。
    pub external_account: Option<&'a ExternalAccountBinding>,
}

/// 建立账号，返回可持久化的凭据。
pub(crate) async fn establish(
    input: &EstablishAccountInput<'_>,
    http: Option<Box<dyn instant_acme::HttpClient>>,
) -> Result<(Account, AccountCredentials)> {
    let builder = match http {
        Some(client) => Account::builder_with_http(client),
        None => Account::builder()
            .map_err(|e| AcmeError::Transport(format!("无法构造 ACME HTTP 客户端: {e}")))?,
    };

    let eak = match input.external_account {
        Some(eab) => Some(build_external_account_key(eab)?),
        None => None,
    };

    let (account, credentials) = match &input.mode {
        AccountMode::Create { contacts } => {
            let contact_refs: Vec<&str> = contacts.iter().map(String::as_str).collect();
            let new_account = NewAccount {
                contact: &contact_refs,
                terms_of_service_agreed: true,
                only_return_existing: false,
            };
            builder
                .create(&new_account, input.directory_url.to_owned(), eak.as_ref())
                .await
                .map_err(map_account_error)?
        }

        AccountMode::Bind { key_pkcs8_pem } => {
            let pkcs8 = decode_pkcs8(key_pkcs8_pem)?;
            let key = Key::from_pkcs8_der(pkcs8.clone_key())
                .map_err(|e| AcmeError::Crypto(format!("私钥不被接受: {e}")))?;
            builder
                .from_key(
                    (key, PrivateKeyDer::from(pkcs8)),
                    input.directory_url.to_owned(),
                )
                .await
                .map_err(map_account_error)?
        }

        AccountMode::Reuse { key_pkcs8_pem, kid } => {
            let pkcs8 = decode_pkcs8(key_pkcs8_pem)?;
            let account = builder
                .from_parts(
                    kid.clone(),
                    pkcs8.clone_key(),
                    input.directory_url.to_owned(),
                )
                .await
                .map_err(map_account_error)?;
            // `from_parts` 不返回凭据（凭据本就已存在），
            // 用 KID + 私钥 + 端点重新组装一份等价凭据。
            let credentials = AccountCredentials::from_parts(kid, &pkcs8, input.directory_url);
            return Ok((account, credentials));
        }
    };

    let credentials = AccountCredentials::from_inner(&credentials)?;
    Ok((account, credentials))
}

impl AccountCredentials {
    /// 由账号 ID、私钥与端点直接组装凭据。
    ///
    /// 用于 [`AccountMode::Reuse`]：KID 已知时无需再向 CA 请求。
    ///
    /// 这里手工拼的是**底层库的序列化格式**，两处细节不能想当然：
    /// `key_pkcs8` 是 url-safe 无填充的 base64，`directory` 是字符串而不是对象。
    /// 写错的话，凭据会一直「看起来没问题」，直到某次真正反序列化（复用账号）时才炸——
    /// 所以 [`AccountCredentials::from_inner`] 与下面的往返测试都是必要的护栏。
    pub(crate) fn from_parts(
        kid: &str,
        key_pkcs8: &PrivatePkcs8KeyDer<'_>,
        directory_url: &str,
    ) -> Self {
        let json = serde_json::json!({
            "id": kid,
            "key_pkcs8": base64::engine::general_purpose::URL_SAFE_NO_PAD
                .encode(key_pkcs8.secret_pkcs8_der()),
            "directory": directory_url,
        });
        Self(json.to_string())
    }

    /// 账号 ID（KID）。
    #[must_use]
    pub fn kid(&self) -> Option<String> {
        let value: serde_json::Value = serde_json::from_str(&self.0).ok()?;
        value.get("id")?.as_str().map(ToOwned::to_owned)
    }
}

/// 把 EAB 输入转为底层类型。
fn build_external_account_key(eab: &ExternalAccountBinding) -> Result<ExternalAccountKey> {
    // EAB 的 HMAC 密钥按 RFC 8555 §7.3.4 以 base64url 无填充形式给出。
    let decoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(eab.hmac_key.trim())
        .or_else(|_| base64::engine::general_purpose::STANDARD.decode(eab.hmac_key.trim()))
        .map_err(|e| AcmeError::Account(format!("EAB HMAC 密钥不是合法的 base64: {e}")))?;

    Ok(ExternalAccountKey::new(eab.kid.clone(), &decoded))
}

/// 从 PKCS#8 PEM 解码出 DER 字节。
fn decode_pkcs8_pem(pem: &str) -> Result<Vec<u8>> {
    let text = pem.trim();
    if let (Some(start), Some(end)) = (text.find(PKCS8_PEM_HEADER), text.rfind(PKCS8_PEM_FOOTER)) {
        let body = &text[start + PKCS8_PEM_HEADER.len()..end];
        let body: String = body.chars().filter(|c| !c.is_whitespace()).collect();
        return base64::engine::general_purpose::STANDARD
            .decode(body)
            .map_err(|e| AcmeError::Crypto(format!("私钥 PEM 解码失败: {e}")));
    }

    // 不是 PEM 时按裸 base64(DER) 处理。
    base64::engine::general_purpose::STANDARD
        .decode(text)
        .map_err(|e| AcmeError::Crypto(format!("私钥既不是 PEM 也不是合法 base64: {e}")))
}

/// 从 PKCS#8 PEM 解码出 DER 私钥。
fn decode_pkcs8(pem: &str) -> Result<PrivatePkcs8KeyDer<'static>> {
    Ok(PrivatePkcs8KeyDer::from(decode_pkcs8_pem(pem)?))
}

/// 把账号相关错误翻译成更可操作的提示。
fn map_account_error(err: instant_acme::Error) -> AcmeError {
    let mapped = AcmeError::from(err);
    match mapped {
        // CA 要求 EAB 是很常见的首次注册失败原因，给出可操作的提示。
        AcmeError::Problem {
            requires_external_account: true,
            ..
        } => AcmeError::Account(format!(
            "该 CA 要求提供 EAB（External Account Binding）凭据，\
             请在 ACME 账号中填写 EAB KID 与 HMAC 密钥后重试：{mapped}"
        )),
        other => other,
    }
}

/// 把 DER 私钥编码为 PKCS#8 PEM。
#[must_use]
pub fn encode_pkcs8_pem(der: &PrivateKeyDer<'_>) -> String {
    let body = base64::engine::general_purpose::STANDARD.encode(der.secret_der());
    let mut out = String::with_capacity(body.len() + 64);
    out.push_str(PKCS8_PEM_HEADER);
    for chunk in body.as_bytes().chunks(64) {
        out.push('\n');
        out.push_str(&String::from_utf8_lossy(chunk));
    }
    out.push('\n');
    out.push_str(PKCS8_PEM_FOOTER);
    out.push('\n');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 生成一把临时 ECDSA 私钥，返回 PEM。
    fn sample_key_pem() -> String {
        let (_, pkcs8) = Key::generate_pkcs8().expect("应能生成密钥");
        encode_pkcs8_pem(&PrivateKeyDer::from(pkcs8))
    }

    /// 生成一把临时 ECDSA 私钥，返回 PKCS#8 DER。
    fn sample_pkcs8() -> PrivatePkcs8KeyDer<'static> {
        let (_, pkcs8) = Key::generate_pkcs8().expect("应能生成密钥");
        pkcs8
    }

    #[test]
    fn generated_key_pem_roundtrips() {
        let pem = sample_key_pem();
        assert!(pem.starts_with(PKCS8_PEM_HEADER));
        assert!(pem.trim_end().ends_with(PKCS8_PEM_FOOTER));

        // 解码回来应与原 PEM 等价（内部 DER 一致）。
        let der = decode_pkcs8_pem(&pem).expect("应能解码");
        assert!(!der.is_empty());
        assert_eq!(
            encode_pkcs8_pem(&PrivateKeyDer::from(PrivatePkcs8KeyDer::from(der))),
            pem
        );
    }

    #[test]
    fn pem_decoding_tolerates_whitespace() {
        let pem = sample_key_pem();
        let mangled = pem.replace('\n', "  \n\t");
        assert!(decode_pkcs8_pem(&mangled).is_ok());
    }

    #[test]
    fn invalid_key_material_is_rejected() {
        assert!(decode_pkcs8_pem("not-a-key").is_err());
    }

    #[test]
    fn credentials_json_is_rejected_when_malformed() {
        assert!(AccountCredentials::from_json("nope").is_err());
        assert!(AccountCredentials::from_json("{\"foo\":1}").is_err());
    }

    #[test]
    fn credentials_json_is_accepted_when_shaped_right() {
        let creds =
            AccountCredentials::from_json("{\"id\":\"https://ca/acct/1\"}").expect("应接受");
        assert_eq!(creds.kid(), Some("https://ca/acct/1".to_owned()));
    }

    #[test]
    fn credentials_kid_is_extractable() {
        let creds = AccountCredentials::from_parts(
            "https://ca/acct/42",
            &sample_pkcs8(),
            "https://ca/directory",
        );
        assert_eq!(creds.kid(), Some("https://ca/acct/42".to_owned()));
    }

    #[test]
    fn credentials_from_parts_roundtrips_as_json() {
        let creds = AccountCredentials::from_parts(
            "kid-1",
            &sample_pkcs8(),
            "https://ca.internal/directory",
        );
        let reparsed = AccountCredentials::from_json(creds.as_json()).expect("应能还原");
        assert_eq!(reparsed.kid(), Some("kid-1".to_owned()));
        assert_eq!(reparsed.as_json(), creds.as_json());
    }

    #[test]
    fn credentials_from_parts_can_be_read_back_by_the_library() {
        // 手工拼的字段格式一旦与底层库不一致，这份凭据会一直「看起来没问题」——
        // 它的 kid() 照样取得出来——直到某次真正复用账号时才炸。
        // 让底层库自己解一遍，是唯一能提前发现这种偏差的办法。
        let creds = AccountCredentials::from_parts(
            "kid-9",
            &sample_pkcs8(),
            "https://ca.internal/directory",
        );

        // 解出来即可——内容归底层库管，这里要的是「它能读进这份格式」。
        let _ = creds
            .to_inner()
            .expect("from_parts 生成的 JSON 应能被底层库反序列化");
    }

    #[test]
    fn eab_hmac_accepts_urlsafe_and_standard_base64() {
        let raw = b"0123456789abcdef";
        let urlsafe = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(raw);
        let standard = base64::engine::general_purpose::STANDARD.encode(raw);

        let eab = ExternalAccountBinding {
            kid: "k".to_owned(),
            hmac_key: urlsafe,
        };
        assert!(build_external_account_key(&eab).is_ok());

        let eab = ExternalAccountBinding {
            kid: "k".to_owned(),
            hmac_key: standard,
        };
        assert!(build_external_account_key(&eab).is_ok());
    }

    #[test]
    fn eab_with_invalid_hmac_is_rejected() {
        let eab = ExternalAccountBinding {
            kid: "k".to_owned(),
            hmac_key: "!!!not-base64!!!".to_owned(),
        };
        match build_external_account_key(&eab) {
            Ok(_) => panic!("非法的 HMAC 应被拒绝"),
            Err(err) => assert!(err.to_string().contains("base64"), "{err}"),
        }
    }
}
