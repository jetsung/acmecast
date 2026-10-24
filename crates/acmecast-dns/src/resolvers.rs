//! 解析器配置的外部扩展：统一 `config.toml` 的 `[[resolvers]]` + 环境变量合并。
//!
//! - 解析器条目来自 `config.toml` 的 `[[resolvers]]` 段（`type + endpoint`），
//!   由调用方解析后传入；不再读取 `ACMECAST_DNS_RESOLVERS_FILE`。
//! - `type` 仅校验，不改变 DoH 行为；`dot`/`dns` 目前仅校验并给出诊断提示。
//! - 合并顺序：builtin → config 条目 → env，按 endpoint 去重。

use std::collections::HashSet;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::http::HttpTransport;
use crate::propagation::{DnsResolver, DohResolver};

/// 单个解析器条目，对应 `config.toml` 中一个 `[[resolvers]]`。
///
/// 字段经 `serde` 直接反序列化，端点的语义校验（如 DoH 须 `https://`）
/// 在合并时进行，非法条目跳过并告警。
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct ResolverEntry {
    /// 解析器类型：`doh`/`dot`/`dns`。
    #[serde(rename = "type")]
    pub kind: String,
    /// 端点：DoH 为完整 `https://` URL，DoT 为 `host[:port]`，DNS 为 IP。
    pub endpoint: String,
    /// 可选名称；未提供时由端点主机名推导。
    #[serde(default)]
    pub name: Option<String>,
}

/// 合并内置解析器、config 条目与环境变量，按 endpoint 去重。
///
/// - `ACMECAST_DOH_RESOLVERS=none` 时短路返回空集。
/// - `entries` 中的非法条目跳过并 `warn`，不影响其余条目。
/// - `dot`/`dns` 条目仅做校验，去重键为归一化后的 endpoint。
#[must_use]
pub fn load_extended_resolvers(
    http: Arc<dyn HttpTransport>,
    entries: &[ResolverEntry],
) -> Vec<Box<dyn DnsResolver>> {
    let builtin = crate::propagation::default_resolvers(Arc::clone(&http));
    // `ACMECAST_DOH_RESOLVERS=none` 时短路，不再合并扩展。
    if std::env::var(crate::propagation::RESOLVERS_ENV_KEY)
        .is_ok_and(|v| v.trim().eq_ignore_ascii_case("none"))
    {
        return Vec::new();
    }

    if entries.is_empty() {
        return builtin;
    }

    let mut seen: HashSet<String> = builtin
        .iter()
        .map(|r| normalize(r.name().to_owned()))
        .collect();
    // builtin 已按 endpoint 去重过；这里用 endpoint 的归一化键去重。
    let mut builtin_endpoints: HashSet<String> = HashSet::new();
    for r in &builtin {
        // 仅 DoH 有真实 endpoint，其他类型名称即键；DoH 的端点可通过 name 反推受限，故用 name 的归一化近似。
        builtin_endpoints.insert(normalize_endpoint(r.name()));
    }
    let mut out = builtin;
    let mut added = 0usize;
    let mut skipped = 0usize;

    for entry in entries {
        let kind = entry.kind.trim().to_lowercase();
        let endpoint = entry.endpoint.trim().to_owned();
        if endpoint.is_empty() {
            tracing::warn!(kind = %kind, "解析器条目 endpoint 为空，已跳过");
            skipped += 1;
            continue;
        }
        let key = match kind.as_str() {
            "doh" => {
                if !endpoint.starts_with("https://") {
                    tracing::warn!(endpoint = %endpoint, "DoH endpoint 必须以 https:// 开头，已跳过");
                    skipped += 1;
                    continue;
                }
                normalize_endpoint(&endpoint)
            }
            "dot" => {
                if endpoint.contains("://") {
                    tracing::warn!(endpoint = %endpoint, "DoT endpoint 应为 host[:port] 形式，已跳过");
                    skipped += 1;
                    continue;
                }
                tracing::warn!(endpoint = %endpoint, "DoT 解析器已配置但传输层尚未实现，已跳过");
                skipped += 1;
                continue;
            }
            "dns" => {
                if endpoint.parse::<std::net::IpAddr>().is_err() {
                    tracing::warn!(endpoint = %endpoint, "DNS endpoint 必须为 IP，已跳过");
                    skipped += 1;
                    continue;
                }
                normalize_endpoint(&endpoint)
            }
            _ => {
                tracing::warn!(kind = %kind, endpoint = %endpoint, "未知解析器 type，已跳过");
                skipped += 1;
                continue;
            }
        };

        if builtin_endpoints.contains(&key) || seen.contains(&key) {
            continue;
        }
        let name = entry
            .name
            .clone()
            .filter(|n| !n.trim().is_empty())
            .unwrap_or_else(|| crate::propagation::host_of(&endpoint).to_owned());
        out.push(Box::new(DohResolver::custom(
            Arc::clone(&http),
            name,
            endpoint,
        )));
        seen.insert(key.clone());
        builtin_endpoints.insert(key);
        added += 1;
    }

    if added > 0 || skipped > 0 {
        tracing::info!(added, skipped, "已应用 config.toml 解析器扩展");
    }

    // 最后与 ACMECAST_DOH_RESOLVERS 的环境变量条目再去重一次（环境变量优先级更高但不覆盖已去重键）。
    if let Ok(spec) = std::env::var(crate::propagation::RESOLVERS_ENV_KEY) {
        if spec.trim().eq_ignore_ascii_case("none") || spec.trim().is_empty() {
            return out;
        }
        // 解析环境变量条目并追加去重。
        for raw in spec.split(',') {
            let raw = raw.trim();
            if raw.is_empty() {
                continue;
            }
            let (name, endpoint) = match raw.split_once('=') {
                Some((n, e)) => (n.trim().to_owned(), e.trim().to_owned()),
                None => (crate::propagation::host_of(raw).to_owned(), raw.to_owned()),
            };
            if endpoint.is_empty() || !endpoint.starts_with("https://") {
                continue;
            }
            let key = normalize_endpoint(&endpoint);
            if seen.contains(&key) || builtin_endpoints.contains(&key) {
                continue;
            }
            out.push(Box::new(DohResolver::custom(
                Arc::clone(&http),
                name,
                endpoint.clone(),
            )));
            seen.insert(key.clone());
            builtin_endpoints.insert(key);
        }
    }

    out
}

fn normalize(s: String) -> String {
    s.trim().to_lowercase()
}

/// 全局出网代理的环境变量键名。
///
/// 与 `ServerConfig.proxy_url` 同源（同一份配置，合并优先级
/// `builtin < config.toml < env`）：DoH 解析器与凭据 `proxy_mode=inherit`
/// 共用它。原 DoH 专用的 `ACMECAST_DOH_PROXY_URL` 已废弃合并到此处。
pub const PROXY_URL_ENV: &str = "ACMECAST_PROXY_URL";

/// 按给定代理 URL（`None` 即直连）构建 DoH 解析器专用传输层。
///
/// 校验口径与凭据级 `proxy_url` 完全一致（同一 [`validate_proxy_url`]）；
/// 该传输层**仅供传播 DoH 解析器使用**——凭据的出网客户端各自独立构建，
/// 不会读到这里的配置，未配 `proxy_url` 的凭据始终保持直连。
pub fn doh_transport_from(proxy_url: Option<&str>) -> crate::Result<Arc<dyn HttpTransport>> {
    let Some(url) = proxy_url.map(str::trim).filter(|v| !v.is_empty()) else {
        return Ok(Arc::new(crate::ReqwestTransport::new(
            acmecast_core::NETWORK_TIMEOUT,
        )?));
    };
    crate::validate_proxy_url(url)?;
    Ok(Arc::new(crate::ReqwestTransport::with_proxy(
        url,
        acmecast_core::NETWORK_TIMEOUT,
    )?))
}

/// 从环境变量 [`PROXY_URL_ENV`] 读取全局代理并构建 DoH 解析器传输层。
///
/// 未设置或留空即直连；非法值（不支持的 scheme、无法解析）返回错误，
/// 由调用方决定拒绝启动。
pub fn doh_transport() -> crate::Result<Arc<dyn HttpTransport>> {
    doh_transport_from(std::env::var(PROXY_URL_ENV).ok().as_deref())
}

fn normalize_endpoint(endpoint: &str) -> String {
    endpoint.trim().trim_end_matches('/').to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_doh_transport_defaults_to_direct_connection() {
        // 未配置（None）与留空都应走直连且构建成功。
        doh_transport_from(None).expect("未配置应直连构建成功");
        doh_transport_from(Some("")).expect("留空应视为直连");
        doh_transport_from(Some("   ")).expect("空白应视为直连");
    }

    #[test]
    fn a_doh_transport_with_a_valid_proxy_is_built() {
        for url in [
            "socks5://127.0.0.1:7890",
            "http://proxy.local:8080",
            "https://p.example.com",
        ] {
            doh_transport_from(Some(url))
                .unwrap_or_else(|e| panic!("{url} 应能构建 DoH 传输层: {e}"));
        }
    }

    #[test]
    fn an_invalid_doh_proxy_is_rejected_with_scheme_hint() {
        for url in ["ftp://127.0.0.1:21", "not-a-url"] {
            let err = doh_transport_from(Some(url)).expect_err("非法代理应被拒绝");
            assert!(
                err.to_string().contains("socks5"),
                "应提示支持的 scheme: {err}"
            );
        }
    }

    /// `doh_transport()` 读统一后的 `ACMECAST_PROXY_URL`（与
    /// `ServerConfig.proxy_url` 同源）；非法值同样被拒。
    #[test]
    fn the_env_key_is_the_unified_proxy_url() {
        assert_eq!(PROXY_URL_ENV, "ACMECAST_PROXY_URL");
    }
}
