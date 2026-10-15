//! Generation jobs：排队领取、状态更新与重启恢复。

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::{query, query_as, query_scalar, FromRow, Sqlite, SqliteConnection};

use super::{
    insert_attachment_rows, insert_message, AttachmentRecord, MessageRecord, NewAttachment, StoredMessage,
    StoredToolCall,
};
use crate::db;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CodexGenerationSettings {
    pub provider: Option<String>,
    pub model: Option<String>,
    pub reasoning_level: Option<String>,
}

impl CodexGenerationSettings {
    pub fn apply_to(&self, conversation: &mut super::ConversationRecord) {
        conversation.codex_provider = self.provider.clone();
        conversation.codex_model = self.model.clone();
        conversation.codex_reasoning_level = self.reasoning_level.clone();
    }
}

#[derive(Debug, Clone, Serialize, FromRow)]
pub struct GenerationJobRecord {
    pub id: uuid::Uuid,
    pub conversation_id: i64,
    #[serde(skip_serializing)]
    pub user_id: String,
    pub status: String,
    pub error_message: Option<String>,
    pub enable_tools: bool,
    #[serde(skip_serializing)]
    pub codex_settings: Option<sqlx::types::Json<CodexGenerationSettings>>,
    #[serde(skip_serializing)]
    pub claude_settings: Option<sqlx::types::Json<ClaudeGenerationSettings>>,
    #[sqlx(try_from = "crate::db::Timestamp")]
    pub created_at: DateTime<Utc>,
    #[sqlx(try_from = "crate::db::Timestamp")]
    pub updated_at: DateTime<Utc>,
    #[sqlx(try_from = "crate::db::NullableTimestamp")]
    pub started_at: Option<DateTime<Utc>>,
    #[sqlx(try_from = "crate::db::NullableTimestamp")]
    pub completed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ClaudeGenerationSettings {
    #[serde(default)]
    pub provider: Option<String>,
    pub model: Option<String>,
    #[serde(default)]
    pub reasoning_level: Option<String>,
}

impl ClaudeGenerationSettings {
    pub fn apply_to(&self, conversation: &mut super::ConversationRecord) {
        conversation.claude_model = self.model.clone();
        conversation.claude_provider = self.provider.clone();
        conversation.claude_reasoning_level = self.reasoning_level.clone();
    }
}

/// Creates a queued job together with its user message and attachments, so a
/// worker can never claim the job before the message it answers is visible.
#[cfg(test)]
pub async fn create_queued_user_turn(
    job_id: uuid::Uuid, conversation_id: i64, user_id: &str, enable_tools: bool, content: String,
    attachments: &[NewAttachment],
) -> anyhow::Result<(MessageRecord, Vec<AttachmentRecord>)> {
    let pool = db::pool();
    let mut tx = pool.begin_with("BEGIN IMMEDIATE").await?;
    let result =
        insert_queued_user_turn(&mut tx, job_id, conversation_id, user_id, enable_tools, content, attachments).await?;
    tx.commit().await?;
    crate::assistant_events::notify(Some(user_id), "conversations");
    Ok(result)
}

pub(super) async fn insert_queued_user_turn(
    conn: &mut SqliteConnection, job_id: uuid::Uuid, conversation_id: i64, user_id: &str, enable_tools: bool,
    content: String, attachments: &[NewAttachment],
) -> anyhow::Result<(MessageRecord, Vec<AttachmentRecord>)> {
    query(
        r#"
        INSERT INTO assistant_generation_jobs (id, conversation_id, user_id, status, enable_tools, codex_settings, claude_settings)
        VALUES (?1, ?2, ?3, 'queued', ?4,
                (SELECT json_object('provider', codex_provider, 'model', codex_model,
                                    'reasoning_level', codex_reasoning_level)
                 FROM assistant_conversations WHERE id = ?2 AND user_id = ?3 AND agent = 'codex'),
                (SELECT json_object('provider', claude_provider, 'model', claude_model, 'reasoning_level', claude_reasoning_level)
                 FROM assistant_conversations WHERE id = ?2 AND user_id = ?3 AND agent = 'claude'))
        "#,
    )
    .bind(job_id)
    .bind(conversation_id)
    .bind(user_id)
    .bind(enable_tools)
    .execute(&mut *conn)
    .await?;
    let message = StoredMessage {
        role: "user".to_string(),
        content,
        tool_call_id: None,
        tool_calls: None,
        metadata: None,
        is_error: false,
        generation_job_id: Some(job_id),
    };
    let stored = insert_message(conn, conversation_id, message).await?;
    let stored_attachments = insert_attachment_rows(conn, stored.id, attachments).await?;
    Ok((stored, stored_attachments))
}

pub async fn find_active_generation(
    conversation_id: i64, user_id: &str,
) -> anyhow::Result<Option<GenerationJobRecord>> {
    let pool = db::pool();
    let record = query_as::<_, GenerationJobRecord>(
        r#"
        SELECT id, conversation_id, user_id, status AS status, error_message, enable_tools, codex_settings, claude_settings, created_at, updated_at, started_at, completed_at
        FROM assistant_generation_jobs
        WHERE conversation_id = ?1 AND user_id = ?2 AND status = 'running'
        ORDER BY created_at DESC
        LIMIT 1
        "#,
    )
    .bind(conversation_id)
    .bind(user_id)
    .fetch_optional(pool)
    .await?;
    Ok(record)
}

pub async fn get_generation_job(id: uuid::Uuid, user_id: &str) -> anyhow::Result<Option<GenerationJobRecord>> {
    let record = query_as::<_, GenerationJobRecord>(
        r#"
        SELECT id, conversation_id, user_id, status AS status, error_message, enable_tools, codex_settings, claude_settings, created_at, updated_at, started_at, completed_at
        FROM assistant_generation_jobs
        WHERE id = ?1 AND user_id = ?2
        "#,
    )
    .bind(id)
    .bind(user_id)
    .fetch_optional(db::pool())
    .await?;
    Ok(record)
}

pub async fn generation_requires_snapshot(id: uuid::Uuid) -> anyhow::Result<bool> {
    Ok(query_scalar(
        "SELECT EXISTS(SELECT 1 FROM assistant_messages WHERE generation_job_id=?1
         AND (role <> 'user' OR json_extract(metadata, '$.restart_recovery') IN (1, 'true')))",
    )
    .bind(id)
    .fetch_one(db::pool())
    .await?)
}

/// 删除尚未开始运行的生成任务及其用户消息，返回需要清理的附件路径。
pub async fn delete_queued_generation(id: uuid::Uuid, user_id: &str) -> anyhow::Result<Option<(i64, Vec<String>)>> {
    let mut tx = db::pool().begin_with("BEGIN IMMEDIATE").await?;
    let conversation_id =
        query_scalar::<_, i64>("SELECT conversation_id FROM assistant_generation_jobs WHERE id = ?1 AND user_id = ?2")
            .bind(id)
            .bind(user_id)
            .fetch_optional(&mut *tx)
            .await?;
    let Some(_conversation_id) = conversation_id else {
        tx.rollback().await?;
        return Ok(None);
    };

    let job = query_as::<_, (i64, String)>(
        "SELECT conversation_id, status FROM assistant_generation_jobs WHERE id = ?1 AND user_id = ?2",
    )
    .bind(id)
    .bind(user_id)
    .fetch_optional(&mut *tx)
    .await?;
    let Some((conversation_id, status)) = job else {
        tx.rollback().await?;
        return Ok(None);
    };
    if status != "queued" {
        tx.rollback().await?;
        return Ok(None);
    }
    let paths = query_scalar::<_, String>(
        "SELECT a.storage_path FROM assistant_message_attachments a JOIN assistant_messages m ON m.id = a.message_id WHERE m.generation_job_id = ?1 AND m.role = 'user'",
    )
    .bind(id)
    .fetch_all(&mut *tx)
    .await?;
    let deleted = query("DELETE FROM assistant_messages WHERE generation_job_id = ?1 AND role = 'user'")
        .bind(id)
        .execute(&mut *tx)
        .await?
        .rows_affected();
    query("DELETE FROM assistant_generation_jobs WHERE id = ?1").bind(id).execute(&mut *tx).await?;
    if deleted > 0 {
        query("UPDATE assistant_conversations SET message_count = MAX(message_count - ?2, 0) WHERE id = ?1")
            .bind(conversation_id)
            .bind(deleted as i64)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    crate::assistant_events::notify(Some(user_id), "conversations");
    Ok(Some((conversation_id, paths)))
}

/// Atomically claims the oldest queued turn when the conversation has no running turn.
pub async fn claim_next_queued_generation(
    conversation_id: i64, user_id: &str,
) -> anyhow::Result<Option<GenerationJobRecord>> {
    claim_next_queued_generation_with_pool(db::pool(), conversation_id, user_id).await
}

pub(super) async fn claim_next_queued_generation_with_pool(
    pool: &sqlx::SqlitePool, conversation_id: i64, user_id: &str,
) -> anyhow::Result<Option<GenerationJobRecord>> {
    let mut tx = pool.begin_with("BEGIN IMMEDIATE").await?;

    let record = query_as::<_, GenerationJobRecord>(
        r#"
        WITH next_job AS (
            SELECT id
            FROM assistant_generation_jobs
            WHERE conversation_id = ?1
              AND user_id = ?2
              AND status = 'queued'
              AND EXISTS (
                  SELECT 1 FROM assistant_conversations c WHERE c.id=?1 AND c.user_id=?2
                  AND (c.workspace_mode='shared' OR c.workspace_state IN ('pending','preparing','ready'))
              )
              AND NOT EXISTS (
                  SELECT 1 FROM assistant_generation_jobs active
                  WHERE active.conversation_id = ?1 AND active.status = 'running'
              )
            ORDER BY created_at ASC, id ASC
            LIMIT 1
        )
        UPDATE assistant_generation_jobs
        SET status = 'running', updated_at = CAST(unixepoch('subsec') * 1000000 AS INTEGER),
            started_at = COALESCE(started_at, CAST(unixepoch('subsec') * 1000000 AS INTEGER)), completed_at = NULL
        WHERE id = (SELECT id FROM next_job) AND status = 'queued'
        RETURNING id, conversation_id, user_id, status, error_message, enable_tools, codex_settings, claude_settings, created_at, updated_at, started_at, completed_at
        "#,
    )
    .bind(conversation_id)
    .bind(user_id)
    .fetch_optional(&mut *tx)
    .await?;
    tx.commit().await?;
    crate::assistant_events::notify(Some(user_id), "conversations");
    Ok(record)
}

pub async fn list_queued_generation_conversations() -> anyhow::Result<Vec<(i64, String)>> {
    let rows = query_as::<_, (i64, String)>(
        r#"
        SELECT conversation_id, user_id
        FROM assistant_generation_jobs
        WHERE status = 'queued'
        GROUP BY conversation_id, user_id
        "#,
    )
    .fetch_all(db::pool())
    .await?;
    Ok(rows)
}

#[derive(FromRow)]
struct GenerationContextRow {
    #[sqlx(flatten)]
    message: MessageRecord,
    context_created_at: i64,
}

pub async fn fetch_generation_context_messages(
    conversation_id: i64, user_id: &str, current_job_id: uuid::Uuid,
) -> anyhow::Result<Vec<MessageRecord>> {
    let mut rows = query_as::<_, GenerationContextRow>(
        r#"
        SELECT m.id, m.conversation_id, m.role, m.content, m.tool_call_id, m.tool_calls,
               m.metadata, m.is_error, m.created_at, m.generation_job_id,
               j.status AS processing_status, j.error_message AS processing_error,
               j.started_at AS processing_started_at, j.completed_at AS processing_completed_at,
               COALESCE(j.created_at, m.created_at) AS context_created_at
        FROM assistant_messages m
        JOIN assistant_conversations c ON c.id = m.conversation_id
        LEFT JOIN assistant_generation_jobs j ON j.id = m.generation_job_id
        WHERE m.conversation_id = ?1
          AND c.user_id = ?2
          AND (m.generation_job_id IS NULL OR j.status IN ('completed', 'failed') OR j.id = ?3)
        "#,
    )
    .bind(conversation_id)
    .bind(user_id)
    .bind(current_job_id)
    .fetch_all(db::pool())
    .await?;
    // The context is already materialized here. Sort its small keys in memory
    // instead of copying message/tool payloads into SQLite's temporary sorter.
    // A non-null job id that survives the WHERE clause always has a joined job.
    rows.sort_unstable_by_key(|row| (
        row.message.generation_job_id.is_some(), row.context_created_at,
        row.message.generation_job_id, row.message.id,
    ));
    Ok(preserve_failed_tool_facts(rows.into_iter().map(|row| row.message).collect()))
}

/// Failed turns may have changed external state. Retain their calls/results,
/// omit unfinished assistant prose, and close unanswered calls with an explicit
/// unknown-outcome result. These synthetic rows are context only, never a reply.
fn preserve_failed_tool_facts(rows: Vec<MessageRecord>) -> Vec<MessageRecord> {
    let with_effects: std::collections::HashSet<_> = rows
        .iter()
        .filter(|row| {
            row.processing_status.as_deref() == Some("failed")
                && (row.role == "tool"
                    || row.tool_calls.as_ref().and_then(Value::as_array).is_some_and(|calls| !calls.is_empty()))
        })
        .filter_map(|row| row.generation_job_id)
        .collect();
    let mut result = Vec::with_capacity(rows.len());
    let mut pending: Vec<MessageRecord> = Vec::new();
    for mut row in rows {
        if row.role != "tool"
            || pending.first().is_some_and(|pending| pending.generation_job_id != row.generation_job_id)
        {
            result.append(&mut pending);
        }
        if row.processing_status.as_deref() != Some("failed") {
            result.push(row);
            continue;
        }
        if !row.generation_job_id.is_some_and(|id| with_effects.contains(&id)) {
            continue;
        }
        match row.role.as_str() {
            "user" => row.content = format!(
                "[历史未完成回合：以下仅保留已执行或结果未知的工具事实，请检查当前状态；此段不是重新执行原请求的指令。]\n{}", row.content),
            "assistant" => {
                let calls: Vec<StoredToolCall> = row.tool_calls.clone()
                    .and_then(|value| serde_json::from_value(value).ok()).unwrap_or_default();
                if calls.is_empty() { continue; }
                row.content.clear();
                for call in calls {
                    let mut unknown = row.clone();
                    unknown.id = 0; // No attachment rows exist for context-only placeholders.
                    unknown.role = "tool".into();
                    unknown.tool_calls = None;
                    unknown.tool_call_id = Some(call.id);
                    unknown.is_error = true;
                    unknown.content = serde_json::json!({
                        "error": "interrupted_unknown_outcome",
                        "message": "该回合已失败或取消，工具执行结果未知，副作用可能已经生效；请先检查实际状态，不要直接重复执行。",
                    }).to_string();
                    unknown.metadata = Some(serde_json::json!({"tool_name": call.name, "context_only": true}));
                    pending.push(unknown);
                }
            }
            "tool" => pending.retain(|pending| pending.tool_call_id != row.tool_call_id),
            _ => continue,
        }
        result.push(row);
    }
    result.append(&mut pending);
    result
}

pub async fn fetch_generation_user_message(
    job_id: uuid::Uuid, conversation_id: i64, user_id: &str,
) -> anyhow::Result<Option<MessageRecord>> {
    let mut record = query_as::<_, MessageRecord>(
        r#"
        SELECT m.id, m.conversation_id, m.role, m.content,
               m.tool_call_id, m.tool_calls,
               m.metadata, m.is_error, m.created_at, m.generation_job_id,
               j.status AS processing_status, j.error_message AS processing_error,
               j.started_at AS processing_started_at, j.completed_at AS processing_completed_at
        FROM assistant_messages m
        JOIN assistant_conversations c ON c.id = m.conversation_id
        JOIN assistant_generation_jobs j ON j.id = m.generation_job_id
        WHERE j.id = ?1 AND m.conversation_id = ?2 AND c.user_id = ?3 AND m.role = 'user'
        LIMIT 1
        "#,
    )
    .bind(job_id)
    .bind(conversation_id)
    .bind(user_id)
    .fetch_optional(db::pool())
    .await?;
    if let Some(message) = &mut record {
        let recovering = message
            .metadata
            .as_ref()
            .and_then(|metadata| metadata.get("restart_recovery"))
            .is_some_and(|flag| flag.as_bool() == Some(true) || flag.as_str() == Some("true"));
        if recovering {
            message.content.push_str(
                "\n\n[服务重启恢复] 请从原会话的最新进度继续完成任务，保留并检查已经完成的工作，不要从头重复执行。",
            );
        }
    }
    Ok(record)
}

pub async fn update_generation_status(
    id: uuid::Uuid, status: &str, error_message: Option<&str>,
) -> anyhow::Result<()> {
    let owner: Option<String> = db::retry_busy("update generation status", || async {
        let mut tx = db::pool().begin_with("BEGIN IMMEDIATE").await?;
        let owner = query_scalar("UPDATE assistant_generation_jobs
            SET status=?2,error_message=?3,updated_at=CAST(unixepoch('subsec') * 1000000 AS INTEGER),
                completed_at=CASE WHEN ?2 IN ('completed','failed') THEN CAST(unixepoch('subsec') * 1000000 AS INTEGER) ELSE NULL END
            WHERE id=?1 RETURNING user_id")
            .bind(id).bind(status).bind(error_message).fetch_optional(&mut *tx).await?;
        tx.commit().await?;
        Ok(owner)
    }).await?;
    if let Some(owner) = owner { crate::assistant_events::notify(Some(&owner), "conversations"); }
    Ok(())
}

/// Keep the running claim until the worker exits, while making a user's stop
/// request durable so restart recovery cannot accidentally execute it again.
pub async fn request_generation_cancellation(id: uuid::Uuid, user_id: &str) -> anyhow::Result<()> {
    db::retry_busy("request generation cancellation", || async {
        let result = query(
            "UPDATE assistant_messages
             SET metadata=json_set(COALESCE(metadata, '{}'), '$.cancellation_requested', json('true'))
             WHERE generation_job_id=?1 AND role='user'
               AND EXISTS(SELECT 1 FROM assistant_generation_jobs WHERE id=?1 AND user_id=?2
                 AND status='running')",
        )
        .bind(id)
        .bind(user_id)
        .execute(db::pool())
        .await?;
        anyhow::ensure!(result.rows_affected() > 0, "running generation input not found");
        Ok(())
    })
    .await
}

/// 服务重启时恢复上次进程退出时被中断（status 仍为 running）的生成任务，
/// 由启动流程中的队列恢复逻辑继续执行，而不是直接标记失败。
///
/// 不同 agent 的恢复策略：
/// - firefly：对话历史完全来自 DB，删除部分输出会让模型忘记已执行过的工具调用，
///   可能重复产生副作用（例如重复修改文件）。因此保留全部消息，并为缺少结果的
///   工具调用补写一条 is_error 的“中断”占位结果，让续跑的模型明确知道这些调用的
///   结果未知、副作用可能已生效。若最后一条消息已是最终回复（无工具调用的
///   assistant 文本），说明回复已完整落库，只是完成状态没来得及写入，直接标记完成。
/// - codex / claude：保留本轮输出和附件，补齐未知的工具结果，并标记用户消息，
///   续跑时复用原 thread/session，同时注入恢复提示，避免重发原请求后从头执行。
#[derive(Debug, Default)]
pub struct InterruptedRecovery {
    pub requeued: u64,
    pub completed: u64,
    pub failed: u64,
}

pub async fn requeue_interrupted_generations() -> anyhow::Result<InterruptedRecovery> {
    let pool = db::pool();
    let mut tx = pool.begin_with("BEGIN IMMEDIATE").await?;

    let jobs = query_as::<_, (uuid::Uuid, i64, String)>(
        r#"
        SELECT j.id, j.conversation_id, c.agent
        FROM assistant_generation_jobs j
        JOIN assistant_conversations c ON c.id = j.conversation_id
        WHERE j.status = 'running'
        ORDER BY j.created_at
        "#,
    )
    .fetch_all(&mut *tx)
    .await?;

    let mut recovery = InterruptedRecovery::default();
    for (job_id, conversation_id, agent) in &jobs {
        recover_interrupted_job(&mut tx, *job_id, *conversation_id, agent, &mut recovery).await?;
    }

    reconcile_tool_details(&mut tx).await?;
    tx.commit().await?;
    Ok(recovery)
}

// Recovery writes context-only tool results without going through API
// persistence. Reconcile these results, including older recoveries, before a
// resumed parent starts so its previous handoffs cannot keep running forever.
async fn reconcile_tool_details(tx: &mut sqlx::Transaction<'_, Sqlite>) -> anyhow::Result<()> {
    query("WITH results AS (
        SELECT d.rowid AS detail_id,m.content,m.is_error,m.created_at,
            json_extract(m.metadata,'$.interrupted') AS interrupted
        FROM assistant_tool_details d JOIN assistant_messages m ON m.id=(
            SELECT id FROM assistant_messages
            WHERE conversation_id=d.conversation_id AND generation_job_id=unhex(replace(d.job_id,'-',''))
                AND role='tool' AND tool_call_id=d.call_id ORDER BY id DESC LIMIT 1)
        WHERE d.parent_id='' AND d.status IN ('queued','running'))
        UPDATE assistant_tool_details AS d SET
            status=CASE WHEN r.interrupted=1 OR CASE WHEN json_valid(r.content) THEN
                json_extract(r.content,'$.error') IN ('interrupted_by_restart','interrupted_unknown_outcome') ELSE 0 END
                THEN 'unknown' WHEN r.is_error THEN 'error' ELSE 'completed' END,
            output=CASE WHEN json_valid(r.content) THEN r.content ELSE json_quote(r.content) END,
            ended_at=COALESCE(d.ended_at,r.created_at/1000),
            revision=(SELECT COALESCE(MAX(revision),0)+1 FROM assistant_tool_details WHERE conversation_id=d.conversation_id)
        FROM results r WHERE d.rowid=r.detail_id")
        .execute(&mut **tx).await?;
    Ok(())
}

/// 保留现场，补写中断占位结果后续跑；firefly 的最终回复已落库时直接标记完成。
async fn recover_interrupted_job(
    tx: &mut sqlx::Transaction<'_, Sqlite>, job_id: uuid::Uuid, conversation_id: i64, agent: &str,
    recovery: &mut InterruptedRecovery,
) -> anyhow::Result<()> {
    let cancelled: bool = query_scalar(
        "SELECT EXISTS(SELECT 1 FROM assistant_messages WHERE generation_job_id=?1
         AND role='user' AND json_extract(metadata, '$.cancellation_requested')=1)",
    )
    .bind(job_id)
    .fetch_one(&mut **tx)
    .await?;
    if cancelled {
        set_generation_status_tx(tx, job_id, "failed", Some("用户已停止生成")).await?;
        recovery.failed += 1;
        return Ok(());
    }
    let messages = query_as::<_, (String, Option<Value>, Option<String>)>(
        // Empty presentation records are not completed assistant replies.
        "SELECT role, tool_calls, tool_call_id FROM assistant_messages WHERE generation_job_id = ?1
         AND NOT (role='assistant' AND trim(content)='' AND (tool_calls IS NULL OR tool_calls='[]')
             AND NOT EXISTS (SELECT 1 FROM assistant_message_attachments WHERE message_id=assistant_messages.id)) ORDER BY id",
    )
    .bind(job_id)
    .fetch_all(&mut **tx)
    .await?;

    if !messages.iter().any(|(role, ..)| role == "user") {
        // 用户消息缺失，无法重建输入，只能标记失败。
        set_generation_status_tx(tx, job_id, "failed", Some("用户消息缺失，无法恢复")).await?;
        recovery.failed += 1;
        return Ok(());
    }

    // 保留已完成结果；只为结果未知的调用补写占位，重复重启不会重复补写。
    let answered: std::collections::HashSet<&str> = messages
        .iter()
        .filter(|(role, ..)| role == "tool")
        .filter_map(|(.., tool_call_id)| tool_call_id.as_deref())
        .collect();
    let mut pending: Vec<(String, String)> = Vec::new();
    for (role, tool_calls, _) in &messages {
        if role != "assistant" {
            continue;
        }
        let Some(calls) = tool_calls else { continue };
        let Ok(calls) = serde_json::from_value::<Vec<StoredToolCall>>(calls.clone()) else {
            continue;
        };
        for call in calls {
            if !answered.contains(call.id.as_str()) {
                pending.push((call.id, call.name));
            }
        }
    }

    for (call_id, name) in &pending {
        let content = serde_json::to_string(&serde_json::json!({
            "error": "interrupted_by_restart",
            "message": "服务重启导致此次工具调用中断，执行结果未知。该调用可能已部分或全部生效（例如文件可能已被修改），请先检查实际状态再决定是否重试。",
        }))?;
        let metadata = serde_json::json!({ "tool_name": name, "interrupted": true });
        query(
            "INSERT INTO assistant_messages (conversation_id, role, content, tool_call_id, metadata, is_error, generation_job_id) VALUES (?1, 'tool', ?2, ?3, ?4, TRUE, ?5)",
        )
        .bind(conversation_id)
        .bind(&content)
        .bind(call_id)
        .bind(&metadata)
        .bind(job_id)
        .execute(&mut **tx)
        .await?;
    }
    if !pending.is_empty() {
        query("UPDATE assistant_conversations SET message_count = message_count + ?2 WHERE id = ?1")
            .bind(conversation_id)
            .bind(pending.len() as i64)
            .execute(&mut **tx)
            .await?;
    }

    // 外部 agent 的纯文本也可能只是进度说明，不能据此判断整轮已完成。
    let unfinished_tasks: bool = query_scalar("SELECT EXISTS(SELECT 1 FROM assistant_tasks WHERE generation_job_id=?1 AND detached=0 AND (status IN ('queued','running','cancelling') OR observed=0))")
        .bind(job_id.to_string()).fetch_one(&mut **tx).await?;
    let final_reply_persisted = !unfinished_tasks
        && agent == "firefly"
        && pending.is_empty()
        && matches!(messages.last(), Some((role, tool_calls, _))
            if role == "assistant"
                && tool_calls.as_ref().and_then(Value::as_array).is_none_or(Vec::is_empty));
    if final_reply_persisted {
        set_generation_status_tx(tx, job_id, "completed", None).await?;
        recovery.completed += 1;
    } else {
        query(
            "UPDATE assistant_messages SET metadata = json_set(COALESCE(metadata, '{}'), '$.restart_recovery', json('true')) WHERE generation_job_id = ?1 AND role = 'user'",
        )
        .bind(job_id)
        .execute(&mut **tx)
        .await?;
        set_generation_status_tx(tx, job_id, "queued", None).await?;
        recovery.requeued += 1;
    }
    Ok(())
}

async fn set_generation_status_tx(
    tx: &mut sqlx::Transaction<'_, Sqlite>, job_id: uuid::Uuid, status: &str, error_message: Option<&str>,
) -> anyhow::Result<()> {
    query(
        "UPDATE assistant_generation_jobs SET status = ?2, error_message = ?3, updated_at = CAST(unixepoch('subsec') * 1000000 AS INTEGER), completed_at = CASE WHEN ?2 IN ('completed', 'failed') THEN CAST(unixepoch('subsec') * 1000000 AS INTEGER) ELSE NULL END WHERE id = ?1",
    )
    .bind(job_id)
    .bind(status)
    .bind(error_message)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

#[cfg(test)]
mod timing_tests {
    use super::*;

    #[test]
    fn old_claude_snapshots_keep_their_model_and_inherit_platform_and_effort() {
        let settings: ClaudeGenerationSettings = serde_json::from_value(serde_json::json!({"model":"opus"})).unwrap();
        assert_eq!(settings.model.as_deref(), Some("opus"));
        assert!(settings.provider.is_none() && settings.reasoning_level.is_none());
    }

    #[tokio::test]
    async fn generation_timing_excludes_queue_wait_and_persists_across_recovery() {
        let pool = crate::db::test_pool().await;
        crate::db::set_test_pool(pool.clone());
        let conversation: i64 = query_scalar("INSERT INTO assistant_conversations(user_id,system_prompt) VALUES ('owner','') RETURNING id")
            .fetch_one(&pool).await.unwrap();
        let id = uuid::Uuid::new_v4();
        create_queued_user_turn(id, conversation, "owner", true, "question".into(), &[]).await.unwrap();
        query("UPDATE assistant_generation_jobs SET created_at=1000000 WHERE id=?1").bind(id).execute(&pool).await.unwrap();
        let queued = get_generation_job(id, "owner").await.unwrap().unwrap();
        assert!(queued.started_at.is_none());
        assert!(queued.completed_at.is_none());
        let claimed = claim_next_queued_generation(conversation, "owner").await.unwrap().unwrap();
        assert!(claimed.started_at.unwrap() > queued.created_at);
        assert!(claimed.completed_at.is_none());
        update_generation_status(id, "queued", None).await.unwrap();
        let resumed = claim_next_queued_generation(conversation, "owner").await.unwrap().unwrap();
        assert_eq!(resumed.started_at, claimed.started_at);
        update_generation_status(id, "completed", None).await.unwrap();
        let completed = get_generation_job(id, "owner").await.unwrap().unwrap();
        assert_eq!(completed.started_at, claimed.started_at);
        assert!(completed.completed_at.unwrap() >= completed.started_at.unwrap());
        let records = crate::assistant_store::fetch_job_process_messages(conversation, "owner", Some(id), None, None).await.unwrap();
        assert_eq!(records[0].processing_started_at, completed.started_at);
        assert_eq!(records[0].processing_completed_at, completed.completed_at);
        let page = crate::assistant_store::fetch_messages_page(conversation, "owner", 20, None).await.unwrap().unwrap();
        assert_eq!(page.messages[0].processing_started_at, completed.started_at);
        assert_eq!(page.messages[0].processing_completed_at, completed.completed_at);
    }
}
