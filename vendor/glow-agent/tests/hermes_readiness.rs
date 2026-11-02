//! Compatibility probes for using g underneath the enterprise Hermes runtime.
//! Includes regression coverage for instructions, authorization and cancellation.

use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use async_trait::async_trait;
use futures_util::future::join_all;
use glow_agent::{
    Agent, AgentError, Content, Message, Model, ModelError, ModelRequest, ModelResponse, Policy,
    PolicyError, Role, RunLimits, RunRequest, Runtime, Tool, ToolBehavior, ToolContext, ToolError,
    ToolSpec,
};
use serde_json::{Value, json};
use tokio::sync::{Barrier, Notify};
use tokio_util::sync::CancellationToken;

struct ConcurrentEcho(Barrier);

#[async_trait]
impl Model for ConcurrentEcho {
    async fn generate(&self, request: ModelRequest) -> Result<ModelResponse, ModelError> {
        self.0.wait().await;
        Ok(ModelResponse::new(Message::assistant(
            request.messages.last().unwrap().text_content(),
        )))
    }
}

#[tokio::test]
async fn eight_runs_share_an_agent_without_sharing_conversation_state() {
    let agent = Agent::new(Arc::new(ConcurrentEcho(Barrier::new(8))));
    let results = tokio::time::timeout(
        Duration::from_secs(2),
        join_all((0..8).map(|id| agent.run(format!("owner-{id}")))),
    )
    .await
    .expect("all eight model calls must enter concurrently");
    for (id, result) in results.into_iter().enumerate() {
        let output = result.unwrap();
        assert_eq!(output.final_text, format!("owner-{id}"));
        assert_eq!(output.messages.len(), 2);
        assert_eq!(output.messages[0].text_content(), format!("owner-{id}"));
    }
}

struct CheckHistory(Vec<Message>);

#[async_trait]
impl Model for CheckHistory {
    async fn generate(&self, request: ModelRequest) -> Result<ModelResponse, ModelError> {
        assert_eq!(*request.messages, self.0);
        Ok(ModelResponse::new(Message::assistant("resumed")))
    }
}

#[tokio::test]
async fn caller_can_restore_serialized_history_including_tool_results() {
    let history = vec![
        Message::system("role instructions"),
        Message::user("previous question"),
        Message::new(
            Role::Assistant,
            vec![Content::ToolCall {
                id: "call-old".into(),
                name: "lookup".into(),
                arguments: json!({"id": 7}),
            }],
        ),
        Message::new(
            Role::Tool,
            vec![Content::ToolResult {
                call_id: "call-old".into(),
                result: json!({"value": 42}),
                is_error: false,
            }],
        ),
        Message::assistant("previous answer"),
        Message::user("follow-up question"),
    ];
    let stored = serde_json::to_string(&history).unwrap();
    let restored = serde_json::from_str(&stored).unwrap();
    let agent = Agent::new(Arc::new(CheckHistory(history.clone())));
    let output = Runtime::new()
        .run(&agent, RunRequest::new(restored))
        .await
        .unwrap();
    assert_eq!(output.messages[..history.len()], history);
    assert_eq!(output.final_text, "resumed");
}

#[tokio::test]
async fn low_level_runtime_composes_system_instruction() {
    let messages = vec![Message::user("hello")];
    let agent = Agent::new(Arc::new(CheckHistory(vec![
        Message::system("runtime instruction"),
        Message::user("hello"),
    ])))
    .instruction("runtime instruction");
    Runtime::new()
        .run(&agent, RunRequest::new(messages))
        .await
        .unwrap();
}

struct DenyAll;

#[async_trait]
impl Policy for DenyAll {
    async fn authorize(&self, _: &ToolContext, _: &ToolSpec, _: &Value) -> Result<(), PolicyError> {
        Err(PolicyError::new("denied by business grant"))
    }
}

struct CallThenAnswer(&'static str);

#[async_trait]
impl Model for CallThenAnswer {
    async fn generate(&self, request: ModelRequest) -> Result<ModelResponse, ModelError> {
        if request.messages.last().unwrap().role == Role::Tool {
            return Ok(ModelResponse::new(Message::assistant("complete")));
        }
        Ok(ModelResponse::new(Message::new(
            Role::Assistant,
            vec![Content::ToolCall {
                id: "call-1".into(),
                name: self.0.into(),
                arguments: json!({"task": "delegated task"}),
            }],
        )))
    }
}

struct MustNotRunTool;

#[async_trait]
impl Tool for MustNotRunTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "write".into(),
            description: "protected business action".into(),
            input_schema: json!({"type": "object"}),
            behavior: ToolBehavior::default(),
        }
    }

    async fn call(&self, _: ToolContext, _: Value) -> Result<Value, ToolError> {
        panic!("a denied business action must not execute")
    }
}

#[tokio::test]
async fn ordinary_tools_obey_business_policy() {
    let agent = Agent::new(Arc::new(CallThenAnswer("write")))
        .tool(MustNotRunTool)
        .with_policy(Arc::new(DenyAll));
    assert!(matches!(
        agent.run("do it").await,
        Err(AgentError::Policy(_))
    ));
}

#[tokio::test]
async fn handoff_obeys_parent_policy() {
    let child = Agent::new(Arc::new(CheckHistory(vec![Message::user(
        "delegated task",
    )])))
    .name("child");
    let parent = Agent::new(Arc::new(CallThenAnswer("handoff_to_child")))
        .handoff([child])
        .with_policy(Arc::new(DenyAll));
    assert!(matches!(
        parent.run("delegate").await,
        Err(AgentError::Policy(_))
    ));
}

struct PendingModel {
    entered: Notify,
    dropped: AtomicBool,
    finished: Notify,
}

impl PendingModel {
    fn new() -> Self {
        Self {
            entered: Notify::new(),
            dropped: AtomicBool::new(false),
            finished: Notify::new(),
        }
    }
}

struct ModelGuard<'a>(&'a PendingModel);

impl Drop for ModelGuard<'_> {
    fn drop(&mut self) {
        self.0.dropped.store(true, Ordering::SeqCst);
        self.0.finished.notify_one();
    }
}

#[async_trait]
impl Model for PendingModel {
    async fn generate(&self, _: ModelRequest) -> Result<ModelResponse, ModelError> {
        let _guard = ModelGuard(self);
        self.entered.notify_one();
        std::future::pending().await
    }
}

#[tokio::test]
async fn explicit_cancellation_drops_an_in_flight_model_request() {
    let model = Arc::new(PendingModel::new());
    let agent = Agent::new(model.clone());
    let cancellation = CancellationToken::new();
    let request =
        RunRequest::new(vec![Message::user("wait")]).with_cancellation_token(cancellation.clone());
    let handle = tokio::spawn(async move { Runtime::new().run(&agent, request).await });
    tokio::time::timeout(Duration::from_secs(2), model.entered.notified())
        .await
        .unwrap();
    cancellation.cancel();
    let result = tokio::time::timeout(Duration::from_secs(2), handle)
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(result, Err(AgentError::Cancelled)));
    assert!(model.dropped.load(Ordering::SeqCst));
}

#[tokio::test]
async fn dropping_stream_cancels_without_waiting_for_next_event() {
    let model = Arc::new(PendingModel::new());
    let agent = Agent::new(model.clone()).with_limits(RunLimits {
        timeout: Duration::from_millis(300),
        ..RunLimits::default()
    });
    let stream = agent.stream_run("wait");
    tokio::time::timeout(Duration::from_secs(2), model.entered.notified())
        .await
        .unwrap();
    drop(stream);
    tokio::time::timeout(Duration::from_millis(100), model.finished.notified())
        .await
        .expect("stream drop must cancel before the 300ms run timeout");
    assert!(model.dropped.load(Ordering::SeqCst));
}
