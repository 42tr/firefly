use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::{Message, ModelError, ToolSpec};

/// A model request. Both fields are shared so the runtime can hand the history to
/// the model each turn without copying it.
#[derive(Clone, Debug)]
pub struct ModelRequest {
    pub messages: Arc<Vec<Message>>,
    pub tools: Arc<[ToolSpec]>,
}

impl ModelRequest {
    pub fn new(messages: impl Into<Arc<Vec<Message>>>, tools: impl Into<Arc<[ToolSpec]>>) -> Self {
        Self {
            messages: messages.into(),
            tools: tools.into(),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    /// Portion of `input_tokens` served from the provider's prompt cache.
    pub cached_input_tokens: u64,
    /// Portion of `output_tokens` spent on reasoning.
    pub reasoning_tokens: u64,
}

impl Usage {
    pub fn add(&mut self, other: Self) {
        self.input_tokens = self.input_tokens.saturating_add(other.input_tokens);
        self.output_tokens = self.output_tokens.saturating_add(other.output_tokens);
        self.cached_input_tokens = self
            .cached_input_tokens
            .saturating_add(other.cached_input_tokens);
        self.reasoning_tokens = self.reasoning_tokens.saturating_add(other.reasoning_tokens);
    }
}

#[derive(Clone, Debug)]
pub struct ModelResponse {
    pub message: Message,
    pub usage: Usage,
}

impl ModelResponse {
    pub fn new(message: Message) -> Self {
        Self {
            message,
            usage: Usage::default(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ModelEvent {
    TextDelta { text: String },
}

#[async_trait]
pub trait ModelEventSink: Send + Sync {
    async fn emit(&self, event: ModelEvent);
}

#[async_trait]
pub trait Model: Send + Sync {
    async fn generate(&self, request: ModelRequest) -> Result<ModelResponse, ModelError>;

    async fn generate_stream(
        &self,
        request: ModelRequest,
        event_sink: &dyn ModelEventSink,
    ) -> Result<ModelResponse, ModelError> {
        let response = self.generate(request).await?;
        let text = response.message.text_content();
        if !text.is_empty() {
            event_sink.emit(ModelEvent::TextDelta { text }).await;
        }
        Ok(response)
    }
}
