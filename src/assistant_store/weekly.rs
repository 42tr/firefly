//! 周报：本周 Codex / Claude 对话的工作记录。

use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::query_as;

use crate::db;

/// 一个对话在本周的工作记录：只看用户发的消息和 agent 最后一次返回。
#[derive(Debug, Clone, Serialize)]
pub struct WeeklyWorkRecord {
    pub conversation_id: i64,
    pub agent: String,
    pub title: Option<String>,
    pub cwd: Option<String>,
    pub last_message_at: DateTime<Utc>,
    /// 本周内用户发出的消息（按时间升序）。
    pub user_messages: Vec<String>,
    /// 该对话 agent 最后一次返回（不限于本周，代表最新结果）。
    pub last_assistant_reply: Option<String>,
}

/// 查询 `since` 以来有活动的 Codex / Claude 对话，供周报子智能体汇总。
/// 用户消息按 `since` 过滤；最后一次回复取对话整体最新的一条，反映当前结果。
pub async fn fetch_weekly_agent_work(user_id: &str, since: DateTime<Utc>) -> anyhow::Result<Vec<WeeklyWorkRecord>> {
    fetch_weekly_agent_work_with_pool(db::pool(), user_id, since).await
}

pub(super) async fn fetch_weekly_agent_work_with_pool(
    pool: &sqlx::SqlitePool, user_id: &str, since: DateTime<Utc>,
) -> anyhow::Result<Vec<WeeklyWorkRecord>> {
    let conversations = query_as::<_, (i64, String, Option<String>, Option<String>, crate::db::Timestamp)>(
        r#"
        SELECT id, agent, title, codex_cwd, last_message_at
        FROM assistant_conversations
        WHERE user_id = ?1 AND agent IN ('codex', 'claude') AND last_message_at >= ?2
        ORDER BY last_message_at ASC, id ASC
        "#,
    )
    .bind(user_id)
    .bind((since).timestamp_micros())
    .fetch_all(pool)
    .await?;
    if conversations.is_empty() {
        return Ok(Vec::new());
    }
    let ids: Vec<i64> = conversations.iter().map(|row| row.0).collect();
    // Three queries regardless of conversation count. Keep the original message
    // order and include the latest reply even when it predates the reporting week.
    let user_rows = query_as::<_, (i64, String)>(
        "SELECT conversation_id, content FROM assistant_messages
         WHERE conversation_id IN (SELECT value FROM json_each(?1)) AND role = 'user' AND created_at >= ?2 AND trim(content) <> ''
         ORDER BY conversation_id, id",
    )
    .bind(sqlx::types::Json(&ids))
    .bind((since).timestamp_micros())
    .fetch_all(pool)
    .await?;
    let replies = query_as::<_, (i64, String)>(
        "SELECT conversation_id, content FROM (SELECT conversation_id, content, ROW_NUMBER() OVER (PARTITION BY conversation_id ORDER BY id DESC) AS rn FROM assistant_messages WHERE conversation_id IN (SELECT value FROM json_each(?1)) AND role = 'assistant' AND trim(content) <> '') WHERE rn = 1 ORDER BY conversation_id",
    )
    .bind(sqlx::types::Json(&ids))
    .fetch_all(pool)
    .await?;
    let mut messages_by_conversation = std::collections::HashMap::<i64, Vec<String>>::new();
    for (id, content) in user_rows {
        messages_by_conversation.entry(id).or_default().push(content);
    }
    let mut replies_by_conversation: std::collections::HashMap<_, _> = replies.into_iter().collect();
    let mut records = Vec::with_capacity(conversations.len());
    for (id, agent, title, cwd, last_message_at) in conversations {
        let user_messages = messages_by_conversation.remove(&id).unwrap_or_default();
        let last_assistant_reply = replies_by_conversation.remove(&id);

        // 本周只有工具过程消息、没有任何可参考内容时直接跳过
        if user_messages.is_empty() && last_assistant_reply.is_none() {
            continue;
        }
        records.push(WeeklyWorkRecord {
            conversation_id: id,
            agent,
            title,
            cwd,
            last_message_at: last_message_at.try_into()?,
            user_messages,
            last_assistant_reply,
        });
    }
    Ok(records)
}
