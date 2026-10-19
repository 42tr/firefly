//! Conversation state, workspace metadata, settings, and deletion guards.
use chrono::{DateTime, Utc};
use serde_json::Value;
use sqlx::{query, query_as, query_scalar};

use super::{ConversationRecord, NewConversation};
use crate::db;

// Formatted SQL only inserts this fixed projection; all request values use bind parameters.
const CONVERSATION_COLUMNS: &str =
    "id, user_id, title, system_prompt, agent, codex_cwd, codex_thread_id, codex_provider, codex_model, codex_reasoning_level, claude_session_id, claude_model, claude_provider, claude_reasoning_level, created_at, updated_at, last_message_at, message_count, read_message_id, COALESCE((SELECT MAX(id) FROM assistant_messages WHERE conversation_id = assistant_conversations.id), 0) AS last_message_id, is_pinned, is_archived, workspace_mode, source_cwd, workspace_id, workspace_state, workspace_error, workspace_metadata";

pub async fn create_conversation(user_id: &str, conversation: NewConversation<'_>) -> anyhow::Result<ConversationRecord> {
    let NewConversation { system_prompt, title, agent, codex_cwd, provider, model, reasoning_level, parallel } = conversation;
    let pool = db::pool();
    let sql = format!(
        "INSERT INTO assistant_conversations (user_id, title, system_prompt, agent, codex_cwd, codex_provider, codex_model, codex_reasoning_level, workspace_mode, source_cwd, workspace_id, workspace_state, claude_model, claude_provider, claude_reasoning_level) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15) RETURNING {}",
        CONVERSATION_COLUMNS
    );
    let rec = query_as::<_, ConversationRecord>(sqlx::AssertSqlSafe(sql))
        .bind(user_id)
        .bind(title)
        .bind(system_prompt)
        .bind(agent)
        .bind(codex_cwd)
        .bind(if agent == "codex" { provider } else { None })
        .bind(if agent == "codex" { model } else { None })
        .bind(if agent == "codex" { reasoning_level } else { None })
        .bind(if parallel.is_some() { "parallel" } else { "shared" })
        .bind(parallel.map(|p| p.0))
        .bind(parallel.map(|p| p.1))
        .bind(if parallel.is_some() { "pending" } else { "ready" })
        .bind(if agent == "claude" { model } else { None })
        .bind(if agent == "claude" { provider } else { None })
        .bind(if agent == "claude" { reasoning_level } else { None })
        .fetch_one(pool)
        .await?;
    crate::assistant_events::notify(Some(user_id), "conversations");
    Ok(rec)
}

pub async fn set_workspace_state(
    id: i64, user_id: &str, state: &str, error: Option<&str>, metadata: Option<&Value>,
) -> anyhow::Result<()> {
    query("UPDATE assistant_conversations SET workspace_state=?3, workspace_error=?4, workspace_metadata=COALESCE(?5, workspace_metadata), updated_at=CAST(unixepoch('subsec') * 1000000 AS INTEGER) WHERE id=?1 AND user_id=?2 AND workspace_mode='parallel'")
        .bind(id).bind(user_id).bind(state).bind(error).bind(metadata).execute(db::pool()).await?;
    crate::assistant_events::notify(Some(user_id), "conversations");
    Ok(())
}

/// Retry only the turn that failed during workspace preparation, retaining its message/attachments.
pub async fn retry_workspace(id: i64, user_id: &str) -> anyhow::Result<bool> {
    retry_workspace_with_pool(db::pool(), id, user_id).await
}

pub(super) async fn retry_workspace_with_pool(pool: &sqlx::SqlitePool, id: i64, user_id: &str) -> anyhow::Result<bool> {
    let mut tx = pool.begin_with("BEGIN IMMEDIATE").await?;

    let job_id: Option<Option<String>> = query_scalar(
        "UPDATE assistant_conversations SET workspace_state=CASE WHEN workspace_state='missing' THEN 'ready' ELSE 'pending' END, workspace_error=NULL, updated_at=CAST(unixepoch('subsec') * 1000000 AS INTEGER)
         WHERE id=?1 AND user_id=?2 AND is_archived = FALSE AND workspace_mode='parallel' AND workspace_state IN ('failed','cancelled','missing')
         AND NOT EXISTS (SELECT 1 FROM assistant_generation_jobs WHERE conversation_id=?1 AND status='running')
         RETURNING workspace_metadata->>'job_id'"
    ).bind(id).bind(user_id).fetch_optional(&mut *tx).await?;
    if let Some(Some(job_id)) = &job_id {
        query("UPDATE assistant_generation_jobs SET status='queued', error_message=NULL, started_at=NULL, completed_at=NULL, updated_at=CAST(unixepoch('subsec') * 1000000 AS INTEGER) WHERE id=?1 AND conversation_id=?2 AND user_id=?3 AND status='failed'")
            .bind(uuid::Uuid::parse_str(job_id)?).bind(id).bind(user_id).execute(&mut *tx).await?;
        query("UPDATE assistant_messages SET metadata=json_remove(COALESCE(metadata, '{}'), '$.cancellation_requested') WHERE generation_job_id=?1 AND role='user'")
            .bind(uuid::Uuid::parse_str(job_id)?).execute(&mut *tx).await?;
    }
    tx.commit().await?;
    crate::assistant_events::notify(Some(user_id), "conversations");
    Ok(job_id.is_some())
}

pub async fn set_codex_thread_id(conversation_id: i64, user_id: &str, thread_id: &str) -> anyhow::Result<()> {
    db::retry_busy("set codex thread id", || async {
        query("UPDATE assistant_conversations SET codex_thread_id = ?3, updated_at = CAST(unixepoch('subsec') * 1000000 AS INTEGER) WHERE id = ?1 AND user_id = ?2")
            .bind(conversation_id)
            .bind(user_id)
            .bind(thread_id)
            .execute(db::pool())
            .await?;
        Ok(())
    })
    .await
}

pub async fn set_claude_session_id(conversation_id: i64, user_id: &str, session_id: &str) -> anyhow::Result<()> {
    db::retry_busy("set claude session id", || async {
        query(
            "UPDATE assistant_conversations SET claude_session_id = ?3, updated_at = CAST(unixepoch('subsec') * 1000000 AS INTEGER) WHERE id = ?1 AND user_id = ?2",
        )
        .bind(conversation_id)
        .bind(user_id)
        .bind(session_id)
        .execute(db::pool())
        .await?;
        Ok(())
    })
    .await
}

pub async fn get_conversation(conversation_id: i64, user_id: &str) -> anyhow::Result<Option<ConversationRecord>> {
    let pool = db::pool();
    let sql = format!(
        "SELECT {} FROM assistant_conversations WHERE id = ?1 AND user_id = ?2 AND is_archived = FALSE",
        CONVERSATION_COLUMNS
    );
    let rec = query_as::<_, ConversationRecord>(sqlx::AssertSqlSafe(sql))
        .bind(conversation_id)
        .bind(user_id)
        .fetch_optional(pool)
        .await?;
    Ok(rec)
}

pub async fn get_conversation_for_read(
    conversation_id: i64, user_id: &str,
) -> anyhow::Result<Option<ConversationRecord>> {
    let pool = db::pool();
    let sql = format!("SELECT {} FROM assistant_conversations WHERE id = ?1 AND user_id = ?2", CONVERSATION_COLUMNS);
    let rec = query_as::<_, ConversationRecord>(sqlx::AssertSqlSafe(sql))
        .bind(conversation_id)
        .bind(user_id)
        .fetch_optional(pool)
        .await?;
    Ok(rec)
}

pub async fn list_workspace_conversations(
    pool: &sqlx::SqlitePool, user_id: &str, archived: bool, limit: i64,
    before: Option<(bool, DateTime<Utc>, i64)>, search: &str,
) -> anyhow::Result<Vec<ConversationRecord>> {
    let sql = format!(
        "SELECT {} FROM assistant_conversations
         WHERE user_id = ?1 AND is_archived = ?7 AND message_count > 0
           AND (?3 IS NULL OR (is_pinned, last_message_at, id) < (?2, ?3, ?4))
           AND (?5 = '' OR instr(firefly_lower(concat_ws(' ', COALESCE(NULLIF(trim(title), ''), '对话 #' || id), codex_cwd, source_cwd, agent)), firefly_lower(?5)) > 0)
         ORDER BY is_pinned DESC, last_message_at DESC, id DESC LIMIT ?6",
        CONVERSATION_COLUMNS
    );
    Ok(query_as::<_, ConversationRecord>(sqlx::AssertSqlSafe(sql))
        .bind(user_id)
        .bind(before.map(|(pinned, _, _)| pinned))
        .bind(before.map(|(_, at, _)| at.timestamp_micros()))
        .bind(before.map(|(_, _, id)| id))
        .bind(search.trim())
        .bind(limit)
        .bind(archived)
        .fetch_all(pool)
        .await?)
}

pub async fn list_active_generation_conversation_ids(user_id: &str) -> anyhow::Result<Vec<i64>> {
    let rows = query_as::<_, (i64,)>(
        r#"
        SELECT conversation_id
        FROM assistant_generation_jobs
        WHERE user_id = ?1 AND status IN ('queued', 'running')
        "#,
    )
    .bind(user_id)
    .fetch_all(db::pool())
    .await?;
    Ok(rows.into_iter().map(|(conversation_id,)| conversation_id).collect())
}

pub async fn delete_conversation(conversation_id: i64, user_id: &str) -> anyhow::Result<Option<Vec<String>>> {
    delete_conversation_with_pool(db::pool(), conversation_id, user_id, None).await
}

#[derive(Debug, thiserror::Error)]
#[error("当前对话仍有生成任务，暂时无法删除")]
pub struct ConversationDeletionBlocked;

/// Recheck retention and active jobs while holding the SQLite writer lock, so
/// a conversation updated or queued since the scan cannot be deleted.
pub(crate) async fn delete_conversation_with_pool(
    pool: &sqlx::SqlitePool, conversation_id: i64, user_id: &str, updated_before: Option<DateTime<Utc>>,
) -> anyhow::Result<Option<Vec<String>>> {
    let mut tx = pool.begin_with("BEGIN IMMEDIATE").await?;

    // Also protect ordinary user deletion: an API-level precheck can race with
    // a queue claim.
    let execution_pending: bool = query_scalar(
        "SELECT EXISTS(SELECT 1 FROM assistant_conversations c WHERE c.id=?1 AND c.user_id=?2
           AND EXISTS(SELECT 1 FROM assistant_generation_jobs j WHERE j.conversation_id=c.id AND j.status='running'))",
    )
    .bind(conversation_id)
    .bind(user_id)
    .fetch_one(&mut *tx)
    .await?;
    if execution_pending {
        tx.rollback().await?;
        return if updated_before.is_some() { Ok(None) } else { Err(ConversationDeletionBlocked.into()) };
    }

    if let Some(cutoff) = updated_before {
        let eligible: bool = query_scalar(
            "SELECT EXISTS(SELECT 1 FROM assistant_conversations c
             WHERE c.id = ?1 AND c.user_id = ?2 AND c.updated_at <= ?3
               AND NOT EXISTS(SELECT 1 FROM assistant_generation_jobs j
                              WHERE j.conversation_id = c.id AND j.status IN ('queued', 'running')))",
        )
        .bind(conversation_id)
        .bind(user_id)
        .bind(cutoff.timestamp_micros())
        .fetch_one(&mut *tx)
        .await?;
        if !eligible || crate::assistant_job::global_registry().find_active_by_conversation(conversation_id).is_some() {
            tx.rollback().await?;
            return Ok(None);
        }
    }

    let storage_paths = sqlx::query_scalar::<_, String>(
        r#"
        SELECT a.storage_path
        FROM assistant_message_attachments a
        JOIN assistant_messages m ON m.id = a.message_id
        JOIN assistant_conversations c ON c.id = m.conversation_id
        WHERE c.id = ?1 AND c.user_id = ?2
        "#,
    )
    .bind(conversation_id)
    .bind(user_id)
    .fetch_all(&mut *tx)
    .await?;

    let result = query("DELETE FROM assistant_conversations WHERE id = ?1 AND user_id = ?2")
        .bind(conversation_id)
        .bind(user_id)
        .execute(&mut *tx)
        .await?;

    if result.rows_affected() == 0 {
        tx.rollback().await?;
        return Ok(None);
    }

    tx.commit().await?;
    crate::assistant_events::notify(Some(user_id), "conversations");
    Ok(Some(storage_paths))
}

#[derive(Debug, thiserror::Error)]
#[error("当前对话仍有生成任务，暂时无法归档")]
pub struct ConversationArchivalBlocked;

pub(super) const CONVERSATION_BUSY_SQL: &str =
    "SELECT EXISTS(SELECT 1 FROM assistant_generation_jobs WHERE conversation_id=?1 AND status IN ('queued','running'))";

#[derive(Debug, thiserror::Error)]
pub enum ConversationSettingsError {
    #[error("仅 Codex 对话支持修改模型设置")]
    NotCodex,
    #[error("仅 Claude 对话支持修改 Claude 模型设置")]
    NotClaude,
    #[error("请先取消归档，再修改模型设置")]
    Archived,
}

pub async fn set_codex_settings(
    conversation_id: i64, user_id: &str, provider: Option<&str>, model: Option<&str>, reasoning_level: Option<&str>,
) -> anyhow::Result<Option<ConversationRecord>> {
    // Serialize with submission: accepted messages retain their snapshots; later submissions use these settings.
    let mut tx = db::pool().begin_with("BEGIN IMMEDIATE").await?;
    let current: Option<(String, bool)> = query_as(
        "SELECT agent, is_archived FROM assistant_conversations WHERE id=?1 AND user_id=?2",
    ).bind(conversation_id).bind(user_id).fetch_optional(&mut *tx).await?;
    let Some((agent, archived)) = current else { return Ok(None) };
    if agent != "codex" { return Err(ConversationSettingsError::NotCodex.into()); }
    if archived { return Err(ConversationSettingsError::Archived.into()); }
    query("UPDATE assistant_conversations SET codex_provider=?3, codex_model=?4, codex_reasoning_level=?5,
           updated_at=CAST(unixepoch('subsec') * 1000000 AS INTEGER) WHERE id=?1 AND user_id=?2")
        .bind(conversation_id).bind(user_id).bind(provider).bind(model).bind(reasoning_level)
        .execute(&mut *tx).await?;
    let sql = format!("SELECT {CONVERSATION_COLUMNS} FROM assistant_conversations WHERE id=?1 AND user_id=?2");
    let record = query_as::<_, ConversationRecord>(sqlx::AssertSqlSafe(sql))
        .bind(conversation_id).bind(user_id).fetch_one(&mut *tx).await?;
    tx.commit().await?;
    crate::assistant_events::notify(Some(user_id), "conversations");
    Ok(Some(record))
}

pub async fn set_claude_settings(
    conversation_id: i64, user_id: &str, provider: Option<&str>, model: Option<&str>, reasoning_level: Option<&str>,
) -> anyhow::Result<Option<ConversationRecord>> {
    // Use the same writer transaction as message submission to protect queued snapshots.
    let mut tx = db::pool().begin_with("BEGIN IMMEDIATE").await?;
    let current: Option<(String, bool)> = query_as(
        "SELECT agent,is_archived FROM assistant_conversations WHERE id=?1 AND user_id=?2",
    ).bind(conversation_id).bind(user_id).fetch_optional(&mut *tx).await?;
    let Some((agent, archived)) = current else { return Ok(None) };
    if agent != "claude" { return Err(ConversationSettingsError::NotClaude.into()); }
    if archived { return Err(ConversationSettingsError::Archived.into()); }
    query("UPDATE assistant_conversations SET claude_model=?3, claude_provider=?4, claude_reasoning_level=?5,
           updated_at=CAST(unixepoch('subsec') * 1000000 AS INTEGER) WHERE id=?1 AND user_id=?2")
        .bind(conversation_id).bind(user_id).bind(model).bind(provider).bind(reasoning_level).execute(&mut *tx).await?;
    let sql = format!("SELECT {CONVERSATION_COLUMNS} FROM assistant_conversations WHERE id=?1 AND user_id=?2");
    let record = query_as::<_, ConversationRecord>(sqlx::AssertSqlSafe(sql))
        .bind(conversation_id).bind(user_id).fetch_one(&mut *tx).await?;
    tx.commit().await?;
    crate::assistant_events::notify(Some(user_id), "conversations");
    Ok(Some(record))
}

pub async fn set_conversation_archived(conversation_id: i64, user_id: &str, archived: bool) -> anyhow::Result<bool> {
    // Serialize with submission/retry transactions so a new turn cannot race archival.
    let mut tx = db::pool().begin_with("BEGIN IMMEDIATE").await?;
    let current: Option<bool> = query_scalar(
        "SELECT is_archived FROM assistant_conversations WHERE id=?1 AND user_id=?2",
    ).bind(conversation_id).bind(user_id).fetch_optional(&mut *tx).await?;
    let Some(current) = current else { return Ok(false) };
    if current == archived { return Ok(true) }
    if archived {
        let busy: bool = query_scalar(CONVERSATION_BUSY_SQL)
            .bind(conversation_id).fetch_one(&mut *tx).await?;
        if busy || crate::assistant_job::global_registry().find_active_by_conversation(conversation_id).is_some() {
            return Err(ConversationArchivalBlocked.into());
        }
    }
    query("UPDATE assistant_conversations SET is_archived=?3, updated_at=CAST(unixepoch('subsec') * 1000000 AS INTEGER) WHERE id=?1 AND user_id=?2")
        .bind(conversation_id).bind(user_id).bind(archived).execute(&mut *tx).await?;
    tx.commit().await?;
    crate::assistant_events::notify(Some(user_id), "conversations");
    Ok(true)
}

pub async fn set_conversation_pinned(conversation_id: i64, user_id: &str, pinned: bool) -> anyhow::Result<bool> {
    let result = query(
        "UPDATE assistant_conversations
         SET updated_at = CASE WHEN is_pinned <> ?3 THEN CAST(unixepoch('subsec') * 1000000 AS INTEGER) ELSE updated_at END,
             is_pinned = ?3
         WHERE id = ?1 AND user_id = ?2",
    )
    .bind(conversation_id)
    .bind(user_id)
    .bind(pinned)
    .execute(db::pool())
    .await?;
    if result.rows_affected() > 0 {
        crate::assistant_events::notify(Some(user_id), "conversations");
    }
    Ok(result.rows_affected() > 0)
}

pub async fn set_generated_conversation_title(
    conversation_id: i64, user_id: &str, original_title: Option<&str>, title: &str,
) -> anyhow::Result<bool> {
    let result = query(
        "UPDATE assistant_conversations
         SET title = ?3, updated_at = CAST(unixepoch('subsec') * 1000000 AS INTEGER)
         WHERE id = ?1 AND user_id = ?2 AND title IS ?4",
    )
    .bind(conversation_id)
    .bind(user_id)
    .bind(title)
    .bind(original_title)
    .execute(db::pool())
    .await?;
    if result.rows_affected() > 0 {
        crate::assistant_events::notify(Some(user_id), "conversations");
    }
    Ok(result.rows_affected() > 0)
}

pub async fn rename_conversation(conversation_id: i64, user_id: &str, title: &str) -> anyhow::Result<()> {
    let pool = db::pool();
    query(
        r#"
        UPDATE assistant_conversations
        SET title = ?3, updated_at = CAST(unixepoch('subsec') * 1000000 AS INTEGER)
        WHERE id = ?1 AND user_id = ?2
        "#,
    )
    .bind(conversation_id)
    .bind(user_id)
    .bind(title)
    .execute(pool)
    .await?;
    crate::assistant_events::notify(Some(user_id), "conversations");
    Ok(())
}
