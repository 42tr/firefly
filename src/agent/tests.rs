use super::subagent_limits;
use async_trait::async_trait;
use g::{
    Agent, AgentError, Content, Message, Model, ModelError, ModelRequest, ModelResponse, Role,
    RunLimits, RunRequest, Runtime, Tool, ToolContext, ToolError, ToolSpec,
};
use serde_json::{json, Value};
use std::{
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};
use tokio_util::sync::CancellationToken;

struct CountingModel {
    tool: &'static str,
    calls: usize,
    turns: AtomicUsize,
}

#[async_trait]
impl Model for CountingModel {
    async fn generate(&self, _: ModelRequest) -> Result<ModelResponse, ModelError> {
        let turn = self.turns.fetch_add(1, Ordering::SeqCst);
        Ok(ModelResponse::new(if turn < self.calls {
            Message::new(
                Role::Assistant,
                vec![Content::ToolCall {
                    id: format!("call-{turn}"),
                    name: self.tool.into(),
                    arguments: if self.tool.starts_with("handoff_to_") {
                        json!({"task":"work"})
                    } else {
                        json!({})
                    },
                }],
            )
        } else {
            Message::assistant("done")
        }))
    }
}

struct Echo;
#[async_trait]
impl Tool for Echo {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "echo".into(),
            description: String::new(),
            input_schema: json!({"type":"object"}),
            behavior: Default::default(),
        }
    }
    async fn call(&self, _: ToolContext, _: Value) -> Result<Value, ToolError> {
        Ok(json!("ok"))
    }
}

#[tokio::test]
async fn subagent_exceeds_all_previous_call_caps_without_using_parent_budget() {
    let model = Arc::new(CountingModel {
        tool: "echo",
        calls: 401,
        turns: 0.into(),
    });
    let child = Agent::new(model.clone())
        .name("child")
        .tool(Echo)
        .with_limits(subagent_limits(Duration::from_secs(10)));
    let parent = Agent::new(Arc::new(CountingModel {
        tool: "handoff_to_child",
        calls: 1,
        turns: 0.into(),
    }))
    .with_limits(RunLimits {
        max_tool_calls: 1,
        timeout: Duration::from_secs(10),
    })
    .handoff([child]);
    let output = parent.run("work").await.unwrap();
    assert_eq!(output.final_text, "done");
    assert_eq!(output.tool_calls, 1);
    assert_eq!(model.turns.load(Ordering::SeqCst), 402);
    assert!(output.messages.iter().flat_map(|message| &message.content).any(|content|
        matches!(content, Content::ToolResult { result, is_error: false, .. } if result["response"] == "done")));
}

struct PendingModel;
#[async_trait]
impl Model for PendingModel {
    async fn generate(&self, _: ModelRequest) -> Result<ModelResponse, ModelError> {
        std::future::pending().await
    }
}

#[tokio::test]
async fn subagent_without_call_cap_still_times_out_and_accepts_cancellation() {
    let child =
        Agent::new(Arc::new(PendingModel)).with_limits(subagent_limits(Duration::from_millis(10)));
    assert!(matches!(child.run("work").await, Err(AgentError::Timeout)));
    let cancelled = CancellationToken::new();
    cancelled.cancel();
    let result = Runtime::new()
        .run(
            &child,
            RunRequest::new(vec![Message::user("work")]).with_cancellation_token(cancelled),
        )
        .await;
    assert!(matches!(result, Err(AgentError::Cancelled)));
}
