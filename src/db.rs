use std::{str::FromStr, sync::OnceLock, time::Duration};

use anyhow::Context;
use chrono::{DateTime, Utc};
use sqlx::{
    sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous},
    SqlitePool,
};

/// Fixed storage scope for single-user data.
pub const SINGLE_SCOPE: &str = "1";

static DB_POOL: OnceLock<SqlitePool> = OnceLock::new();
pub static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations_sqlite");

pub fn pool() -> &'static SqlitePool {
    #[cfg(test)]
    if let Some(pool) = TEST_POOL.with(|slot| *slot.borrow()) {
        return pool;
    }
    DB_POOL.get().expect("database pool not initialized")
}

pub fn maybe_pool() -> Option<&'static SqlitePool> {
    #[cfg(test)]
    if let Some(pool) = TEST_POOL.with(|slot| *slot.borrow()) {
        return Some(pool);
    }
    DB_POOL.get()
}

/// All persisted TIMESTAMPTZ replacements use UTC microseconds, never SQLx's
/// default integer datetime decoder (which interprets integers as seconds).
#[derive(Debug, Clone, Copy, sqlx::Type)]
#[sqlx(transparent)]
pub struct Timestamp(pub i64);

impl TryFrom<Timestamp> for DateTime<Utc> {
    type Error = std::io::Error;
    fn try_from(value: Timestamp) -> Result<Self, Self::Error> {
        DateTime::from_timestamp_micros(value.0)
            .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidData, "timestamp outside chrono range"))
    }
}

#[derive(Debug)]
pub struct NullableTimestamp(pub Option<i64>);
impl sqlx::Type<sqlx::Sqlite> for NullableTimestamp {
    fn type_info() -> sqlx::sqlite::SqliteTypeInfo {
        <i64 as sqlx::Type<sqlx::Sqlite>>::type_info()
    }
    fn compatible(ty: &sqlx::sqlite::SqliteTypeInfo) -> bool {
        <i64 as sqlx::Type<sqlx::Sqlite>>::compatible(ty)
    }
}
impl<'r> sqlx::Decode<'r, sqlx::Sqlite> for NullableTimestamp {
    fn decode(value: sqlx::sqlite::SqliteValueRef<'r>) -> Result<Self, sqlx::error::BoxDynError> {
        Ok(Self(<Option<i64> as sqlx::Decode<sqlx::Sqlite>>::decode(value)?))
    }
}
impl TryFrom<NullableTimestamp> for Option<DateTime<Utc>> {
    type Error = std::io::Error;
    fn try_from(value: NullableTimestamp) -> Result<Self, Self::Error> {
        value.0.map(|v| Timestamp(v).try_into()).transpose()
    }
}

pub fn options(url: &str, create: bool) -> anyhow::Result<SqliteConnectOptions> {
    anyhow::ensure!(url.starts_with("sqlite:"), "SQLite DATABASE_URL required");
    Ok(SqliteConnectOptions::from_str(url)?
        .create_if_missing(create)
        .journal_mode(SqliteJournalMode::Wal)
        // WAL + NORMAL stays corruption-safe; only fsyncs at checkpoints, so a
        // commit no longer holds the writer lock for a slow fsync under I/O load.
        // Power loss may drop the last few commits.
        .synchronous(SqliteSynchronous::Normal)
        .foreign_keys(true)
        .busy_timeout(Duration::from_secs(30)))
}

/// SQLITE_BUSY / SQLITE_LOCKED (including extended codes) anywhere in the chain.
pub fn is_busy(err: &anyhow::Error) -> bool {
    err.chain().any(|cause| match cause.downcast_ref::<sqlx::Error>() {
        Some(sqlx::Error::Database(db)) => {
            db.code().and_then(|code| code.parse::<i32>().ok()).is_some_and(|code| matches!(code & 0xff, 5 | 6))
        }
        _ => false,
    })
}

/// Retries writer contention that outlasted `busy_timeout`. Only wrap units that
/// are safe to rerun: a single idempotent statement or a whole transaction
/// (a failed transaction is rolled back when dropped).
pub async fn retry_busy<T, F, Fut>(what: &str, mut op: F) -> anyhow::Result<T>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = anyhow::Result<T>>,
{
    for delay in [1, 3, 10] {
        match op().await {
            Err(err) if is_busy(&err) => {
                tracing::warn!("{what}: database busy, retrying in {delay}s: {err:#}");
                tokio::time::sleep(Duration::from_secs(delay)).await;
            }
            result => return result,
        }
    }
    op().await
}

pub async fn check_version(pool: &SqlitePool) -> anyhow::Result<()> {
    let version: String = sqlx::query_scalar("SELECT sqlite_version()").fetch_one(pool).await?;
    let parts: Vec<u32> = version.split('.').map(str::parse).collect::<Result<_, _>>()?;
    anyhow::ensure!(
        parts.as_slice() >= [3, 51, 3].as_slice(),
        "SQLite {version} lacks required WAL fixes; rebuild with cargo build --locked (>= 3.51.3 required)"
    );
    Ok(())
}

/// 让查询计划器依据统计信息工作。`PRAGMA optimize`（SQLite 3.18+）只会对统计
/// 变化较大的表补跑 ANALYZE，比无条件全库 ANALYZE 便宜，适合定期执行。它会写
/// sqlite_stat1，因此按普通写操作处理忙等重试。
pub async fn optimize() -> anyhow::Result<()> {
    retry_busy("PRAGMA optimize", || async {
        sqlx::query("PRAGMA optimize").execute(pool()).await?;
        Ok(())
    })
    .await
}

/// Explicit initialization only: never silently create an empty production DB.
pub async fn initialize(url: &str) -> anyhow::Result<()> {
    let options = options(url, false)?;
    let path = options.get_filename();
    anyhow::ensure!(path.is_absolute(), "initialization requires an absolute database path");
    let mut file = std::fs::OpenOptions::new();
    file.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        file.mode(0o600);
    }
    file.open(path)?;
    let pool = SqlitePoolOptions::new().max_connections(1).connect_with(options).await?;
    check_version(&pool).await?;
    MIGRATOR.run(&pool).await?;
    sqlx::query("PRAGMA wal_checkpoint(TRUNCATE)").execute(&pool).await?;
    pool.close().await;
    Ok(())
}

pub async fn init(url: &str) -> anyhow::Result<()> {
    let cfg = crate::config::get();
    let opts = options(url, false)?;
    let pool = SqlitePoolOptions::new()
        .after_connect(|conn, _| Box::pin(crate::db_functions::install(conn)))
        .max_connections(cfg.db_max_connections.max(2))
        .acquire_timeout(Duration::from_secs(cfg.db_acquire_timeout_secs))
        .idle_timeout(Duration::from_secs(cfg.db_idle_timeout_secs))
        .connect_with(opts)
        .await
        .context("failed to open SQLite database (initialize it with --init-db first)")?;
    check_version(&pool).await?;
    let initialized: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='_sqlx_migrations')")
            .fetch_one(&pool)
            .await?;
    anyhow::ensure!(initialized, "database was not initialized; use --init-db explicitly");
    MIGRATOR.run(&pool).await.context("SQLite migrations failed")?;
    DB_POOL.set(pool).map_err(|_| anyhow::anyhow!("database pool already initialized"))?;
    Ok(())
}

#[cfg(test)]
pub(crate) fn set_test_pool(pool: SqlitePool) {
    TEST_POOL.with(|slot| *slot.borrow_mut() = Some(Box::leak(Box::new(pool))));
}

#[cfg(test)]
thread_local! { static TEST_POOL: std::cell::RefCell<Option<&'static SqlitePool>> = const { std::cell::RefCell::new(None) }; }

#[cfg(test)]
pub(crate) async fn test_pool() -> SqlitePool {
    let directory = std::sync::Arc::new(tempfile::tempdir().unwrap());
    let url = format!("sqlite://{}", directory.path().join("test.sqlite3").display());
    let pool = SqlitePoolOptions::new()
        .max_connections(4)
        .after_connect(move |conn, _| {
            let directory = directory.clone();
            Box::pin(async move {
                let _keep_directory = directory;
                crate::db_functions::install(conn).await
            })
        })
        .connect_with(options(&url, true).unwrap())
        .await
        .unwrap();
    MIGRATOR.run(&pool).await.unwrap();
    pool
}

#[cfg(test)]
pub(crate) fn test_connection() -> rusqlite::Connection {
    let connection = rusqlite::Connection::open_in_memory().unwrap();
    connection.execute_batch("PRAGMA foreign_keys=ON").unwrap();
    for migration in MIGRATOR.iter() {
        connection.execute_batch(migration.sql.as_str()).unwrap();
    }
    connection
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn db_tests_runtime_version_time_and_cancelled_writer() {
        let pool = test_pool().await;
        check_version(&pool).await.unwrap();
        let timestamp = -1234567890123i64;
        let (value, none): (Timestamp, NullableTimestamp) =
            sqlx::query_as("SELECT ?1, NULL").bind(timestamp).fetch_one(&pool).await.unwrap();
        let decoded: DateTime<Utc> = value.try_into().unwrap();
        assert_eq!(decoded.timestamp_micros(), timestamp);
        assert!(Option::<DateTime<Utc>>::try_from(none).unwrap().is_none());
        let mut tx = pool.begin_with("BEGIN IMMEDIATE").await.unwrap();
        sqlx::query("INSERT INTO assistant_conversations (user_id,system_prompt,title) VALUES ('test','','rollback')").execute(&mut *tx).await.unwrap();
        drop(tx); // Same RAII rollback path used when a request future is cancelled.
        let mut next = pool.begin_with("BEGIN IMMEDIATE").await.unwrap();
        let count: i64 = sqlx::query_scalar("SELECT count(*) FROM assistant_conversations").fetch_one(&mut *next).await.unwrap();
        assert_eq!(count, 0);
        next.rollback().await.unwrap();
    }

    #[tokio::test]
    async fn db_tests_retry_busy_waits_out_writer() {
        let directory = tempfile::tempdir().unwrap();
        let url = format!("sqlite://{}", directory.path().join("busy.sqlite3").display());
        let holder =
            SqlitePoolOptions::new().max_connections(1).connect_with(options(&url, true).unwrap()).await.unwrap();
        MIGRATOR.run(&holder).await.unwrap();
        let impatient = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options(&url, false).unwrap().busy_timeout(Duration::ZERO))
            .await
            .unwrap();
        let write = || async {
            sqlx::query("INSERT INTO assistant_conversations (user_id,system_prompt,title) VALUES ('test','','busy')").execute(&impatient).await?;
            Ok(())
        };

        let tx = holder.begin_with("BEGIN IMMEDIATE").await.unwrap();
        let err = write().await.unwrap_err();
        assert!(is_busy(&err), "{err:#}");
        assert!(!is_busy(&anyhow::anyhow!("other")));

        let release = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(300)).await;
            tx.rollback().await.unwrap();
        });
        retry_busy("test write", write).await.unwrap();
        release.await.unwrap();
        let count: i64 = sqlx::query_scalar("SELECT count(*) FROM assistant_conversations").fetch_one(&holder).await.unwrap();
        assert_eq!(count, 1);
    }
}

#[cfg(test)]
#[path = "db/schema_tests.rs"]
mod schema_tests;
