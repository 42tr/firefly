use async_trait::async_trait;
use serde_json::Value;
use uuid::Uuid;

use crate::{Message, RunOutput, Usage};

/// Run lifecycle events. Every event carries the `run_id` of the run that produced it,
/// so events from handoff child runs can be told apart from the parent's.
#[derive(Clone, Debug)]
pub enum RunEvent {
    Started {
        run_id: Uuid,
    },
    ModelStarted {
        run_id: Uuid,
        turn: usize,
    },
    /// A retryable model error occurred before any output was streamed; the request
    /// is retried after `backoff`.
    ModelRetry {
        run_id: Uuid,
        turn: usize,
        attempt: u32,
        error: String,
        backoff: std::time::Duration,
    },
    ModelCompleted {
        run_id: Uuid,
        turn: usize,
        message: Message,
        usage: Usage,
    },
    TextDelta {
        run_id: Uuid,
        turn: usize,
        text: String,
    },
    ToolStarted {
        run_id: Uuid,
        call_id: String,
        name: String,
    },
    ToolCompleted {
        run_id: Uuid,
        call_id: String,
        name: String,
        result: Value,
        is_error: bool,
    },
    HandoffStarted {
        run_id: Uuid,
        call_id: String,
        agent: String,
    },
    HandoffCompleted {
        run_id: Uuid,
        call_id: String,
        agent: String,
        child_run_id: Option<Uuid>,
        is_error: bool,
    },
    Completed {
        run_id: Uuid,
        turns: usize,
        tool_calls: usize,
        usage: Usage,
    },
    /// Last item of a `stream_run` stream after a successful run. Only delivered
    /// through the stream, never to an agent's `EventSink`.
    Finished {
        output: Box<RunOutput>,
    },
}

#[async_trait]
pub trait EventSink: Send + Sync {
    async fn emit(&self, event: RunEvent);
}

#[derive(Debug, Default)]
pub struct NoopEventSink;

#[async_trait]
impl EventSink for NoopEventSink {
    async fn emit(&self, _event: RunEvent) {}
}
