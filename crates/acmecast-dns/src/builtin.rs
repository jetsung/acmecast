//! 内置 DNS/DoT/DoH 解析器清单。
//!
//! 规格：
//! - 规模受控，不直接消费 `dnsdata/` 全量高密度数据（避免第三方订阅与重复解析开销）。
//! - 选型以国内外主流公共 DNS 为代表，国内可达优先，海外兜底。
//! - 端点按归一化去重，调用方无需自行去重。

/// DNS（UDP 53）常用端点：`ip` 形式，供传统 DNS 查询。
pub const DNS_SERVERS: &[&str] = &[
    "223.5.5.5",       // 阿里
    "223.6.6.6",       // 阿里
    "119.29.29.29",    // 腾讯 DNSPod
    "114.114.114.114", // 114
    "114.114.115.115", // 114
    "180.76.76.76",    // 百度
    "1.1.1.1",         // Cloudflare
    "8.8.8.8",         // Google
    "9.9.9.9",         // Quad9
    "149.112.112.112", // Quad9
];

/// DoT（853/TLS）常用端点：`host` 形式，端口 853 由调用方附回。
pub const DOT_SERVERS: &[&str] = &[
    "dns.alidns.com",
    "dot.pub",
    "dns.google",
    "one.one.one.one",
    "dns.quad9.net",
];

/// DoH（HTTPS）常用端点：完整 `https://` URL。
pub const DOH_SERVERS: &[&str] = &[
    "https://dns.alidns.com/dns-query",
    "https://doh.pub/dns-query",
    "https://1.12.12.12/dns-query",
    "https://120.53.53.53/dns-query",
    "https://223.5.5.5/dns-query",
    "https://dns.google/dns-query",
    "https://cloudflare-dns.com/dns-query",
    "https://1.1.1.1/dns-query",
];

/// 权威引导阶段的可信公共 DNS（仅一跳，不走系统 resolv.conf）。
pub const BOOTSTRAP_SERVERS: &[&str] = &[
    "223.5.5.5",
    "119.29.29.29",
    "114.114.114.114",
    "180.76.76.76",
    "1.1.1.1",
    "8.8.8.8",
    "9.9.9.9",
];
