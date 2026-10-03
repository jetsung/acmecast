//! `init` 子命令的集成测试。
//!
//! 覆盖：模板生成、已存在文件未加 `--force` 时的非零退出与不覆盖、
//! `--force` 的显式覆盖。路径由全局 `--config` 指定到临时目录，
//! 数据目录经 `ACMECAST_DATA_DIR` 重定向，避免污染仓库工作区。

use std::path::PathBuf;
use std::process::Command;

/// 唯一的临时目录，避免并行用例相互覆盖。
fn unique_dir(name: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("系统时间应晚于 Unix 纪元")
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("acmecast-init-cmd-{name}-{nanos}"));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn run_init(config: &PathBuf, extra_args: &[&str]) -> (bool, String) {
    let output = Command::new(env!("CARGO_BIN_EXE_acmecast-server"))
        .env("ACMECAST_DATA_DIR", config.parent().unwrap().join("data"))
        .arg("init")
        .arg("--config")
        .arg(config)
        .args(extra_args)
        .output()
        .expect("应能启动子命令");
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    (output.status.success(), combined)
}

#[test]
fn init_creates_template_then_respects_force_semantics() {
    let dir = unique_dir("create");
    let path = dir.join("config.toml");

    // 首次生成：成功且内容为带注释模板。
    let (ok, log) = run_init(&path, &[]);
    assert!(ok, "init 应成功: {log}");
    assert!(log.contains('✓'), "成功输出应带 ✓ 标记: {log}");
    let content = std::fs::read_to_string(&path).unwrap();
    assert!(content.contains("[server]"), "{content}");
    assert!(content.contains("[[resolvers]]"), "{content}");

    // 已存在且未 --force：非零退出，文件内容保持不变。
    std::fs::write(&path, "user content").unwrap();
    let (ok, log) = run_init(&path, &[]);
    assert!(!ok, "已存在且未 --force 应失败: {log}");
    assert!(log.contains("--force"), "错误信息应提示 --force: {log}");
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "user content",
        "失败路径不得覆盖已有文件"
    );

    // --force：覆盖为模板。
    let (ok, log) = run_init(&path, &["--force"]);
    assert!(ok, "--force 应成功: {log}");
    assert!(
        std::fs::read_to_string(&path).unwrap().contains("[server]"),
        "--force 后应为模板内容"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
