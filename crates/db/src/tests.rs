use super::*;

static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("../../migrations_sqlite");

async fn test_pool() -> SqlitePool {
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    MIGRATOR.run(&pool).await.unwrap();
    pool
}

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

#[test]
fn db_tests_in_memory_urls_have_no_file_to_create() {
    assert!(is_in_memory("sqlite::memory:"));
    assert!(is_in_memory("sqlite://:memory:"));
    assert!(is_in_memory("sqlite://scratch?cache=shared&mode=memory"));
    assert!(!is_in_memory("sqlite://firefly.sqlite3"));
    assert!(!is_in_memory("sqlite:///var/lib/firefly/firefly.sqlite3?cache=shared"));
}

#[tokio::test]
async fn db_tests_create_database_if_missing_skips_in_memory() {
    let opts = options("sqlite::memory:", false).unwrap();
    assert!(!create_database_if_missing("sqlite::memory:", &opts, &MIGRATOR).await.unwrap());
}

#[tokio::test]
async fn db_tests_create_database_if_missing_is_owner_only_and_keeps_data() {
    let directory = tempfile::tempdir().unwrap();
    let url = format!("sqlite://{}", directory.path().join("auto.sqlite3").display());
    let opts = options(&url, false).unwrap();

    assert!(create_database_if_missing(&url, &opts, &MIGRATOR).await.unwrap());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(opts.get_filename()).unwrap().permissions().mode() & 0o777, 0o600);
    }

    let first = SqlitePoolOptions::new().max_connections(1).connect_with(opts.clone()).await.unwrap();
    let migrated: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='_sqlx_migrations')")
            .fetch_one(&first)
            .await
            .unwrap();
    assert!(migrated);
    sqlx::query("INSERT INTO assistant_conversations (user_id,system_prompt,title) VALUES ('test','','keep')")
        .execute(&first)
        .await
        .unwrap();
    first.close().await;

    assert!(!create_database_if_missing(&url, &opts, &MIGRATOR).await.unwrap());
    let second = SqlitePoolOptions::new().max_connections(1).connect_with(opts).await.unwrap();
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM assistant_conversations").fetch_one(&second).await.unwrap();
    assert_eq!(count, 1);
}
