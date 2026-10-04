//! HmacSHA256 → base64 签名与签名方案。
//!
//! 飞书与钉钉的自定义机器人签名规则不同（key 与待签内容互换、时间戳
//! 精度不同、附加位置不同），但底层运算相同，收口在这里避免多处各写
//! 一遍。签名方案 [`SignKind`] 把两套规则写死在代码中，配置只指定方式
//! 标识（`feishu` / `dingtalk`），不暴露算法细节。

use base64::Engine as _;
use hmac::Mac;
use sha2::Sha256;

/// 签名方式标识：飞书（Lark）官方加签。
pub const SIGN_FEISHU: &str = "feishu";

/// 签名方式标识：钉钉官方加签。
pub const SIGN_DINGTALK: &str = "dingtalk";

/// 全部可用的签名方式标识，供配置校验提示。
pub const SIGN_KINDS: [&str; 2] = [SIGN_FEISHU, SIGN_DINGTALK];

/// 签名方案。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignKind {
    /// 飞书：以 `"{秒级时间戳}\n{secret}"` 为密钥对空串计算签名，签名与
    /// 时间戳放进请求体顶层字段。
    Feishu,
    /// 钉钉：以 `secret` 为密钥对 `"{毫秒时间戳}\n{secret}"` 计算签名，
    /// 经 URL 编码后与毫秒时间戳一并追加到地址的查询参数。
    DingTalk,
}

impl SignKind {
    /// 按配置标识解析签名方式；未知标识返回 `None`。
    #[must_use]
    pub fn from_id(id: &str) -> Option<Self> {
        match id {
            SIGN_FEISHU => Some(Self::Feishu),
            SIGN_DINGTALK => Some(Self::DingTalk),
            _ => None,
        }
    }

    /// 配置文件中的方式标识。
    #[must_use]
    pub fn id(&self) -> &'static str {
        match self {
            Self::Feishu => SIGN_FEISHU,
            Self::DingTalk => SIGN_DINGTALK,
        }
    }

    /// 以当前时刻计算签名，返回 `(时间戳, 签名值)`。
    #[must_use]
    pub fn sign(&self, secret: &str) -> (i64, String) {
        match self {
            Self::Feishu => {
                let timestamp = chrono::Utc::now().timestamp();
                (timestamp, self.sign_at(secret, timestamp))
            }
            Self::DingTalk => {
                let timestamp = chrono::Utc::now().timestamp_millis();
                (timestamp, self.sign_at(secret, timestamp))
            }
        }
    }

    /// 以给定时间戳计算签名（测试固定向量用）。
    #[must_use]
    pub fn sign_at(&self, secret: &str, timestamp: i64) -> String {
        match self {
            Self::Feishu => hmac_sha256_base64(format!("{timestamp}\n{secret}").as_bytes(), b""),
            Self::DingTalk => hmac_sha256_base64(
                secret.as_bytes(),
                format!("{timestamp}\n{secret}").as_bytes(),
            ),
        }
    }
}

/// 计算 `HmacSHA256(key, data)` 并以标准 base64 编码。
#[must_use]
pub fn hmac_sha256_base64(key: &[u8], data: &[u8]) -> String {
    let mut mac =
        <hmac::Hmac<Sha256> as Mac>::new_from_slice(key).expect("HMAC 可接受任意长度的密钥");
    mac.update(data);
    base64::engine::general_purpose::STANDARD.encode(mac.finalize().into_bytes())
}

/// 百分号编码（RFC 3986 unreserved 之外的字符全部转义）。
///
/// 只用于钉钉加签的 `sign` 查询参数——base64 里的 `+` `/` `=` 若不转义，
/// `+` 会被服务端当空格解析导致校验失败。
#[must_use]
pub fn percent_encode(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                encoded.push(*byte as char);
            }
            other => {
                encoded.push_str(&format!("%{other:02X}"));
            }
        }
    }
    encoded
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hmac_matches_known_vector() {
        // RFC 4231 用例 2：key = "Jefe"，data = "what do ya want for nothing?"
        let signature = hmac_sha256_base64(b"Jefe", b"what do ya want for nothing?");
        assert_eq!(signature, "W9zBRr9gdU5qBCQmCJV1x1oAPwidJzmDnexYuWTsOEM=");
    }

    #[test]
    fn percent_encode_escapes_base64_specials() {
        assert_eq!(percent_encode("aB+z/9="), "aB%2Bz%2F9%3D");
        assert_eq!(percent_encode("plain-_~1."), "plain-_~1.");
    }

    #[test]
    fn sign_kinds_match_reference_vectors() {
        // 期望值与参考实现（openssl dgst -sha256 -hmac）对拍得出：
        // 飞书 key = "1700000000\ntest"、data 空；钉钉反之。
        let timestamp = 1_700_000_000;
        let secret = "test";
        assert_eq!(
            SignKind::Feishu.sign_at(secret, timestamp),
            "eSJQnOl8XqPTMPHWz9e5IzeHS/tqoc68g2967ekIPmg="
        );
        assert_eq!(
            SignKind::DingTalk.sign_at(secret, timestamp),
            "kGpCwDXoxaOMw3Ib+ZushW+l3qXCzrtIgh3R3ygNLqA="
        );
    }

    #[test]
    fn sign_kind_ids_round_trip() {
        assert_eq!(SignKind::from_id("feishu"), Some(SignKind::Feishu));
        assert_eq!(SignKind::from_id("dingtalk"), Some(SignKind::DingTalk));
        assert_eq!(SignKind::from_id("hmac"), None);
        assert_eq!(SignKind::Feishu.id(), "feishu");
        assert_eq!(SignKind::DingTalk.id(), "dingtalk");
    }
}
