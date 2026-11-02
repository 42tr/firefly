use std::sync::Arc;

use crate::{Model, ModelError};

mod common;
pub mod openai;
pub mod openai_chat;

/// Create an OpenAI-compatible model provider from environment variables.
///
/// Reads `OPENAI_TYPE` to decide which endpoint to use:
/// - `"responses"` → `OpenAIModel` (Responses API)
/// - `"chat"` or `"chat_completions"` → `OpenAIChatModel` (Chat Completions API)
///
/// Defaults to `"chat"` (Chat Completions API) when `OPENAI_TYPE` is not set.
///
/// Other environment variables are read by the chosen provider:
/// - `OPENAI_API_KEY` (required)
/// - `OPENAI_MODEL` (optional, provider-specific default)
/// - `OPENAI_BASE_URL` (optional, defaults to `https://api.openai.com/v1`)
pub fn openai_from_env() -> Result<Arc<dyn Model>, ModelError> {
    let model_type = std::env::var("OPENAI_TYPE").unwrap_or_else(|_| "chat".into());
    match model_type.as_str() {
        "responses" => Ok(Arc::new(openai::OpenAIModel::from_env()?)),
        "chat" | "chat_completions" => Ok(Arc::new(openai_chat::OpenAIChatModel::from_env()?)),
        other => Err(ModelError::new(format!(
            "unsupported OPENAI_TYPE `{other}`; expected `responses`, `chat`, or `chat_completions`"
        ))),
    }
}
