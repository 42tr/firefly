//! Engine-specific generation workflows and shared event persistence.
//! Re-exports preserve the assistant API's execution and message-conversion entry points.

mod claude;
mod codex;
mod events;
mod firefly;

pub(super) use claude::run_claude_turn;
pub(super) use codex::{is_text_attachment, run_codex_turn};
pub(super) use events::to_g_message;
pub(super) use firefly::run_conversation;

#[cfg(test)]
#[path = "generation/workflow_tests.rs"]
mod workflow_tests;
#[cfg(test)]
#[path = "generation/event_contract_tests.rs"]
mod event_contract_tests;
#[cfg(test)]
#[path = "generation/codex_retry_tests.rs"]
mod codex_retry_tests;
