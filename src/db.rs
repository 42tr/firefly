use std::{sync::OnceLock, time::Duration};

use sqlx::SqlitePool;
#[cfg(test)]
use sqlx::sqlite::SqlitePoolOptions;

pub use firefly_db::{retry_busy, NullableTimestamp, Timestamp};
#[cfg(test)]
use firefly_db::options;

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

pub async fn optimize() -> anyhow::Result<()> {
    firefly_db::optimize(pool()).await
}

pub async fn initialize(url: &str) -> anyhow::Result<()> {
    firefly_db::initialize(url, &MIGRATOR).await
}

pub async fn init(url: &str) -> anyhow::Result<()> {
    let cfg = crate::config::get();
    let config = firefly_db::PoolConfig {
        max_connections: cfg.db_max_connections.max(2),
        acquire_timeout: Duration::from_secs(cfg.db_acquire_timeout_secs),
        idle_timeout: Duration::from_secs(cfg.db_idle_timeout_secs),
        timezone: crate::config::timezone(),
    };
    let pool = firefly_db::connect(url, config, &MIGRATOR).await?;
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
    let timezone = crate::config::timezone();
    let pool = SqlitePoolOptions::new()
        .max_connections(4)
        .after_connect(move |conn, _| {
            let directory = directory.clone();
            Box::pin(async move {
                let _keep_directory = directory;
                firefly_db::functions::install(conn, timezone).await
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
#[path = "db/schema_tests.rs"]
mod schema_tests;
