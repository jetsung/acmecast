//! 产物重名的告警。
//!
//! 覆盖本身是允许的（多域名流水线依赖它），但覆盖**不会**以失败的形式暴露——
//! 部署步骤照样成功，只是内容可能不是本意。所以那条 warn 是这种情况唯一的线索，
//! 值得单独钉住：既要它真的发出来，也要它在不该发的时候保持安静。

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use acmecast_pipeline::Artifacts;

/// 把 tracing 输出写进内存缓冲区。
#[derive(Clone, Default)]
struct CapturedLogs(Arc<Mutex<Vec<u8>>>);

impl CapturedLogs {
    fn contents(&self) -> String {
        String::from_utf8_lossy(&self.0.lock().expect("锁不应中毒")).into_owned()
    }
}

impl std::io::Write for CapturedLogs {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().expect("锁不应中毒").extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for CapturedLogs {
    type Writer = Self;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

/// 接上捕获日志的 subscriber，返回缓冲区与守卫。
///
/// 守卫必须被调用方持有到测试结束——一旦 drop，订阅者就撤销了。
fn capture() -> (CapturedLogs, tracing::subscriber::DefaultGuard) {
    let logs = CapturedLogs::default();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(logs.clone())
        .with_max_level(tracing::Level::TRACE)
        .finish();
    let guard = tracing::subscriber::set_default(subscriber);
    (logs, guard)
}

fn produced(items: &[(&str, serde_json::Value)]) -> BTreeMap<String, serde_json::Value> {
    items
        .iter()
        .map(|(name, value)| ((*name).to_owned(), value.clone()))
        .collect()
}

#[test]
fn overwriting_an_artifact_emits_a_warning_that_names_both_sides() {
    let (logs, _guard) = capture();

    let mut artifacts = Artifacts::new();
    artifacts.merge(
        0,
        "cert.apply",
        produced(&[("cert_pem", serde_json::json!("A"))]),
    );
    artifacts.merge(
        2,
        "cert.import",
        produced(&[("cert_pem", serde_json::json!("B"))]),
    );

    let captured = logs.contents();
    assert!(!captured.is_empty(), "本用例的前提是确实捕获到了输出");
    assert!(captured.contains("产物重名"), "应发出告警：{captured}");

    // 光说「重名」不够用——得能看出是谁覆盖了谁，否则排查时无从下手。
    assert!(
        captured.contains("cert_pem"),
        "应指出是哪个产物：{captured}"
    );
    assert!(
        captured.contains("cert.apply"),
        "应指出原产出者：{captured}"
    );
    assert!(captured.contains("cert.import"), "应指出覆盖者：{captured}");
}

#[test]
fn a_merge_without_collisions_stays_silent() {
    // 不覆盖就不该有告警——否则日志里全是噪音，真出问题时反而看不见。
    let (logs, _guard) = capture();

    let mut artifacts = Artifacts::new();
    artifacts.merge(
        0,
        "cert.apply",
        produced(&[("cert_pem", serde_json::json!("A"))]),
    );
    artifacts.merge(
        1,
        "cert.deploy",
        produced(&[("deployed_to", serde_json::json!("/x"))]),
    );

    let captured = logs.contents();
    assert!(
        !captured.contains("产物重名"),
        "没有发生覆盖时不该告警：{captured}"
    );
}
