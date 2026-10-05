//! HTTP-01 挑战的应答内容。
//!
//! 与 DNS-01 的关系是**对称的**：两者都由 ACME 客户端给出「这次要证明什么」，
//! 区别只在投放方式——DNS-01 写一条 TXT 记录，HTTP-01 在一个约定路径下
//! 返回一段文本。
//!
//! 本模块只负责**算出该投放什么**。把它真正挂到可达的 HTTP 路径上由调用方完成：
//! spec 明确「由调用方负责把授权串投放到可达的 HTTP 路径」——那属于部署或服务层，
//! 不是挑战本身的事。

use acmecast_acme::{ChallengeInfo, ChallengeKind as AcmeChallengeKind, ChallengeMaterials};

use crate::error::{Error, Result};

/// HTTP-01 需要投放的内容。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Http01Answer {
    /// ACME 令牌。
    pub token: String,
    /// 需要投放的完整路径，形如 `/.well-known/acme-challenge/{token}`。
    pub path: String,
    /// 该路径应返回的响应体：完整的键值授权串（`{token}.{账号密钥指纹}`）。
    pub key_authorization: String,
}

impl Http01Answer {
    /// 由 ACME 的挑战信息与应答材料组装。
    ///
    /// 拿到的不是 HTTP-01 挑战时返回错误：DNS-01 的材料里也有一个
    /// `key_authorization` 字段，但那是给摘要用的原料，直接拿去当 HTTP 响应体
    /// 会得到一个**看起来合理**却永远校验不过的结果。
    pub fn from_challenge(info: &ChallengeInfo, materials: &ChallengeMaterials) -> Result<Self> {
        if info.kind != AcmeChallengeKind::Http01 {
            return Err(Error::UnsupportedChallenge(format!(
                "这是 {} 挑战，不是 HTTP-01",
                info.kind.as_acme_name()
            )));
        }

        let answer = Self {
            token: info.token.clone(),
            path: info.http_path.clone(),
            key_authorization: materials.key_authorization.clone(),
        };
        answer.ensure_well_formed()?;
        Ok(answer)
    }

    /// 授权串是否确实是「令牌 + 指纹」的形式。
    ///
    /// spec 要求返回**完整的**键值授权串。写错这个串时 CA 只会回一个笼统的
    /// 校验失败，因此在这里先自检一遍：它以令牌开头、后面还跟着指纹。
    pub fn ensure_well_formed(&self) -> Result<()> {
        let prefix = format!("{}.", self.token);
        if !self.key_authorization.starts_with(&prefix) {
            return Err(Error::invalid_credentials(
                "key_authorization",
                format!(
                    "键值授权串应以 `{prefix}` 开头，实际是 `{}`",
                    self.key_authorization
                ),
            ));
        }
        if self.key_authorization.len() == prefix.len() {
            return Err(Error::invalid_credentials(
                "key_authorization",
                "键值授权串缺少账号密钥指纹部分",
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use acmecast_acme::ChallengeKind as AcmeKind;

    use crate::challenge::ChallengeKind;

    /// 一份 HTTP-01 的挑战信息。
    fn http_challenge(token: &str) -> ChallengeInfo {
        ChallengeInfo {
            kind: AcmeKind::Http01,
            token: token.to_owned(),
            http_path: format!("/.well-known/acme-challenge/{token}"),
        }
    }

    fn materials(key_authorization: &str) -> ChallengeMaterials {
        ChallengeMaterials {
            key_authorization: key_authorization.to_owned(),
            dns_txt_value: "无关".to_owned(),
        }
    }

    #[test]
    fn an_http01_challenge_yields_token_path_and_authorization() {
        // spec 场景：返回令牌与需投放的完整键值授权串。
        let answer = Http01Answer::from_challenge(
            &http_challenge("token-abc"),
            &materials("token-abc.thumbprint-xyz"),
        )
        .expect("应能组装");

        assert_eq!(answer.token, "token-abc");
        assert_eq!(answer.path, "/.well-known/acme-challenge/token-abc");
        assert_eq!(answer.key_authorization, "token-abc.thumbprint-xyz");
    }

    #[test]
    fn a_dns01_challenge_is_refused() {
        // DNS-01 的材料里也有 key_authorization，但那是给摘要用的原料；
        // 误当成 HTTP 响应体会得到一个「看起来合理」却永远校验不过的结果。
        let dns = ChallengeInfo {
            kind: AcmeKind::Dns01,
            token: "token-abc".to_owned(),
            http_path: String::new(),
        };

        let err = Http01Answer::from_challenge(&dns, &materials("token-abc.x"))
            .expect_err("DNS-01 不该被当成 HTTP-01");
        assert!(err.to_string().contains("dns-01"), "{err}");
    }

    #[test]
    fn an_authorization_without_the_token_prefix_is_refused() {
        let err =
            Http01Answer::from_challenge(&http_challenge("token-abc"), &materials("别的什么"))
                .expect_err("授权串不以令牌开头应被拒绝");
        assert!(err.to_string().contains("token-abc"), "{err}");
    }

    #[test]
    fn an_authorization_missing_the_thumbprint_is_refused() {
        // 只有令牌、没有指纹：长这样但校验必挂。
        let err =
            Http01Answer::from_challenge(&http_challenge("token-abc"), &materials("token-abc."))
                .expect_err("缺少指纹部分应被拒绝");
        assert!(err.to_string().contains("指纹"), "{err}");
    }

    // ---- 与协议层类型的转换 ----

    #[test]
    fn the_protocol_kinds_we_support_convert_cleanly() {
        assert_eq!(
            ChallengeKind::try_from(AcmeKind::Dns01).unwrap(),
            ChallengeKind::Dns01
        );
        assert_eq!(
            ChallengeKind::try_from(AcmeKind::Http01).unwrap(),
            ChallengeKind::Http01
        );
    }

    #[test]
    fn an_unsupported_protocol_kind_is_refused_by_name() {
        let err = ChallengeKind::try_from(AcmeKind::TlsAlpn01).expect_err("首版不支持 TLS-ALPN-01");
        assert!(err.to_string().contains("tls-alpn-01"), "{err}");
    }
}
