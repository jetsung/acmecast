//! `cert.store`：把申请到的证书写进数据目录并入库。
//!
//! 文件落盘走 [`FileStore`](acmecast_store::FileStore)（相对路径 + 受限权限），
//! 记录入库走 [`CertRepository`](acmecast_store::CertRepository)。
//! 只要本次成功签发新证书且存在同域名集合旧记录且指纹不同，即归档旧证书
//! 到 `certs/archived/<旧指纹>/` 并新建记录；指纹相同幂等不新建。

use std::path::PathBuf;

use acmecast_pipeline::{PipelineStep, Result, StepContext, StepOutput};
use acmecast_store::FileStore;
use acmecast_store::repository::{CertInput, CertRepository};
use schemars::JsonSchema;
use sea_orm::DatabaseConnection;
use serde::Deserialize;

use super::domain_error;

/// `cert.store` 的输入。
#[derive(Debug, Clone, Default, Deserialize, JsonSchema)]
pub struct CertStoreInput {
    /// 签发该证书的 ACME 账号凭据标识；记录它之后吊销才知道用哪个账号。
    /// 手动上传场景可以不填。
    #[serde(default)]
    pub acme_account_credential_id: Option<i64>,
}

/// 证书入库步骤。
#[derive(Debug)]
pub struct CertStoreStep {
    db: DatabaseConnection,
    data_dir: PathBuf,
}

impl CertStoreStep {
    /// 用数据库连接与数据目录装配。
    #[must_use]
    pub fn new(db: DatabaseConnection, data_dir: PathBuf) -> Self {
        Self { db, data_dir }
    }
}

#[async_trait::async_trait]
impl PipelineStep for CertStoreStep {
    fn type_id(&self) -> &'static str {
        "cert.store"
    }

    fn input_schema(&self) -> Option<schemars::schema::RootSchema> {
        Some(schemars::schema_for!(CertStoreInput))
    }

    fn required_artifacts(&self) -> &'static [&'static str] {
        &["cert_pem", "key_pem", "domains", "fingerprint"]
    }

    async fn execute(&self, ctx: &mut StepContext<'_>) -> Result<StepOutput> {
        let input: CertStoreInput = ctx
            .input_as()
            .map_err(|error| domain_error("cert.store 输入不合法", error))?;

        let cert_pem: String = serde_json::from_value(
            ctx.artifact("cert_pem")
                .map_err(|e| domain_error("读取产物", e))?
                .clone(),
        )
        .map_err(|e| domain_error("cert_pem 产物", e))?;
        let key_pem: String = serde_json::from_value(
            ctx.artifact("key_pem")
                .map_err(|e| domain_error("读取产物", e))?
                .clone(),
        )
        .map_err(|e| domain_error("key_pem 产物", e))?;
        let domains: Vec<String> = serde_json::from_value(
            ctx.artifact("domains")
                .map_err(|e| domain_error("读取产物", e))?
                .clone(),
        )
        .map_err(|e| domain_error("domains 产物", e))?;
        let fingerprint: String = serde_json::from_value(
            ctx.artifact("fingerprint")
                .map_err(|e| domain_error("读取产物", e))?
                .clone(),
        )
        .map_err(|e| domain_error("fingerprint 产物", e))?;

        // 解析叶子证书拿到有效期与签发者——这些是仓库记录与续期扫描的依据。
        let leaf =
            acmecast_cert::parse_pem_leaf(&cert_pem).map_err(|e| domain_error("解析证书", e))?;

        let store = FileStore::open(&self.data_dir)
            .await
            .map_err(|e| domain_error("打开数据目录", e))?;
        // 文件名前缀取申请时的域名顺序——第一个；证书 SAN 兜底（手动构造的
        // 流水线里两者理论上一致，但证书是事实来源，输入是用户意图）。
        let primary_domain = domains
            .first()
            .map(String::as_str)
            .or_else(|| leaf.primary_domain())
            .unwrap_or("certificate");
        // 只要申请成功且存在同域名集合旧记录且指纹不同，即归档旧证书并新建记录。
        // 指纹相同视为幂等不新建；归档失败不阻断新证书入库（仅 warn）。
        let repository = CertRepository::new(&self.db);
        let existing = repository
            .find_by_domains(&domains)
            .await
            .map_err(|e| domain_error("查询既有证书", e))?;
        let mut should_archive = false;
        let mut old_fingerprint: Option<String> = None;
        if let Some(old) = &existing {
            if old.fingerprint != fingerprint {
                should_archive = true;
                old_fingerprint = Some(old.fingerprint.clone());
                match store
                    .archive_replaced_certificate(
                        &old.cert_pem_path,
                        &old.key_pem_path,
                        &old.fingerprint,
                    )
                    .await
                {
                    Ok(Some(archived_paths)) => {
                        // 归档成功需同步更新旧记录路径与 archived_at，否则旧记录仍指向活跃目录。
                        if let Err(error) =
                            repository.set_archived_paths(old.id, &archived_paths, chrono::Utc::now()).await
                        {
                            tracing::warn!(
                                old_fingerprint = %old.fingerprint,
                                %error,
                                "旧证书归档后更新库路径失败，文件已归档但库路径未同步"
                            );
                        }
                        ctx.log_info(format!(
                            "旧证书已归档到 archived/{}（主域名 {}）",
                            old.fingerprint, primary_domain
                        ));
                    }
                    Ok(None) => {
                        tracing::warn!(
                            old_fingerprint = %old.fingerprint,
                            "旧证书材料缺失，跳过归档，直接写入新证书"
                        );
                    }
                    Err(error) => {
                        tracing::warn!(
                            old_fingerprint = %old.fingerprint,
                            %error,
                            "旧证书归档失败，仍继续写入新证书"
                        );
                    }
                }
            }
        }
        let paths = store
            .write_certificate(&fingerprint, primary_domain, &cert_pem, &key_pem)
            .await
            .map_err(|e| domain_error("写入证书文件", e))?;
        ctx.log_info(format!(
            "证书文件已写入数据目录：{}、{}",
            paths.cert_pem, paths.key_pem
        ));

        // 从私钥检测算法展示名（RSA / ECDSA P-256 等）；检测失败不阻断入库，留空展示。
        let key_algorithm = acmecast_cert::detect_algorithm(&key_pem)
            .ok()
            .map(|algo| algo.as_str().to_owned());
        if should_archive {
            // 指纹不同且已尝试归档：新建记录，不覆盖旧记录。
            // 若指纹已存在（并发/重试导致冲突），返回 Conflict 交由调用方感知。
            let cert = repository
                .create(CertInput {
                    domains: domains.clone(),
                    cert_pem_path: paths.cert_pem,
                    key_pem_path: paths.key_pem,
                    fingerprint: fingerprint.clone(),
                    issuer: (!leaf.issuer.is_empty()).then_some(leaf.issuer.clone()),
                    not_before: leaf.not_before,
                    not_after: leaf.not_after,
                    acme_account_access_id: input.acme_account_credential_id,
                    key_algorithm,
                    description: None,
                })
                .await
                .map_err(|e| domain_error("入库证书记录", e))?;
            let cert_id = cert.id;
            ctx.log_info(format!("新证书已入库：#{cert_id}（旧证书 {} 已归档）", old_fingerprint.unwrap_or_default()));
            return Ok(StepOutput::empty()
                .with_artifact("cert_id", serde_json::json!(cert_id))
                .with_artifact("fingerprint", serde_json::json!(fingerprint)));
        }
        let outcome = repository
            .save(CertInput {
                domains: domains.clone(),
                cert_pem_path: paths.cert_pem,
                key_pem_path: paths.key_pem,
                fingerprint: fingerprint.clone(),
                issuer: (!leaf.issuer.is_empty()).then_some(leaf.issuer.clone()),
                not_before: leaf.not_before,
                not_after: leaf.not_after,
                acme_account_access_id: input.acme_account_credential_id,
                key_algorithm,
                description: None,
            })
            .await
            .map_err(|e| domain_error("入库证书记录", e))?;
        let created = matches!(outcome, acmecast_store::repository::SaveOutcome::Created(_));
        let cert_id = outcome.model().id;
        if created {
            ctx.log_info(format!("新证书已入库：#{cert_id}"));
        } else {
            ctx.log_info(format!("同域名集合的证书已更新：#{cert_id}"));
        }

        Ok(StepOutput::empty()
            .with_artifact("cert_id", serde_json::json!(cert_id))
            .with_artifact("fingerprint", serde_json::json!(fingerprint)))
    }
}
