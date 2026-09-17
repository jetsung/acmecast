//! 带幂等判断的部署执行器。
//!
//! 判断本身很朴素：目标是同一处、指纹没变、且没勾强制 → 跳过写入。真正容易做错的是**跳过之后**的事：很多人顺手把整个部署（含重载）一起跳过，结果「证书换了、服务没重新加载」，这个状态会一直持续到下一次部署才被纠正。所以这里只跳过**写入**，重载照旧走完。

use std::sync::Arc;

use acmecast_access::CredentialStore;
use chrono::Utc;
use serde_json::Value;

use crate::error::Result;
use crate::state::{DeploymentEntry, DeploymentStateStore, target_ref_of};
use crate::target::{CertMaterials, DeployMode, DeployOutcome, DeploymentTarget};

/// 部署执行器。
#[derive(Debug)]
pub struct Deployer {
    state: Arc<dyn DeploymentStateStore>,
}

impl Deployer {
    /// 装配一个执行器。
    #[must_use]
    pub fn new(state: Arc<dyn DeploymentStateStore>) -> Self {
        Self { state }
    }

    /// 部署一次。
    ///
    /// - 指纹与上次一致且未勾 `force` → 跳过写入，但**重载仍执行**，结果里标为 `skipped_write`；
    /// - 勾了 `force` → 无论指纹是否一致都重新写入。
    ///
    /// 记录发生在**部署成功之后**——失败的那次不该被当成「已部署」。否则重试时会被判成「指纹一致 → 跳过写入」，那份证书就永远不会真的被写进去。
    pub async fn deploy(
        &self,
        target: &dyn DeploymentTarget,
        input: &Value,
        materials: &CertMaterials,
        credentials: &CredentialStore<'_>,
        force: bool,
    ) -> Result<DeployOutcome> {
        let reference = target_ref_of(target.type_id(), input);
        let deployed = self.state.deployed_fingerprint(&reference).await?;

        let mode = decide_mode(deployed.as_deref(), &materials.fingerprint, force);

        let outcome = target.deploy(input, materials, credentials, mode).await?;

        self.state
            .record(DeploymentEntry {
                target: reference,
                fingerprint: materials.fingerprint.clone(),
                skipped_write: outcome.skipped_write,
                deployed_at: Utc::now(),
                // 路径与重载输出是运维回溯时要看的，一并记下。
                paths: outcome.paths.clone(),
                reload_output: outcome.reload_output.clone(),
            })
            .await?;

        Ok(outcome)
    }
}

/// 幂等的规则：指纹一致且未强制 → 跳过写入。
///
/// 抽成函数是为了让这条规则能被单独测试。
fn decide_mode(deployed: Option<&str>, current: &str, force: bool) -> DeployMode {
    match deployed {
        Some(previous) if previous == current && !force => DeployMode::SkipWrite,
        _ => DeployMode::Write,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn never_deployed_means_write() {
        assert_eq!(decide_mode(None, "sha256:abc", false), DeployMode::Write);
    }

    #[test]
    fn the_same_fingerprint_skips() {
        assert_eq!(
            decide_mode(Some("sha256:abc"), "sha256:abc", false),
            DeployMode::SkipWrite
        );
    }

    #[test]
    fn force_overrides_a_matching_fingerprint() {
        assert_eq!(
            decide_mode(Some("sha256:abc"), "sha256:abc", true),
            DeployMode::Write
        );
    }

    #[test]
    fn a_changed_fingerprint_always_writes() {
        assert_eq!(
            decide_mode(Some("sha256:old"), "sha256:new", false),
            DeployMode::Write
        );
    }
}
