//! Records and inputs shared by assistant persistence modules.
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::FromRow;

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

#[derive(Debug, Clone)]
pub struct MessagePage {
    pub messages: Vec<MessageRecord>,
    pub has_more: bool,
}
