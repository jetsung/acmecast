//! 证书私钥算法的存量回填。
//!
//! `key_algorithm` 列是后来新增的：迁移前的历史证书记录没有算法值，
//! 而算法只能从私钥文件解析（`acmecast_cert::detect_algorithm`）。
//! 服务启动时（迁移之后）扫一遍 `key_algorithm IS NULL` 的记录，
//! 逐条读取 `key_pem_path` 检测并回填。
//!
//! 失败策略：单条失败（文件丢失、私钥解析失败）只记日志跳过，不阻断启动——
//! 列表页对空算法展示占位即可，不值得为此拒绝服务。

use acmecast_store::repository::CertRepository;
use acmecast_store::FileStore;
use sea_orm::DatabaseConnection;

/// 回填所有缺失 `key_algorithm` 的证书记录，返回回填条数。
pub async fn backfill_key_algorithms(db: &DatabaseConnection, data_dir: &std::path::Path) -> usize {
    let repository = CertRepository::new(db);
    let missing = match repository.list_missing_key_algorithm().await {
        Ok(rows) => rows,
        Err(error) => {
            tracing::warn!(%error, "查询待回填算法的证书记录失败，本轮跳过");
            return 0;
        }
    };
    if missing.is_empty() {
        return 0;
    }

    let Ok(store) = FileStore::open(data_dir).await else {
        tracing::warn!("打开数据目录失败，证书算法回填本轮跳过");
        return 0;
    };

    let mut filled = 0usize;
    for row in missing {
        let algorithm = match store.read(&row.key_pem_path).await {
            Ok(key_pem) => acmecast_cert::detect_algorithm(&key_pem)
                .ok()
                .map(|algo| algo.as_str().to_owned()),
            Err(_) => None,
        };
        let Some(algorithm) = algorithm else {
            tracing::warn!(
                cert_id = row.id,
                key_pem_path = %row.key_pem_path,
                "证书私钥算法检测失败（文件缺失或无法解析），保留为空"
            );
            continue;
        };
        if repository.update_key_algorithm(row.id, &algorithm).await.is_ok() {
            filled += 1;
        } else {
            tracing::warn!(cert_id = row.id, "证书算法回填写库失败，保留为空");
        }
    }
    if filled > 0 {
        tracing::info!(count = filled, "证书私钥算法存量回填完成");
    }
    filled
}
