//! 5.8 凭据 Debug 脱敏。
//!
//! 一次收口检查：凡是可能承载凭据明文的类型，`{:?}` 输出都不得泄漏内容。
//!
//! 放在同一个文件里是有意的——脱敏是**整体性质**，散在各 crate 的单测里
//! 很容易漏掉某个新加的结构体；集中一处，新增类型时也更容易想起来补一条。

use acmecast_access::{AcmeAccountFields, ResolvedCredential};
use acmecast_acme::{AccountCredentials, AccountMode, ExternalAccountBinding};
use acmecast_core::config::SecurityConfig;

/// 一个够独特的明文，不会与脱敏输出里的其他文本偶然相撞。
const SECRET: &str = "sk-live-DO-NOT-LEAK-4f8a2c";
/// 脱敏占位符。
const MARK: &str = "<redacted>";

/// 断言格式化输出里没有明文，且**确实做过脱敏**（而不是恰好什么都没打印）。
fn assert_redacted(rendered: &str, label: &str) {
    assert!(
        !rendered.contains(SECRET),
        "{label} 的 Debug 输出泄漏了明文：{rendered}"
    );
    assert!(
        rendered.contains(MARK),
        "{label} 的 Debug 输出应带脱敏占位符：{rendered}"
    );
}

// ---- core ----

#[test]
fn security_config_hides_every_key() {
    let config = SecurityConfig {
        credential_key: Some(SECRET.to_owned()),
        jwt_secret: Some(SECRET.to_owned()),
        token_ttl_hours: 24,
        admin_username: "admin".to_owned(),
        admin_password_hash: Some(SECRET.to_owned()),
    };

    let rendered = format!("{config:?}");
    assert_redacted(&rendered, "SecurityConfig");

    // 非秘密字段照常显示——脱敏不该把排障信息一并抹掉。
    assert!(rendered.contains("admin"), "用户名不是秘密：{rendered}");
    assert!(rendered.contains("24"), "有效期不是秘密：{rendered}");
}

#[test]
fn security_config_still_shows_which_keys_are_missing() {
    // 「没配」与「配了但不该看」必须区分得开，否则排障时看不出是哪种情况。
    let empty = SecurityConfig::default();
    let rendered = format!("{empty:?}");

    assert!(rendered.contains("None"), "未配置应显示 None：{rendered}");
    assert!(!rendered.contains(MARK), "没有密钥时无须脱敏：{rendered}");
}

// ---- acme ----

#[test]
fn account_credentials_hides_the_key_material() {
    let credentials = AccountCredentials::from_json(&format!(
        r#"{{"id":"kid-1","key_pkcs8":"{SECRET}","directory":"https://ca.example/directory"}}"#
    ))
    .expect("应接受形态合法的凭据");

    let rendered = format!("{credentials:?}");
    assert_redacted(&rendered, "AccountCredentials");

    // KID 不是秘密，保留它才能从日志看出是哪一份凭据。
    assert!(rendered.contains("kid-1"), "KID 应保留：{rendered}");
}

#[test]
fn account_mode_hides_the_private_key_in_both_carrying_variants() {
    for mode in [
        AccountMode::Bind {
            key_pkcs8_pem: SECRET.to_owned(),
        },
        AccountMode::Reuse {
            key_pkcs8_pem: SECRET.to_owned(),
            kid: "kid-1".to_owned(),
        },
    ] {
        let rendered = format!("{mode:?}");
        assert_redacted(&rendered, "AccountMode");
    }

    // 不携带私钥的那种不该被无谓地打码。
    let created = AccountMode::Create {
        contacts: vec!["mailto:ops@example.com".to_owned()],
    };
    let rendered = format!("{created:?}");
    assert!(rendered.contains("ops@example.com"), "{rendered}");
}

#[test]
fn external_account_binding_hides_the_hmac_key() {
    let binding = ExternalAccountBinding {
        kid: "eab-kid".to_owned(),
        hmac_key: SECRET.to_owned(),
    };

    let rendered = format!("{binding:?}");
    assert_redacted(&rendered, "ExternalAccountBinding");
    assert!(
        rendered.contains("eab-kid"),
        "EAB 的 KID 应保留：{rendered}"
    );
}

// ---- access ----

#[test]
fn acme_account_fields_hides_the_secrets() {
    let fields = AcmeAccountFields {
        ca: "letsencrypt".to_owned(),
        directory_url: Some("https://acme-v02.api.letsencrypt.org/directory".to_owned()),
        eab_kid: Some("eab-kid".to_owned()),
        eab_hmac_key: Some(SECRET.to_owned()),
        credentials: Some(format!(r#"{{"id":"kid-1","key_pkcs8":"{SECRET}"}}"#)),
    };

    let rendered = format!("{fields:?}");
    assert_redacted(&rendered, "AcmeAccountFields");

    // CA 与 Directory 不是秘密，正是排障时要看的东西。
    assert!(rendered.contains("letsencrypt"), "{rendered}");
    assert!(
        rendered.contains("acme-v02.api.letsencrypt.org"),
        "{rendered}"
    );
}

#[test]
fn resolved_credential_hides_the_decrypted_fields() {
    // 这是最要紧的一处：`fields` 就是解密后的明文本身。
    let resolved = ResolvedCredential {
        id: 7,
        name: "生产 DNS".to_owned(),
        type_id: "acme.account".to_owned(),
        fields: serde_json::json!({ "api_token": SECRET }),
    };

    let rendered = format!("{resolved:?}");
    assert_redacted(&rendered, "ResolvedCredential");

    // 标识与类型照常显示，否则日志里只剩一个光秃秃的 `<redacted>`。
    assert!(rendered.contains('7'), "{rendered}");
    assert!(rendered.contains("生产 DNS"), "{rendered}");
    assert!(rendered.contains("acme.account"), "{rendered}");
}

#[test]
fn the_placeholder_is_shared_across_crates() {
    // 各 crate 用同一个占位符，避免「看起来脱敏了、其实拼错了」。
    let resolved = ResolvedCredential {
        id: 1,
        name: "x".to_owned(),
        type_id: "y".to_owned(),
        fields: serde_json::json!({ "secret": SECRET }),
    };
    assert!(
        format!("{resolved:?}").contains(acmecast_core::REDACTED),
        "应使用 core 定义的统一占位符"
    );
}
