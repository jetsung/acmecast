//! 4.3 私钥与证书公钥一致性校验。
//!
//! 用 `rcgen` 现场生成密钥对与自签证书，覆盖匹配、不匹配、跨算法三种情形。

use acmecast_cert::{
    Error, KeyAlgorithm, detect_algorithm, matches_der, public_key_bytes, verify_matches_der,
    verify_matches_pem,
};
use rcgen::{CertificateParams, KeyPair, SignatureAlgorithm};

/// 生成一对密钥及其自签证书，返回 (私钥 PEM, 证书 PEM, 证书 DER)。
fn keypair_and_cert(alg: &'static SignatureAlgorithm) -> (String, String, Vec<u8>) {
    let key_pair = KeyPair::generate_for(alg).expect("应能生成密钥");
    let params = CertificateParams::default();
    let cert = params.self_signed(&key_pair).expect("应能自签");

    (key_pair.serialize_pem(), cert.pem(), cert.der().to_vec())
}

/// 默认算法（ECDSA P-256）的一对密钥与证书。
fn default_pair() -> (String, String, Vec<u8>) {
    keypair_and_cert(&rcgen::PKCS_ECDSA_P256_SHA256)
}

// ---- 需求：匹配时通过 ----

#[test]
fn matching_key_and_certificate_are_accepted() {
    let (key_pem, cert_pem, _der) = default_pair();
    verify_matches_pem(&key_pem, &cert_pem).expect("同一密钥生成的证书与私钥应匹配");
}

#[test]
fn matches_der_reports_true_for_matching_pair() {
    let (key_pem, _cert_pem, der) = default_pair();
    assert!(matches_der(&key_pem, &der).unwrap());
}

#[test]
fn verify_matches_der_succeeds_for_matching_pair() {
    let (key_pem, _cert_pem, der) = default_pair();
    verify_matches_der(&key_pem, &der).expect("应通过校验");
}

// ---- 需求：不匹配时拒绝 ----

#[test]
fn mismatched_key_is_rejected_by_verify() {
    let (key_a, _cert_a, _) = default_pair();
    let (_key_b, cert_b, _) = default_pair();

    match verify_matches_pem(&key_a, &cert_b) {
        Ok(()) => panic!("不同密钥生成的证书与私钥不应匹配"),
        Err(err) => {
            assert!(matches!(err, Error::KeyMismatch(_)), "{err:?}");
            let text = err.to_string();
            assert!(text.contains("不匹配"), "错误应说明不匹配: {text}");
            assert!(text.contains("ECDSA"), "错误应指出私钥算法以便定位: {text}");
        }
    }
}

#[test]
fn matches_der_reports_false_for_mismatched_pair() {
    let (key_a, _cert_a, _) = default_pair();
    let (_key_b, _cert_b, der_b) = default_pair();

    assert!(
        !matches_der(&key_a, &der_b).unwrap(),
        "应返回 false 而非报错——不匹配是正常结果"
    );
}

#[test]
fn same_key_but_different_certificate_subject_still_matches() {
    // 同一密钥签发的两张不同证书（指纹不同），公钥相同，都应匹配。
    let key_pair = KeyPair::generate().unwrap();
    let key_pem = key_pair.serialize_pem();

    let mut params_a = CertificateParams::default();
    params_a
        .distinguished_name
        .push(rcgen::DnType::CommonName, "a.test");
    let cert_a = params_a.self_signed(&key_pair).unwrap();

    let mut params_b = CertificateParams::default();
    params_b
        .distinguished_name
        .push(rcgen::DnType::CommonName, "b.test");
    let cert_b = params_b.self_signed(&key_pair).unwrap();

    assert_ne!(
        cert_a.der().to_vec(),
        cert_b.der().to_vec(),
        "两张证书应确实不同"
    );
    verify_matches_der(&key_pem, cert_a.der()).unwrap();
    verify_matches_der(&key_pem, cert_b.der()).unwrap();
}

// ---- 跨算法 ----

#[test]
fn cross_algorithm_key_certificate_is_rejected() {
    let (p256_key, _, _) = keypair_and_cert(&rcgen::PKCS_ECDSA_P256_SHA256);
    let (_, _, p384_der) = keypair_and_cert(&rcgen::PKCS_ECDSA_P384_SHA384);

    // P-256 的公钥 65 字节，P-384 是 97 字节——长度都不同，必然不匹配。
    assert!(
        !matches_der(&p256_key, &p384_der).unwrap(),
        "不同曲线的密钥与证书不应匹配"
    );
}

// ---- 算法识别 ----

#[test]
fn detects_ecdsa_p256() {
    let (key_pem, _, _) = keypair_and_cert(&rcgen::PKCS_ECDSA_P256_SHA256);
    assert_eq!(detect_algorithm(&key_pem).unwrap(), KeyAlgorithm::EcdsaP256);
}

#[test]
fn detects_ecdsa_p384() {
    let (key_pem, _, _) = keypair_and_cert(&rcgen::PKCS_ECDSA_P384_SHA384);
    assert_eq!(detect_algorithm(&key_pem).unwrap(), KeyAlgorithm::EcdsaP384);
}

#[test]
fn detects_ed25519() {
    let (key_pem, _, _) = keypair_and_cert(&rcgen::PKCS_ED25519);
    assert_eq!(detect_algorithm(&key_pem).unwrap(), KeyAlgorithm::Ed25519);
}

#[test]
fn ed25519_key_and_certificate_match() {
    let (key_pem, cert_pem, _) = keypair_and_cert(&rcgen::PKCS_ED25519);
    verify_matches_pem(&key_pem, &cert_pem).expect("Ed25519 密钥与证书应匹配");
}

#[test]
fn p384_key_and_certificate_match() {
    let (key_pem, cert_pem, _) = keypair_and_cert(&rcgen::PKCS_ECDSA_P384_SHA384);
    verify_matches_pem(&key_pem, &cert_pem).expect("P-384 密钥与证书应匹配");
}

#[test]
fn detected_algorithm_name_is_human_readable() {
    let (key_pem, _, _) = keypair_and_cert(&rcgen::PKCS_ECDSA_P256_SHA256);
    let name = detect_algorithm(&key_pem).unwrap().as_str();
    assert_eq!(name, "ECDSA P-256");
}

// ---- 公钥字节的可比性 ----

#[test]
fn public_key_bytes_length_matches_algorithm() {
    // EC 公钥是未压缩点：P-256 为 65 字节（04 || X || Y），P-384 为 97 字节。
    let (p256_key, _, _) = keypair_and_cert(&rcgen::PKCS_ECDSA_P256_SHA256);
    assert_eq!(public_key_bytes(&p256_key).unwrap().len(), 65);

    let (p384_key, _, _) = keypair_and_cert(&rcgen::PKCS_ECDSA_P384_SHA384);
    assert_eq!(public_key_bytes(&p384_key).unwrap().len(), 97);

    // Ed25519 公钥固定 32 字节。
    let (ed_key, _, _) = keypair_and_cert(&rcgen::PKCS_ED25519);
    assert_eq!(public_key_bytes(&ed_key).unwrap().len(), 32);
}

#[test]
fn public_key_bytes_are_stable_across_calls() {
    let (key_pem, _, _) = default_pair();
    assert_eq!(
        public_key_bytes(&key_pem).unwrap(),
        public_key_bytes(&key_pem).unwrap()
    );
}

// ---- 非法输入 ----

#[test]
fn garbage_private_key_is_rejected() {
    let (_key, cert_pem, _) = default_pair();
    match verify_matches_pem("not a key at all", &cert_pem) {
        Ok(()) => panic!("非私钥文本应报错"),
        Err(err) => assert!(matches!(err, Error::KeyMismatch(_)), "{err:?}"),
    }
}

#[test]
fn pkcs1_style_key_hints_at_conversion() {
    // PKCS#1 的 `BEGIN RSA PRIVATE KEY` 不是 PKCS#8，应提示转换方式。
    let pkcs1 = "-----BEGIN RSA PRIVATE KEY-----\nMIIBOgIBAAJBAK\n-----END RSA PRIVATE KEY-----\n";
    let (_key, cert_pem, _) = default_pair();

    match verify_matches_pem(pkcs1, &cert_pem) {
        Ok(()) => panic!("PKCS#1 私钥应被拒绝"),
        Err(err) => {
            let text = err.to_string();
            assert!(text.contains("PKCS#8"), "{text}");
        }
    }
}

#[test]
fn garbage_certificate_is_rejected() {
    let (key_pem, _, _) = default_pair();
    match verify_matches_pem(&key_pem, "not a certificate") {
        Ok(()) => panic!("非证书文本应报错"),
        Err(err) => assert!(
            !matches!(err, Error::KeyMismatch(_)),
            "应是 PEM 错误: {err:?}"
        ),
    }
}

#[test]
fn mismatched_key_is_never_silently_accepted() {
    // 防御性断言：连续多对随机密钥之间都不应互相匹配。
    let mut keys = Vec::new();
    let mut ders = Vec::new();
    for _ in 0..5 {
        let (key, _cert, der) = default_pair();
        keys.push(key);
        ders.push(der);
    }

    for (i, key) in keys.iter().enumerate() {
        for (j, der) in ders.iter().enumerate() {
            let matched = matches_der(key, der).unwrap();
            assert_eq!(matched, i == j, "第 {i} 把私钥与第 {j} 张证书的判定错误");
        }
    }
}
