use anyhow::Context;
use chrono::{DateTime, Duration, Utc};
use sqlx::SqlitePool;
use std::{collections::HashSet, time::{Duration as StdDuration, SystemTime}};
use tracing::{info, warn};

use crate::{assistant_store, db};

pub async fn clean_inactive_conversations() -> anyhow::Result<()> {
    let deleted = clean_with_pool(db::pool(), Utc::now(), crate::config::get().conversation_retention_days).await?;
    info!(deleted, "Inactive conversation cleanup completed");
    Ok(())
}

/// Remove upload files left behind by a process crash before the DB commit.
/// A grace period avoids touching a slow upload still being written.
pub async fn clean_orphan_attachments() -> anyhow::Result<()> {
    let directory = std::path::PathBuf::from(crate::config::get().assistant_upload_dir.clone());
    let mut entries = match tokio::fs::read_dir(&directory).await {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    let referenced: HashSet<String> = sqlx::query_scalar::<_, String>("SELECT storage_path FROM assistant_message_attachments")
        .fetch_all(crate::db::pool())
        .await?
        .into_iter()
        .collect();
    let cutoff = SystemTime::now().checked_sub(StdDuration::from_secs(24 * 60 * 60));
    let mut deleted = 0;
    while let Some(entry) = entries.next_entry().await? {
        let file_type = entry.file_type().await?;
        if !file_type.is_file() || referenced.contains(&entry.path().to_string_lossy().into_owned()) {
            continue;
        }
        let modified = entry.metadata().await?.modified().ok();
        if cutoff.is_some_and(|cutoff| modified.is_some_and(|modified| modified <= cutoff)) {
            match tokio::fs::remove_file(entry.path()).await {
                Ok(()) => deleted += 1,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => warn!(path = %entry.path().display(), "failed to remove orphan attachment: {error}"),
            }
        }
    }
    info!(deleted, "Orphan attachment cleanup completed");
    Ok(())
}

async fn clean_with_pool(pool: &SqlitePool, now: DateTime<Utc>, retention_days: u32) -> anyhow::Result<usize> {
    if retention_days == 0 { return Ok(0); }
    let cutoff = now.checked_sub_signed(Duration::days(i64::from(retention_days)))
        .context("CONVERSATION_RETENTION_DAYS exceeds the supported date range")?;
    let mut deleted = 0;
    loop {
        let candidates: Vec<(i64, String)> = sqlx::query_as(
            "SELECT c.id, c.user_id FROM assistant_conversations c
             WHERE c.updated_at <= ?1
               AND NOT EXISTS(SELECT 1 FROM assistant_generation_jobs j
                              WHERE j.conversation_id = c.id AND j.status IN ('queued', 'running'))
             ORDER BY c.updated_at, c.id LIMIT 100",
        )
        .bind(cutoff.timestamp_micros())
        .fetch_all(pool)
        .await?;
        if candidates.is_empty() {
            break;
        }
        for (id, user_id) in candidates {
            let paths = db::retry_busy("delete inactive conversation", || {
                assistant_store::delete_conversation_with_pool(pool, id, &user_id, Some(cutoff))
            })
            .await?;
            let Some(paths) = paths else { continue };
            deleted += 1;
            for path in paths {
                if let Err(err) = tokio::fs::remove_file(&path).await {
                    if err.kind() != std::io::ErrorKind::NotFound {
                        warn!("failed to remove expired conversation attachment {}: {}", path, err);
                    }
                }
            }
        }
    }
    Ok(deleted)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::{query, query_scalar};

    async fn conversation(pool: &SqlitePool, user: &str, at: i64) -> i64 {
        // Databases are isolated, but the active-job registry is process-wide.
        // Distinct fixture IDs prevent other tests' jobs from blocking cleanup.
        use std::sync::atomic::{AtomicI64, Ordering};
        static NEXT_ID: AtomicI64 = AtomicI64::new(1_000_000);
        query_scalar(
            "INSERT INTO assistant_conversations(id, user_id, system_prompt, updated_at)
             VALUES (?1, ?2, '', ?3) RETURNING id",
        )
        .bind(NEXT_ID.fetch_add(1, Ordering::Relaxed))
        .bind(user)
        .bind(at)
        .fetch_one(pool)
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn configured_retention_can_be_disabled_or_changed() {
        let pool = db::test_pool().await;
        let now = Utc::now();
        let old = conversation(&pool, "owner", (now - Duration::days(45)).timestamp_micros()).await;
        let recent = conversation(&pool, "owner", (now - Duration::days(10)).timestamp_micros()).await;
        assert_eq!(clean_with_pool(&pool, now, 0).await.unwrap(), 0);
        assert_eq!(clean_with_pool(&pool, now, 60).await.unwrap(), 0);
        assert_eq!(query_scalar::<_, i64>("SELECT count(*) FROM assistant_conversations").fetch_one(&pool).await.unwrap(), 2);
        assert_eq!(clean_with_pool(&pool, now, 30).await.unwrap(), 1);
        let remaining: Vec<i64> = query_scalar("SELECT id FROM assistant_conversations").fetch_all(&pool).await.unwrap();
        assert_eq!(remaining, [recent]);
        assert!(!remaining.contains(&old));
        assert_eq!(clean_with_pool(&pool, now, 1).await.unwrap(), 1);
    }

    #[tokio::test]
    async fn db_tests_inactivity_cleanup_retention_and_cascades() {
        let pool = db::test_pool().await;
        let now = Utc::now();
        let cutoff = (now - Duration::days(30)).timestamp_micros();
        let expired = conversation(&pool, "owner", cutoff - 1).await;
        let other_owner = conversation(&pool, "other", cutoff - 1).await;
        let boundary = conversation(&pool, "owner", cutoff).await;
        let recent = conversation(&pool, "owner", cutoff + 1).await;
        let queued = conversation(&pool, "owner", cutoff - 1).await;
        let running = conversation(&pool, "owner", cutoff - 1).await;
        let completed_job = uuid::Uuid::new_v4();
        for (id, job, status) in [
            (expired, completed_job, "completed"),
            (queued, uuid::Uuid::new_v4(), "queued"),
            (running, uuid::Uuid::new_v4(), "running"),
        ] {
            query("INSERT INTO assistant_generation_jobs(id, conversation_id, user_id, status) VALUES (?1, ?2, 'owner', ?3)")
                .bind(job).bind(id).bind(status).execute(&pool).await.unwrap();
        }
        let message: i64 = query_scalar(
            "INSERT INTO assistant_messages(conversation_id, role, content, generation_job_id)
             VALUES (?1, 'user', 'expired', ?2) RETURNING id",
        )
        .bind(expired)
        .bind(completed_job)
        .fetch_one(&pool)
        .await
        .unwrap();
        let directory = tempfile::tempdir().unwrap();
        let attachment = directory.path().join("attachment");
        tokio::fs::write(&attachment, b"expired attachment").await.unwrap();
        for path in [attachment.to_str().unwrap(), directory.path().join("already-missing").to_str().unwrap()] {
            query("INSERT INTO assistant_message_attachments(message_id, file_name, content_type, file_size, storage_path)
                   VALUES (?1, 'attachment', 'text/plain', 18, ?2)")
                .bind(message).bind(path).execute(&pool).await.unwrap();
        }
        query("INSERT INTO assistant_workspace_submissions(user_id, conversation_id, request_id, payload_sha256, generation_job_id)
               VALUES ('owner', ?1, ?2, ?3, ?4)")
            .bind(expired).bind(uuid::Uuid::new_v4()).bind("a".repeat(64)).bind(completed_job).execute(&pool).await.unwrap();
        query("INSERT INTO assistant_firefly_branches(conversation_id, user_id, request_id) VALUES (?1, 'owner', ?2)")
            .bind(expired)
            .bind(uuid::Uuid::new_v4())
            .execute(&pool)
            .await
            .unwrap();

        assert_eq!(clean_with_pool(&pool, now, 30).await.unwrap(), 3);
        assert!(!attachment.exists());
        let retained: Vec<i64> =
            query_scalar("SELECT id FROM assistant_conversations ORDER BY id").fetch_all(&pool).await.unwrap();
        assert_eq!(retained, vec![recent, queued, running]);
        assert!(!retained.contains(&other_owner));
        assert!(!retained.contains(&boundary));
        for table in [
            "assistant_messages",
            "assistant_message_attachments",
            "assistant_workspace_submissions",
            "assistant_firefly_branches",
        ] {
            let count: i64 = query_scalar(sqlx::AssertSqlSafe(format!("SELECT COUNT(*) FROM {table}")))
                .fetch_one(&pool)
                .await
                .unwrap();
            assert_eq!(count, 0, "{table}");
        }
        let jobs: i64 = query_scalar("SELECT COUNT(*) FROM assistant_generation_jobs").fetch_one(&pool).await.unwrap();
        assert_eq!(jobs, 2);
        assert_eq!(clean_with_pool(&pool, now, 30).await.unwrap(), 0);
        query("UPDATE assistant_generation_jobs SET status = 'completed'").execute(&pool).await.unwrap();
        assert_eq!(clean_with_pool(&pool, now, 30).await.unwrap(), 2);
    }

    #[tokio::test]
    async fn db_tests_inactivity_cleanup_rechecks_updates_and_active_jobs() {
        let pool = db::test_pool().await;
        db::set_test_pool(pool.clone());
        let cutoff = Utc::now() - Duration::days(30);
        let id = conversation(&pool, "owner", cutoff.timestamp_micros() - 1).await;
        // A job can be queued after the cleanup scan; deletion must recheck.
        let job_id = uuid::Uuid::new_v4();
        query("INSERT INTO assistant_generation_jobs(id, conversation_id, user_id, status) VALUES (?1, ?2, 'owner', 'queued')")
            .bind(job_id).bind(id).execute(&pool).await.unwrap();
        assert!(assistant_store::delete_conversation_with_pool(&pool, id, "owner", Some(cutoff))
            .await
            .unwrap()
            .is_none());
        query("UPDATE assistant_generation_jobs SET status = 'completed' WHERE id = ?1")
            .bind(job_id)
            .execute(&pool)
            .await
            .unwrap();
        let registry = crate::assistant_job::global_registry();
        registry.insert(std::sync::Arc::new(crate::assistant_job::GenerationJob::new(job_id, id, "owner".into())));
        let result = assistant_store::delete_conversation_with_pool(&pool, id, "owner", Some(cutoff)).await;
        registry.remove(job_id);
        assert!(result.unwrap().is_none());

        // A new message or rename after scanning must renew retention.
        assistant_store::append_message(id, assistant_store::StoredMessage {
            role: "user".into(), content: "still working".into(), tool_call_id: None,
            tool_calls: None, metadata: None, is_error: false, generation_job_id: None,
        }).await.unwrap();
        assert!(assistant_store::delete_conversation_with_pool(&pool, id, "owner", Some(cutoff))
            .await.unwrap().is_none());
        query("UPDATE assistant_conversations SET updated_at = ?2 WHERE id = ?1")
            .bind(id).bind(cutoff.timestamp_micros() - 1).execute(&pool).await.unwrap();
        assistant_store::rename_conversation(id, "owner", "renamed").await.unwrap();
        assert_eq!(clean_with_pool(&pool, Utc::now(), 30).await.unwrap(), 0);
        query("UPDATE assistant_conversations SET updated_at = ?2 WHERE id = ?1")
            .bind(id).bind(cutoff.timestamp_micros() - 1).execute(&pool).await.unwrap();
        assert!(assistant_store::delete_conversation_with_pool(&pool, id, "other", Some(cutoff))
            .await.unwrap().is_none());
        assert_eq!(clean_with_pool(&pool, Utc::now(), 30).await.unwrap(), 1);
    }
}
