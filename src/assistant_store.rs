use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::{query, query_as, query_scalar, FromRow, QueryBuilder, Sqlite, SqliteConnection};

use crate::db;

mod submissions;
pub use submissions::{enforce_submission_capacity, submit_queued_user_turn, SubmissionConflict};

mod weekly;
pub use weekly::fetch_weekly_agent_work;

mod generation;
#[cfg(test)]
pub use generation::CodexGenerationSettings;
pub use generation::{
    claim_next_queued_generation, delete_queued_generation, fetch_generation_context_messages,
    fetch_generation_user_message, find_active_generation, generation_requires_snapshot,
    get_generation_job, list_queued_generation_conversations, request_generation_cancellation,
    requeue_interrupted_generations, update_generation_status, GenerationJobRecord,
};
#[cfg(test)]
pub use generation::create_queued_user_turn;

pub mod schedules;

// Formatted SQL only inserts this fixed projection; all request values use bind parameters.
const CONVERSATION_COLUMNS: &str =
    "id, user_id, title, system_prompt, agent, codex_cwd, codex_thread_id, codex_provider, codex_model, codex_reasoning_level, claude_session_id, claude_model, claude_provider, claude_reasoning_level, created_at, updated_at, last_message_at, message_count, read_message_id, COALESCE((SELECT MAX(id) FROM assistant_messages WHERE conversation_id = assistant_conversations.id), 0) AS last_message_id, is_pinned, is_archived, workspace_mode, source_cwd, workspace_id, workspace_state, workspace_error, workspace_metadata";

#[derive(Debug, Clone, Serialize, FromRow)]
pub struct ConversationRecord {
    pub id: i64,
    #[serde(skip_serializing)]
    pub user_id: String,
    pub title: Option<String>,
    pub system_prompt: String,
    pub agent: String,
    pub codex_cwd: Option<String>,
    pub codex_thread_id: Option<String>,
    pub codex_provider: Option<String>,
    pub codex_model: Option<String>,
    pub codex_reasoning_level: Option<String>,
    pub claude_session_id: Option<String>,
    pub claude_model: Option<String>,
    pub claude_provider: Option<String>,
    pub claude_reasoning_level: Option<String>,
    #[sqlx(try_from = "crate::db::Timestamp")]
    pub created_at: DateTime<Utc>,
    #[sqlx(try_from = "crate::db::Timestamp")]
    pub updated_at: DateTime<Utc>,
    #[sqlx(try_from = "crate::db::Timestamp")]
    pub last_message_at: DateTime<Utc>,
    pub message_count: i64,
    pub read_message_id: i64,
    pub last_message_id: i64,
    pub is_pinned: bool,
    pub is_archived: bool,
    pub workspace_mode: String,
    pub source_cwd: Option<String>,
    pub workspace_id: Option<uuid::Uuid>,
    pub workspace_state: String,
    pub workspace_error: Option<String>,
    pub workspace_metadata: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct MessageRecord {
    pub id: i64,
    pub conversation_id: i64,
    pub role: String,
    pub content: String,
    pub tool_call_id: Option<String>,
    pub tool_calls: Option<Value>,
    pub metadata: Option<Value>,
    pub is_error: bool,
    #[sqlx(try_from = "crate::db::Timestamp")]
    pub created_at: DateTime<Utc>,
    pub generation_job_id: Option<uuid::Uuid>,
    pub processing_status: Option<String>,
    pub processing_error: Option<String>,
    #[serde(default)]
    #[sqlx(default, try_from = "crate::db::NullableTimestamp")]
    pub processing_started_at: Option<DateTime<Utc>>,
    #[serde(default)]
    #[sqlx(default, try_from = "crate::db::NullableTimestamp")]
    pub processing_completed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, FromRow)]
pub struct AttachmentRecord {
    pub id: i64,
    pub message_id: i64,
    pub file_name: String,
    pub content_type: String,
    pub file_size: i64,
    pub storage_path: String,
    #[sqlx(try_from = "crate::db::Timestamp")]
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredToolCall {
    pub id: String,
    pub name: String,
    pub arguments: Value,
}

#[derive(Debug, Clone)]
pub struct StoredMessage {
    pub role: String,
    pub content: String,
    pub tool_call_id: Option<String>,
    pub tool_calls: Option<Vec<StoredToolCall>>,
    pub metadata: Option<Value>,
    pub is_error: bool,
    pub generation_job_id: Option<uuid::Uuid>,
}

#[derive(Debug, Clone)]
pub struct NewAttachment {
    pub file_name: String,
    pub content_type: String,
    pub file_size: i64,
    pub storage_path: String,
}

pub struct NewConversation<'a> {
    pub system_prompt: &'a str,
    pub title: Option<&'a str>,
    pub agent: &'a str,
    pub codex_cwd: Option<&'a str>,
    pub provider: Option<&'a str>,
    pub model: Option<&'a str>,
    pub reasoning_level: Option<&'a str>,
    pub parallel: Option<(&'a str, uuid::Uuid)>,
}

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

async fn retry_workspace_with_pool(pool: &sqlx::SqlitePool, id: i64, user_id: &str) -> anyhow::Result<bool> {
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

#[derive(Debug, Clone)]
pub struct MessagePage {
    pub messages: Vec<MessageRecord>,
    pub has_more: bool,
}

/// Fetch one page of messages, newest first at the database level and restored
/// to chronological order for the chat UI. Returns `None` when the conversation
/// does not exist or does not belong to the user (callers map this to 404).
///
/// 分页按「问答气泡数」计而不是原始记录数：user 记录算 1 条；assistant 记录
/// 的文本算 1 条；工具调用/工具结果已收进前端的过程块，算 0 条。
/// 气泡预算只决定初始边界；再向前补齐涉及的用户提问，避免长任务的中间文本
/// 占满预算，压缩后页面只剩没有提问的执行摘要。排队消息和生成输出可能交错，
/// 因此补齐时按 job 找提问，并继续检查新纳入的区间，直到所有轮次完整。
/// The message id remains a stable cursor; adjacent pages do not overlap.
pub async fn fetch_messages_page(
    conversation_id: i64, user_id: &str, limit: i64, before_id: Option<i64>,
) -> anyhow::Result<Option<MessagePage>> {
    fetch_messages_page_with_pool(db::pool(), conversation_id, user_id, limit, before_id).await
}

const PAGE_QUESTION_SQL: &str = r#"
    SELECT MIN(id) FROM (
        SELECT (
            SELECT id FROM assistant_messages
            WHERE generation_job_id = jobs.generation_job_id AND role = 'user'
              AND conversation_id = ?1 AND id < ?2
        ) AS id
        FROM (
            SELECT DISTINCT generation_job_id FROM assistant_messages
            WHERE conversation_id = ?1 AND id BETWEEN ?2 AND ?3
              AND generation_job_id IS NOT NULL
        ) jobs
        UNION ALL
        SELECT MAX(id) FROM assistant_messages
        WHERE conversation_id = ?1 AND role = 'user' AND generation_job_id IS NULL
          AND id <= (
              SELECT MIN(id) FROM assistant_messages
              WHERE conversation_id = ?1 AND id BETWEEN ?2 AND ?3
                AND generation_job_id IS NULL
          )
    )
    "#;

async fn fetch_messages_page_with_pool(
    pool: &sqlx::SqlitePool, conversation_id: i64, user_id: &str, limit: i64, before_id: Option<i64>,
) -> anyhow::Result<Option<MessagePage>> {
    let upper_id = before_id.map(|id| id.saturating_sub(1)).unwrap_or(i64::MAX);
    let (conversation_exists, boundary) = sqlx::query_as::<_, (bool, Option<i64>)>(
        r#"
        WITH boundary AS (
            SELECT id
            FROM assistant_messages
            WHERE conversation_id = ?1
              AND id <= ?3
              AND (role = 'user' OR (role = 'assistant' AND trim(content) <> ''))
            ORDER BY id DESC
            LIMIT 1 OFFSET ?4
        )
        SELECT EXISTS(SELECT 1 FROM assistant_conversations WHERE id = ?1 AND user_id = ?2) AS conversation_exists,
               (SELECT id FROM boundary)
        "#,
    )
    .bind(conversation_id)
    .bind(user_id)
    .bind(upper_id)
    .bind(limit.max(0))
    .fetch_one(pool)
    .await?;
    if !conversation_exists {
        return Ok(None);
    }
    let mut lower_id = boundary.unwrap_or(i64::MIN);
    let mut scan_upper_id = upper_id;
    while lower_id != i64::MIN {
        // Only inspect the newly included range. Job-bound questions use the
        // generation-user index; legacy messages use their preceding question.
        let question_id: Option<i64> = sqlx::query_scalar(PAGE_QUESTION_SQL)
            .bind(conversation_id)
            .bind(lower_id)
            .bind(scan_upper_id)
            .fetch_one(pool)
            .await?;
        match question_id {
            Some(id) if id < lower_id => {
                scan_upper_id = lower_id - 1;
                lower_id = id;
            }
            _ => break,
        }
    }
    let meta_fut = sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS(SELECT 1 FROM assistant_messages WHERE conversation_id = ?1 AND id < ?2)",
    )
    .bind(conversation_id)
    .bind(lower_id)
    .fetch_one(pool);
    let rows_fut = query_as::<_, MessageRecord>(
        r#"
        SELECT m.id, m.conversation_id, m.role, m.content, m.tool_call_id, m.tool_calls,
               m.metadata, m.is_error, m.created_at, m.generation_job_id,
               j.status AS processing_status, j.error_message AS processing_error,
               j.started_at AS processing_started_at, j.completed_at AS processing_completed_at
        FROM assistant_messages m
        JOIN assistant_conversations c ON c.id = m.conversation_id
        LEFT JOIN assistant_generation_jobs j ON j.id = m.generation_job_id
        WHERE m.conversation_id = ?1 AND c.user_id = ?2
          AND m.id <= ?3
          AND m.id >= ?4
        ORDER BY m.id DESC
        "#,
    )
    .bind(conversation_id)
    .bind(user_id)
    .bind(upper_id)
    .bind(lower_id)
    .fetch_all(pool);

    let (has_more, mut rows) = tokio::try_join!(meta_fut, rows_fut)?;
    rows.reverse();
    Ok(Some(MessagePage { messages: rows, has_more }))
}

/// 取某个生成任务的全部消息记录（含工具输入/输出、handoff 子步骤等过程详情），
/// 供前端展开「执行过程」时按需加载。
pub async fn fetch_job_process_messages(
    conversation_id: i64, user_id: &str, job_id: Option<uuid::Uuid>, start_id: Option<i64>, end_id: Option<i64>,
) -> anyhow::Result<Vec<MessageRecord>> {
    let pool = db::pool();
    let rows = query_as::<_, MessageRecord>(
        r#"
        SELECT
            m.id,
            m.conversation_id,
            m.role,
            m.content,
            m.tool_call_id,
            m.tool_calls,
            m.metadata,
            m.is_error,
            m.created_at,
            m.generation_job_id,
            j.status AS processing_status,
            j.error_message AS processing_error,
            j.started_at AS processing_started_at,
            j.completed_at AS processing_completed_at
        FROM assistant_messages m
        JOIN assistant_conversations c ON c.id = m.conversation_id
        LEFT JOIN assistant_generation_jobs j ON j.id = m.generation_job_id
        WHERE m.conversation_id = ?1
          AND c.user_id = ?2
          AND (?3 IS NULL OR m.generation_job_id = ?3)
          AND (?4 IS NULL OR m.id >= ?4)
          AND (?5 IS NULL OR m.id <= ?5)
        ORDER BY m.id ASC
        "#,
    )
    .bind(conversation_id)
    .bind(user_id)
    .bind(job_id)
    .bind(start_id)
    .bind(end_id)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

pub async fn append_message(conversation_id: i64, message: StoredMessage) -> anyhow::Result<MessageRecord> {
    let (inserted, owner) = db::retry_busy("append assistant message", || async {
        let mut tx = db::pool().begin_with("BEGIN IMMEDIATE").await?;
        let inserted = insert_message(&mut tx, conversation_id, message.clone()).await?;
        let owner: String = query_scalar("SELECT user_id FROM assistant_conversations WHERE id = ?1")
            .bind(conversation_id)
            .fetch_one(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok((inserted, owner))
    })
    .await?;
    crate::assistant_events::notify(Some(&owner), "conversations");
    Ok(inserted)
}

async fn insert_message(
    conn: &mut SqliteConnection, conversation_id: i64, message: StoredMessage,
) -> anyhow::Result<MessageRecord> {
    let StoredMessage { role, content, tool_call_id, tool_calls, metadata, is_error, generation_job_id } = message;

    let tool_calls_json = match tool_calls {
        Some(calls) => Some(serde_json::to_value(calls)?),
        None => None,
    };

    const SQL: &str = "INSERT INTO assistant_messages (conversation_id, role, content, tool_call_id, tool_calls, metadata, is_error, generation_job_id) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8) RETURNING id, conversation_id, role, content, tool_call_id, tool_calls, metadata, is_error, created_at, generation_job_id, NULL AS processing_status, NULL AS processing_error";
    let inserted = query_as::<_, MessageRecord>(SQL)
        .bind(conversation_id)
        .bind(&role)
        .bind(&content)
        .bind(&tool_call_id)
        .bind(tool_calls_json)
        .bind(&metadata)
        .bind(is_error)
        .bind(generation_job_id)
        .fetch_one(&mut *conn)
        .await?;

    query(
        r#"
        UPDATE assistant_conversations
        SET updated_at = CAST(unixepoch('subsec') * 1000000 AS INTEGER), last_message_at = ?2, message_count = message_count + 1
        WHERE id = ?1
        "#,
    )
    .bind(conversation_id)
    .bind((inserted.created_at).timestamp_micros())
    .execute(&mut *conn)
    .await?;
    Ok(inserted)
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

const CONVERSATION_BUSY_SQL: &str =
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

pub async fn is_first_user_message(conversation_id: i64, message_id: i64) -> anyhow::Result<bool> {
    Ok(query_scalar::<_, Option<i64>>(
        "SELECT MIN(id) FROM assistant_messages WHERE conversation_id = ?1 AND role = 'user'",
    )
    .bind(conversation_id)
    .fetch_one(db::pool())
    .await?
        == Some(message_id))
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

pub async fn fetch_attachments_for_messages(
    conversation_id: i64, user_id: &str, message_ids: &[i64],
) -> anyhow::Result<Vec<AttachmentRecord>> {
    if message_ids.is_empty() {
        return Ok(Vec::new());
    }

    let pool = db::pool();
    let mut builder = QueryBuilder::<Sqlite>::new(
        r#"
        SELECT
            a.id,
            a.message_id,
            a.file_name,
            a.content_type,
            a.file_size,
            a.storage_path,
            a.created_at
        FROM assistant_message_attachments a
        JOIN assistant_messages m ON m.id = a.message_id
        JOIN assistant_conversations c ON c.id = m.conversation_id
        WHERE m.conversation_id = "#,
    );
    builder.push_bind(conversation_id).push(" AND c.user_id = ").push_bind(user_id).push(" AND m.id IN (");
    {
        let mut separated = builder.separated(", ");
        for message_id in message_ids {
            separated.push_bind(message_id);
        }
    }
    builder.push(") ORDER BY a.id ASC");
    let rows = builder.build_query_as::<AttachmentRecord>().fetch_all(pool).await?;
    Ok(rows)
}

pub async fn insert_attachments(
    message_id: i64, attachments: &[NewAttachment],
) -> anyhow::Result<Vec<AttachmentRecord>> {
    if attachments.is_empty() {
        return Ok(Vec::new());
    }

    db::retry_busy("insert assistant attachments", || async {
        let mut tx = db::pool().begin_with("BEGIN IMMEDIATE").await?;
        let records = insert_attachment_rows(&mut tx, message_id, attachments).await?;
        tx.commit().await?;
        Ok(records)
    })
    .await
}

async fn insert_attachment_rows(
    conn: &mut SqliteConnection, message_id: i64, attachments: &[NewAttachment],
) -> anyhow::Result<Vec<AttachmentRecord>> {
    if attachments.is_empty() {
        return Ok(Vec::new());
    }

    let mut query_builder = QueryBuilder::<Sqlite>::new(
        "INSERT INTO assistant_message_attachments (message_id, file_name, content_type, file_size, storage_path) ",
    );

    query_builder.push_values(attachments, |mut b, attachment| {
        b.push_bind(message_id)
            .push_bind(&attachment.file_name)
            .push_bind(&attachment.content_type)
            .push_bind(attachment.file_size)
            .push_bind(&attachment.storage_path);
    });

    query_builder.push(" RETURNING id, message_id, file_name, content_type, file_size, storage_path, created_at");

    Ok(query_builder.build_query_as::<AttachmentRecord>().fetch_all(&mut *conn).await?)
}

pub async fn get_attachment_for_user(attachment_id: i64, user_id: &str) -> anyhow::Result<Option<AttachmentRecord>> {
    let pool = db::pool();
    let record = query_as::<_, AttachmentRecord>(
        r#"
        SELECT
            a.id,
            a.message_id,
            a.file_name,
            a.content_type,
            a.file_size,
            a.storage_path,
            a.created_at
        FROM assistant_message_attachments a
        JOIN assistant_messages m ON m.id = a.message_id
        JOIN assistant_conversations c ON c.id = m.conversation_id
        WHERE a.id = ?1 AND c.user_id = ?2
        "#,
    )
    .bind(attachment_id)
    .bind(user_id)
    .fetch_optional(pool)
    .await?;
    Ok(record)
}

// 上传文件使用 UUID 作为磁盘文件名；始终通过所属会话校验访问权限。
pub async fn get_attachment_by_file_for_user(
    file_id: uuid::Uuid, user_id: &str,
) -> anyhow::Result<Option<AttachmentRecord>> {
    get_attachment_by_file_with_pool(db::pool(), file_id, user_id).await
}

async fn get_attachment_by_file_with_pool(
    pool: &sqlx::SqlitePool, file_id: uuid::Uuid, user_id: &str,
) -> anyhow::Result<Option<AttachmentRecord>> {
    let file_name = file_id.to_string();
    Ok(query_as::<_, AttachmentRecord>(
        r#"
        SELECT a.id, a.message_id, a.file_name, a.content_type,
               a.file_size, a.storage_path, a.created_at
        FROM assistant_message_attachments a
        JOIN assistant_messages m ON m.id = a.message_id
        JOIN assistant_conversations c ON c.id = m.conversation_id
        WHERE (a.storage_path = ?1 OR a.storage_path LIKE ?2) AND c.user_id = ?3
        LIMIT 1
        "#,
    )
    .bind(&file_name)
    .bind(format!("%/{file_name}"))
    .bind(user_id)
    .fetch_optional(pool)
    .await?)
}

#[cfg(test)]
mod paging_tests {
    use super::*;
    use super::generation::{claim_next_queued_generation_with_pool, create_queued_user_turn};
    use super::weekly::fetch_weekly_agent_work_with_pool;

    #[test]
    fn archival_checks_skip_completed_history() {
        let connection = crate::db::test_connection();
        connection.execute_batch("INSERT INTO assistant_conversations(id,user_id,system_prompt) VALUES (1,'owner','');
            WITH RECURSIVE n(i) AS (VALUES(1) UNION ALL SELECT i+1 FROM n WHERE i<20000)
            INSERT INTO assistant_generation_jobs(id,conversation_id,user_id,status)
            SELECT randomblob(16),1,'owner','completed' FROM n;
            WITH RECURSIVE n(i) AS (VALUES(1) UNION ALL SELECT i+1 FROM n WHERE i<20000)
            INSERT INTO assistant_messages(conversation_id,role,content,metadata)
            SELECT 1,'tool','output','{}' FROM n;").unwrap();
        for analyze in [false, true] {
            if analyze { connection.execute_batch("ANALYZE").unwrap(); }
            let mut statement = connection.prepare(CONVERSATION_BUSY_SQL).unwrap();
            let busy: bool = statement.query_row([1], |row| row.get(0)).unwrap();
            assert!(!busy);
            let steps = statement.get_status(rusqlite::StatementStatus::VmStep);
            assert!(steps < 100, "archival scanned history: {steps} steps");
        }
        connection.execute_batch("INSERT INTO assistant_generation_jobs(id,conversation_id,user_id,status)
            VALUES (zeroblob(16),1,'owner','queued');").unwrap();
        assert!(connection.query_row(CONVERSATION_BUSY_SQL, [1], |row| row.get::<_, bool>(0)).unwrap());
    }

    #[tokio::test]
    async fn generation_context_keeps_legacy_and_interleaved_job_order() {
        let pool = crate::db::test_pool().await;
        crate::db::set_test_pool(pool.clone());
        let conversation: i64 = query_scalar("INSERT INTO assistant_conversations(user_id,system_prompt) VALUES ('owner','') RETURNING id")
            .fetch_one(&pool).await.unwrap();
        let a = uuid::Uuid::from_u128(1);
        let b = uuid::Uuid::from_u128(2);
        let current = uuid::Uuid::from_u128(3);
        let queued = uuid::Uuid::from_u128(4);
        for (job, status, at) in [(a,"completed",10_i64),(b,"completed",10),(current,"running",20),(queued,"queued",30)] {
            query("INSERT INTO assistant_generation_jobs(id,conversation_id,user_id,status,created_at) VALUES (?1,?2,'owner',?3,?4)")
                .bind(job).bind(conversation).bind(status).bind(at).execute(&pool).await.unwrap();
        }
        let mut ids = Vec::new();
        for (job, at) in [(Some(b),1_i64),(None,200),(Some(current),2),(Some(a),3),
            (None,100),(Some(b),0),(Some(queued),4)] {
            let id: i64 = query_scalar("INSERT INTO assistant_messages(conversation_id,role,content,generation_job_id,created_at) VALUES (?1,'assistant','payload',?2,?3) RETURNING id")
                .bind(conversation).bind(job).bind(at).fetch_one(&pool).await.unwrap();
            ids.push(id);
        }
        let rows = fetch_generation_context_messages(conversation, "owner", current).await.unwrap();
        assert_eq!(rows.iter().map(|m| m.id).collect::<Vec<_>>(), [ids[4],ids[1],ids[3],ids[0],ids[5],ids[2]]);
        assert!(fetch_generation_context_messages(conversation, "other", current).await.unwrap().is_empty());
    }

    #[test]
    fn message_page_question_lookup_skips_job_history() {
        let connection = crate::db::test_connection();
        connection.execute_batch("INSERT INTO assistant_conversations(id,user_id,system_prompt) VALUES (1,'owner',''),(2,'other','');
            INSERT INTO assistant_generation_jobs(id,conversation_id,user_id,status) VALUES (zeroblob(16),1,'owner','completed');
            INSERT INTO assistant_messages(conversation_id,role,content,generation_job_id) VALUES (1,'user','legacy',NULL),(1,'user','modern',zeroblob(16));
            WITH RECURSIVE n(i) AS (VALUES(1) UNION ALL SELECT i+1 FROM n WHERE i<20000)
            INSERT INTO assistant_messages(conversation_id,role,content,generation_job_id)
            SELECT 1,'assistant','modern output',zeroblob(16) FROM n;
            INSERT INTO assistant_messages(conversation_id,role,content) VALUES (2,'user','other question'),(1,'assistant','legacy reply');").unwrap();
        for analyze in [false, true] {
            if analyze { connection.execute_batch("ANALYZE").unwrap(); }
            let mut statement = connection.prepare(PAGE_QUESTION_SQL).unwrap();
            let question: i64 = statement.query_row([1, 20002, 20004], |row| row.get(0)).unwrap();
            assert_eq!(question, 1);
            let steps = statement.get_status(rusqlite::StatementStatus::VmStep);
            assert!(steps < 1000, "question lookup walked modern history: {steps} steps");
            let mut plan = connection.prepare(&format!("EXPLAIN QUERY PLAN {PAGE_QUESTION_SQL}")).unwrap();
            let details: Vec<String> = plan.query_map([1, 20002, 20004], |row| row.get(3))
                .unwrap().collect::<Result<_, _>>().unwrap();
            assert_eq!(details.iter().filter(|detail| detail.contains("COVERING INDEX idx_assistant_messages_conversation_id")).count(), 2, "{details:?}");
        }
        // No preceding legacy question must also be a cheap miss; the job's
        // question still completes the page and other conversations stay out.
        connection.execute("DELETE FROM assistant_messages WHERE id=1", []).unwrap();
        let mut statement = connection.prepare(PAGE_QUESTION_SQL).unwrap();
        let question: i64 = statement.query_row([1, 20002, 20004], |row| row.get(0)).unwrap();
        assert_eq!(question, 2);
        assert!(statement.get_status(rusqlite::StatementStatus::VmStep) < 1000);
    }

    #[tokio::test]
    async fn attachment_file_lookup_supports_storage_paths_and_checks_owner() {
        let pool = crate::db::test_pool().await;
        let conversation: i64 =
            query_scalar("INSERT INTO assistant_conversations(user_id,system_prompt) VALUES ('owner','') RETURNING id")
                .fetch_one(&pool).await.unwrap();
        let message: i64 = query_scalar(
            "INSERT INTO assistant_messages(conversation_id,role,content) VALUES (?1,'user','image') RETURNING id",
        ).bind(conversation).fetch_one(&pool).await.unwrap();
        for prefix in ["/srv/firefly/storage/assistant_uploads/", "storage/assistant_uploads/", "custom/uploads/", ""] {
            let file_id = uuid::Uuid::new_v4();
            let storage_path = format!("{prefix}{file_id}");
            query("INSERT INTO assistant_message_attachments(message_id,file_name,content_type,file_size,storage_path) VALUES (?1,'图片.png','image/png',4,?2)")
                .bind(message).bind(&storage_path).execute(&pool).await.unwrap();
            let record = get_attachment_by_file_with_pool(&pool, file_id, "owner").await.unwrap().unwrap();
            assert_eq!(record.storage_path, storage_path);
            assert_eq!(record.content_type, "image/png");
            assert!(get_attachment_by_file_with_pool(&pool, file_id, "other").await.unwrap().is_none());
        }
        assert!(get_attachment_by_file_with_pool(&pool, uuid::Uuid::new_v4(), "owner").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn db_tests_conversation_deletion_blocks_running_jobs_and_serializes_with_claims() {
        let pool = crate::db::test_pool().await;
        crate::db::set_test_pool(pool.clone());
        let conversation: i64 = query_scalar("INSERT INTO assistant_conversations(user_id,agent,system_prompt) VALUES ('owner','claude','') RETURNING id")
            .fetch_one(&pool).await.unwrap();
        let job = uuid::Uuid::new_v4();
        create_queued_user_turn(job, conversation, "owner", true, "hello".into(), &[]).await.unwrap();
        claim_next_queued_generation(conversation, "owner").await.unwrap().unwrap();
        assert!(delete_conversation(conversation, "owner").await.unwrap_err().is::<ConversationDeletionBlocked>());
        assert!(delete_conversation(conversation, "other").await.unwrap().is_none());
        update_generation_status(job, "failed", Some("stopped")).await.unwrap();
        assert!(delete_conversation(conversation, "owner").await.unwrap().is_some());

        // Pure queued turns can still be deleted, and deletion/claim compete
        // under the same writer lock so they can never both succeed.
        let queued: i64 =
            query_scalar("INSERT INTO assistant_conversations(user_id,system_prompt) VALUES ('owner','') RETURNING id")
                .fetch_one(&pool)
                .await
                .unwrap();
        create_queued_user_turn(uuid::Uuid::new_v4(), queued, "owner", true, "queued".into(), &[]).await.unwrap();
        assert!(delete_conversation(queued, "owner").await.unwrap().is_some());
        let competing: i64 =
            query_scalar("INSERT INTO assistant_conversations(user_id,system_prompt) VALUES ('owner','') RETURNING id")
                .fetch_one(&pool)
                .await
                .unwrap();
        create_queued_user_turn(uuid::Uuid::new_v4(), competing, "owner", true, "race".into(), &[]).await.unwrap();
        let (claimed, deleted) =
            tokio::join!(claim_next_queued_generation(competing, "owner"), delete_conversation(competing, "owner"),);
        match (claimed.unwrap(), deleted) {
            (Some(_), Err(error)) => assert!(error.is::<ConversationDeletionBlocked>()),
            (None, Ok(Some(_))) => {}
            result => panic!("claim/delete must serialize: {result:?}"),
        }
    }

    #[tokio::test]
    async fn db_tests_conversation_reasoning_survives_reload() {
        let pool = crate::db::test_pool().await;
        crate::db::set_test_pool(pool.clone());
        let workspace_id = uuid::Uuid::new_v4();
        let conversation = create_conversation("owner", crate::assistant_store::NewConversation {
            system_prompt: "",
            title: None,
            agent: "codex",
            codex_cwd: Some("project"),
            provider: Some("relay"),
            model: Some("model-a"),
            reasoning_level: Some("xhigh"),
            parallel: Some(("source", workspace_id)),
        })
        .await
        .unwrap();
        let saved = get_conversation(conversation.id, "owner").await.unwrap().unwrap();
        assert_eq!(saved.codex_reasoning_level.as_deref(), Some("xhigh"));
        assert_eq!(saved.codex_model.as_deref(), Some("model-a"));
        assert_eq!(saved.workspace_mode, "parallel");
        assert_eq!(saved.source_cwd.as_deref(), Some("source"));
        assert_eq!(saved.workspace_id, Some(workspace_id));
        let legacy: i64 =
            query_scalar("INSERT INTO assistant_conversations(user_id,system_prompt) VALUES ('owner','') RETURNING id")
                .fetch_one(&pool)
                .await
                .unwrap();
        let saved = get_conversation(legacy, "owner").await.unwrap().unwrap();
        assert_eq!(saved.codex_reasoning_level, None);
    }

    #[tokio::test]
    async fn db_tests_empty_assistant_records_do_not_complete_interrupted_turns() {
        let pool = crate::db::test_pool().await;
        crate::db::set_test_pool(pool.clone());
        let conversation: i64 = query_scalar(
            "INSERT INTO assistant_conversations(user_id,agent,system_prompt) VALUES ('owner','firefly','') RETURNING id",
        ).fetch_one(&pool).await.unwrap();
        let job = uuid::Uuid::new_v4();
        create_queued_user_turn(job, conversation, "owner", false, "question".into(), &[]).await.unwrap();
        claim_next_queued_generation(conversation, "owner").await.unwrap().unwrap();
        query("INSERT INTO assistant_messages(conversation_id,generation_job_id,role,content,metadata) VALUES (?1,?2,'assistant','','{}')")
            .bind(conversation).bind(job).execute(&pool).await.unwrap();
        let recovery = requeue_interrupted_generations().await.unwrap();
        assert_eq!((recovery.requeued, recovery.completed, recovery.failed), (1, 0, 0));
        assert_eq!(get_generation_job(job, "owner").await.unwrap().unwrap().status, "queued");
    }

    #[tokio::test]
    async fn db_tests_generation_input_and_restart_recovery() {
        let pool = crate::db::test_pool().await;
        crate::db::set_test_pool(pool.clone());
        let conversation: i64 = query_scalar(
            "INSERT INTO assistant_conversations(user_id,agent,system_prompt) VALUES ('owner','codex','') RETURNING id",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        let job = uuid::Uuid::new_v4();
        query(
            "INSERT INTO assistant_generation_jobs(id,conversation_id,user_id,status) VALUES (?1,?2,'owner','running')",
        )
        .bind(job)
        .bind(conversation)
        .execute(&pool)
        .await
        .unwrap();
        let original = "继续处理原任务\n保留 '引用' 与已有文件";
        let message_id: i64 = query_scalar("INSERT INTO assistant_messages(conversation_id,role,content,generation_job_id) VALUES (?1,'user',?2,?3) RETURNING id")
            .bind(conversation).bind(original).bind(job).fetch_one(&pool).await.unwrap();
        let fresh = fetch_generation_user_message(job, conversation, "owner").await.unwrap().unwrap();
        assert_eq!(fresh.content, original);
        assert_eq!(fresh.processing_status.as_deref(), Some("running"));
        assert!(fetch_generation_user_message(job, conversation, "other").await.unwrap().is_none());
        assert!(fetch_generation_user_message(job, conversation + 1, "owner").await.unwrap().is_none());
        assert!(fetch_generation_user_message(uuid::Uuid::new_v4(), conversation, "owner").await.unwrap().is_none());

        let recovery = requeue_interrupted_generations().await.unwrap();
        assert_eq!((recovery.requeued, recovery.completed, recovery.failed), (1, 0, 0));
        let hint = "\n\n[服务重启恢复] 请从原会话的最新进度继续完成任务，保留并检查已经完成的工作，不要从头重复执行。";
        for _ in 0..2 {
            let restored = fetch_generation_user_message(job, conversation, "owner").await.unwrap().unwrap();
            assert_eq!(restored.content, format!("{original}{hint}"));
            assert_eq!(restored.processing_status.as_deref(), Some("queued"));
            assert_eq!(restored.metadata.unwrap()["restart_recovery"], true);
        }
        assert_eq!(requeue_interrupted_generations().await.unwrap().requeued, 0);
        // Preserve PostgreSQL's previous handling of a string "true", while
        // false, null, and numeric flags must not gain a recovery instruction.
        for (flag, recovering) in [
            (serde_json::json!("true"), true),
            (serde_json::json!(false), false),
            (serde_json::json!("false"), false),
            (serde_json::json!(null), false),
            (serde_json::json!(1), false),
        ] {
            query("UPDATE assistant_messages SET metadata=?1 WHERE id=?2")
                .bind(serde_json::json!({"restart_recovery":flag}))
                .bind(message_id)
                .execute(&pool)
                .await
                .unwrap();
            let loaded = fetch_generation_user_message(job, conversation, "owner").await.unwrap().unwrap();
            assert_eq!(loaded.content, if recovering { format!("{original}{hint}") } else { original.into() });
        }
        let persisted: String = query_scalar("SELECT content FROM assistant_messages WHERE id=?1")
            .bind(message_id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(persisted, original);
    }

    #[tokio::test]
    async fn db_tests_failed_turn_context_preserves_tool_facts_and_closes_unknown_results() {
        let pool = crate::db::test_pool().await;
        crate::db::set_test_pool(pool.clone());
        let conversation: i64 =
            query_scalar("INSERT INTO assistant_conversations(user_id,system_prompt) VALUES ('owner','') RETURNING id")
                .fetch_one(&pool)
                .await
                .unwrap();
        let failed = uuid::Uuid::new_v4();
        let without_effects = uuid::Uuid::new_v4();
        let current = uuid::Uuid::new_v4();
        for (id, timestamp) in [(failed, 1_i64), (without_effects, 2), (current, 3)] {
            create_queued_user_turn(id, conversation, "owner", true, "original request".into(), &[]).await.unwrap();
            query("UPDATE assistant_generation_jobs SET status=?2, created_at=?3 WHERE id=?1")
                .bind(id)
                .bind(if id == current { "running" } else { "failed" })
                .bind(timestamp)
                .execute(&pool)
                .await
                .unwrap();
        }
        let calls = serde_json::json!([
            {"id":"known", "name":"write_file", "arguments":{}},
            {"id":"unknown", "name":"send_request", "arguments":{}},
        ]);
        query("INSERT INTO assistant_messages(conversation_id,role,content,tool_calls,generation_job_id) VALUES (?1,'assistant','partial prose',?2,?3)")
            .bind(conversation).bind(calls).bind(failed).execute(&pool).await.unwrap();
        query("INSERT INTO assistant_messages(conversation_id,role,content,tool_call_id,generation_job_id) VALUES (?1,'tool','file was written','known',?2)")
            .bind(conversation).bind(failed).execute(&pool).await.unwrap();
        query("INSERT INTO assistant_messages(conversation_id,role,content,generation_job_id) VALUES (?1,'assistant','unfinished final prose',?2)")
            .bind(conversation).bind(failed).execute(&pool).await.unwrap();
        let context = fetch_generation_context_messages(conversation, "owner", current).await.unwrap();
        assert_eq!(
            context.iter().map(|row| row.role.as_str()).collect::<Vec<_>>(),
            ["user", "assistant", "tool", "tool", "user"]
        );
        assert!(context[0].content.contains("不是重新执行原请求的指令"));
        assert!(context[1].content.is_empty());
        assert_eq!(context[2].content, "file was written");
        assert_eq!(context[3].tool_call_id.as_deref(), Some("unknown"));
        assert!(context[3].is_error);
        assert!(context[3].content.contains("interrupted_unknown_outcome"));
        assert_eq!(context[4].generation_job_id, Some(current));
        assert!(context.iter().all(|row| row.generation_job_id != Some(without_effects)));
        assert_eq!(
            query_scalar::<_, i64>("SELECT COUNT(*) FROM assistant_messages WHERE tool_call_id='unknown'")
                .fetch_one(&pool)
                .await
                .unwrap(),
            0
        );
    }

    #[tokio::test]
    async fn db_tests_queued_user_turn_is_atomic() {
        let pool = crate::db::test_pool().await;
        crate::db::set_test_pool(pool.clone());
        let conversation: i64 =
            query_scalar("INSERT INTO assistant_conversations(user_id,system_prompt) VALUES ('owner','') RETURNING id")
                .fetch_one(&pool)
                .await
                .unwrap();
        let attachment = NewAttachment {
            file_name: "a.txt".into(),
            content_type: "text/plain".into(),
            file_size: 3,
            storage_path: "stored/a.txt".into(),
        };
        let job = uuid::Uuid::new_v4();
        let (message, attachments) =
            create_queued_user_turn(job, conversation, "owner", true, "hi".into(), std::slice::from_ref(&attachment))
                .await
                .unwrap();
        assert_eq!(message.generation_job_id, Some(job));
        assert_eq!(attachments.len(), 1);
        assert_eq!(attachments[0].message_id, message.id);
        let claimed = claim_next_queued_generation_with_pool(&pool, conversation, "owner").await.unwrap().unwrap();
        assert_eq!(claimed.id, job);
        let count: i64 = query_scalar("SELECT message_count FROM assistant_conversations WHERE id=?1")
            .bind(conversation)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(count, 1);

        // A failed attachment insert must not leave a claimable job or an orphan message behind.
        query("CREATE TRIGGER reject_attachment BEFORE INSERT ON assistant_message_attachments BEGIN SELECT RAISE(ABORT, 'rejected'); END")
            .execute(&pool)
            .await
            .unwrap();
        query("UPDATE assistant_generation_jobs SET status='completed' WHERE id=?1")
            .bind(job)
            .execute(&pool)
            .await
            .unwrap();
        let failed = uuid::Uuid::new_v4();
        assert!(create_queued_user_turn(failed, conversation, "owner", true, "again".into(), &[attachment])
            .await
            .is_err());
        assert!(claim_next_queued_generation_with_pool(&pool, conversation, "owner").await.unwrap().is_none());
        let messages: i64 = query_scalar("SELECT COUNT(*) FROM assistant_messages WHERE conversation_id=?1")
            .bind(conversation)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(messages, 1);
    }

    #[tokio::test]
    async fn db_tests_competing_claims_and_recovery() {
        let pool = crate::db::test_pool().await;
        crate::db::set_test_pool(pool.clone());
        let conversation: i64 =
            query_scalar("INSERT INTO assistant_conversations(user_id,system_prompt) VALUES ('owner','') RETURNING id")
                .fetch_one(&pool)
                .await
                .unwrap();
        let first = uuid::Uuid::new_v4();
        let second = uuid::Uuid::new_v4();
        for (job, at) in [(first, 1i64), (second, 2)] {
            query("INSERT INTO assistant_generation_jobs(id,conversation_id,user_id,status,created_at) VALUES (?1,?2,'owner','queued',?3)").bind(job).bind(conversation).bind(at).execute(&pool).await.unwrap();
        }
        let claims = futures_util::future::join_all(
            (0..12).map(|_| claim_next_queued_generation_with_pool(&pool, conversation, "owner")),
        )
        .await;
        let claimed: Vec<_> = claims.into_iter().filter_map(Result::unwrap).collect();
        assert_eq!(claimed.len(), 1);
        assert_eq!(claimed[0].id, first);
        assert!(claim_next_queued_generation_with_pool(&pool, conversation, "other").await.unwrap().is_none());
        query("UPDATE assistant_generation_jobs SET status='completed' WHERE id=?1")
            .bind(first)
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(
            claim_next_queued_generation_with_pool(&pool, conversation, "owner").await.unwrap().unwrap().id,
            second
        );
    }

    #[tokio::test]
    async fn db_tests_conversation_pages() {
        let pool = crate::db::test_pool().await;
        let at = Utc::now();
        let mut ids = Vec::new();
        for (owner, agent, count) in [
            ("owner", "codex", 1),
            ("owner", "claude", 1),
            ("owner", "codex", 1),
            ("other", "codex", 1),
            ("owner", "codex", 1),
            ("owner", "firefly", 1),
            ("owner", "codex", 0),
        ] {
            let id: i64 = query_scalar(
                "INSERT INTO assistant_conversations
                 (user_id, agent, system_prompt, message_count, last_message_at, source_cwd)
                 VALUES (?1, ?2, '', ?3, ?4, 'project_100%') RETURNING id",
            )
            .bind(owner)
            .bind(agent)
            .bind(count as i64)
            .bind((at).timestamp_micros())
            .fetch_one(&pool)
            .await
            .unwrap();
            ids.push(id);
        }
        let first = list_workspace_conversations(&pool, "owner", false, 2, None, "").await.unwrap();
        assert_eq!(first.iter().map(|r| r.id).collect::<Vec<_>>(), vec![ids[5], ids[4]]);
        // Removing the boundary record cannot shift the next page or invalidate its cursor.
        query("DELETE FROM assistant_conversations WHERE id = ?1").bind(ids[4]).execute(&pool).await.unwrap();
        let second = list_workspace_conversations(&pool, "owner", false, 2, Some((false, at, ids[4])), "").await.unwrap();
        assert_eq!(second.iter().map(|r| r.id).collect::<Vec<_>>(), vec![ids[2], ids[1]]);
        let third = list_workspace_conversations(&pool, "owner", false, 2, Some((false, at, ids[1])), "").await.unwrap();
        assert_eq!(third.iter().map(|r| r.id).collect::<Vec<_>>(), vec![ids[0]]);
        let end = list_workspace_conversations(&pool, "owner", false, 2, Some((false, at, ids[0])), "").await.unwrap();
        assert!(end.is_empty());
        let matched = list_workspace_conversations(&pool, "owner", false, 1, None, " PROJECT_100% ").await.unwrap();
        assert_eq!(matched[0].id, ids[5]);
        assert!(list_workspace_conversations(&pool, "owner", false, 2, None, "missing%").await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn db_tests_parallel_workspace_queue_retry_and_ownership() {
        let pool = crate::db::test_pool().await;
        let workspace_id = uuid::Uuid::new_v4();
        let id: i64 = query_scalar("INSERT INTO assistant_conversations (user_id, agent, system_prompt, codex_cwd, workspace_mode, source_cwd, workspace_id, workspace_state) VALUES ('owner','codex','','.assistant-parallel/test/work','parallel','project',?1,'pending') RETURNING id")
            .bind(workspace_id).fetch_one(&pool).await.unwrap();
        let first = uuid::Uuid::new_v4();
        let second = uuid::Uuid::new_v4();
        for (job, seconds) in [(first, 2), (second, 1)] {
            query("INSERT INTO assistant_generation_jobs (id,conversation_id,user_id,status,enable_tools,created_at) VALUES (?1,?2,'owner','queued',true,CAST(unixepoch('subsec') * 1000000 AS INTEGER)-CAST(?3 * 1000000 AS INTEGER))")
                .bind(job).bind(id).bind(seconds as f64).execute(&pool).await.unwrap();
            query("INSERT INTO assistant_messages (conversation_id,role,content,generation_job_id) VALUES (?1,'user','retained question',?2)")
                .bind(id).bind(job).execute(&pool).await.unwrap();
        }
        assert!(claim_next_queued_generation_with_pool(&pool, id, "other").await.unwrap().is_none());
        assert_eq!(claim_next_queued_generation_with_pool(&pool, id, "owner").await.unwrap().unwrap().id, first);
        assert!(claim_next_queued_generation_with_pool(&pool, id, "owner").await.unwrap().is_none());
        query("UPDATE assistant_conversations SET workspace_state='failed', workspace_metadata=?2 WHERE id=?1")
            .bind(id)
            .bind(serde_json::json!({"job_id":first}))
            .execute(&pool)
            .await
            .unwrap();
        assert!(!retry_workspace_with_pool(&pool, id, "owner").await.unwrap());
        query("UPDATE assistant_generation_jobs SET status='failed' WHERE id=?1")
            .bind(first)
            .execute(&pool)
            .await
            .unwrap();
        query("UPDATE assistant_messages SET metadata=json('{\"cancellation_requested\":true}') WHERE generation_job_id=?1")
            .bind(first).execute(&pool).await.unwrap();
        for state in ["failed", "cancelled", "missing"] {
            query("UPDATE assistant_conversations SET workspace_state=?2 WHERE id=?1")
                .bind(id)
                .bind(state)
                .execute(&pool)
                .await
                .unwrap();
            assert!(claim_next_queued_generation_with_pool(&pool, id, "owner").await.unwrap().is_none());
        }
        assert!(!retry_workspace_with_pool(&pool, id, "other").await.unwrap());
        // Missing workspaces can only be checked again: they never return to pending/reset.
        assert!(retry_workspace_with_pool(&pool, id, "owner").await.unwrap());
        assert!(!query_scalar::<_, bool>("SELECT EXISTS(SELECT 1 FROM assistant_messages WHERE generation_job_id=?1 AND json_extract(metadata, '$.cancellation_requested')=1)")
            .bind(first).fetch_one(&pool).await.unwrap());
        let state: String = query_scalar("SELECT workspace_state FROM assistant_conversations WHERE id=?1")
            .bind(id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(state, "ready");
        assert_eq!(claim_next_queued_generation_with_pool(&pool, id, "owner").await.unwrap().unwrap().id, first);
        query("UPDATE assistant_generation_jobs SET status='failed' WHERE id=?1")
            .bind(first)
            .execute(&pool)
            .await
            .unwrap();
        query("UPDATE assistant_conversations SET workspace_state='cancelled' WHERE id=?1")
            .bind(id)
            .execute(&pool)
            .await
            .unwrap();
        assert!(retry_workspace_with_pool(&pool, id, "owner").await.unwrap());
        let state: String = query_scalar("SELECT workspace_state FROM assistant_conversations WHERE id=?1")
            .bind(id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(state, "pending");
        let count: i64 = query_scalar("SELECT COUNT(*) FROM assistant_messages WHERE conversation_id=?1")
            .bind(id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(count, 2);
        query("UPDATE assistant_conversations SET workspace_state='failed' WHERE id=?1")
            .bind(id)
            .execute(&pool)
            .await
            .unwrap();
        assert!(!retry_workspace_with_pool(&pool, id, "other").await.unwrap());
        let shared: (String, String) = sqlx::query_as("INSERT INTO assistant_conversations (user_id,system_prompt) VALUES ('legacy','') RETURNING workspace_mode,workspace_state")
            .fetch_one(&pool).await.unwrap();
        assert_eq!(shared, ("shared".into(), "ready".into()));
    }

    #[tokio::test]
    async fn db_tests_weekly_batch_preserves_scope_and_message_order() {
        let pool = crate::db::test_pool().await;
        let since = DateTime::parse_from_rfc3339("2026-09-07T00:00:00Z").unwrap().with_timezone(&Utc);
        let mut ids = Vec::new();
        for (owner, agent) in [
            ("owner", "codex"),
            ("owner", "claude"),
            ("other", "codex"),
            ("owner", "firefly"),
            ("owner", "codex"),
        ] {
            let id: i64 = sqlx::query_scalar(
                "INSERT INTO assistant_conversations (user_id, agent, system_prompt, last_message_at)
                 VALUES (?1, ?2, '', ?3) RETURNING id",
            )
            .bind(owner)
            .bind(agent)
            .bind((since).timestamp_micros())
            .fetch_one(&pool)
            .await
            .unwrap();
            ids.push(id);
        }
        for id in &ids {
            for (role, content, days) in [
                ("user", "outside week", -1),
                ("assistant", "older reply", -1),
                ("user", "first question", 0),
                ("user", "second question", 1),
                ("assistant", "latest reply", 1),
                ("assistant", "   ", 2),
                ("user", "  ", 2),
            ] {
                sqlx::query("INSERT INTO assistant_messages (conversation_id, role, content, created_at) VALUES (?1, ?2, ?3, ?4)")
                    .bind(id).bind(role).bind(content).bind((since + chrono::Duration::days(days)).timestamp_micros())
                    .execute(&pool).await.unwrap();
            }
        }
        // A conversation's latest reply may predate this week.
        sqlx::query("DELETE FROM assistant_messages WHERE conversation_id = ?1 AND created_at >= ?2")
            .bind(ids[1])
            .bind((since).timestamp_micros())
            .execute(&pool)
            .await
            .unwrap();
        // Empty/tool-only conversations remain omitted.
        sqlx::query("UPDATE assistant_messages SET role = 'tool' WHERE conversation_id = ?1")
            .bind(ids[4])
            .execute(&pool)
            .await
            .unwrap();
        let records = fetch_weekly_agent_work_with_pool(&pool, "owner", since).await.unwrap();
        assert_eq!(records.iter().map(|r| r.conversation_id).collect::<Vec<_>>(), vec![ids[0], ids[1]]);
        assert_eq!(records[0].user_messages, ["first question", "second question"]);
        assert_eq!(records[0].last_assistant_reply.as_deref(), Some("latest reply"));
        assert!(records[1].user_messages.is_empty());
        assert_eq!(records[1].last_assistant_reply.as_deref(), Some("older reply"));
        assert!(fetch_weekly_agent_work_with_pool(&pool, "missing", since).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn db_tests_message_page_completes_bubble_boundaries() {
        let pool = crate::db::test_pool().await;
        let conversation: i64 = sqlx::query_scalar(
            "INSERT INTO assistant_conversations (user_id, system_prompt) VALUES ('owner', '') RETURNING id",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        sqlx::query(
            "WITH RECURSIVE series(i) AS (VALUES(1) UNION ALL SELECT i+1 FROM series WHERE i<4000) INSERT INTO assistant_messages (conversation_id, role, content)
            SELECT ?1, CASE WHEN i % 4 = 1 THEN 'user' WHEN i % 4 = 3 THEN 'tool' ELSE 'assistant' END,
                CASE WHEN i % 4 = 2 THEN '' ELSE 'message message message' END
            FROM series ORDER BY i",
        )
        .bind(conversation)
        .execute(&pool)
        .await
        .unwrap();
        for before in [None, Some(3900), Some(4), Some(i64::MIN)] {
            for limit in [0, 1, 2, 20] {
                // The bubble budget selects a suffix, then the fixture's
                // four-record turns must be completed back to their question.
                let budget_ids: Vec<i64> = sqlx::query_scalar("WITH weighted AS (
                    SELECT id, CASE WHEN role = 'user' OR (role = 'assistant' AND trim(content) <> '') THEN 1 ELSE 0 END AS weight
                    FROM assistant_messages WHERE conversation_id = ?1 AND (?2 IS NULL OR id < ?2)
                ), counted AS (SELECT *, SUM(weight) OVER (ORDER BY id DESC) AS total FROM weighted)
                SELECT id FROM counted WHERE total - weight <= ?3 ORDER BY id")
                    .bind(conversation).bind(before).bind(limit).fetch_all(&pool).await.unwrap();
                let expected: Vec<i64> = match (budget_ids.first(), budget_ids.last()) {
                    (Some(&first), Some(&last)) => (((first - 1) / 4 * 4 + 1)..=last).collect(),
                    _ => Vec::new(),
                };
                let page =
                    fetch_messages_page_with_pool(&pool, conversation, "owner", limit, before).await.unwrap().unwrap();
                assert_eq!(page.messages.iter().map(|m| m.id).collect::<Vec<_>>(), expected);
                assert_eq!(page.has_more, expected.first().is_some_and(|&id| id > 1));
            }
        }
        // 无归属的用户拿到 None（handler 映射为 404），而不是空页
        assert!(fetch_messages_page_with_pool(&pool, conversation, "other", 20, None).await.unwrap().is_none());
        sqlx::query("UPDATE assistant_messages SET role = 'tool'").execute(&pool).await.unwrap();
        let tools = fetch_messages_page_with_pool(&pool, conversation, "owner", 1, Some(5)).await.unwrap().unwrap();
        assert_eq!(tools.messages.len(), 4);
        assert!(!tools.has_more);
    }

    #[tokio::test]
    async fn db_tests_message_page_completes_interleaved_jobs_without_gaps() {
        let pool = crate::db::test_pool().await;
        let conversation: i64 = query_scalar(
            "INSERT INTO assistant_conversations (user_id, system_prompt) VALUES ('owner', '') RETURNING id",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        let a = uuid::Uuid::new_v4();
        let b = uuid::Uuid::new_v4();
        let c = uuid::Uuid::new_v4();
        for job in [a, b, c] {
            query("INSERT INTO assistant_generation_jobs (id, conversation_id, user_id, status) VALUES (?1, ?2, 'owner', 'completed')")
                .bind(job).bind(conversation).execute(&pool).await.unwrap();
        }
        let mut ids = Vec::new();
        // Completing C includes A's output, and completing A includes B's
        // output. B's question must also be included, despite the queue order.
        for (role, content, job) in [
            ("user", "older question", None),
            ("assistant", "older answer", None),
            ("user", "question B", Some(b)),
            ("user", "question A", Some(a)),
            ("tool", "output B", Some(b)),
            ("user", "question C", Some(c)),
            ("tool", "output A", Some(a)),
            ("tool", "output C", Some(c)),
            ("assistant", "answer C", Some(c)),
        ] {
            let id: i64 = query_scalar("INSERT INTO assistant_messages (conversation_id, role, content, generation_job_id) VALUES (?1, ?2, ?3, ?4) RETURNING id")
                .bind(conversation).bind(role).bind(content).bind(job).fetch_one(&pool).await.unwrap();
            ids.push(id);
        }
        let newest = fetch_messages_page_with_pool(&pool, conversation, "owner", 1, None).await.unwrap().unwrap();
        assert_eq!(newest.messages.iter().map(|m| m.id).collect::<Vec<_>>(), ids[2..]);
        assert!(newest.has_more);
        // Appending new messages cannot move the stored cursor or duplicate
        // any of the already loaded turns on the next page.
        query("INSERT INTO assistant_messages (conversation_id, role, content) VALUES (?1, 'user', 'new question')")
            .bind(conversation)
            .execute(&pool)
            .await
            .unwrap();
        let older =
            fetch_messages_page_with_pool(&pool, conversation, "owner", 1, Some(ids[2])).await.unwrap().unwrap();
        assert_eq!(older.messages.iter().map(|m| m.id).collect::<Vec<_>>(), ids[..2]);
        assert!(!older.has_more);
        assert!(fetch_messages_page_with_pool(&pool, conversation, "other", 1, None).await.unwrap().is_none());
    }
}
