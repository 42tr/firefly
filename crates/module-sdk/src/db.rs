use chrono::{DateTime, Utc};
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqliteSynchronous};
use std::{str::FromStr, time::Duration};
pub const SINGLE_SCOPE: &str = "1";
/// All persisted TIMESTAMPTZ replacements use UTC microseconds, never SQLx's
/// default integer datetime decoder (which interprets integers as seconds).
#[derive(Debug, Clone, Copy, sqlx::Type)]
#[sqlx(transparent)]
pub struct Timestamp(pub i64);

impl TryFrom<Timestamp> for DateTime<Utc> {
    type Error = std::io::Error;
    fn try_from(value: Timestamp) -> Result<Self, Self::Error> {
        DateTime::from_timestamp_micros(value.0).ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "timestamp outside chrono range",
            )
        })
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
        Ok(Self(<Option<i64> as sqlx::Decode<sqlx::Sqlite>>::decode(
            value,
        )?))
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
    err.chain()
        .any(|cause| match cause.downcast_ref::<sqlx::Error>() {
            Some(sqlx::Error::Database(db)) => db
                .code()
                .and_then(|code| code.parse::<i32>().ok())
                .is_some_and(|code| matches!(code & 0xff, 5 | 6)),
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
