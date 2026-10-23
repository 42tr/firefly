//! Assistant persistence grouped by conversations, messages, and attachments.
//! Public re-exports keep callers independent of the internal file layout.

mod attachments;
mod conversations;
mod messages;
mod types;
mod memories;

pub use attachments::{
    fetch_attachments_for_messages, get_attachment_by_file_for_user, get_attachment_for_user,
    insert_attachments,
};
pub(crate) use conversations::delete_conversation_with_pool;
pub use conversations::{
    create_conversation, delete_conversation, get_conversation, get_conversation_for_read,
    list_active_generation_conversation_ids, list_workspace_conversations, rename_conversation,
    retry_workspace, set_claude_session_id, set_claude_settings, set_codex_settings,
    set_codex_thread_id, set_conversation_archived, set_conversation_pinned,
    set_generated_conversation_title, set_workspace_state, ConversationArchivalBlocked,
    ConversationDeletionBlocked, ConversationSettingsError,
};
pub use messages::{
    append_message, fetch_job_process_messages, fetch_messages_page, is_first_user_message,
};
pub use types::{
    AttachmentRecord, ConversationRecord, MessagePage, MessageRecord, NewAttachment, NewConversation,
    StoredMessage, StoredToolCall,
};
pub use memories::{
    create_memory, delete_memory, list_enabled_memories, list_memories, update_memory, MemoryRecord,
    list_all_memories, list_memory_embeddings, save_memory_embedding, MemoryEmbeddingRecord,
    MAX_MEMORY_CONTENT_LEN, MAX_MEMORY_COUNT, MAX_MEMORY_TITLE_LEN,
};
#[cfg(test)]
pub use memories::create_reviewed_memory;

mod submissions;
#[allow(unused_imports)]
pub use submissions::{enforce_submission_capacity, submit_queued_user_turn, submit_queued_user_turn_with_metadata, SubmissionConflict};

mod goals;
pub use goals::{
    apply_goal_check, create_goal, get_active_goal, get_goal, get_goal_by_id, list_stages, replace_goal_stages, set_goal_status,
    GoalRecord, GoalStageRecord,
};

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

#[cfg(test)]
#[path = "assistant_store/tests.rs"]
mod paging_tests;
