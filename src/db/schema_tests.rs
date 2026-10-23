use super::*;
use sqlx::migrate::MigrateError;

#[tokio::test]
async fn fresh_database_contains_only_active_tables() {
    let pool = test_pool().await;
    let tables: Vec<String> = sqlx::query_scalar(
        "SELECT name FROM sqlite_master WHERE type='table' AND name NOT GLOB 'sqlite_*' ORDER BY name",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(tables, [
        "_sqlx_migrations",
        "assistant_agent_settings",
        "assistant_conversations",
        "assistant_firefly_branches",
        "assistant_generation_jobs",
        "assistant_goal_stages",
        "assistant_goal_turns",
        "assistant_goals",
        "assistant_message_attachments",
        "assistant_messages",
        "assistant_schedules",
        "assistant_task_events",
        "assistant_tasks",
        "assistant_tool_details",
        "assistant_workspace_submissions",
        "external_module_jobs",
        "external_module_runs",
        "external_module_settings",
        "model_limits_cache",
    ]);
    MIGRATOR.run(&pool).await.unwrap();
}

#[tokio::test]
async fn fresh_database_supports_read_state_and_cascades_conversation_deletion() {
    let pool = test_pool().await;
    let conversation: (i64, i64, bool, bool) = sqlx::query_as(
        "INSERT INTO assistant_conversations(user_id,system_prompt) VALUES ('owner','')
         RETURNING id,read_message_id,is_pinned,is_archived",
    ).fetch_one(&pool).await.unwrap();
    assert_eq!((conversation.1, conversation.2, conversation.3), (0, false, false));
    let job = uuid::Uuid::new_v4();
    let timing: (Option<i64>, Option<i64>, Option<String>, Option<String>) = sqlx::query_as(
        "INSERT INTO assistant_generation_jobs(id,conversation_id,user_id) VALUES (?1,?2,'owner')
         RETURNING started_at,completed_at,codex_settings,claude_settings",
    ).bind(job).bind(conversation.0).fetch_one(&pool).await.unwrap();
    assert_eq!(timing, (None, None, None, None));
    let message: i64 = sqlx::query_scalar(
        "INSERT INTO assistant_messages(conversation_id,generation_job_id,role,content)
         VALUES (?1,?2,'assistant','reply') RETURNING id",
    ).bind(conversation.0).bind(job).fetch_one(&pool).await.unwrap();
    sqlx::query("UPDATE assistant_conversations SET read_message_id=?1,is_pinned=TRUE WHERE id=?2")
        .bind(message).bind(conversation.0).execute(&pool).await.unwrap();
    // Reopening a current database must preserve read state.
    MIGRATOR.run(&pool).await.unwrap();
    let read: i64 = sqlx::query_scalar("SELECT read_message_id FROM assistant_conversations WHERE id=?1")
        .bind(conversation.0).fetch_one(&pool).await.unwrap();
    assert_eq!(read, message);
    sqlx::query("DELETE FROM assistant_conversations WHERE id=?1")
        .bind(conversation.0).execute(&pool).await.unwrap();
    let remaining: (i64, i64) = sqlx::query_as(
        "SELECT (SELECT count(*) FROM assistant_messages), (SELECT count(*) FROM assistant_generation_jobs)",
    ).fetch_one(&pool).await.unwrap();
    assert_eq!(remaining, (0, 0));
    assert!(sqlx::query("PRAGMA foreign_key_check").fetch_all(&pool).await.unwrap().is_empty());
}

#[tokio::test]
async fn unknown_migration_versions_are_rejected() {
    let pool = test_pool().await;
    let unknown = 29990101000000_i64;
    sqlx::query("INSERT INTO _sqlx_migrations (version,description,success,checksum,execution_time) VALUES (?1,'unknown',1,zeroblob(48),0)")
        .bind(unknown).execute(&pool).await.unwrap();
    assert!(matches!(MIGRATOR.run(&pool).await, Err(MigrateError::VersionMissing(version)) if version == unknown));
}

#[tokio::test]
async fn claude_platform_migration_preserves_existing_sessions() {
    let pool = sqlx::SqlitePool::connect("sqlite::memory:").await.unwrap();
    sqlx::raw_sql(include_str!("../../migrations_sqlite/0001_firefly_baseline.sql")).execute(&pool).await.unwrap();
    let id: i64 = sqlx::query_scalar("INSERT INTO assistant_conversations(user_id,system_prompt,agent,claude_session_id,claude_model)
        VALUES ('owner','prompt','claude','old-session','opus') RETURNING id").fetch_one(&pool).await.unwrap();
    sqlx::raw_sql(include_str!("../../migrations_sqlite/0002_claude_platform_and_effort.sql")).execute(&pool).await.unwrap();
    let stored: (String, String, Option<String>, Option<String>) = sqlx::query_as(
        "SELECT claude_session_id,claude_model,claude_provider,claude_reasoning_level FROM assistant_conversations WHERE id=?1")
        .bind(id).fetch_one(&pool).await.unwrap();
    assert_eq!(stored, ("old-session".into(), "opus".into(), None, None));
}
