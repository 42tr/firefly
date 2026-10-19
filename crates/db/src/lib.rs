//! SQLite connections, timestamp decoding, scalar functions, and verified backups.
//!
//! The application owns its pool and migration definitions. Connection settings,
//! timezone, and migrations are supplied explicitly to this library.

use std::{str::FromStr, time::Duration};

use anyhow::Context;
use chrono::{DateTime, Utc};
use sqlx::{
    sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous},
    SqlitePool,
};

pub mod backup;
pub mod functions;
#[cfg(test)]
mod tests;

/// Settings for a caller-owned pool. Each connection receives its own timezone.
#[derive(Clone, Copy, Debug)]
pub struct PoolConfig {
    pub max_connections: u32,
    pub acquire_timeout: Duration,
    pub idle_timeout: Duration,
    pub timezone: chrono_tz::Tz,
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
pub async fn optimize(pool: &SqlitePool) -> anyhow::Result<()> {
    retry_busy("PRAGMA optimize", || async {
        sqlx::query("PRAGMA optimize").execute(pool).await?;
        Ok(())
    })
    .await
}

/// Explicit provisioning ahead of startup; requires an absolute path so the
/// new database location is chosen deliberately. Startup itself creates a
/// missing database through [`connect`].
pub async fn initialize(url: &str, migrator: &sqlx::migrate::Migrator) -> anyhow::Result<()> {
    let options = options(url, false)?;
    anyhow::ensure!(options.get_filename().is_absolute(), "initialization requires an absolute database path");
    create_database(&options, migrator).await
}

/// Creates `options`' database file owner-only and applies all migrations.
/// `create_new` refuses to replace an existing file.
async fn create_database(options: &SqliteConnectOptions, migrator: &sqlx::migrate::Migrator) -> anyhow::Result<()> {
    let mut file = std::fs::OpenOptions::new();
    file.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        file.mode(0o600);
    }
    file.open(options.get_filename())?;
    let pool = SqlitePoolOptions::new().max_connections(1).connect_with(options.clone()).await?;
    check_version(&pool).await?;
    migrator.run(&pool).await?;
    sqlx::query("PRAGMA wal_checkpoint(TRUNCATE)").execute(&pool).await?;
    pool.close().await;
    Ok(())
}

/// Mirrors SQLx's URL parsing to spot databases that never touch disk:
/// `sqlite::memory:` and `?mode=memory` URLs have no file to create.
fn is_in_memory(url: &str) -> bool {
    let rest = url.trim_start_matches("sqlite://").trim_start_matches("sqlite:");
    let (database, params) = rest.split_once('?').unwrap_or((rest, ""));
    database == ":memory:" || params.split('&').any(|param| param == "mode=memory")
}

/// Creates the database for `url` when it names a file that does not exist
/// yet, returning true when one was created. Existing files pass through so
/// the migrations-table check in `connect` can reject a foreign database, and
/// in-memory URLs have nothing to create.
async fn create_database_if_missing(
    url: &str, options: &SqliteConnectOptions, migrator: &sqlx::migrate::Migrator,
) -> anyhow::Result<bool> {
    if is_in_memory(url) || options.get_filename().exists() {
        return Ok(false);
    }
    let path = options.get_filename().to_path_buf();
    create_database(options, migrator).await.with_context(|| format!("failed to create SQLite database {}", path.display()))?;
    tracing::info!("created SQLite database {} and applied migrations", path.display());
    Ok(true)
}

/// Open and migrate a database using caller-provided settings and migrations.
/// Missing files are provisioned owner-only; existing foreign databases are rejected.
pub async fn connect(
    url: &str, config: PoolConfig, migrator: &sqlx::migrate::Migrator,
) -> anyhow::Result<SqlitePool> {
    let opts = options(url, false)?;
    create_database_if_missing(url, &opts, migrator).await?;
    let timezone = config.timezone;
    let pool = SqlitePoolOptions::new()
        .after_connect(move |conn, _| Box::pin(functions::install(conn, timezone)))
        .max_connections(config.max_connections)
        .acquire_timeout(config.acquire_timeout)
        .idle_timeout(config.idle_timeout)
        .connect_with(opts)
        .await
        .context("failed to open SQLite database; check DATABASE_URL and directory permissions")?;
    check_version(&pool).await?;
    let initialized: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='_sqlx_migrations')")
            .fetch_one(&pool)
            .await?;
    anyhow::ensure!(initialized, "database file exists but is not initialized; move it aside or delete it so startup can create a fresh one");
    migrator.run(&pool).await.context("SQLite migrations failed")?;
    Ok(pool)
}
