//! Conversation API entry points, wire types, and shared response projections.
use serde::{Deserialize, Serialize};

use super::ConversationMessageResponse;

mod handlers;
mod history;
mod presentation;
mod settings;

pub use handlers::{
    create_conversation, delete_conversation, list_conversations, mark_conversation_read,
    update_conversation,
};
pub use history::{get_conversation_messages, get_conversation_process};
pub use settings::{update_claude_settings, update_codex_settings};
pub(super) use presentation::{
    map_attachment_record, message_from_record, summarize_process_messages, to_message_response,
};
pub(super) use settings::{prepare_conversation, system_prompt_for_agent};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MarkConversationReadRequest {
    pub message_id: i64,
}

#[derive(Debug, Deserialize)]
pub struct ProcessDetailQuery {
    pub job_id: Option<uuid::Uuid>,
    pub start_id: Option<i64>,
    pub end_id: Option<i64>,
}

#[derive(Debug, Serialize)]
pub struct ProcessDetailResponse {
    pub conversation_id: i64,
    pub job_id: Option<uuid::Uuid>,
    pub messages: Vec<ConversationMessageResponse>,
}

#[cfg(test)]
mod codex_settings_tests;
#[cfg(test)]
mod claude_settings_tests;
#[cfg(test)]
mod process_summary_tests;
#[cfg(test)]
mod list_tests;
