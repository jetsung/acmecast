//! `hash-password` 子命令的集成测试。
//!
//! 覆盖两层：库函数 `hash_password` 的**生成-校验闭环**（生成的哈希必须能
//! 被鉴权端的 `verify_password` 通过），以及**子命令进程本身**的输出与
//! 错误处理——哈希格式错了，运行环境的登录就会静默失效，因此两层都要钉住。

use std::process::Command;
use std::time::Duration;

use acmecast_server::auth::AuthConfig;
use acmecast_server::hash_password;

/// 用生成的哈希构造鉴权配置（与生产解析端同一实现）。
fn auth_config_from(hash: &str) -> AuthConfig {
    AuthConfig::new(
        "admin",
        Some(hash.to_owned()),
        "hash-cmd-test-secret",
        Duration::from_secs(3600),
    )
}

// ---- 库函数：生成-校验闭环 ----

#[test]
fn generated_hash_is_valid_phc_and_verifies_the_password() {
    let hash = hash_password("correct-horse-battery").expect("应能生成哈希");

    // PHC 格式：算法、版本、参数、盐、哈希五段齐全，且是 argon2id。
    assert!(
        hash.starts_with("$argon2id$v=19$"),
        "应为 argon2id 的 PHC 字符串：{hash}"
    );

    let config = auth_config_from(&hash);
    assert!(
        config.verify_password("correct-horse-battery"),
        "正确口令应通过校验"
    );
    assert!(
        !config.verify_password("wrong-password"),
        "错误口令应被拒绝"
    );
}

#[test]
fn repeated_calls_are_salted_differently() {
    let first = hash_password("same-password").expect("第一次生成应成功");
    let second = hash_password("same-password").expect("第二次生成应成功");

    assert_ne!(first, second, "同口令两次生成应因随机盐而不同");

    // 两个哈希都能校验同一口令——盐不同不影响验证语义。
    assert!(auth_config_from(&first).verify_password("same-password"));
    assert!(auth_config_from(&second).verify_password("same-password"));
}

// ---- 子命令进程 ----

#[test]
fn subcommand_prints_a_usable_hash() {
    let output = Command::new(env!("CARGO_BIN_EXE_acmecast-server"))
        .args(["hash-password", "--password", "from-subcommand"])
        .output()
        .expect("应能启动子命令");

    assert!(output.status.success(), "子命令应成功退出");
    let stdout = String::from_utf8_lossy(&output.stdout);
    // 首行是 PHC 哈希，其后是给终端用户的提示行。
    let hash = stdout.lines().next().unwrap_or_default().trim().to_owned();
    assert!(
        hash.starts_with("$argon2id$"),
        "子命令首行应输出 PHC 哈希：{stdout}"
    );

    // 子命令输出必须与库函数产物等效：能通过鉴权端的校验。
    let config = auth_config_from(&hash);
    assert!(
        config.verify_password("from-subcommand"),
        "子命令生成的哈希应能校验原口令"
    );
}

#[test]
fn subcommand_without_a_password_fails_on_non_terminal_stdin() {
    // 测试进程的 stdin 是管道（非终端），交互式输入不可用，应以非零退出码失败。
    let output = Command::new(env!("CARGO_BIN_EXE_acmecast-server"))
        .args(["hash-password"])
        .output()
        .expect("应能启动子命令");

    assert!(
        !output.status.success(),
        "无口令且 stdin 非终端应以非零退出码失败"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("stdin") || stderr.contains("口令"),
        "错误信息应说明未提供口令且 stdin 非终端：{stderr}"
    );
}

// ---- 鉴权模式联动 ----

#[test]
fn auth_mode_accepts_the_generated_hash_via_environment_shape() {
    // 模拟环境变量路径：AuthMode::from_environment 解析 PHC 字符串
    // （AuthConfig::from_environment 对非法 PHC 会直接拒绝启动）。
    let hash = hash_password("env-style-password").expect("应能生成哈希");

    let config = AuthConfig::new(
        "admin",
        Some(hash.clone()),
        "secret",
        Duration::from_secs(1),
    );
    assert!(config.login_available(), "配置了哈希后登录应可用");
    assert!(config.verify_password("env-style-password"));

    // PHC 字符串可被解析端再次解析（AuthConfig::from_environment 的校验逻辑）。
    assert!(
        argon2::password_hash::phc::PasswordHash::new(&hash).is_ok(),
        "生成的哈希必须是合法的 PHC 字符串"
    );
}
