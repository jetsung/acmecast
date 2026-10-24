//! 内置的部署目标。

use crate::target::CertMaterials;

pub mod local;
pub mod s3;
pub mod ssh;
pub mod tencent;
pub mod webdav;

pub use local::{LocalInput, LocalTarget};
pub use s3::{S3Input, S3Target};
pub use ssh::{
    BastionConfig, ResolvedSshConfig, RusshConnector, SshAuth, SshAuthSource, SshConnector,
    SshInput, SshTarget, SshTransport, probe_host,
};
pub use tencent::{
    TencentSslInput, TencentSslTarget, format_cert_label, resolve_tencent_credentials,
};
pub use webdav::{WebdavInput, WebdavTarget};

/// 把「以 `/` 结尾视为目录」的字段补全为完整文件路径。
///
/// 空白、缺省与非斜杠后缀保持原样。补全只做字符串拼接、不探测文件系统：
/// 本地与远端（SSH/S3/WebDAV）必须用同一判据，否则保存时的入库结果会和
/// 执行期兜底产生分歧。
pub(crate) fn fill_directory_field(
    config: &mut serde_json::Map<String, serde_json::Value>,
    field: &str,
    default_name: &str,
) {
    let Some(serde_json::Value::String(value)) = config.get_mut(field) else {
        return;
    };
    let trimmed = value.trim();
    if trimmed.is_empty() || !trimmed.ends_with('/') {
        return;
    }
    let filled = format!("{trimmed}{default_name}");
    *value = filled;
}

/// 归一化对象键/基路径前缀：去空白与首尾 `/`，非空时补一个 `/`。
pub(crate) fn normalize_joined_prefix(
    config: &mut serde_json::Map<String, serde_json::Value>,
    field: &str,
) {
    let Some(serde_json::Value::String(value)) = config.get_mut(field) else {
        return;
    };
    let trimmed = value.trim().trim_matches('/').to_owned();
    *value = if trimmed.is_empty() {
        trimmed
    } else {
        format!("{trimmed}/")
    };
}

/// 证书主域 → 文件名前缀（`*.example.com` → `_wildcard.example.com`）。
///
/// 与 `certs/ssl/` 落盘命名同源的清洗规则：通配转 `_wildcard`，其余字符
/// 保留字母数字与 `. - _`，空结果回落 `certificate`。
pub(crate) fn domain_file_prefix(domain: &str) -> String {
    let normalized = if let Some(stripped) = domain.strip_prefix("*.") {
        format!("_wildcard.{stripped}")
    } else if domain == "*" {
        "_wildcard".to_owned()
    } else {
        domain.to_owned()
    };
    let cleaned: String = normalized
        .chars()
        .map(|c| match c {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '.' | '-' | '_' => c,
            _ => '_',
        })
        .collect();
    if cleaned.is_empty() {
        "certificate".to_owned()
    } else {
        cleaned
    }
}

/// 从证书材料解析主域并转文件名前缀；无主域时返回 `None`。
pub(crate) fn primary_domain_prefix(materials: &CertMaterials) -> Option<String> {
    let leaf = acmecast_cert::parse_pem_leaf(&materials.chain_pem).ok()?;
    let domain = leaf.primary_domain()?;
    Some(domain_file_prefix(domain))
}

/// 目录补全的三件默认文件名（fullchain / ca / key 顺序）。
///
/// 主域前缀已知时为 `<prefix>.fullchain.cer` 等（与 `certs/ssl/` 落盘命名
/// 同源）；未知时回落静态名 `fullchain.cer` / `ca.cer` / `key`。
pub(crate) fn default_file_names(prefix: Option<&str>) -> [String; 3] {
    match prefix {
        Some(prefix) => [
            format!("{prefix}.fullchain.cer"),
            format!("{prefix}.ca.cer"),
            format!("{prefix}.key"),
        ],
        None => [
            "fullchain.cer".to_owned(),
            "ca.cer".to_owned(),
            "key".to_owned(),
        ],
    }
}

/// 拼接基路径与文件路径，保证衔接处只有且仅有一个 `/`。
///
/// 基路径的尾部 `/` 与文件路径的头部 `/` 都先剥掉再用单个 `/` 衔接，
/// 路径内部的连续 `/` 一并折叠——协议部分（`s3://`、`https://`）在上游
/// 单独组装，不经过这里。
pub(crate) fn join_path(base: &str, path: &str) -> String {
    let base = base.trim_end_matches('/');
    let path = path.trim_start_matches('/');
    let joined = if base.is_empty() {
        path.to_owned()
    } else if path.is_empty() {
        base.to_owned()
    } else {
        format!("{base}/{path}")
    };
    let mut collapsed = String::with_capacity(joined.len());
    let mut prev_slash = false;
    for ch in joined.chars() {
        if ch == '/' {
            if prev_slash {
                continue;
            }
            prev_slash = true;
        } else {
            prev_slash = false;
        }
        collapsed.push(ch);
    }
    collapsed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn join_collapses_the_junction_to_a_single_slash() {
        assert_eq!(join_path("acme/", "/tmp/t3/x.cer"), "acme/tmp/t3/x.cer");
        assert_eq!(join_path("dav/acmecast", "/x.key"), "dav/acmecast/x.key");
        assert_eq!(join_path("", "/a/b"), "a/b");
        assert_eq!(join_path("a//b", "//c//d"), "a/b/c/d");
        assert_eq!(join_path("a/", "b"), "a/b");
    }
}
