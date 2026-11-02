use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
    time::Duration,
};

use async_trait::async_trait;
use futures_util::StreamExt;
use glow_agent::{
    Agent, AgentError, Content, Message, Model, ModelError, ModelRequest, ModelResponse, Role,
    RunLimits, RunRequest, Runtime, Tool, ToolBehavior, ToolContext, ToolError, ToolSpec,
};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

struct ScriptedModel {
    responses: Mutex<VecDeque<Result<ModelResponse, ModelError>>>,
    requests: Mutex<Vec<ModelRequest>>,
}

impl ScriptedModel {
    fn new(responses: impl IntoIterator<Item = ModelResponse>) -> Self {
        Self {
            responses: Mutex::new(responses.into_iter().map(Ok).collect()),
            requests: Mutex::new(Vec::new()),
        }
    }
}

#[async_trait]
impl Model for ScriptedModel {
    async fn generate(&self, request: ModelRequest) -> Result<ModelResponse, ModelError> {
        self.requests.lock().unwrap().push(request);
        self.responses
            .lock()
            .unwrap()
            .pop_front()
            .expect("scripted model ran out of responses")
    }
}

struct AddTool;

#[async_trait]
impl Tool for AddTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "add".into(),
            description: "Add two integers".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "left": { "type": "integer" },
                    "right": { "type": "integer" }
                },
                "required": ["left", "right"]
            }),
            behavior: ToolBehavior {
                read_only: true,
                idempotent: true,
                parallel_safe: true,
            },
        }
    }

    async fn call(&self, _context: ToolContext, input: Value) -> Result<Value, ToolError> {
        let left = input["left"]
            .as_i64()
            .ok_or_else(|| ToolError::new("left must be an integer"))?;
        let right = input["right"]
            .as_i64()
            .ok_or_else(|| ToolError::new("right must be an integer"))?;
        Ok(json!({ "sum": left + right }))
    }
}

fn tool_call_response() -> ModelResponse {
    ModelResponse::new(Message::new(
        Role::Assistant,
        vec![Content::ToolCall {
            id: "call-1".into(),
            name: "add".into(),
            arguments: json!({ "left": 20, "right": 22 }),
        }],
    ))
}

#[tokio::test]
async fn returns_a_direct_model_answer() {
    let model = Arc::new(ScriptedModel::new([ModelResponse::new(
        Message::assistant("hello"),
    )]));
    let agent = Agent::new(model.clone()).instruction("Answer briefly.");

    let output = agent.run("hi").await.unwrap();

    assert_eq!(output.final_text, "hello");
    assert_eq!(output.turns, 1);
    assert_eq!(output.tool_calls, 0);
    assert_eq!(output.messages.len(), 3);
    assert_eq!(
        model.requests.lock().unwrap()[0].messages[0].role,
        Role::System
    );
}

#[tokio::test]
async fn executes_a_tool_and_sends_its_result_to_the_model() {
    let model = Arc::new(ScriptedModel::new([
        tool_call_response(),
        ModelResponse::new(Message::assistant("The answer is 42.")),
    ]));
    let agent = Agent::new(model.clone()).tools([AddTool]);

    let output = Runtime::new()
        .run(
            &agent,
            RunRequest::new(vec![Message::user("What is 20 + 22?")]),
        )
        .await
        .unwrap();

    assert_eq!(output.final_text, "The answer is 42.");
    assert_eq!(output.turns, 2);
    assert_eq!(output.tool_calls, 1);

    let requests = model.requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    let tool_message = requests[1].messages.last().unwrap();
    assert_eq!(tool_message.role, Role::Tool);
    assert_eq!(
        tool_message.content,
        vec![Content::ToolResult {
            call_id: "call-1".into(),
            result: json!({ "sum": 42 }),
            is_error: false,
        }]
    );
}

#[tokio::test]
async fn continues_beyond_the_former_turn_limit() {
    let mut responses = vec![tool_call_response(); 40];
    responses.push(ModelResponse::new(Message::assistant("done")));
    let model = Arc::new(ScriptedModel::new(responses));
    let mut agent = Agent::new(model).with_limits(RunLimits {
        max_tool_calls: 40,
        timeout: Duration::from_secs(5),
    });
    agent.register_tool(Arc::new(AddTool)).unwrap();
    let output = Runtime::new()
        .run(&agent, RunRequest::new(vec![Message::user("keep going")]))
        .await
        .unwrap();
    assert_eq!(output.turns, 41);
    assert_eq!(output.tool_calls, 40);
}

struct PendingModel;

#[async_trait]
impl Model for PendingModel {
    async fn generate(&self, _request: ModelRequest) -> Result<ModelResponse, ModelError> {
        std::future::pending().await
    }
}

#[tokio::test]
async fn supports_cancellation() {
    let agent = Agent::new(Arc::new(PendingModel));
    let cancellation_token = CancellationToken::new();
    cancellation_token.cancel();

    let error = Runtime::new()
        .run(
            &agent,
            RunRequest::new(vec![Message::user("wait")])
                .with_cancellation_token(cancellation_token),
        )
        .await
        .unwrap_err();

    assert!(matches!(error, AgentError::Cancelled));
}

#[tokio::test]
async fn hands_a_task_to_a_named_agent_and_returns_the_result_to_the_parent() {
    let child_model = Arc::new(ScriptedModel::new([ModelResponse::new(
        Message::assistant("42"),
    )]));
    let child = Agent::new(child_model)
        .name("math")
        .description("Solve arithmetic tasks")
        .instruction("Return only the answer.");

    let parent_model = Arc::new(ScriptedModel::new([
        ModelResponse::new(Message::new(
            Role::Assistant,
            vec![Content::ToolCall {
                id: "handoff-1".into(),
                name: "handoff_to_math".into(),
                arguments: json!({ "task": "Calculate 20 + 22" }),
            }],
        )),
        ModelResponse::new(Message::assistant("The math agent returned 42.")),
    ]));
    let parent = Agent::new(parent_model.clone()).handoff([child]);

    let output = parent.run("What is 20 + 22?").await.unwrap();

    assert_eq!(output.final_text, "The math agent returned 42.");
    assert_eq!(output.tool_calls, 1);
    let requests = parent_model.requests.lock().unwrap();
    assert_eq!(requests[0].tools[0].name, "handoff_to_math");
    assert_eq!(
        requests[0].tools[0].input_schema["required"],
        json!(["task"])
    );
    assert_eq!(
        requests[1].messages.last().unwrap().content,
        vec![Content::ToolResult {
            call_id: "handoff-1".into(),
            result: json!({ "agent": "math", "response": "42" }),
            is_error: false,
        }]
    );
}

#[tokio::test]
async fn rejects_an_unnamed_handoff_agent_before_calling_the_model() {
    let child = Agent::new(Arc::new(ScriptedModel::new(Vec::<ModelResponse>::new())));
    let parent_model = Arc::new(ScriptedModel::new(Vec::<ModelResponse>::new()));
    let parent = Agent::new(parent_model.clone()).handoff([child]);

    let error = parent.run("route this").await.unwrap_err();

    assert!(matches!(error, AgentError::InvalidConfiguration(_)));
    assert!(parent_model.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn streams_text_and_lifecycle_events() {
    let model = Arc::new(ScriptedModel::new([ModelResponse::new(
        Message::assistant("streamed answer"),
    )]));
    let agent = Agent::new(model);
    let mut stream = agent.stream_run("hello");
    let mut text = String::new();
    let mut completed = false;

    while let Some(event) = stream.next().await {
        match event.unwrap() {
            glow_agent::RunEvent::TextDelta { text: delta, .. } => text.push_str(&delta),
            glow_agent::RunEvent::Completed { .. } => completed = true,
            _ => {}
        }
    }

    assert_eq!(text, "streamed answer");
    assert!(completed);
}

/// Parallel-safe tool that waits until `expected` calls are in flight at once.
struct RendezvousTool {
    name: &'static str,
    barrier: Arc<tokio::sync::Barrier>,
}

#[async_trait]
impl Tool for RendezvousTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: self.name.into(),
            description: "wait for the other tool".into(),
            input_schema: json!({"type": "object"}),
            behavior: ToolBehavior {
                read_only: true,
                idempotent: true,
                parallel_safe: true,
            },
        }
    }

    async fn call(&self, _context: ToolContext, _input: Value) -> Result<Value, ToolError> {
        self.barrier.wait().await;
        Ok(json!({ "tool": self.name }))
    }
}

fn calls(names: &[&str]) -> ModelResponse {
    ModelResponse::new(Message::new(
        Role::Assistant,
        names
            .iter()
            .enumerate()
            .map(|(index, name)| Content::ToolCall {
                id: format!("call-{index}"),
                name: (*name).into(),
                arguments: json!({}),
            })
            .collect(),
    ))
}

#[tokio::test]
async fn runs_parallel_safe_tools_concurrently_and_keeps_result_order() {
    let barrier = Arc::new(tokio::sync::Barrier::new(2));
    let model = Arc::new(ScriptedModel::new([
        calls(&["first", "second"]),
        ModelResponse::new(Message::assistant("done")),
    ]));
    let agent = Agent::new(model.clone())
        .tool(RendezvousTool {
            name: "first",
            barrier: barrier.clone(),
        })
        .tool(RendezvousTool {
            name: "second",
            barrier,
        })
        .with_limits(RunLimits {
            timeout: Duration::from_secs(2),
            ..RunLimits::default()
        });

    // Sequential execution would deadlock on the barrier and time out.
    let output = agent.run("go").await.unwrap();

    let results: Vec<_> = output
        .messages
        .iter()
        .flat_map(|message| &message.content)
        .filter_map(|content| match content {
            Content::ToolResult {
                call_id, result, ..
            } => Some((call_id.as_str(), result["tool"].clone())),
            _ => None,
        })
        .collect();
    assert_eq!(
        results,
        vec![("call-0", json!("first")), ("call-1", json!("second"))]
    );
}

#[tokio::test]
async fn retries_retryable_model_errors() {
    let model = Arc::new(ScriptedModel {
        responses: Mutex::new(VecDeque::from([
            Err(ModelError::retryable("503")),
            Ok(ModelResponse::new(Message::assistant("recovered"))),
        ])),
        requests: Mutex::new(Vec::new()),
    });
    let agent = Agent::new(model.clone()).with_retry_policy(glow_agent::RetryPolicy {
        max_retries: 1,
        initial_backoff: Duration::from_millis(1),
        max_backoff: Duration::from_millis(1),
    });

    let output = agent.run("hi").await.unwrap();

    assert_eq!(output.final_text, "recovered");
    assert_eq!(model.requests.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn does_not_retry_non_retryable_model_errors() {
    let model = Arc::new(ScriptedModel {
        responses: Mutex::new(VecDeque::from([Err(ModelError::new("bad request"))])),
        requests: Mutex::new(Vec::new()),
    });
    let agent = Agent::new(model.clone());

    let error = agent.run("hi").await.unwrap_err();

    assert!(matches!(error, AgentError::Model(_)));
    assert_eq!(model.requests.lock().unwrap().len(), 1);
}

struct DenyAdd;

#[async_trait]
impl glow_agent::Policy for DenyAdd {
    async fn authorize(
        &self,
        _context: &ToolContext,
        tool: &ToolSpec,
        _arguments: &Value,
    ) -> Result<(), glow_agent::PolicyError> {
        if tool.name == "add" {
            return Err(glow_agent::PolicyError::new("add is not allowed"));
        }
        Ok(())
    }
}

#[tokio::test]
async fn can_report_policy_denials_to_the_model() {
    let model = Arc::new(ScriptedModel::new([
        tool_call_response(),
        ModelResponse::new(Message::assistant("I may not add.")),
    ]));
    let agent = Agent::new(model.clone())
        .tools([AddTool])
        .with_policy(Arc::new(DenyAdd))
        .on_policy_denial(glow_agent::PolicyDenial::ReportToModel);

    let output = agent.run("What is 20 + 22?").await.unwrap();

    assert_eq!(output.final_text, "I may not add.");
    let requests = model.requests.lock().unwrap();
    assert_eq!(
        requests[1].messages.last().unwrap().content,
        vec![Content::ToolResult {
            call_id: "call-1".into(),
            result: json!({
                "error": { "code": "policy_denied", "message": "add is not allowed" }
            }),
            is_error: true,
        }]
    );
}

#[tokio::test]
async fn stream_ends_with_the_run_output_and_events_carry_the_run_id() {
    let model = Arc::new(ScriptedModel::new([
        tool_call_response(),
        ModelResponse::new(Message::assistant("42")),
    ]));
    let agent = Agent::new(model).tools([AddTool]);
    let events: Vec<_> = agent
        .stream_run("What is 20 + 22?")
        .map(Result::unwrap)
        .collect()
        .await;

    let glow_agent::RunEvent::Started { run_id } = &events[0] else {
        panic!("first event must be Started");
    };
    assert!(events.iter().any(|event| matches!(
        event,
        glow_agent::RunEvent::ToolCompleted { run_id: id, .. } if id == run_id
    )));
    let Some(glow_agent::RunEvent::Finished { output }) = events.last() else {
        panic!("last event must be Finished");
    };
    assert_eq!(output.run_id, *run_id);
    assert_eq!(output.final_text, "42");
    assert_eq!(output.conversation_messages.len(), 4);
}

#[tokio::test]
async fn handoff_is_authorized_with_a_handoff_origin() {
    struct RecordOrigin(Mutex<Vec<glow_agent::ToolOrigin>>);
    #[async_trait]
    impl glow_agent::Policy for RecordOrigin {
        async fn authorize(
            &self,
            _: &ToolContext,
            _: &ToolSpec,
            _: &Value,
        ) -> Result<(), glow_agent::PolicyError> {
            Ok(())
        }
        async fn authorize_extension(
            &self,
            _: &ToolContext,
            _: &ToolSpec,
            _: &Value,
            origin: &glow_agent::ToolOrigin,
            _: &glow_agent::InvocationScope,
        ) -> Result<(), glow_agent::PolicyError> {
            self.0.lock().unwrap().push(origin.clone());
            Ok(())
        }
    }

    let child = Agent::new(Arc::new(ScriptedModel::new([ModelResponse::new(
        Message::assistant("42"),
    )])))
    .name("math");
    let parent_model = Arc::new(ScriptedModel::new([
        ModelResponse::new(Message::new(
            Role::Assistant,
            vec![Content::ToolCall {
                id: "handoff-1".into(),
                name: "handoff_to_math".into(),
                arguments: json!({ "task": "Calculate 20 + 22" }),
            }],
        )),
        ModelResponse::new(Message::assistant("42")),
    ]));
    let policy = Arc::new(RecordOrigin(Mutex::new(Vec::new())));
    let parent = Agent::new(parent_model)
        .handoff([child])
        .with_policy(policy.clone());

    parent.run("What is 20 + 22?").await.unwrap();

    assert_eq!(
        *policy.0.lock().unwrap(),
        vec![glow_agent::ToolOrigin::Handoff {
            agent: "math".into()
        }]
    );
}
