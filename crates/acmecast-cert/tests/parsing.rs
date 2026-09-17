//! 4.1 证书解析：PEM / DER 解析与转换。
//!
//! 证书在测试中现场生成而非把样本写进仓库——固定样本一旦过期，
//! 断言就会变成噪声，且无法构造「多 SAN」「通配符」「自签 CA」等特定形态。

use acmecast_cert::{
    CertStatus, CertificateInfo, ExpiryPolicy, chain_to_pem, der_to_pem, parse_der, parse_pem,
    parse_pem_leaf, pem_to_der_blocks,
};
use chrono::{Duration, Utc};
use rcgen::{BasicConstraints, CertificateParams, DnType, IsCa, KeyPair, KeyUsagePurpose, SanType};

/// 生成一张自签证书，返回 (DER, PEM)。
fn self_signed(
    common_name: &str,
    sans: &[&str],
    is_ca: bool,
    validity_days: i64,
) -> (Vec<u8>, String) {
    let mut params = CertificateParams::default();
    params
        .distinguished_name
        .push(DnType::CommonName, common_name);
    params.subject_alt_names = sans
        .iter()
        .map(|name| SanType::DnsName((*name).try_into().expect("域名应合法")))
        .collect();
    params.is_ca = if is_ca {
        IsCa::Ca(BasicConstraints::Unconstrained)
    } else {
        IsCa::NoCa
    };
    if is_ca {
        params.key_usages.push(KeyUsagePurpose::KeyCertSign);
    }

    let now = Utc::now();
    params.not_before = rcgen::date_time_ymd(now.format("%Y").to_string().parse().unwrap(), 1, 1);
    params.not_after = rcgen::date_time_ymd(
        (now + Duration::days(validity_days))
            .format("%Y")
            .to_string()
            .parse()
            .unwrap(),
        12,
        31,
    );

    let key_pair = KeyPair::generate().expect("应能生成密钥");
    let cert = params.self_signed(&key_pair).expect("应能自签");

    (cert.der().to_vec(), cert.pem())
}

/// 一张含多个 SAN（包含通配符）的证书。
fn multi_san() -> (Vec<u8>, String) {
    self_signed(
        "example.com",
        &["example.com", "www.example.com", "*.example.com"],
        false,
        90,
    )
}

// ---- 需求：能解析出全部 SAN 域名 ----

#[test]
fn parses_every_san_domain_in_order() {
    let (der, _pem) = multi_san();
    let info = parse_der(&der).expect("应能解析");

    assert_eq!(
        info.domains,
        vec!["example.com", "www.example.com", "*.example.com"],
        "应解析出全部 SAN 且保持证书中的原始顺序"
    );
}

#[test]
fn parses_single_domain_certificate() {
    let (der, _pem) = self_signed("solo.example.com", &["solo.example.com"], false, 30);
    let info = parse_der(&der).unwrap();

    assert_eq!(info.domains, vec!["solo.example.com"]);
    assert_eq!(info.primary_domain(), Some("solo.example.com"));
}

#[test]
fn wildcard_is_detected() {
    let (der, _pem) = self_signed("wild.test", &["*.wild.test"], false, 30);
    assert!(parse_der(&der).unwrap().is_wildcard());

    let (der, _pem) = self_signed("plain.test", &["plain.test"], false, 30);
    assert!(!parse_der(&der).unwrap().is_wildcard());
}

#[test]
fn non_dns_san_types_are_excluded() {
    // IP 与 URI 不属于 ACME 语义下的「域名」，不应混入 domains。
    let mut params = CertificateParams::default();
    params
        .distinguished_name
        .push(DnType::CommonName, "mixed.test");
    params.subject_alt_names = vec![
        SanType::DnsName("mixed.test".try_into().unwrap()),
        SanType::IpAddress("10.0.0.1".parse().unwrap()),
        SanType::URI("https://mixed.test/".try_into().unwrap()),
    ];

    let key_pair = KeyPair::generate().unwrap();
    let cert = params.self_signed(&key_pair).unwrap();
    let info = parse_der(cert.der()).unwrap();

    assert_eq!(info.domains, vec!["mixed.test"], "只应保留 DNS 类型的 SAN");
}

// ---- 需求：能解析出签发者 ----

#[test]
fn parses_issuer_and_subject_separately() {
    // 由 CA 签发的叶子证书，issuer 应指向 CA 而 subject 指向自己。
    let ca_key = KeyPair::generate().unwrap();
    let mut ca_params = CertificateParams::default();
    ca_params
        .distinguished_name
        .push(DnType::CommonName, "Test Root CA");
    ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    let ca_cert = ca_params.self_signed(&ca_key).unwrap();
    assert!(
        parse_der(ca_cert.der()).unwrap().is_ca,
        "CA 自身的证书也应是 CA"
    );

    let ee_key = KeyPair::generate().unwrap();
    let mut ee_params = CertificateParams::default();
    ee_params
        .distinguished_name
        .push(DnType::CommonName, "leaf.example.com");
    ee_params.subject_alt_names = vec![SanType::DnsName("leaf.example.com".try_into().unwrap())];

    let issuer = rcgen::Issuer::new(ca_params, ca_key);
    let ee_cert = ee_params.signed_by(&ee_key, &issuer).unwrap();

    let info = parse_der(ee_cert.der()).unwrap();
    assert!(
        info.issuer.contains("Test Root CA"),
        "issuer 应指向 CA，实际: {}",
        info.issuer
    );
    assert!(
        info.subject.contains("leaf.example.com"),
        "subject 应是自己的 CN，实际: {}",
        info.subject
    );
    assert_ne!(
        info.issuer, info.subject,
        "叶子证书的 issuer 与 subject 不应相同"
    );
}

#[test]
fn self_signed_has_matching_issuer_and_subject() {
    let (der, _pem) = self_signed("self.test", &["self.test"], false, 30);
    let info = parse_der(&der).unwrap();
    assert_eq!(
        info.issuer, info.subject,
        "自签证书的 issuer 应等于 subject"
    );
}

// ---- 需求：能解析出生效与到期时间 ----

#[test]
fn parses_validity_window() {
    let (der, _pem) = self_signed("time.test", &["time.test"], false, 90);
    let info = parse_der(&der).unwrap();

    assert!(
        info.not_after > info.not_before,
        "到期时间应晚于生效时间: {} → {}",
        info.not_before,
        info.not_after
    );
    assert!(
        info.not_after - info.not_before >= Duration::days(300),
        "跨年构造的有效期应接近一年，实际 {} 天",
        (info.not_after - info.not_before).num_days()
    );
}

#[test]
fn is_valid_at_respects_the_window() {
    let (der, _pem) = self_signed("window.test", &["window.test"], false, 90);
    let info = parse_der(&der).unwrap();

    assert!(info.is_valid_at(Utc::now()), "新生成的证书当前应有效");
    assert!(
        !info.is_valid_at(info.not_before - Duration::days(1)),
        "生效前一天应无效"
    );
    assert!(
        !info.is_valid_at(info.not_after + Duration::days(1)),
        "到期后一天应无效"
    );
}

// ---- 其他解析字段 ----

#[test]
fn fingerprint_matches_independently_computed_sha256() {
    use sha2::{Digest, Sha256};

    let (der, _pem) = multi_san();
    let info = parse_der(&der).unwrap();

    let expected: String = Sha256::digest(&der)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();

    assert_eq!(
        info.fingerprint_sha256, expected,
        "指纹应为 DER 的 SHA-256，且为小写十六进制"
    );
    assert_eq!(info.fingerprint_sha256.len(), 64);
}

#[test]
fn serial_is_present_and_non_empty() {
    let (der, _pem) = multi_san();
    let info = parse_der(&der).unwrap();
    assert!(!info.serial.is_empty(), "序列号不应为空");
}

#[test]
fn ca_certificate_is_flagged() {
    let (der, _pem) = self_signed("Root CA", &[], true, 3650);
    assert!(parse_der(&der).unwrap().is_ca, "CA 证书应被标记");

    let (der, _pem) = multi_san();
    assert!(!parse_der(&der).unwrap().is_ca, "叶子证书不应被标记为 CA");
}

#[test]
fn san_missing_falls_back_to_common_name() {
    // 旧式证书可能只有 CN 而没有 SAN。
    let mut params = CertificateParams::default();
    params
        .distinguished_name
        .push(DnType::CommonName, "legacy.test");
    params.subject_alt_names = vec![];

    let key_pair = KeyPair::generate().unwrap();
    let cert = params.self_signed(&key_pair).unwrap();

    let info = parse_der(cert.der()).unwrap();
    assert_eq!(info.domains, vec!["legacy.test"], "SAN 缺失时应回退到 CN");
}

// ---- PEM 解析与转换 ----

#[test]
fn parses_certificate_from_pem() {
    let (der, pem) = multi_san();
    let info = parse_pem_leaf(&pem).expect("应能从 PEM 解析");

    assert_eq!(
        info,
        parse_der(&der).unwrap(),
        "PEM 与 DER 应解析出相同结果"
    );
}

#[test]
fn pem_and_der_conversions_roundtrip() {
    let (der, _pem) = multi_san();

    // DER → PEM → DER
    let pem = der_to_pem(&der);
    let blocks = pem_to_der_blocks(&pem).unwrap();
    assert_eq!(blocks.len(), 1);
    assert_eq!(blocks[0], der, "转换往返后 DER 应完全一致");
}

#[test]
fn parses_a_certificate_chain() {
    // 构造 叶子 + 中间 + 根 的链。
    let leaf = self_signed("leaf.chain", &["leaf.chain"], false, 90).1;
    let intermediate = self_signed("Intermediate CA", &[], true, 3650).1;
    let root = self_signed("Root CA", &[], true, 3650).1;

    let chain_pem = format!("{leaf}{intermediate}{root}");
    let chain = parse_pem(&chain_pem).expect("应能解析证书链");

    assert_eq!(chain.len(), 3, "链中三张证书都应被解析");
    assert_eq!(chain[0].domains, vec!["leaf.chain"], "首项应是叶子证书");
    assert!(chain[1].is_ca);
    assert!(chain[2].is_ca);
}

#[test]
fn chain_to_pem_roundtrips_all_certificates() {
    let (a, _) = self_signed("a.test", &["a.test"], false, 30);
    let (b, _) = self_signed("b.test", &["b.test"], false, 30);

    let pem = chain_to_pem(&[a.clone(), b.clone()]);
    let blocks = pem_to_der_blocks(&pem).unwrap();

    assert_eq!(blocks.len(), 2);
    assert_eq!(blocks[0], a);
    assert_eq!(blocks[1], b);
}

#[test]
fn parse_pem_leaf_skips_leading_private_key() {
    // 常见的「私钥 + 证书」合并文件：应取出证书而不是报错。
    use base64::Engine;
    let (der, cert_pem) = multi_san();
    let combined = format!(
        "-----BEGIN PRIVATE KEY-----\n{}\n-----END PRIVATE KEY-----\n{cert_pem}",
        base64::engine::general_purpose::STANDARD.encode([7u8; 32])
    );

    let info = parse_pem_leaf(&combined).expect("应跳过私钥块");
    assert_eq!(info, parse_der(&der).unwrap());
}

#[test]
fn pem_with_single_certificate_is_not_treated_as_chain() {
    let (_der, pem) = multi_san();
    assert_eq!(parse_pem(&pem).unwrap().len(), 1);
}

#[test]
fn parsed_domains_survive_chain_parsing() {
    // 验证链解析不会把不同证书的域名混在一起。
    let (_, leaf_pem) = self_signed("leaf.multi", &["leaf.multi", "alt.multi"], false, 30);
    let (_, ca_pem) = self_signed("CA", &["ca.example"], true, 3650);

    let chain = parse_pem(&format!("{leaf_pem}{ca_pem}")).unwrap();
    assert_eq!(chain[0].domains, vec!["leaf.multi", "alt.multi"]);
    assert_eq!(chain[1].domains, vec!["ca.example"]);
}

#[test]
fn derived_helpers_agree_with_parsed_fields() {
    let (der, _pem) = multi_san();
    let info: CertificateInfo = parse_der(&der).unwrap();

    assert!(info.is_wildcard());
    assert_eq!(info.primary_domain(), Some("example.com"));
    assert!(info.is_valid_at(Utc::now()));
}

// ---- 4.2 解析结果串联到期判定 ----

/// 生成一张有效期由调用方指定的证书。
fn cert_with_validity(not_before: (i32, u8, u8), not_after: (i32, u8, u8)) -> CertificateInfo {
    let mut params = CertificateParams::default();
    params
        .distinguished_name
        .push(DnType::CommonName, "dated.test");
    params.subject_alt_names = vec![SanType::DnsName("dated.test".try_into().unwrap())];
    params.not_before = rcgen::date_time_ymd(not_before.0, not_before.1, not_before.2);
    params.not_after = rcgen::date_time_ymd(not_after.0, not_after.1, not_after.2);

    let key_pair = KeyPair::generate().unwrap();
    let cert = params.self_signed(&key_pair).unwrap();
    parse_der(cert.der()).unwrap()
}

#[test]
fn long_lived_certificate_parses_as_healthy() {
    let info = cert_with_validity((2020, 1, 1), (2099, 12, 31));
    assert_eq!(
        info.status_at(Utc::now(), &ExpiryPolicy::with_warn_days(30)),
        CertStatus::Healthy,
        "有效期到 2099 年的证书应判为健康"
    );
}

#[test]
fn already_expired_certificate_parses_as_expired() {
    let info = cert_with_validity((2020, 1, 1), (2021, 1, 1));
    let status = info.status_at(Utc::now(), &ExpiryPolicy::with_warn_days(30));

    assert_eq!(status, CertStatus::Expired);
    assert!(status.needs_renewal(), "过期证书应需要续期");
    assert!(info.remaining_days(Utc::now()) < 0);
}

#[test]
fn parsed_certificate_feeds_renewal_decision() {
    // 串联验证：解析 → 剩余天数 → 是否需要续期。
    let healthy = cert_with_validity((2020, 1, 1), (2099, 12, 31));
    assert!(!healthy.status(Utc::now()).needs_renewal());

    let expired = cert_with_validity((2020, 1, 1), (2021, 1, 1));
    assert!(expired.status(Utc::now()).needs_renewal());
}
