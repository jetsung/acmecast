//! 数据库连接：按 URL scheme 切换 SQLite / MySQL / PostgreSQL。
//!
//! 三种方言共用本模块返回的 [`DatabaseConnection`]，上层业务代码
//! 不感知底层方言差异。

use std::time::Duration;

use acmecast_core::{config::DatabaseConfig, config::DatabaseKind};
use sea_orm::{ConnectOptions, ConnectionTrait, Database, DatabaseConnection, Statement};
use tracing::info;

use crate::error::Result;

/// 依据配置建立连接池。
///
/// - SQLite：URL 形如 `sqlite://./data/acmecast.db?mode=rwc`，会自动创建目录。
/// - MySQL：`mysql://user:pass@host:3306/db`
/// - PostgreSQL：`postgres://user:pass@host:5432/db`
pub async fn connect(config: &DatabaseConfig) -> Result<DatabaseConnection> {
    let kind = config.kind()?;

    // SQLite 是文件库，父目录不存在时 sqlx 会报晦涩的 CannotOpen；
    // 此处提前创建并给出语义清晰的错误。
    if matches!(kind, DatabaseKind::Sqlite) {
        ensure_sqlite_parent_dir(&config.url)?;
        if let Some(true) = is_sqlite_memory(&config.url) {
            info!("使用内存 SQLite，进程退出后数据不保留");
        }
    }

    let mut options = ConnectOptions::new(&config.url);
    options
        .max_connections(config.max_connections)
        .min_connections(1)
        .connect_timeout(Duration::from_secs(config.connect_timeout_secs))
        // SQL 语句中可能含域名与证书信息，交由 tracing 统一过滤。
        .sqlx_logging(true)
        .sqlx_logging_level(tracing::log::LevelFilter::Debug);

    // MySQL 的公共等待超时默认 8 小时，超过后连接会被服务端静默断开。
    // 设置空闲回收，避免复用死连接。
    if matches!(kind, DatabaseKind::MySql) {
        options.idle_timeout(Duration::from_secs(3600));
    }

    let connection = Database::connect(options).await?;

    info!(
        kind = ?kind,
        max_connections = config.max_connections,
        "数据库连接已建立"
    );

    Ok(connection)
}

/// 用当前连接的方言执行一条简单查询，用于连通性自检。
pub async fn ping(db: &DatabaseConnection) -> Result<()> {
    // `SELECT 1` 在 SQLite / MySQL / PostgreSQL 上语义一致。
    db.query_one(Statement::from_string(
        db.get_database_backend(),
        "SELECT 1".to_owned(),
    ))
    .await?;
    Ok(())
}

/// 判断是否为内存 SQLite（`:memory:` 或 `mode=memory`）。
fn is_sqlite_memory(url: &str) -> Option<bool> {
    let lower = url.to_ascii_lowercase();
    if !lower.contains("sqlite") {
        return None;
    }
    Some(lower.contains(":memory:") || lower.contains("mode=memory"))
}

/// 为 SQLite 文件库创建父目录。
fn ensure_sqlite_parent_dir(url: &str) -> Result<()> {
    if is_sqlite_memory(url) == Some(true) {
        return Ok(());
    }
    // `sqlite://./data/acmecast.db?mode=rwc` → `./data/acmecast.db`
    let path_part = url.split("://").nth(1).unwrap_or("");
    let path_part = path_part.split('?').next().unwrap_or(path_part);
    if path_part.is_empty() {
        return Ok(());
    }

    let path = std::path::Path::new(path_part);
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent).map_err(|e| {
            crate::error::Error::Config(format!(
                "无法创建 SQLite 数据目录 {}: {e}",
                parent.display()
            ))
        })?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 建立一个临时文件 SQLite 连接。
    async fn temp_sqlite() -> DatabaseConnection {
        let dir = std::env::temp_dir().join(format!("acmecast-store-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let url = format!("sqlite://{}/test.db?mode=rwc", dir.display());
        let config = DatabaseConfig {
            url,
            ..Default::default()
        };
        connect(&config).await.unwrap()
    }

    #[tokio::test]
    async fn sqlite_connects_and_pings() {
        let db = temp_sqlite().await;
        ping(&db).await.expect("SQLite 连通性自检应成功");
    }

    #[tokio::test]
    async fn sqlite_memory_connects() {
        let config = DatabaseConfig {
            url: "sqlite::memory:".into(),
            ..Default::default()
        };
        let db = connect(&config).await.unwrap();
        ping(&db).await.unwrap();
    }

    #[tokio::test]
    async fn sqlite_reads_back_written_value() {
        use sea_orm::ConnectionTrait;
        let db = temp_sqlite().await;
        db.execute_unprepared("CREATE TABLE t (v INTEGER)")
            .await
            .unwrap();
        db.execute_unprepared("INSERT INTO t VALUES (42)")
            .await
            .unwrap();

        let rows = db
            .query_all(Statement::from_string(
                db.get_database_backend(),
                "SELECT v FROM t".to_owned(),
            ))
            .await
            .unwrap();
        let value: i32 = rows[0].try_get("", "v").unwrap();
        assert_eq!(value, 42);
    }

    #[tokio::test]
    async fn unsupported_dialect_is_rejected() {
        let config = DatabaseConfig {
            url: "mongodb://localhost/db".into(),
            ..Default::default()
        };
        let err = connect(&config).await.unwrap_err();
        assert!(matches!(err, crate::error::Error::Core(_)));
    }

    #[tokio::test]
    async fn sqlite_parent_dir_is_created() {
        let dir = std::env::temp_dir().join(format!("acmecast-newdir-{}", uuid::Uuid::new_v4()));
        let url = format!("sqlite://{}/nested/sub/a.db?mode=rwc", dir.display());
        let config = DatabaseConfig {
            url,
            ..Default::default()
        };
        connect(&config).await.expect("父目录应被自动创建");
        assert!(dir.join("nested/sub").exists());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn memory_sqlite_is_detected() {
        assert_eq!(is_sqlite_memory("sqlite::memory:"), Some(true));
        assert_eq!(is_sqlite_memory("sqlite://file.db?mode=memory"), Some(true));
        assert_eq!(is_sqlite_memory("sqlite://file.db?mode=rwc"), Some(false));
        assert_eq!(is_sqlite_memory("mysql://localhost/db"), None);
    }

    /// 三方言共用同一套断言，只有 URL 不同。SQLite 始终运行；
    /// MySQL / PostgreSQL 由 CI 的 service container 提供连接串。
    ///
    /// 本地开发无需额外参数即可跑通 SQLite；开启 `integration-db`
    /// 并设置 `ACMECAST__DATABASE__URL` 即可覆盖另外两种方言。
    #[cfg(feature = "integration-db")]
    #[tokio::test]
    async fn three_dialects_behave_equivalently() {
        let url = match DatabaseConfig::default().url.as_str() {
            u if std::env::var("ACMECAST__DATABASE__URL").is_ok() => {
                std::env::var("ACMECAST__DATABASE__URL").unwrap()
            }
            u => u.to_owned(),
        };
        let config = DatabaseConfig {
            url,
            ..Default::default()
        };

        let db = connect(&config).await.expect("应按方言建立连接");
        ping(&db).await.expect("连通性自检应成功");

        // 同一种 SQL 在三方言上返回相同结果——SeaORM 负责抹平差异。
        use sea_orm::ConnectionTrait;
        db.execute_unprepared("CREATE TABLE dialect_check (v INTEGER)")
            .await
            .expect("建表应成功");
        db.execute_unprepared("INSERT INTO dialect_check VALUES (7)")
            .await
            .expect("插入应成功");
        let rows = db
            .query_all(Statement::from_string(
                db.get_database_backend(),
                "SELECT v FROM dialect_check".to_owned(),
            ))
            .await
            .expect("查询应成功");
        let value: i32 = rows[0].try_get("", "v").expect("应读回整型");
        assert_eq!(value, 7, "三方言应返回相同结果");

        db.execute_unprepared("DROP TABLE dialect_check").await.ok();
    }
}
