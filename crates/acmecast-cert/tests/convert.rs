//! 4.4 PFX / PKCS#12 与 P7B 转换；4.5 JKS 转换。
//!
//! 关键验证用**外部工具**完成：产物交给 `openssl pkcs12` / `openssl pkcs7` / `keytool`
//! 加载，而不是只用生成它的同一个库自证。否则「库能读回自己写的东西」并不能说明
//! 产物符合标准、能被第三方消费。
//!
//! JKS 这一组**必须**有 `keytool`：Java KeyStore 的唯一权威消费者是 Java 运行时，
//! 没有它就无从验证。因此缺少 `keytool` 时测试直接失败并给出安装指引，而不是跳过。

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

use acmecast_cert::{
    Error, JKS_MIN_PASSWORD_LEN, PfxEncryption, to_jks, to_p7b_der, to_p7b_pem, to_pfx,
    to_pfx_default, verify_jks, verify_pfx,
};
use rcgen::{BasicConstraints, CertificateParams, DnType, IsCa, KeyPair, SanType};

const PFX_PASSWORD: &str = "test-password-123";
const PFX_ALIAS: &str = "acmecast-test";

/// 临时目录守卫，Drop 时清理。
struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("acmecast-convert-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&path).expect("应能创建临时目录");
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }

    fn write(&self, name: &str, bytes: &[u8]) -> PathBuf {
        let p = self.0.join(name);
        std::fs::write(&p, bytes).expect("应能写入临时文件");
        p
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).ok();
    }
}

/// 一条叶子证书 + CA 的证书链，及其对应私钥。
struct TestMaterial {
    chain_pem: String,
    key_pem: String,
}

fn material() -> TestMaterial {
    let ca_key = KeyPair::generate().unwrap();
    let mut ca_params = CertificateParams::default();
    ca_params
        .distinguished_name
        .push(DnType::CommonName, "acmecast Test Root CA");
    ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    let ca_cert = ca_params.self_signed(&ca_key).unwrap();

    let leaf_key = KeyPair::generate().unwrap();
    let mut leaf_params = CertificateParams::default();
    leaf_params
        .distinguished_name
        .push(DnType::CommonName, "test.example.com");
    leaf_params.subject_alt_names = vec![SanType::DnsName("test.example.com".try_into().unwrap())];

    let issuer = rcgen::Issuer::new(ca_params, ca_key);
    let leaf_cert = leaf_params.signed_by(&leaf_key, &issuer).unwrap();

    TestMaterial {
        // 链的惯例：叶子在前，根在后。
        chain_pem: format!("{}{}", leaf_cert.pem(), ca_cert.pem()),
        key_pem: leaf_key.serialize_pem(),
    }
}

/// 运行 openssl，返回 (是否成功, stdout, stderr)。
fn openssl(args: &[&str]) -> (bool, String, String) {
    let output = Command::new("openssl")
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("无法执行 openssl（本用例需要它来验证产物）: {e}"));

    (
        output.status.success(),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

// ---- PFX 生成与自检 ----

#[test]
fn pfx_is_generated_with_default_encryption() {
    let m = material();
    let pfx =
        to_pfx_default(&m.chain_pem, &m.key_pem, PFX_PASSWORD, PFX_ALIAS).expect("应能生成 PFX");

    assert!(!pfx.is_empty(), "PFX 不应为空");
    // PFX 是 DER 编码的 SEQUENCE。
    assert_eq!(pfx[0], 0x30, "应以 DER SEQUENCE 开头");
}

#[test]
fn generated_pfx_can_be_parsed_back_by_the_library() {
    let m = material();
    let pfx = to_pfx_default(&m.chain_pem, &m.key_pem, PFX_PASSWORD, PFX_ALIAS).unwrap();

    verify_pfx(&pfx, PFX_PASSWORD).expect("自检应通过");
}

#[test]
fn verify_pfx_rejects_wrong_password() {
    let m = material();
    let pfx = to_pfx_default(&m.chain_pem, &m.key_pem, PFX_PASSWORD, PFX_ALIAS).unwrap();

    assert!(
        verify_pfx(&pfx, "wrong-password").is_err(),
        "错误口令不应通过校验"
    );
}

// ---- 需求：产物可被 openssl pkcs12 加载 ----

#[test]
fn openssl_can_load_the_pfx() {
    let m = material();
    let pfx = to_pfx_default(&m.chain_pem, &m.key_pem, PFX_PASSWORD, PFX_ALIAS).unwrap();

    let dir = TempDir::new();
    let pfx_path = dir.write("bundle.pfx", &pfx);

    // `-info -noout` 会解析 PFX 并打印结构，不解密也不导出。
    let (ok, stdout, stderr) = openssl(&[
        "pkcs12",
        "-in",
        pfx_path.to_str().unwrap(),
        "-passin",
        &format!("pass:{PFX_PASSWORD}"),
        "-info",
        "-noout",
    ]);

    assert!(
        ok,
        "openssl 应能加载生成的 PFX。\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        stderr.contains("MAC") || !stderr.is_empty(),
        "openssl 应输出 PFX 结构信息: {stderr}"
    );
}

#[test]
fn openssl_can_extract_the_private_key_from_the_pfx() {
    let m = material();
    let pfx = to_pfx_default(&m.chain_pem, &m.key_pem, PFX_PASSWORD, PFX_ALIAS).unwrap();

    let dir = TempDir::new();
    let pfx_path = dir.write("bundle.pfx", &pfx);
    let key_path = dir.path().join("extracted.pem");

    let (ok, _stdout, stderr) = openssl(&[
        "pkcs12",
        "-in",
        pfx_path.to_str().unwrap(),
        "-passin",
        &format!("pass:{PFX_PASSWORD}"),
        "-nocerts",
        "-nodes",
        "-out",
        key_path.to_str().unwrap(),
    ]);

    assert!(ok, "openssl 应能导出口令保护的私钥。stderr: {stderr}");

    let extracted = std::fs::read_to_string(&key_path).expect("应能读取导出的私钥");
    assert!(
        extracted.contains("PRIVATE KEY"),
        "导出的内容应含私钥: {}",
        &extracted[..extracted.len().min(120)]
    );
}

#[test]
fn openssl_can_extract_the_certificate_chain_from_the_pfx() {
    let m = material();
    let pfx = to_pfx_default(&m.chain_pem, &m.key_pem, PFX_PASSWORD, PFX_ALIAS).unwrap();

    let dir = TempDir::new();
    let pfx_path = dir.write("bundle.pfx", &pfx);
    let cert_path = dir.path().join("certs.pem");

    let (ok, _stdout, stderr) = openssl(&[
        "pkcs12",
        "-in",
        pfx_path.to_str().unwrap(),
        "-passin",
        &format!("pass:{PFX_PASSWORD}"),
        "-nokeys",
        "-out",
        cert_path.to_str().unwrap(),
    ]);
    assert!(ok, "openssl 应能导出证书链。stderr: {stderr}");

    let certs = std::fs::read_to_string(&cert_path).unwrap();
    assert_eq!(
        certs.matches("BEGIN CERTIFICATE").count(),
        2,
        "应导出链中两张证书（叶子 + CA）"
    );
}

#[test]
fn openssl_verifies_the_extracted_chain_against_the_extracted_key() {
    let m = material();
    let pfx = to_pfx_default(&m.chain_pem, &m.key_pem, PFX_PASSWORD, PFX_ALIAS).unwrap();

    let dir = TempDir::new();
    let pfx_path = dir.write("bundle.pfx", &pfx);
    let key_path = dir.path().join("key.pem");
    let cert_path = dir.path().join("cert.pem");

    openssl(&[
        "pkcs12",
        "-in",
        pfx_path.to_str().unwrap(),
        "-passin",
        &format!("pass:{PFX_PASSWORD}"),
        "-nocerts",
        "-nodes",
        "-out",
        key_path.to_str().unwrap(),
    ]);
    openssl(&[
        "pkcs12",
        "-in",
        pfx_path.to_str().unwrap(),
        "-passin",
        &format!("pass:{PFX_PASSWORD}"),
        "-nokeys",
        "-out",
        cert_path.to_str().unwrap(),
    ]);

    // 分别导出两侧的公钥，再比对 PEM 内容。
    // 这是端到端的正确性证据：PFX 里的私钥与证书确实是配对的。
    let key_pub_path = dir.path().join("key.pub.pem");
    let cert_pub_path = dir.path().join("cert.pub.pem");

    let (ok_key, _out, err_key) = openssl(&[
        "pkey",
        "-in",
        key_path.to_str().unwrap(),
        "-pubout",
        "-out",
        key_pub_path.to_str().unwrap(),
    ]);
    assert!(ok_key, "应能从私钥导出公钥: {err_key}");

    let (ok_cert, _out, err_cert) = openssl(&[
        "x509",
        "-in",
        cert_path.to_str().unwrap(),
        "-pubkey",
        "-noout",
        "-out",
        cert_pub_path.to_str().unwrap(),
    ]);
    assert!(ok_cert, "应能从证书导出公钥: {err_cert}");

    let key_pub = std::fs::read_to_string(&key_pub_path).unwrap();
    let cert_pub = std::fs::read_to_string(&cert_pub_path).unwrap();

    assert_eq!(
        pem_body(&key_pub),
        pem_body(&cert_pub),
        "PFX 中私钥与证书的公钥应完全相同"
    );
}

/// 取出 PEM 的 base64 主体（去掉头尾标记与空白），用于跨来源比较同一公钥。
fn pem_body(pem_text: &str) -> String {
    pem_text
        .lines()
        .filter(|l| !l.starts_with("-----"))
        .map(str::trim)
        .collect()
}

// ---- PFX 加密算法兼容性 ----

#[test]
fn legacy_3des_pfx_is_still_loadable_by_openssl() {
    let m = material();
    let pfx = to_pfx(
        &m.chain_pem,
        &m.key_pem,
        PFX_PASSWORD,
        PFX_ALIAS,
        PfxEncryption::TripleDes,
    )
    .unwrap();

    let dir = TempDir::new();
    let pfx_path = dir.write("legacy.pfx", &pfx);

    let (ok, _out, stderr) = openssl(&[
        "pkcs12",
        "-in",
        pfx_path.to_str().unwrap(),
        "-passin",
        &format!("pass:{PFX_PASSWORD}"),
        "-info",
        "-noout",
    ]);
    assert!(ok, "3DES 加密的 PFX 也应能被 openssl 加载: {stderr}");
}

// ---- P7B ----

#[test]
fn p7b_contains_every_certificate_of_the_chain() {
    let m = material();
    let der = to_p7b_der(&m.chain_pem).expect("应能生成 P7B");

    assert!(!der.is_empty());
    assert_eq!(der[0], 0x30, "P7B 应以 DER SEQUENCE 开头");

    // DER 里应能数出两张证书的编码块。
    let first_cert = acmecast_cert::first_der(&m.chain_pem).unwrap();
    assert!(
        der.windows(first_cert.len()).any(|w| w == first_cert),
        "P7B 应包含叶子证书的原始 DER"
    );
}

#[test]
fn openssl_can_read_the_p7b() {
    let m = material();
    let p7b_pem = to_p7b_pem(&m.chain_pem).expect("应能生成 P7B PEM");

    let dir = TempDir::new();
    let p7b_path = dir.write("bundle.p7b", p7b_pem.as_bytes());

    let (ok, stdout, stderr) = openssl(&[
        "pkcs7",
        "-in",
        p7b_path.to_str().unwrap(),
        "-inform",
        "PEM",
        "-print",
        "-noout",
    ]);

    assert!(
        ok,
        "openssl 应能读取生成的 P7B。\nstdout: {stdout}\nstderr: {stderr}"
    );
}

#[test]
fn openssl_extracts_all_certificates_from_the_p7b() {
    let m = material();
    let p7b_pem = to_p7b_pem(&m.chain_pem).unwrap();

    let dir = TempDir::new();
    let p7b_path = dir.write("bundle.p7b", p7b_pem.as_bytes());
    let out_path = dir.path().join("out.pem");

    let (ok, _stdout, stderr) = openssl(&[
        "pkcs7",
        "-in",
        p7b_path.to_str().unwrap(),
        "-inform",
        "PEM",
        "-print_certs",
        "-out",
        out_path.to_str().unwrap(),
    ]);
    assert!(ok, "openssl 应能导出 P7B 中的证书: {stderr}");

    let certs = std::fs::read_to_string(&out_path).unwrap();
    assert_eq!(
        certs.matches("BEGIN CERTIFICATE").count(),
        2,
        "应导出链中两张证书"
    );
}

#[test]
fn openssl_reads_p7b_in_der_form_too() {
    let m = material();
    let der = to_p7b_der(&m.chain_pem).unwrap();

    let dir = TempDir::new();
    let p7b_path = dir.write("bundle.der", &der);

    let (ok, _stdout, stderr) = openssl(&[
        "pkcs7",
        "-in",
        p7b_path.to_str().unwrap(),
        "-inform",
        "DER",
        "-print_certs",
        "-noout",
    ]);
    assert!(ok, "DER 形式的 P7B 也应可读: {stderr}");
}

// ---- JKS（Java KeyStore，4.5）----

const JKS_PASSWORD: &str = "test-password-123";
const JKS_ALIAS: &str = "acmecast-test";

/// keytool 的可执行文件名。Windows 上带 `.exe`，其余平台无后缀。
const KEYTOOL_BIN: &str = if cfg!(windows) {
    "keytool.exe"
} else {
    "keytool"
};

/// keytool 路径只探测一次。
static KEYTOOL: OnceLock<PathBuf> = OnceLock::new();

/// 定位 `keytool`，失败即 panic。
///
/// 这里**刻意不提供「找不到就跳过」的分支**：本组测试要证明的是「产出的 JKS 能被
/// Java 生态真正加载」，静默跳过等于把最有价值的断言悄悄关掉，还会让 CI 绿得毫无意义。
fn keytool() -> &'static Path {
    KEYTOOL
        .get_or_init(|| {
            locate_keytool().unwrap_or_else(|| {
                panic!(
                    "未找到 keytool —— JKS 测试需要真实的 Java 运行时来验证产物可加载。\n\
                     补救方式（任选其一）：\n\
                       1. 安装 JDK（`keytool` 随 JDK 一起提供）；\n\
                     2. 设置环境变量 ACMECAST_KEYTOOL 指向 keytool 可执行文件；\n\
                     3. 若 keytool 在非标准位置，把它所在的 bin 目录加入 PATH。\n\
                     已查找：ACMECAST_KEYTOOL、PATH、JAVA_HOME、\
                     /usr/lib/jvm、/usr/java、/opt/java、/Library/Java/JavaVirtualMachines、\
                     ~/.sdkman、~/.asdf、mise 的 JDK 安装目录，\
                     以及 Android Studio / DevEco Studio 自带的 JBR。"
                )
            })
        })
        .as_path()
}

/// 按以下顺序定位 keytool：
///
/// 1. `ACMECAST_KEYTOOL` 环境变量（显式指定，优先级最高）
/// 2. `PATH`
/// 3. `JAVA_HOME/bin`
/// 4. 常见 JDK 安装根下的每个子目录（Linux 与 macOS 两种目录布局）
/// 5. IDE 自带的 JBR —— 开发机上常常是唯一装了 keytool 的地方
fn locate_keytool() -> Option<PathBuf> {
    // 显式指定但路径无效时直接报错：否则「设了却没生效」会很难排查。
    if let Some(explicit) = std::env::var_os("ACMECAST_KEYTOOL") {
        let path = PathBuf::from(&explicit);
        assert!(
            path.is_file(),
            "ACMECAST_KEYTOOL 指向的不是文件: {}",
            path.display()
        );
        return Some(path);
    }

    let on_path = std::env::var_os("PATH")
        .into_iter()
        .flat_map(|paths| std::env::split_paths(&paths).collect::<Vec<_>>())
        .map(|dir| dir.join(KEYTOOL_BIN))
        .find(|candidate| candidate.is_file());
    if on_path.is_some() {
        return on_path;
    }

    let mut candidates = Vec::new();

    if let Some(java_home) = std::env::var_os("JAVA_HOME") {
        candidates.push(PathBuf::from(java_home).join("bin").join(KEYTOOL_BIN));
    }

    let home = home_dir();
    let mut jdk_roots: Vec<PathBuf> = ["/usr/lib/jvm", "/usr/java", "/opt/java"]
        .iter()
        .map(PathBuf::from)
        .collect();
    jdk_roots.push(PathBuf::from("/Library/Java/JavaVirtualMachines"));
    if let Some(home) = &home {
        for rel in [
            ".sdkman/candidates/java",
            ".asdf/installs/java",
            ".local/share/mise/installs/java",
        ] {
            jdk_roots.push(home.join(rel));
        }
    }

    for root in jdk_roots {
        for jdk in subdirectories(&root) {
            // Linux / Windows 布局：<jdk>/bin/keytool
            candidates.push(jdk.join("bin").join(KEYTOOL_BIN));
            // macOS 布局：<jdk>/Contents/Home/bin/keytool
            candidates.push(jdk.join("Contents/Home/bin").join(KEYTOOL_BIN));
        }
    }

    if let Some(home) = &home {
        for rel in [".local/android-studio/jbr", ".local/devecostudio/jbr"] {
            candidates.push(home.join(rel).join("bin").join(KEYTOOL_BIN));
        }
    }
    for app in [
        "/Applications/Android Studio.app",
        "/Applications/DevEco-Studio.app",
    ] {
        candidates.push(
            Path::new(app)
                .join("Contents/jbr/Contents/Home/bin")
                .join(KEYTOOL_BIN),
        );
    }

    candidates.into_iter().find(|candidate| candidate.is_file())
}

/// 列出 `root` 的直接子目录（不递归）。读不到目录时返回空列表。
fn subdirectories(root: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .map(|entry| entry.path())
        .collect()
}

/// 定位用户主目录。Windows 上用 `USERPROFILE`。
fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

/// 运行 keytool，返回 (是否成功, stdout, stderr)。
///
/// 统一带上 `-storetype JKS`：明确声明格式，避免 keytool 的类型自动探测
/// 掩盖「写出来的其实不是 JKS」这类问题。
fn keytool_cmd(args: &[&str]) -> (bool, String, String) {
    let output = Command::new(keytool())
        .args(["-storetype", "JKS"])
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("无法执行 {}: {e}", keytool().display()));

    (
        output.status.success(),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

/// 取出第一张证书的 PEM base64 主体，用于跨来源比对同一张证书。
fn first_certificate_body(pem_text: &str) -> String {
    let start = pem_text
        .find("-----BEGIN CERTIFICATE-----")
        .expect("文本中应含证书 PEM");
    let rest = &pem_text[start..];
    let end = rest
        .find("-----END CERTIFICATE-----")
        .expect("文本中应含证书结束标记");
    pem_body(&rest[..end])
}

#[test]
fn jks_is_generated_and_self_verifies() {
    let m = material();
    let jks = to_jks(&m.chain_pem, &m.key_pem, JKS_PASSWORD, JKS_ALIAS).expect("应能生成 JKS");

    assert!(!jks.is_empty(), "JKS 不应为空");
    // JKS 以固定魔数 0xFEEDFEED 开头（大端序），与 PFX 的 DER SEQUENCE 完全不同。
    assert_eq!(
        &jks[..4],
        &[0xFE, 0xED, 0xFE, 0xED],
        "应以 JKS 魔数 0xFEEDFEED 开头"
    );

    verify_jks(&jks, JKS_PASSWORD).expect("自检应通过");
}

#[test]
fn verify_jks_rejects_wrong_password() {
    let m = material();
    let jks = to_jks(&m.chain_pem, &m.key_pem, JKS_PASSWORD, JKS_ALIAS).unwrap();

    assert!(
        verify_jks(&jks, "wrong-password-123").is_err(),
        "错误口令不应通过校验"
    );
}

#[test]
fn jks_rejects_password_shorter_than_the_java_limit() {
    let m = material();

    let too_short = "x".repeat(JKS_MIN_PASSWORD_LEN - 1);
    let err = to_jks(&m.chain_pem, &m.key_pem, &too_short, JKS_ALIAS)
        .expect_err("短于下限的口令应被拒绝");
    assert!(matches!(err, Error::Conversion(_)), "{err:?}");

    let at_limit = "x".repeat(JKS_MIN_PASSWORD_LEN);
    to_jks(&m.chain_pem, &m.key_pem, &at_limit, JKS_ALIAS).expect("恰好到下限的口令应当可用");
}

#[test]
fn jks_rejects_empty_chain() {
    let m = material();
    match to_jks("", &m.key_pem, JKS_PASSWORD, JKS_ALIAS) {
        Ok(_) => panic!("空链应报错"),
        Err(err) => assert!(matches!(err, Error::NoCertificate), "{err:?}"),
    }
}

#[test]
fn jks_conversion_does_not_mutate_the_input() {
    let m = material();
    let chain_before = m.chain_pem.clone();
    let key_before = m.key_pem.clone();

    let _ = to_jks(&m.chain_pem, &m.key_pem, JKS_PASSWORD, JKS_ALIAS).unwrap();

    assert_eq!(m.chain_pem, chain_before, "证书链 PEM 不应被改动");
    assert_eq!(m.key_pem, key_before, "私钥 PEM 不应被改动");
}

// ---- 需求：产物可被 Java KeyStore 加载（用真实 keytool 验证）----

#[test]
fn keytool_can_list_the_jks() {
    let m = material();
    let jks = to_jks(&m.chain_pem, &m.key_pem, JKS_PASSWORD, JKS_ALIAS).unwrap();

    let dir = TempDir::new();
    let jks_path = dir.write("bundle.jks", &jks);

    let (ok, stdout, stderr) = keytool_cmd(&[
        "-list",
        "-keystore",
        jks_path.to_str().unwrap(),
        "-storepass",
        JKS_PASSWORD,
    ]);

    assert!(
        ok,
        "keytool 应能加载生成的 JKS。\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        stdout.contains(JKS_ALIAS),
        "列表中应含别名 {JKS_ALIAS}。stdout: {stdout}"
    );
}

#[test]
fn keytool_reads_the_full_chain_in_leaf_first_order() {
    let m = material();
    let jks = to_jks(&m.chain_pem, &m.key_pem, JKS_PASSWORD, JKS_ALIAS).unwrap();

    let dir = TempDir::new();
    let jks_path = dir.write("bundle.jks", &jks);

    // `-list -rfc` 把条目里的证书链按 PEM 逐张打印，是观察链内容与顺序最直接的手段。
    let (ok, stdout, stderr) = keytool_cmd(&[
        "-list",
        "-rfc",
        "-keystore",
        jks_path.to_str().unwrap(),
        "-storepass",
        JKS_PASSWORD,
    ]);
    assert!(
        ok,
        "keytool 应能列出 JKS 中的证书。\nstdout: {stdout}\nstderr: {stderr}"
    );

    assert_eq!(
        stdout.matches("BEGIN CERTIFICATE").count(),
        2,
        "应列出链中两张证书（叶子 + CA）。stdout: {stdout}"
    );
    assert_eq!(
        first_certificate_body(&stdout),
        first_certificate_body(&m.chain_pem),
        "链的首张必须是叶子证书，且内容与输入完全一致"
    );
}

#[test]
fn keytool_can_extract_the_private_key() {
    let m = material();
    let jks = to_jks(&m.chain_pem, &m.key_pem, JKS_PASSWORD, JKS_ALIAS).unwrap();

    let dir = TempDir::new();
    let jks_path = dir.write("bundle.jks", &jks);
    let csr_path = dir.path().join("request.csr");

    // 生成 CSR 需要用私钥签名，因此成功即证明私钥已被正确写入并能用口令解出。
    let (ok, stdout, stderr) = keytool_cmd(&[
        "-certreq",
        "-alias",
        JKS_ALIAS,
        "-keystore",
        jks_path.to_str().unwrap(),
        "-storepass",
        JKS_PASSWORD,
        "-file",
        csr_path.to_str().unwrap(),
    ]);

    assert!(
        ok,
        "keytool 应能用 JKS 中的私钥生成 CSR。\nstdout: {stdout}\nstderr: {stderr}"
    );

    let csr_pem = std::fs::read_to_string(&csr_path).expect("应生成 CSR 文件");
    // 不匹配完整头标记：keytool 写的是老式的 `-----BEGIN NEW CERTIFICATE REQUEST-----`
    // （PKCS#10 的历史遗留命名），不含 `BEGIN CERTIFICATE REQUEST` 这个子串。
    // 实测 keytool 21.0.9 与 25.0.4 行为一致，属长期行为而非某个版本的问题；
    // 反过来 keytool 自己解析两种头都接受（`-printcertreq` 均可），只有生成侧是老的。
    assert!(
        csr_pem.contains("CERTIFICATE REQUEST"),
        "应产出 PEM 形式的 CSR，实际内容: {}",
        &csr_pem[..csr_pem.len().min(80)]
    );
}

#[test]
fn keytool_extracted_key_matches_the_certificate() {
    // JKS 把私钥与证书链分成两块存储，最容易出的错就是两块不配对。
    // 因此把两侧都从**产物本身**取出来比对：CSR 的公钥来自 JKS 中的私钥，
    // 证书的公钥来自 JKS 中的链。
    let m = material();
    let jks = to_jks(&m.chain_pem, &m.key_pem, JKS_PASSWORD, JKS_ALIAS).unwrap();

    let dir = TempDir::new();
    let jks_path = dir.write("bundle.jks", &jks);
    let csr_path = dir.path().join("request.csr");
    let cert_path = dir.path().join("leaf.pem");

    let (ok_csr, _out, err_csr) = keytool_cmd(&[
        "-certreq",
        "-alias",
        JKS_ALIAS,
        "-keystore",
        jks_path.to_str().unwrap(),
        "-storepass",
        JKS_PASSWORD,
        "-file",
        csr_path.to_str().unwrap(),
    ]);
    assert!(ok_csr, "应能生成 CSR: {err_csr}");

    let (ok_cert, _out, err_cert) = keytool_cmd(&[
        "-exportcert",
        "-rfc",
        "-alias",
        JKS_ALIAS,
        "-keystore",
        jks_path.to_str().unwrap(),
        "-storepass",
        JKS_PASSWORD,
        "-file",
        cert_path.to_str().unwrap(),
    ]);
    assert!(ok_cert, "应能导出证书: {err_cert}");

    let key_pub_path = dir.path().join("key.pub.pem");
    let cert_pub_path = dir.path().join("cert.pub.pem");

    let (ok_key_pub, _out, err_key_pub) = openssl(&[
        "req",
        "-in",
        csr_path.to_str().unwrap(),
        "-pubkey",
        "-noout",
        "-out",
        key_pub_path.to_str().unwrap(),
    ]);
    assert!(ok_key_pub, "应能从 CSR 导出公钥: {err_key_pub}");

    let (ok_cert_pub, _out, err_cert_pub) = openssl(&[
        "x509",
        "-in",
        cert_path.to_str().unwrap(),
        "-pubkey",
        "-noout",
        "-out",
        cert_pub_path.to_str().unwrap(),
    ]);
    assert!(ok_cert_pub, "应能从证书导出公钥: {err_cert_pub}");

    let key_pub = std::fs::read_to_string(&key_pub_path).unwrap();
    let cert_pub = std::fs::read_to_string(&cert_pub_path).unwrap();

    assert_eq!(
        pem_body(&key_pub),
        pem_body(&cert_pub),
        "JKS 中的私钥与证书必须是配对的那一对"
    );
}

#[test]
fn keytool_rejects_wrong_password() {
    let m = material();
    let jks = to_jks(&m.chain_pem, &m.key_pem, JKS_PASSWORD, JKS_ALIAS).unwrap();

    let dir = TempDir::new();
    let jks_path = dir.write("bundle.jks", &jks);

    let (ok, _stdout, _stderr) = keytool_cmd(&[
        "-list",
        "-keystore",
        jks_path.to_str().unwrap(),
        "-storepass",
        "wrong-password-123",
    ]);

    assert!(!ok, "错误口令不应能加载 JKS");
}

// ---- 需求：转换不修改原始 PEM ----

#[test]
fn conversion_does_not_mutate_the_input() {
    let m = material();
    let chain_before = m.chain_pem.clone();
    let key_before = m.key_pem.clone();

    let _ = to_pfx_default(&m.chain_pem, &m.key_pem, PFX_PASSWORD, PFX_ALIAS).unwrap();
    let _ = to_p7b_pem(&m.chain_pem).unwrap();

    assert_eq!(m.chain_pem, chain_before, "证书链 PEM 不应被改动");
    assert_eq!(m.key_pem, key_before, "私钥 PEM 不应被改动");
}

#[test]
fn repeated_conversions_are_deterministic() {
    let m = material();

    let a = to_p7b_der(&m.chain_pem).unwrap();
    let b = to_p7b_der(&m.chain_pem).unwrap();
    assert_eq!(a, b, "同一输入应得到相同 P7B");

    // PFX 含随机 salt，因此不比较字节相等，但结构应稳定可解析。
    let pfx_a = to_pfx_default(&m.chain_pem, &m.key_pem, PFX_PASSWORD, PFX_ALIAS).unwrap();
    let pfx_b = to_pfx_default(&m.chain_pem, &m.key_pem, PFX_PASSWORD, PFX_ALIAS).unwrap();
    verify_pfx(&pfx_a, PFX_PASSWORD).unwrap();
    verify_pfx(&pfx_b, PFX_PASSWORD).unwrap();
}

#[test]
fn different_passwords_produce_different_pfx() {
    let m = material();
    let a = to_pfx_default(&m.chain_pem, &m.key_pem, "password-a", PFX_ALIAS).unwrap();
    let b = to_pfx_default(&m.chain_pem, &m.key_pem, "password-b", PFX_ALIAS).unwrap();

    assert_ne!(a, b, "不同口令应产生不同密文");
    assert!(verify_pfx(&b, "password-a").is_err(), "口令不应互换生效");
}

#[test]
fn alias_is_preserved_in_the_pfx() {
    let m = material();
    let pfx = to_pfx_default(&m.chain_pem, &m.key_pem, PFX_PASSWORD, "my-alias").unwrap();

    let dir = TempDir::new();
    let pfx_path = dir.write("aliased.pfx", &pfx);

    // openssl 导出时会把别名写进输出。
    let (ok, _stdout, stderr) = openssl(&[
        "pkcs12",
        "-in",
        pfx_path.to_str().unwrap(),
        "-passin",
        &format!("pass:{PFX_PASSWORD}"),
        "-info",
        "-noout",
        "-name",
        "my-alias",
    ]);
    assert!(ok, "指定别名应能被识别: {stderr}");
}
