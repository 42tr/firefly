//! 长期记忆：手动维护或由 Firefly 自动补充，按相关性注入 system prompt。

use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::{query, query_as, FromRow};

use crate::db;

#[derive(Debug, Clone, Serialize, FromRow)]
pub struct MemoryRecord {
    pub id: i64,
    #[serde(skip_serializing)]
    pub user_id: String,
    pub title: String,
    pub content: String,
    pub enabled: bool,
    pub source: String,
    #[sqlx(try_from = "crate::db::Timestamp")]
    pub created_at: DateTime<Utc>,
    #[sqlx(try_from = "crate::db::Timestamp")]
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, FromRow)]
pub struct MemoryReviewJob {
    pub generation_job_id: uuid::Uuid,
    pub conversation_id: i64,
    pub user_id: String,
    pub final_message_id: i64,
    pub attempts: i64,
}

// Memory SQL also interpolates only fixed columns, with all values bound separately.
const MEMORY_COLUMNS: &str = "id, user_id, title, content, enabled, source, created_at, updated_at";
pub const MAX_MEMORY_TITLE_LEN: usize = 100;
pub const MAX_MEMORY_CONTENT_LEN: usize = 4000;
pub const MAX_MEMORY_COUNT: usize = 100;

/// Agent memories are atomic facts: count Unicode characters, including the title.
pub const MAX_AGENT_MEMORY_CHARS: usize = 200;
pub const MAX_MEMORY_REVIEW_CHANGES: usize = 8;

#[derive(Debug, Clone)]
pub struct ReviewedMemoryChange {
    pub id: Option<i64>,
    pub title: String,
    pub content: String,
    pub evidence: String,
}

pub fn validate_agent_memory(title: &str, content: &str) -> anyhow::Result<()> {
    anyhow::ensure!(
        !title.trim().is_empty() && !content.trim().is_empty(),
        "empty Agent memory"
    );
    anyhow::ensure!(
        title.chars().count() <= MAX_MEMORY_TITLE_LEN
            && title.chars().count() + content.chars().count() <= MAX_AGENT_MEMORY_CHARS,
        "Agent memory exceeds 200 characters (title + content)"
    );
    Ok(())
}

/// Commit a reviewed batch against its exact snapshot, with history in the same transaction.
#[cfg(test)]
pub async fn apply_reviewed_memories(
    user_id: &str,
    changes: &[ReviewedMemoryChange],
    checked: &[MemoryRecord],
) -> anyhow::Result<Option<Vec<MemoryRecord>>> {
    apply_reviewed_memories_for_turn(user_id, changes, checked, None).await
}

pub async fn apply_reviewed_memories_for_turn(
    user_id: &str, changes: &[ReviewedMemoryChange], checked: &[MemoryRecord],
    anchor: Option<(uuid::Uuid, i64)>,
) -> anyhow::Result<Option<Vec<MemoryRecord>>> {
    anyhow::ensure!(
        changes.len() <= MAX_MEMORY_REVIEW_CHANGES,
        "too many memory changes"
    );
    for change in changes {
        validate_agent_memory(&change.title, &change.content)?;
    }
    let mut tx = db::pool().begin_with("BEGIN IMMEDIATE").await?;
    if let Some((job_id, final_message_id)) = anchor {
        let exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM assistant_memory_jobs p
            JOIN assistant_generation_jobs j ON j.id=p.generation_job_id
            WHERE p.generation_job_id=?1 AND p.final_message_id=?2 AND p.user_id=?3
                AND p.status='running' AND j.status='completed')")
            .bind(job_id).bind(final_message_id).bind(user_id).fetch_one(&mut *tx).await?;
        anyhow::ensure!(exists,"记忆后处理所属回合已删除或结束");
    }
    let sql = format!(
        "SELECT {} FROM assistant_memories WHERE user_id = ?1 ORDER BY updated_at DESC",
        MEMORY_COLUMNS
    );
    let current: Vec<MemoryRecord> = query_as(sqlx::AssertSqlSafe(sql))
        .bind(user_id)
        .fetch_all(&mut *tx)
        .await?;
    let unchanged = current.len() == checked.len()
        && current.iter().all(|memory| {
            checked.iter().any(|old| {
                memory.id == old.id
                    && memory.user_id == old.user_id
                    && memory.title == old.title
                    && memory.content == old.content
                    && memory.enabled == old.enabled
                    && memory.source == old.source
                    && memory.updated_at == old.updated_at
            })
        });
    if !unchanged {
        tx.rollback().await?;
        return Ok(None);
    }
    anyhow::ensure!(
        current.len() + changes.iter().filter(|c| c.id.is_none()).count() <= MAX_MEMORY_COUNT,
        "memory capacity exceeded"
    );
    let mut ids = std::collections::HashSet::new();
    for change in changes {
        if let Some(id) = change.id {
            anyhow::ensure!(ids.insert(id), "duplicate memory update");
            anyhow::ensure!(
                current
                    .iter()
                    .any(|m| m.id == id && m.source == "agent" && m.enabled),
                "only enabled Agent memories may be automatically updated"
            );
        }
    }
    // Check the final state, including duplicates within the batch and disabled memories.
    let mut contents = std::collections::HashSet::new();
    for memory in &current {
        if !ids.contains(&memory.id) {
            contents.insert(memory.content.trim());
        }
    }
    for change in changes {
        anyhow::ensure!(
            contents.insert(change.content.trim()),
            "duplicate memory content"
        );
    }
    let mut saved = Vec::new();
    for change in changes {
        let record = if let Some(id) = change.id {
            query("INSERT INTO assistant_memory_revisions (memory_id, title, content, previous_updated_at, evidence)
                SELECT id, title, content, updated_at, ?3 FROM assistant_memories WHERE id=?1 AND user_id=?2")
                .bind(id).bind(user_id).bind(&change.evidence).execute(&mut *tx).await?;
            let sql = format!(
                "UPDATE assistant_memories SET title=?3, content=?4,
                updated_at=MAX(updated_at+1, CAST(unixepoch('subsec') * 1000000 AS INTEGER))
                WHERE id=?1 AND user_id=?2 AND source='agent' AND enabled=TRUE RETURNING {}",
                MEMORY_COLUMNS
            );
            query_as(sqlx::AssertSqlSafe(sql))
                .bind(id)
                .bind(user_id)
                .bind(&change.title)
                .bind(&change.content)
                .fetch_one(&mut *tx)
                .await?
        } else {
            let sql = format!(
                "INSERT INTO assistant_memories (user_id, title, content, enabled, source)
                VALUES (?1, ?2, ?3, TRUE, 'agent') RETURNING {}",
                MEMORY_COLUMNS
            );
            query_as(sqlx::AssertSqlSafe(sql))
                .bind(user_id)
                .bind(&change.title)
                .bind(&change.content)
                .fetch_one(&mut *tx)
                .await?
        };
        saved.push(record);
    }
    if let Some((job_id, final_message_id)) = anchor {
        sqlx::query("UPDATE assistant_memory_jobs SET status='completed',error_message=NULL,
            completed_at=CAST(unixepoch('subsec') * 1000000 AS INTEGER)
            WHERE generation_job_id=?1 AND final_message_id=?2 AND status='running'")
            .bind(job_id).bind(final_message_id).execute(&mut *tx).await?;
    }
    tx.commit().await?;
    Ok(Some(saved))
}

pub async fn claim_next_memory_review() -> anyhow::Result<Option<MemoryReviewJob>> {
    let mut tx = db::pool().begin_with("BEGIN IMMEDIATE").await?;
    let job = query_as::<_, MemoryReviewJob>(
        "UPDATE assistant_memory_jobs
         SET status='running', attempts=attempts+1,
             started_at=CAST(unixepoch('subsec') * 1000000 AS INTEGER),
             updated_at=CAST(unixepoch('subsec') * 1000000 AS INTEGER)
         WHERE generation_job_id=(
             SELECT generation_job_id FROM assistant_memory_jobs
             WHERE status='queued' AND attempts < 5
             ORDER BY created_at, generation_job_id LIMIT 1
         ) AND status='queued'
         RETURNING generation_job_id, conversation_id, user_id, final_message_id, attempts",
    )
    .fetch_optional(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(job)
}

pub async fn requeue_interrupted_memory_reviews() -> anyhow::Result<u64> {
    let result = query("UPDATE assistant_memory_jobs SET status='queued',
        error_message='interrupted_by_restart',
        updated_at=CAST(unixepoch('subsec') * 1000000 AS INTEGER)
        WHERE status='running'")
        .execute(db::pool())
        .await?;
    Ok(result.rows_affected())
}

pub async fn fail_memory_review(job_id: uuid::Uuid, error: &str) -> anyhow::Result<()> {
    query("UPDATE assistant_memory_jobs SET
        status=CASE WHEN attempts >= 5 THEN 'failed' ELSE 'queued' END,
        error_message=?2, updated_at=CAST(unixepoch('subsec') * 1000000 AS INTEGER)
        WHERE generation_job_id=?1 AND status='running'")
        .bind(job_id)
        .bind(error)
        .execute(db::pool())
        .await?;
    Ok(())
}

pub async fn fetch_memory_review_messages(
    job_id: uuid::Uuid, conversation_id: i64, user_id: &str,
) -> anyhow::Result<Vec<super::MessageRecord>> {
    let rows = query_as::<_, super::MessageRecord>(
        "SELECT m.id, m.conversation_id, m.role, m.content, m.tool_call_id, m.tool_calls,
                m.metadata, m.is_error, m.created_at, m.generation_job_id,
                j.status AS processing_status, j.error_message AS processing_error,
                j.started_at AS processing_started_at, j.completed_at AS processing_completed_at
         FROM assistant_messages m
         JOIN assistant_conversations c ON c.id=m.conversation_id
         LEFT JOIN assistant_generation_jobs j ON j.id=m.generation_job_id
         WHERE m.conversation_id=?1 AND c.user_id=?2 AND m.generation_job_id=?3
         ORDER BY m.id",
    )
    .bind(conversation_id)
    .bind(user_id)
    .bind(job_id)
    .fetch_all(db::pool())
    .await?;
    Ok(rows)
}

#[cfg(test)]
pub async fn create_reviewed_memory(
    user_id: &str,
    title: &str,
    content: &str,
    checked: &[MemoryRecord],
) -> anyhow::Result<Option<MemoryRecord>> {
    let change = ReviewedMemoryChange {
        id: None,
        title: title.into(),
        content: content.into(),
        evidence: "[]".into(),
    };
    Ok(apply_reviewed_memories(user_id, &[change], checked)
        .await?
        .and_then(|mut saved| saved.pop()))
}

pub async fn list_memories(user_id: &str) -> anyhow::Result<Vec<MemoryRecord>> {
    let sql = format!(
        "SELECT {} FROM assistant_memories WHERE user_id = ?1 ORDER BY updated_at DESC",
        MEMORY_COLUMNS
    );
    let rows = query_as::<_, MemoryRecord>(sqlx::AssertSqlSafe(sql))
        .bind(user_id)
        .fetch_all(db::pool())
        .await?;
    Ok(rows)
}

/// 返回启用的记忆，按 id 升序保证注入 prompt 时顺序稳定。
pub async fn list_enabled_memories(user_id: &str) -> anyhow::Result<Vec<MemoryRecord>> {
    let sql = format!(
        "SELECT {} FROM assistant_memories WHERE user_id = ?1 AND enabled = TRUE ORDER BY id ASC",
        MEMORY_COLUMNS
    );
    let rows = query_as::<_, MemoryRecord>(sqlx::AssertSqlSafe(sql))
        .bind(user_id)
        .fetch_all(db::pool())
        .await?;
    Ok(rows)
}

pub async fn create_memory(
    user_id: &str,
    title: &str,
    content: &str,
    enabled: bool,
) -> anyhow::Result<MemoryRecord> {
    let sql = format!(
        "INSERT INTO assistant_memories (user_id, title, content, enabled, source) VALUES (?1, ?2, ?3, ?4, ?5) RETURNING {}",
        MEMORY_COLUMNS
    );
    let rec = query_as::<_, MemoryRecord>(sqlx::AssertSqlSafe(sql))
        .bind(user_id)
        .bind(title)
        .bind(content)
        .bind(enabled)
        .bind("manual")
        .fetch_one(db::pool())
        .await?;
    Ok(rec)
}

/// User ownership is checked as part of the write.
/// Source is immutable: editing an Agent memory as a user preserves its provenance.
pub async fn update_memory(
    id: i64,
    user_id: &str,
    title: Option<&str>,
    content: Option<&str>,
    enabled: Option<bool>,
) -> anyhow::Result<Option<MemoryRecord>> {
    let sql = format!(
        r#"
        UPDATE assistant_memories
        SET title = COALESCE(?3, title),
            content = COALESCE(?4, content),
            enabled = COALESCE(?5, enabled),
            updated_at = CAST(unixepoch('subsec') * 1000000 AS INTEGER)
        WHERE id = ?1 AND user_id = ?2
        RETURNING {}
        "#,
        MEMORY_COLUMNS
    );
    let rec = query_as::<_, MemoryRecord>(sqlx::AssertSqlSafe(sql))
        .bind(id)
        .bind(user_id)
        .bind(title)
        .bind(content)
        .bind(enabled)
        .fetch_optional(db::pool())
        .await?;
    Ok(rec)
}

pub async fn delete_memory(id: i64, user_id: &str) -> anyhow::Result<bool> {
    let result = query("DELETE FROM assistant_memories WHERE id = ?1 AND user_id = ?2")
        .bind(id)
        .bind(user_id)
        .execute(db::pool())
        .await?;
    Ok(result.rows_affected() > 0)
}

#[derive(Debug, FromRow)]
pub struct MemoryEmbeddingRecord {
    pub memory_id: i64,
    pub content_hash: String,
    pub dimensions: i64,
    pub chunks: String,
}

pub async fn list_all_memories() -> anyhow::Result<Vec<MemoryRecord>> {
    let sql = format!(
        "SELECT {} FROM assistant_memories ORDER BY id",
        MEMORY_COLUMNS
    );
    Ok(query_as(sqlx::AssertSqlSafe(sql))
        .fetch_all(db::pool())
        .await?)
}

pub async fn list_memory_embeddings(
    user_id: &str,
    space: &str,
) -> anyhow::Result<Vec<MemoryEmbeddingRecord>> {
    Ok(query_as(
        "SELECT e.memory_id, e.content_hash, e.dimensions, e.chunks
        FROM assistant_memory_embeddings e JOIN assistant_memories m ON m.id = e.memory_id
        WHERE m.user_id = ?1 AND e.embedding_space = ?2",
    )
    .bind(user_id)
    .bind(space)
    .fetch_all(db::pool())
    .await?)
}

/// No network call inside a transaction. Discard an embedding if its source was
/// edited/deleted while the request was running; an old worker cannot overwrite a new index.
pub async fn save_memory_embedding(
    memory: &MemoryRecord,
    space: &str,
    hash: &str,
    dimensions: usize,
    chunks: &str,
) -> anyhow::Result<bool> {
    let result = query(
        "INSERT INTO assistant_memory_embeddings
        (memory_id, embedding_space, content_hash, dimensions, chunks)
        SELECT id, ?3, ?4, ?5, ?6 FROM assistant_memories
        WHERE id = ?1 AND user_id = ?2 AND title = ?7 AND content = ?8
        ON CONFLICT(memory_id) DO UPDATE SET embedding_space = excluded.embedding_space,
            content_hash = excluded.content_hash, dimensions = excluded.dimensions,
            chunks = excluded.chunks, updated_at = CAST(unixepoch('subsec') * 1000000 AS INTEGER)",
    )
    .bind(memory.id)
    .bind(&memory.user_id)
    .bind(space)
    .bind(hash)
    .bind(dimensions as i64)
    .bind(chunks)
    .bind(&memory.title)
    .bind(&memory.content)
    .execute(db::pool())
    .await?;
    Ok(result.rows_affected() > 0)
}

#[cfg(test)]
mod tests;
