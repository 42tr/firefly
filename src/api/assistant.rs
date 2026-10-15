pub mod schedules;
mod conversations;
use conversations::*;
pub use conversations::{
    create_conversation, delete_conversation, get_conversation_messages, get_conversation_process, list_conversations,
    mark_conversation_read, update_conversation, update_codex_settings, update_claude_settings,
};
mod workspace;
pub use workspace::{
    codex_providers, claude_models, create_workspace_directory, download_workspace_file, list_workspace, retry_parallel_workspace,
};
mod queue;
use queue::*;
pub use queue::{
    cancel_job, chat_stream, enqueue_conversation_message, get_active_generation, resume_job_stream,
    resume_queued_generations,
};
mod attachments;
pub use attachments::{download_attachment, preview_uploaded_attachment};
use attachments::*;
mod generation;
use generation::*;
mod turn_context;
mod inputs;
mod persistence;
use persistence::*;
mod tool_details;
use tool_details::{save_tool_detail, ToolDetail};
pub use tool_details::get_tool_detail;
mod titles;
use titles::*;

pub mod firefly;

use std::{
    collections::{HashMap, HashSet},
    convert::Infallible,
    path::PathBuf,
    sync::Arc,
    time::Duration,
};

use anyhow::anyhow;
use async_trait::async_trait;
use axum::{
    body::Body,
    extract::{multipart::Field, FromRequest, Multipart, Path, Query},
    http::{header, HeaderMap, StatusCode},
    response::{
        sse::{Event, KeepAlive, Sse},
        Response,
    },
    Json,
};
use backon::{ExponentialBuilder, Retryable};
use g::{
    Agent, Content as GContent, EventSink as GEventSink, Message as GMessage, Model, Role as GRole, RunEvent,
    RunLimits, RunRequest, Runtime,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::{
    fs,
    io::AsyncWriteExt,
    sync::{mpsc, oneshot},
};
use tokio_stream::wrappers::ReceiverStream;
use tracing::warn;

use crate::{
    api::{attachment_utils, auth::Session, error::AppError},
    assistant_job, assistant_store, claude, codex, config, llm, mcp, office, utils,
};

const MAX_GENERATED_IMAGE_BYTES: i64 = 15 * 1024 * 1024;

fn is_codex_retrying_message(message: &str) -> bool {
    let normalized = message.trim().to_ascii_lowercase();
    let Some(rest) = normalized.strip_prefix("reconnecting") else {
        return false;
    };
    let rest = rest.trim_start_matches(['.', ' ', ':']);
    let Some((attempt, total)) = rest.split_once('/') else {
        return false;
    };
    attempt.trim().parse::<u32>().is_ok() && total.trim().parse::<u32>().is_ok()
}

/// 生成任务的对外错误消息：只取 anyhow 的外层消息（不含错误链），并限制长度。
fn safe_generation_error(err: &anyhow::Error) -> String {
    let mut message = err.to_string();
    utils::truncate_to_char_boundary(&mut message, 300);
    message
}

#[derive(Debug, Deserialize)]
pub struct ChatRequest {
    pub message: String,
    pub enable_tools: Option<bool>,
    pub conversation_id: Option<i64>,
}

#[derive(Debug, Deserialize)]
pub struct CreateConversationRequest {
    pub title: Option<String>,
    pub agent: Option<String>,
    pub cwd: Option<String>,
    pub workspace_mode: Option<String>,
    /// Codex 对话绑定的 model_providers 名称（对应 ~/.codex/config.toml），空表示跟随全局默认。
    pub provider: Option<String>,
    /// Codex 对话绑定的模型，空表示跟随平台/全局默认。
    pub model: Option<String>,
    pub reasoning_level: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateConversationRequest {
    pub title: Option<String>,
    pub is_pinned: Option<bool>,
    pub is_archived: Option<bool>,
}

/// Replaces the three Codex settings together; null/empty uses the configured default.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CodexSettingsRequest {
    pub provider: Option<String>,
    pub model: Option<String>,
    pub reasoning_level: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClaudeSettingsRequest {
    pub model: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ConversationSummary {
    pub id: i64,
    pub title: Option<String>,
    pub agent: String,
    pub cwd: Option<String>,
    pub workspace_mode: String,
    pub source_cwd: Option<String>,
    pub workspace_state: String,
    pub workspace_error: Option<String>,
    pub codex_provider: Option<String>,
    pub codex_model: Option<String>,
    pub codex_reasoning_level: Option<String>,
    pub claude_model: Option<String>,
    pub message_count: i64,
    pub read_message_id: i64,
    pub last_message_id: i64,
    pub is_pinned: bool,
    pub is_archived: bool,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
    pub last_message_at: chrono::DateTime<chrono::Utc>,
    pub status: &'static str,
}

#[derive(Debug, Deserialize)]
pub struct WorkspaceQuery {
    pub path: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct WorkspaceFileQuery {
    pub path: String,
}

#[derive(Debug, Deserialize)]
pub struct CreateWorkspaceDirectoryRequest {
    pub path: Option<String>,
    pub name: String,
}

#[derive(Debug, Serialize)]
pub struct ConversationListResponse {
    pub conversations: Vec<ConversationSummary>,
    pub next_cursor: Option<ConversationListCursor>,
    pub selected_conversation: Option<ConversationSummary>,
}

#[derive(Debug, Serialize)]
pub struct ConversationListCursor {
    pub before_pinned: bool,
    pub before_at: chrono::DateTime<chrono::Utc>,
    pub before_id: i64,
}

#[derive(Debug, Deserialize)]
pub struct ConversationListQuery {
    #[serde(default)]
    pub archived: bool,
    pub limit: Option<i64>,
    pub before_pinned: Option<bool>,
    pub before_at: Option<chrono::DateTime<chrono::Utc>>,
    pub before_id: Option<i64>,
    #[serde(default)]
    pub search: String,
    // Keep a selected workspace available even when it falls outside the current page/search.
    pub selected_id: Option<i64>,
}

#[derive(Debug, Serialize)]
pub struct ConversationMessageResponse {
    pub id: i64,
    pub conversation_id: i64,
    pub role: String,
    pub content: String,
    pub tool_call_id: Option<String>,
    pub tool_calls: Option<Value>,
    pub metadata: Option<Value>,
    pub is_error: bool,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub generation_job_id: Option<uuid::Uuid>,
    pub processing_status: Option<String>,
    pub processing_error: Option<String>,
    pub processing_started_at: Option<chrono::DateTime<chrono::Utc>>,
    pub processing_completed_at: Option<chrono::DateTime<chrono::Utc>>,
    /// Tool I/O is omitted; process lists steps and tool-detail reads one call.
    pub detail_omitted: bool,
    pub attachments: Vec<MessageAttachmentResponse>,
}

#[derive(Debug, Serialize)]
pub struct EnqueueMessageResponse {
    pub job_id: uuid::Uuid,
    pub status: String,
    pub message: ConversationMessageResponse,
}

#[derive(Debug, Serialize)]
pub struct MessageListResponse {
    pub conversation_id: i64,
    pub messages: Vec<HistoryMessage>,
    pub has_more: bool,
    pub next_before: Option<i64>,
    pub source_reply: Option<ConversationSourceReply>,
}

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct ConversationSourceReply {
    pub message_id: Option<i64>,
    pub conversation_id: Option<i64>,
    pub preview: String,
}

#[derive(Debug, Serialize)]
#[serde(untagged)]
pub enum HistoryMessage {
    Message(ConversationMessageResponse),
    Process(ProcessSummary),
}

#[derive(Debug, Serialize)]
pub struct ProcessSummary {
    pub id: i64,
    pub conversation_id: i64,
    pub role: &'static str,
    pub generation_job_id: Option<uuid::Uuid>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub started_at: chrono::DateTime<chrono::Utc>,
    pub completed_at: chrono::DateTime<chrono::Utc>,
    pub start_id: i64,
    pub end_id: i64,
    pub status: String,
    pub processing_started_at: Option<chrono::DateTime<chrono::Utc>>,
    pub processing_completed_at: Option<chrono::DateTime<chrono::Utc>>,
}

#[derive(Debug, Deserialize)]
pub struct MessageListQuery {
    pub limit: Option<i64>,
    pub before: Option<i64>,
}

// 分页单位为问答气泡（user / assistant 文本各算 1 条，工具调用收进过程块不计），
// 而不是原始数据库记录数。默认 20 气泡 ≈ 最近 10 轮问答。
const MESSAGE_PAGE_SIZE: i64 = 20;
const MAX_MESSAGE_PAGE_SIZE: i64 = 100;

#[derive(Debug, Serialize, Deserialize)]
pub struct MessageAttachmentResponse {
    pub id: i64,
    pub file_name: String,
    pub content_type: String,
    pub file_size: i64,
}

#[derive(Clone)]
struct AssistantConfig {
    model: Arc<dyn Model>,
    budget: llm::ModelBudget,
    system_prompt: String,
    agent_settings: Option<Arc<crate::agent_settings::Settings>>,
}

impl AssistantConfig {
    /// Read one revision for this turn; subsequent turns see saved changes.
    async fn shared() -> Result<Arc<Self>, AppError> {
        let settings = Arc::new(crate::agent_settings::Settings::load(crate::db::pool()).await?);
        let model = Arc::new(settings.model(&settings.firefly).map_err(AppError::bad_request)?);
        let budget = model.budget();
        Ok(Arc::new(Self { model, budget, system_prompt: settings.firefly.instruction.clone(),
            agent_settings: Some(settings) }))
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Conversation {
    messages: Vec<Message>,
}

impl Conversation {
    fn new() -> Self {
        Self {
            messages: Vec::new(),
        }
    }

    fn add_message(&mut self, message: Message) {
        self.messages.push(message);
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
enum Role {
    User,
    Assistant,
    Tool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct ToolCall {
    id: String,
    name: String,
    arguments: Value,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Message {
    role: Role,
    content: String,
    tool_calls: Option<Vec<ToolCall>>,
    tool_call_id: Option<String>,
    is_error: bool,
    attachments: Vec<MessageAttachment>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct MessageAttachment {
    id: Option<i64>,
    file_name: String,
    content_type: String,
    file_size: i64,
    storage_path: String,
}

impl From<assistant_store::AttachmentRecord> for MessageAttachment {
    fn from(record: assistant_store::AttachmentRecord) -> Self {
        Self {
            id: Some(record.id),
            file_name: record.file_name,
            content_type: record.content_type,
            file_size: record.file_size,
            storage_path: record.storage_path,
        }
    }
}

impl From<mcp::ToolAttachment> for MessageAttachment {
    fn from(attachment: mcp::ToolAttachment) -> Self {
        Self {
            id: None,
            file_name: attachment.file_name,
            content_type: attachment.content_type,
            file_size: attachment.file_size,
            storage_path: attachment.storage_path,
        }
    }
}

impl From<PendingAttachment> for MessageAttachment {
    fn from(pending: PendingAttachment) -> Self {
        Self {
            id: None,
            file_name: pending.file_name,
            content_type: pending.content_type,
            file_size: pending.file_size,
            storage_path: pending.storage_path,
        }
    }
}

impl MessageAttachment {
    fn as_new_attachment(&self) -> assistant_store::NewAttachment {
        assistant_store::NewAttachment {
            file_name: self.file_name.clone(),
            content_type: self.content_type.clone(),
            file_size: self.file_size,
            storage_path: self.storage_path.clone(),
        }
    }
}

#[derive(Debug, Clone)]
struct PendingAttachment {
    file_name: String,
    content_type: String,
    file_size: i64,
    storage_path: String,
    sha256: String,
}

struct AttachmentCleanup {
    entries: Vec<PendingAttachment>,
    active: bool,
}

impl AttachmentCleanup {
    fn new(entries: Vec<PendingAttachment>) -> Self {
        Self { entries, active: true }
    }

    fn entries(&self) -> &[PendingAttachment] {
        &self.entries
    }

    fn disarm(&mut self) {
        self.active = false;
    }
}

impl Drop for AttachmentCleanup {
    fn drop(&mut self) {
        if !self.active {
            return;
        }
        let entries = std::mem::take(&mut self.entries);
        std::thread::spawn(move || {
            for entry in entries {
                let _ = std::fs::remove_file(&entry.storage_path);
            }
        });
    }
}

#[derive(Debug)]
struct ParsedChatPayload {
    message: String,
    enable_tools: Option<bool>,
    conversation_id: Option<i64>,
    attachments: Vec<PendingAttachment>,
}

impl Message {
    fn new(role: Role, content: String) -> Self {
        Self {
            role,
            content,
            tool_calls: None,
            tool_call_id: None,
            is_error: false,
            attachments: Vec::new(),
        }
    }

    fn assistant(content: String, tool_calls: Option<Vec<ToolCall>>) -> Self {
        Self {
            role: Role::Assistant,
            content,
            tool_calls,
            tool_call_id: None,
            is_error: false,
            attachments: Vec::new(),
        }
    }

    fn tool(content: String, tool_call_id: String, is_error: bool) -> Self {
        Self {
            role: Role::Tool,
            content,
            tool_calls: None,
            tool_call_id: Some(tool_call_id),
            is_error,
            attachments: Vec::new(),
        }
    }
}

pub async fn ping(_auth: Session) -> Result<Json<Value>, AppError> {
    Ok(Json(json!({ "ok": true })))
}

pub async fn reset(_auth_user: Session) -> Result<Json<Value>, AppError> {
    Ok(Json(json!({ "ok": true })))
}
