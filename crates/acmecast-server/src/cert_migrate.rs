//! 存量证书归档迁移：按「域名集合仅保留最新一个」收敛。
//!
//! 旧数据特征：同一 `domains`（规范化去重键）可能曾通过 `CertRepository::create`
//! （手动上传）或历史 `save` 未归档旧证书的时期产生多条记录；或旧版本曾把文件
//! 落到 `certs/<指纹>/` 等旧布局。
//!
//! 目标：对每个去重键 `domains` 仅保留一条「活跃」记录（最新到期者，`not_after`
//! 最大，相等时 `id` 最大），其余移入 `certs/archived/<指纹>/` 并同步
//! `cert_pem_path` / `key_pem_path`。过程幂等、逐条失败不阻断整体。

use acmecast_store::FileStore;
use acmecast_store::entity::cert;
use acmecast_store::repository::CertRepository;
use sea_orm::{DatabaseConnection, EntityTrait};

/// 执行存量归档迁移，返回（活跃保留数，被归档数，跳过数）。

pub async fn migrate_archived_certs(
    db: &DatabaseConnection,
    data_dir: &std::path::Path,
) -> (usize, usize, usize) {
    let rows = match cert::Entity::find().all(db).await {
        Ok(rows) => rows,
        Err(error) => {
            tracing::warn!(%error, "查询证书记录失败，存量归档迁移跳过");
            return (0, 0, 0);
        }
    };
    if rows.is_empty() {
        return (0, 0, 0);
    }

    let Ok(store) = FileStore::open(data_dir).await else {
        tracing::warn!("打开数据目录失败，存量归档迁移跳过");
        return (0, 0, 0);
    };
    let repository = CertRepository::new(db);

    // 已吊销的不参与「仅保留最新活跃」的归并，但其旧路径若仍在旧布局也需归位
    // 到 revoked/<指纹>/（由 revoke 流程负责）；此处仅对非吊销的按 domains 分组。
    let mut groups: std::collections::HashMap<String, Vec<cert::Model>> =
        std::collections::HashMap::new();
    for row in rows {
        if row.revoked_at.is_some() {
            continue;
        }
        groups.entry(row.domains.clone()).or_default().push(row);
    }

    let mut kept = 0usize;
    let mut archived = 0usize;
    let mut skipped = 0usize;

    for (_domains, mut members) in groups {
        if members.len() <= 1 {
            // 单条记录是否仍需把旧布局路径扁平到 certs/ssl/ 或 archived
            if let Some(row) = members.pop() {
                match ensure_flat_paths(&store, &repository, &row).await {
                    Ok(true) => archived += 1,
                    Ok(false) => kept += 1,
                    Err(_) => skipped += 1,
                }
            }
            continue;
        }

        // 最新者保留：not_after 降序，相等时 id 降序
        members.sort_by(|a, b| {
            b.not_after
                .cmp(&a.not_after)
                .then_with(|| b.id.cmp(&a.id))
        });
        let winner = members.remove(0);
        match ensure_flat_paths(&store, &repository, &winner).await {
            Ok(true) => archived += 1, // winner 也可能从旧布局扁平到 ssl，算作归档动作
            Ok(false) => kept += 1,
            Err(_) => skipped += 1,
        }

        for old in members {
            // 已在 archived/<指纹>/ 的视为已归档，幂等跳过
            let archived_prefix = format!("certs/archived/{}", old.fingerprint);
            if old.cert_pem_path.starts_with(&archived_prefix)
                || old.key_pem_path.starts_with(&archived_prefix)
            {
                skipped += 1;
                continue;
            }

            // 活跃路径已在新扁平位置的旧记录：直接归档到 archived/<指纹>/
            // 旧布局路径（certs/<指纹>/...）也会被 archive 搬到 archived/<指纹>/
            let result = store
                .archive_replaced_certificate(&old.cert_pem_path, &old.key_pem_path, &old.fingerprint)
                .await;
            match result {
                Ok(Some(paths)) => {
                    if repository.update_paths(old.id, &paths).await.is_ok() {
                        archived += 1;
                        tracing::info!(
                            cert_id = old.id,
                            fingerprint = %old.fingerprint,
                            cert_pem_path = %paths.cert_pem,
                            "存量证书已归档到 archived"
                        );
                    } else {
                        skipped += 1;
                        tracing::warn!(cert_id = old.id, "存量证书归档后更新路径失败");
                        // 回退文件
                        let original = acmecast_store::CertFilePaths {
                            cert_pem: old.cert_pem_path.clone(),
                            key_pem: old.key_pem_path.clone(),
                        };
                        if let Err(e) = store.restore_certificate(&paths, &original).await {
                            tracing::warn!(cert_id = old.id, %e, "归档回退失败，请人工核对");
                        }
                    }
                }
                Ok(None) => {
                    // 文件缺失：仍把 DB 路径指向 archived/<指纹>/ 的预期位置，
                    // 保证「活跃扁平」与数据库一致，不阻断
                    let expected = acmecast_store::CertFilePaths {
                        cert_pem: format!("certs/archived/{}/{}", old.fingerprint, basename(&old.cert_pem_path)),
                        key_pem: format!("certs/archived/{}/{}", old.fingerprint, basename(&old.key_pem_path)),
                    };
                    if repository.update_paths(old.id, &expected).await.is_ok() {
                        archived += 1;
                        tracing::warn!(
                            cert_id = old.id,
                            "存量证书文件缺失，已将路径指向 archived 预期位置"
                        );
                    } else {
                        skipped += 1;
                    }
                }
                Err(error) => {
                    skipped += 1;
                    tracing::warn!(cert_id = old.id, %error, "存量证书归档失败");
                }
            }
        }
    }

    if archived > 0 || kept > 0 {
        tracing::info!(kept, archived, skipped, "存量证书归档迁移完成");
    }
    (kept, archived, skipped)
}

/// 确保单条记录的路径已在新布局：活跃的扁平 `certs/ssl/` 或已归档的 `archived/<指纹>/`。
/// 返回 Ok(true) 表示发生了迁移（旧路径→新位置），Ok(false) 表示已在新布局。

async fn ensure_flat_paths(
    store: &FileStore,
    repository: &CertRepository<'_>,
    row: &cert::Model,
) -> Result<bool, ()> {
    let archived_prefix = format!("certs/archived/{}", row.fingerprint);
    let revoked_prefix = format!("certs/revoked/{}", row.fingerprint);
    // 已在新布局
    if row.cert_pem_path.starts_with("certs/ssl/")
        || row.cert_pem_path.starts_with(&archived_prefix)
        || row.cert_pem_path.starts_with(&revoked_prefix)
    {
        return Ok(false);
    }

    // 旧布局：尝试把活跃文件扁平到 certs/ssl/（若被占用则归档到 archived）
    // 实际存量中旧布局多为「同一 domains 仅一条」，所以直接搬到 ssl/ 即可
    let candidate = FileStore::cert_paths(&row.fingerprint, &primary_from_domains(&row.domains));
    let ssl_cert = candidate.cert_pem.clone();
    let ssl_key = candidate.key_pem.clone();

    // 若 ssl 下已有其他记录的文件，直接把本条归档到 archived
    let ssl_cert_exists = store.read(&ssl_cert).await.is_ok();
    let target = if ssl_cert_exists {
        // 归档到 archived/<指纹>/
        let base_cert = basename(&row.cert_pem_path);
        let base_key = basename(&row.key_pem_path);
        acmecast_store::CertFilePaths {
            cert_pem: format!("certs/archived/{}/{}", row.fingerprint, base_cert),
            key_pem: format!("certs/archived/{}/{}", row.fingerprint, base_key),
        }
    } else {
        candidate
    };

    // 先尝试移动文件（旧路径可能与目标同名，直接 write 覆盖也等价，这里走 move）
    // 若源文件缺失，直接更新 DB 指向目标
    let moved = match store
        .archive_replaced_certificate(&row.cert_pem_path, &row.key_pem_path, &row.fingerprint)
        .await
    {
        Ok(Some(paths)) if target.cert_pem.starts_with("certs/archived/") => paths,
        Ok(Some(paths)) => paths,
        Ok(None) => target,
        Err(_) => return Err(()),
    };

    // 若 moved 是 archived 而目标是 ssl（说明旧文件曾在）需把 archived 内容再搬回 ssl 不适用存量单条；
    // 简化：单条旧布局直接按 moved 更新（若 move 到了 archived，再搬到 ssl 的场景由分组处理）
    // 但单条旧布局的正确终态应为 ssl，因此尝试把 archived 再写回到 ssl（若 ssl 空闲）
    let final_paths = if moved.cert_pem.starts_with("certs/archived/") && !ssl_cert_exists {
        // 把刚归档的文件再移到 ssl（单条旧记录的扁平化）
        match store.read(&moved.cert_pem).await {
            Ok(cert_pem) => match store.read(&moved.key_pem).await {
                Ok(key_pem) => {
                    if store.write(&ssl_cert, &cert_pem).await.is_ok()
                        && store.write(&ssl_key, &key_pem).await.is_ok()
                    {
                        let _ = store.delete(&moved.cert_pem).await;
                        let _ = store.delete(&moved.key_pem).await;
                        acmecast_store::CertFilePaths {
                            cert_pem: ssl_cert.clone(),
                            key_pem: ssl_key.clone(),
                        }
                    } else {
                        moved
                    }
                }
                Err(_) => moved,
            },
            Err(_) => moved,
        }
    } else {
        moved
    };

    if final_paths.cert_pem == row.cert_pem_path && final_paths.key_pem == row.key_pem_path {
        return Ok(false);
    }
    repository
        .update_paths(row.id, &final_paths)
        .await
        .map(|_| true)
        .map_err(|_| ())
}

fn basename(relative: &str) -> String {
    std::path::Path::new(relative)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(relative)
        .to_owned()
}

fn primary_from_domains(encoded: &str) -> String {
    cert::decode_domains(encoded)
        .into_iter()
        .next()
        .unwrap_or_else(|| "certificate".to_owned())
}
