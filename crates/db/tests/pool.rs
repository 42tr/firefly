use std::time::Duration;

use chrono::TimeZone;
use firefly_db::{connect, initialize, PoolConfig};

static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./tests/migrations");

fn config(timezone: chrono_tz::Tz) -> PoolConfig {
    PoolConfig {
        max_connections: 2,
        acquire_timeout: Duration::from_secs(5),
        idle_timeout: Duration::from_secs(60),
        timezone,
    }
}

#[tokio::test]
async fn caller_migrations_and_timezone_are_used_for_every_connection() {
    let directory = tempfile::tempdir().unwrap();
    let utc_url = format!(
        "sqlite://{}",
        directory.path().join("utc.sqlite3").display()
    );
    let ny_url = format!("sqlite://{}", directory.path().join("ny.sqlite3").display());
    let utc = connect(&utc_url, config(chrono_tz::UTC), &MIGRATOR)
        .await
        .unwrap();
    let ny = connect(&ny_url, config(chrono_tz::America::New_York), &MIGRATOR)
        .await
        .unwrap();
    let tables: Vec<String> = sqlx::query_scalar(
        "SELECT name FROM sqlite_master WHERE type='table' AND name NOT GLOB 'sqlite_*' ORDER BY name",
    ).fetch_all(&utc).await.unwrap();
    assert_eq!(tables, ["_sqlx_migrations", "records"]);

    let at = chrono::Utc
        .with_ymd_and_hms(2026, 9, 7, 1, 0, 0)
        .unwrap()
        .timestamp_micros();
    for (pool, expected) in [(&utc, "2026-09-07"), (&ny, "2026-09-06")] {
        // Holding both forces a second connection and exercises its connect hook.
        let mut first = pool.acquire().await.unwrap();
        let mut second = pool.acquire().await.unwrap();
        for connection in [&mut first, &mut second] {
            let (local, legacy, lower, insensitive, sensitive): (String, String, String, bool, bool) = sqlx::query_as(
                "SELECT firefly_local_date(?1),firefly_shanghai_date(?1),firefly_lower('ÄBC'),firefly_ilike('ÄBC','ä%'),firefly_like('ABC','abc')",
            ).bind(at).fetch_one(&mut **connection).await.unwrap();
            assert_eq!(local, expected);
            assert_eq!(legacy, "2026-09-07");
            assert_eq!(lower, "äbc");
            assert!(insensitive);
            assert!(!sensitive);
        }
    }
    utc.close().await;
    ny.close().await;
}

#[tokio::test]
async fn initialize_refuses_overwrite_and_reopening_preserves_data() {
    let directory = tempfile::tempdir().unwrap();
    let url = format!(
        "sqlite://{}",
        directory.path().join("records.sqlite3").display()
    );
    initialize(&url, &MIGRATOR).await.unwrap();
    let pool = connect(&url, config(chrono_tz::UTC), &MIGRATOR)
        .await
        .unwrap();
    sqlx::query("INSERT INTO records(value) VALUES ('keep')")
        .execute(&pool)
        .await
        .unwrap();
    pool.close().await;
    assert!(initialize(&url, &MIGRATOR).await.is_err());

    let reopened = connect(&url, config(chrono_tz::UTC), &MIGRATOR)
        .await
        .unwrap();
    let value: String = sqlx::query_scalar("SELECT value FROM records")
        .fetch_one(&reopened)
        .await
        .unwrap();
    assert_eq!(value, "keep");
    reopened.close().await;
}

#[tokio::test]
async fn foreign_database_is_rejected_without_being_adopted() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("foreign.sqlite3");
    let database = rusqlite::Connection::open(&path).unwrap();
    database
        .execute_batch("CREATE TABLE records(value TEXT); INSERT INTO records VALUES ('foreign')")
        .unwrap();
    let url = format!("sqlite://{}", path.display());
    let error = connect(&url, config(chrono_tz::UTC), &MIGRATOR)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("not initialized"), "{error:#}");
    assert_eq!(
        database
            .query_row("SELECT value FROM records", [], |row| row
                .get::<_, String>(0))
            .unwrap(),
        "foreign"
    );
    let migrated: bool = database
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='_sqlx_migrations')",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(!migrated);
}
