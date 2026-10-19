//! Message writes, complete-turn pagination, and generation process history.
use sqlx::{query, query_as, query_scalar, SqliteConnection};

use super::{MessagePage, MessageRecord, StoredMessage};
use crate::db;

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

pub(super) const PAGE_QUESTION_SQL: &str = r#"
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

pub(super) async fn fetch_messages_page_with_pool(
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

pub(super) async fn insert_message(
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

pub async fn is_first_user_message(conversation_id: i64, message_id: i64) -> anyhow::Result<bool> {
    Ok(query_scalar::<_, Option<i64>>(
        "SELECT MIN(id) FROM assistant_messages WHERE conversation_id = ?1 AND role = 'user'",
    )
    .bind(conversation_id)
    .fetch_one(db::pool())
    .await?
        == Some(message_id))
}
