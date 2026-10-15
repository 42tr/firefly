//! Atomic workspace submissions and durable identities for response-loss retries.
use super::*;

#[derive(Debug, thiserror::Error)]
pub enum SubmissionConflict {
    #[error("该请求 ID 已用于不同的消息，请使用新的请求 ID")]
    PayloadChanged,
    #[error("该消息已被删除，请重新发送新的消息")]
    Removed,
    #[error("会话不存在或无权访问")]
    ConversationUnavailable,
    #[error("独立工作目录尚未就绪，请先重试目录准备")]
    WorkspaceUnavailable,
    #[error("等待中的任务过多，请等待已有任务完成后重试")]
    QueueFull,
}

/// Call inside the submission's writer transaction, after idempotent replay.
pub async fn enforce_submission_capacity(
    conn: &mut SqliteConnection,
    user_id: &str,
    limit: usize,
) -> anyhow::Result<()> {
    let queued: i64 = query_scalar(
        "SELECT COUNT(*) FROM assistant_generation_jobs WHERE user_id=?1 AND status='queued'",
    )
    .bind(user_id)
    .fetch_one(&mut *conn)
    .await?;
    if queued >= limit.max(1).min(i64::MAX as usize) as i64 {
        return Err(SubmissionConflict::QueueFull.into());
    }
    Ok(())
}

#[derive(Debug)]
pub struct QueuedSubmission {
    pub message: MessageRecord,
    pub attachments: Vec<AttachmentRecord>,
    pub replayed: bool,
}

pub async fn submit_queued_user_turn(
    conversation_id: i64,
    user_id: &str,
    request_id: Option<uuid::Uuid>,
    payload_sha256: &str,
    enable_tools: bool,
    content: String,
    attachments: &[NewAttachment],
) -> anyhow::Result<QueuedSubmission> {
    let mut tx = db::pool().begin_with("BEGIN IMMEDIATE").await?;
    // Recheck ownership inside the writer transaction, including retries.
    // The HTTP handler's earlier check alone can race with deletion.
    let allowed: bool = query_scalar(
        "SELECT EXISTS(SELECT 1 FROM assistant_conversations WHERE id = ?1 AND user_id = ?2 AND is_archived = FALSE)",
    )
    .bind(conversation_id)
    .bind(user_id)
    .fetch_one(&mut *tx)
    .await?;
    if !allowed {
        return Err(SubmissionConflict::ConversationUnavailable.into());
    }
    if let Some(request_id) = request_id {
        let existing: Option<(Option<uuid::Uuid>, String)> = query_as(
            "SELECT generation_job_id, payload_sha256 FROM assistant_workspace_submissions
             WHERE user_id = ?1 AND conversation_id = ?2 AND request_id = ?3",
        )
        .bind(user_id)
        .bind(conversation_id)
        .bind(request_id)
        .fetch_optional(&mut *tx)
        .await?;
        if let Some((job_id, fingerprint)) = existing {
            if fingerprint != payload_sha256 {
                return Err(SubmissionConflict::PayloadChanged.into());
            }
            let job_id = job_id.ok_or(SubmissionConflict::Removed)?;
            let message = query_as::<_, MessageRecord>(
                "SELECT id, conversation_id, role, content, tool_call_id, tool_calls, metadata, is_error,
                        created_at, generation_job_id, NULL AS processing_status, NULL AS processing_error
                 FROM assistant_messages WHERE generation_job_id = ?1 AND role = 'user'",
            )
            .bind(job_id)
            .fetch_optional(&mut *tx)
            .await?
            .ok_or(SubmissionConflict::Removed)?;
            let attachments = query_as::<_, AttachmentRecord>(
                "SELECT id, message_id, file_name, content_type, file_size, storage_path, created_at
                 FROM assistant_message_attachments WHERE message_id = ?1 ORDER BY id",
            )
            .bind(message.id)
            .fetch_all(&mut *tx)
            .await?;
            tx.commit().await?;
            return Ok(QueuedSubmission {
                message,
                attachments,
                replayed: true,
            });
        }
    }
    let queue_limit = crate::config::try_get()
        .map(|cfg| cfg.assistant_max_queued_jobs)
        .unwrap_or(32);
    enforce_submission_capacity(&mut tx, user_id, queue_limit).await?;
    let workspace_unavailable: bool = query_scalar(
        "SELECT workspace_mode = 'parallel' AND workspace_state IN ('failed', 'cancelled', 'missing')
         FROM assistant_conversations WHERE id = ?1",
    )
    .bind(conversation_id)
    .fetch_one(&mut *tx)
    .await?;
    if workspace_unavailable {
        return Err(SubmissionConflict::WorkspaceUnavailable.into());
    }
    let job_id = uuid::Uuid::new_v4();
    let (message, attachments) = super::generation::insert_queued_user_turn(
        &mut tx,
        job_id,
        conversation_id,
        user_id,
        enable_tools,
        content,
        attachments,
    )
    .await?;
    if let Some(request_id) = request_id {
        query(
            "INSERT INTO assistant_workspace_submissions(user_id, conversation_id, request_id, payload_sha256, generation_job_id)
             VALUES (?1, ?2, ?3, ?4, ?5)",
        )
        .bind(user_id)
        .bind(conversation_id)
        .bind(request_id)
        .bind(payload_sha256)
        .bind(job_id)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    crate::assistant_events::notify(Some(user_id), "conversations");
    Ok(QueuedSubmission {
        message,
        attachments,
        replayed: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn db_tests_archival_and_submission_cannot_both_succeed() {
        let pool = crate::db::test_pool().await;
        crate::db::set_test_pool(pool.clone());
        for archive_first in [true, false] {
            let id = conversation(&pool, "owner").await;
            let hash = "a".repeat(64);
            let archive = super::super::set_conversation_archived(id, "owner", true);
            let send = submit_queued_user_turn(id, "owner", None, &hash, true, "hello".into(), &[]);
            let (archived, submitted) = if archive_first {
                tokio::join!(archive, send)
            } else {
                let (submitted, archived) = tokio::join!(send, archive);
                (archived, submitted)
            };
            match (archived, submitted) {
                (Ok(true), Err(error)) => assert!(matches!(error.downcast_ref(), Some(SubmissionConflict::ConversationUnavailable))),
                (Err(error), Ok(_)) => assert!(error.is::<super::super::ConversationArchivalBlocked>()),
                result => panic!("archive and submission must exclude each other: {result:?}"),
            }
        }
    }

    async fn conversation(pool: &sqlx::SqlitePool, owner: &str) -> i64 {
        query_scalar("INSERT INTO assistant_conversations(user_id, system_prompt) VALUES (?1, '') RETURNING id")
            .bind(owner)
            .fetch_one(pool)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn db_tests_queue_capacity_is_atomic_scoped_and_allows_idempotent_replay() {
        let pool = db::test_pool().await;
        db::set_test_pool(pool.clone());
        let alice = conversation(&pool, "alice").await;
        let bob = conversation(&pool, "bob").await;
        let limit = crate::config::try_get()
            .map(|cfg| cfg.assistant_max_queued_jobs)
            .unwrap_or(32)
            .max(1);
        let request = uuid::Uuid::new_v4();
        let hash = "a".repeat(64);
        let original = submit_queued_user_turn(
            alice,
            "alice",
            Some(request),
            &hash,
            true,
            "first".into(),
            &[],
        )
        .await
        .unwrap();
        for _ in 1..limit.saturating_sub(1) {
            create_queued_user_turn(
                uuid::Uuid::new_v4(),
                alice,
                "alice",
                true,
                "queued".into(),
                &[],
            )
            .await
            .unwrap();
        }
        if limit > 1 {
            let (a, b) = tokio::join!(
                submit_queued_user_turn(alice, "alice", None, &hash, true, "a".into(), &[]),
                submit_queued_user_turn(alice, "alice", None, &hash, true, "b".into(), &[]),
            );
            assert_ne!(a.is_ok(), b.is_ok());
            let rejected = if let Err(err) = a {
                err
            } else {
                b.unwrap_err()
            };
            assert!(matches!(
                rejected.downcast_ref(),
                Some(SubmissionConflict::QueueFull)
            ));
        }
        let retry = submit_queued_user_turn(
            alice,
            "alice",
            Some(request),
            &hash,
            true,
            "first".into(),
            &[],
        )
        .await
        .unwrap();
        assert!(retry.replayed);
        assert_eq!(retry.message.id, original.message.id);
        assert!(matches!(
            submit_queued_user_turn(alice, "alice", None, &hash, true, "full".into(), &[])
                .await
                .unwrap_err()
                .downcast_ref(),
            Some(SubmissionConflict::QueueFull)
        ));
        assert!(
            submit_queued_user_turn(bob, "bob", None, &hash, true, "other".into(), &[])
                .await
                .is_ok()
        );
        claim_next_queued_generation(alice, "alice")
            .await
            .unwrap()
            .unwrap();
        assert!(
            submit_queued_user_turn(alice, "alice", None, &hash, true, "next".into(), &[])
                .await
                .is_ok()
        );
        let queued: i64 =
            query_scalar("SELECT COUNT(*) FROM assistant_generation_jobs WHERE user_id='alice' AND status='queued'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(queued, limit as i64);
    }

    #[tokio::test]
    async fn db_tests_workspace_submission_retries_are_atomic_and_scoped() {
        let pool = db::test_pool().await;
        db::set_test_pool(pool.clone());
        let first = conversation(&pool, "alice").await;
        let second = conversation(&pool, "alice").await;
        let other = conversation(&pool, "bob").await;
        let request = uuid::Uuid::new_v4();
        let hash = "a".repeat(64);
        let files = [NewAttachment {
            file_name: "a.txt".into(),
            content_type: "text/plain".into(),
            file_size: 3,
            storage_path: "original/a.txt".into(),
        }];
        let send = || {
            submit_queued_user_turn(
                first,
                "alice",
                Some(request),
                &hash,
                true,
                "hello".into(),
                &files,
            )
        };
        // Both calls compete for the same SQLite writer lock.
        let (a, b) = tokio::join!(send(), send());
        let (a, b) = (a.unwrap(), b.unwrap());
        assert_ne!(a.replayed, b.replayed);
        assert_eq!(a.message.id, b.message.id);
        assert_eq!(a.attachments[0].id, b.attachments[0].id);
        let job = a.message.generation_job_id.unwrap();
        update_generation_status(job, "completed", None)
            .await
            .unwrap();
        assert!(send().await.unwrap().replayed);
        assert_eq!(
            query_scalar::<_, i64>("SELECT COUNT(*) FROM assistant_generation_jobs")
                .fetch_one(&pool)
                .await
                .unwrap(),
            1
        );
        assert_eq!(
            query_scalar::<_, i64>(
                "SELECT message_count FROM assistant_conversations WHERE id = ?1"
            )
            .bind(first)
            .fetch_one(&pool)
            .await
            .unwrap(),
            1
        );
        let changed = submit_queued_user_turn(
            first,
            "alice",
            Some(request),
            &"b".repeat(64),
            true,
            "edited".into(),
            &[],
        )
        .await
        .unwrap_err();
        assert!(matches!(
            changed.downcast_ref(),
            Some(SubmissionConflict::PayloadChanged)
        ));
        let forbidden = submit_queued_user_turn(
            first,
            "bob",
            Some(request),
            &hash,
            true,
            "hello".into(),
            &[],
        )
        .await
        .unwrap_err();
        assert!(matches!(
            forbidden.downcast_ref(),
            Some(SubmissionConflict::ConversationUnavailable)
        ));
        for (id, owner) in [(second, "alice"), (other, "bob")] {
            let submission =
                submit_queued_user_turn(id, owner, Some(request), &hash, true, "hello".into(), &[])
                    .await
                    .unwrap();
            assert!(!submission.replayed);
            assert_ne!(submission.message.generation_job_id, Some(job));
        }
        query("DELETE FROM assistant_conversations WHERE id = ?1")
            .bind(first)
            .execute(&pool)
            .await
            .unwrap();
        assert!(matches!(
            send().await.unwrap_err().downcast_ref(),
            Some(SubmissionConflict::ConversationUnavailable)
        ));
    }

    #[tokio::test]
    async fn db_tests_removed_submissions_do_not_restart_and_failed_inserts_can_retry() {
        let pool = db::test_pool().await;
        db::set_test_pool(pool.clone());
        let id = conversation(&pool, "owner").await;
        let request = uuid::Uuid::new_v4();
        let hash = "a".repeat(64);
        let send = || {
            submit_queued_user_turn(id, "owner", Some(request), &hash, true, "hello".into(), &[])
        };
        let original = send().await.unwrap();
        delete_queued_generation(original.message.generation_job_id.unwrap(), "owner")
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(
            send().await.unwrap_err().downcast_ref(),
            Some(SubmissionConflict::Removed)
        ));
        assert_eq!(
            query_scalar::<_, i64>("SELECT COUNT(*) FROM assistant_generation_jobs")
                .fetch_one(&pool)
                .await
                .unwrap(),
            0
        );

        let fresh = uuid::Uuid::new_v4();
        query("CREATE TRIGGER reject_submission BEFORE INSERT ON assistant_workspace_submissions BEGIN SELECT RAISE(ABORT, 'rejected'); END").execute(&pool).await.unwrap();
        assert!(submit_queued_user_turn(
            id,
            "owner",
            Some(fresh),
            &hash,
            true,
            "hello".into(),
            &[]
        )
        .await
        .is_err());
        assert_eq!(
            query_scalar::<_, i64>("SELECT COUNT(*) FROM assistant_messages")
                .fetch_one(&pool)
                .await
                .unwrap(),
            0
        );
        query("DROP TRIGGER reject_submission")
            .execute(&pool)
            .await
            .unwrap();
        assert!(
            !submit_queued_user_turn(id, "owner", Some(fresh), &hash, true, "hello".into(), &[])
                .await
                .unwrap()
                .replayed
        );
        // Legacy clients without an identity continue to create distinct turns.
        for _ in 0..2 {
            assert!(
                !submit_queued_user_turn(id, "owner", None, &hash, true, "hello".into(), &[])
                    .await
                    .unwrap()
                    .replayed
            );
        }
    }
}
