use async_trait::async_trait;
use glow_agent::*;
use serde_json::{Value, json};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};

struct Backend {
    submitted: AtomicUsize,
    ready: AtomicBool,
    acknowledged: AtomicUsize,
}
#[async_trait]
impl TaskBackend for Backend {
    async fn submit(&self, tasks: Vec<TaskSubmission>) -> Result<Vec<String>, ToolError> {
        self.submitted.fetch_add(tasks.len(), Ordering::SeqCst);
        Ok(tasks
            .iter()
            .enumerate()
            .map(|(i, _)| format!("t{i}"))
            .collect())
    }
    async fn control(&self, operation: &str, _: Value) -> Result<Value, ToolError> {
        if operation == "detach_tools" {
            self.ready.store(true, Ordering::SeqCst);
        }
        Ok(
            json!({"completed":[{"id":"t0","result":42}],"pending":[{"id":"t1","status":"running"}]}),
        )
    }
    async fn acknowledge(&self, _: &Value) -> Result<(), ToolError> {
        self.acknowledged.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
    async fn ready_to_finish(&self) -> Result<bool, ToolError> {
        Ok(self.ready.load(Ordering::SeqCst))
    }
}
struct Lookup;
#[async_trait]
impl Tool for Lookup {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "lookup".into(),
            description: String::new(),
            input_schema: json!({"type":"object","properties":{"id":{"type":"integer"}},"required":["id"]}),
            behavior: ToolBehavior::default(),
        }
    }
    async fn call(&self, _: ToolContext, _: Value) -> Result<Value, ToolError> {
        panic!("backend must own task execution")
    }
}
struct Script {
    responses: Mutex<std::collections::VecDeque<Message>>,
    requests: Mutex<Vec<ModelRequest>>,
}
#[async_trait]
impl Model for Script {
    async fn generate(&self, request: ModelRequest) -> Result<ModelResponse, ModelError> {
        self.requests.lock().unwrap().push(request);
        Ok(ModelResponse::new(
            self.responses
                .lock()
                .unwrap()
                .pop_front()
                .expect("unexpected model turn"),
        ))
    }
}
fn call(id: &str, name: &str, args: Value) -> Message {
    Message::new(
        Role::Assistant,
        vec![Content::ToolCall {
            id: id.into(),
            name: name.into(),
            arguments: args,
        }],
    )
}
fn backend() -> Arc<Backend> {
    Arc::new(Backend {
        submitted: AtomicUsize::new(0),
        ready: AtomicBool::new(false),
        acknowledged: AtomicUsize::new(0),
    })
}
fn model(responses: Vec<Message>) -> Arc<Script> {
    Arc::new(Script {
        responses: Mutex::new(responses.into()),
        requests: Mutex::new(Vec::new()),
    })
}

#[tokio::test]
async fn handoff_binds_only_the_childs_explicit_backend() {
    struct Factory {
        calls: Mutex<Vec<(String, String)>>,
        child: Arc<Backend>,
    }
    #[async_trait]
    impl TaskBackend for Factory {
        fn for_handoff(&self, agent: &str, call_id: &str) -> Option<Arc<dyn TaskBackend>> {
            self.calls
                .lock()
                .unwrap()
                .push((agent.into(), call_id.into()));
            Some(self.child.clone())
        }
        async fn submit(&self, _: Vec<TaskSubmission>) -> Result<Vec<String>, ToolError> {
            panic!("unbound child backend used")
        }
        async fn control(&self, _: &str, _: Value) -> Result<Value, ToolError> {
            panic!("unbound child backend used")
        }
        async fn ready_to_finish(&self) -> Result<bool, ToolError> {
            panic!("unbound child backend used")
        }
    }
    let scoped = backend();
    scoped.ready.store(true, Ordering::SeqCst);
    let factory = Arc::new(Factory {
        calls: Mutex::new(vec![]),
        child: scoped.clone(),
    });
    let parent_backend = backend();
    parent_backend.ready.store(true, Ordering::SeqCst);
    let worker_model = model(vec![
        call(
            "batch",
            "lookup",
            json!({"id":1}),
        ),
        Message::assistant("checked"),
    ]);
    let child = Agent::new(worker_model.clone())
        .name("programmer")
        .tool(Lookup)
        .with_task_backend(factory.clone());
    let plain_model = model(vec![Message::assistant("plain answer")]);
    let parent = Agent::new(model(vec![
        call("h1", "handoff_to_programmer", json!({"task":"work"})),
        call("h2", "handoff_to_plain", json!({"task":"read"})),
        Message::assistant("done"),
    ]))
    .with_task_backend(parent_backend.clone())
    .handoff([child, Agent::new(plain_model.clone()).name("plain")]);
    assert_eq!(parent.run("start").await.unwrap().final_text, "done");
    assert_eq!(
        *factory.calls.lock().unwrap(),
        [("programmer".into(), "h1".into())]
    );
    assert_eq!(scoped.submitted.load(Ordering::SeqCst), 1);
    assert_eq!(scoped.acknowledged.load(Ordering::SeqCst), 1);
    assert_eq!(parent_backend.submitted.load(Ordering::SeqCst), 0);
    assert!(!worker_model.requests.lock().unwrap()[0]
        .tools
        .iter()
        .any(|tool| tool.name == "run_tools"));
    assert!(
        !plain_model.requests.lock().unwrap()[0]
            .tools
            .iter()
            .any(|tool| tool.name == "run_tools")
    );
}

#[tokio::test]
async fn receives_partial_results_then_detaches_before_finishing() {
    let backend = backend();
    let model = model(vec![
        call(
            "batch",
            "run_tools",
            json!({"calls":[{"tool":"lookup","arguments":{"id":1},"title":"查询记录 1"},{"tool":"lookup","arguments":{"id":2},"title":"查询记录 2"}]}),
        ),
        Message::assistant("progress"),
        call("detach", "detach_tools", json!({"task_ids":["t1"]})),
        Message::assistant("done"),
    ]);
    let agent = Agent::new(model.clone())
        .tool(Lookup)
        .with_task_backend(backend.clone());
    let output = Runtime::new()
        .run(&agent, RunRequest::new(vec![Message::user("work")]))
        .await
        .unwrap();
    assert_eq!(output.final_text, "done");
    assert_eq!(output.turns, 4);
    assert_eq!(output.tool_calls, 4); // Two controls plus two real tools.
    assert_eq!(backend.submitted.load(Ordering::SeqCst), 2);
    assert_eq!(backend.acknowledged.load(Ordering::SeqCst), 2);
    let requests = model.requests.lock().unwrap();
    assert!(requests[1].messages.iter().flat_map(|m|&m.content).any(|part|matches!(part,Content::ToolResult{call_id,result,..} if call_id=="batch" && result["pending"][0]["id"]=="t1")));
    assert!(
        requests[2]
            .messages
            .last()
            .unwrap()
            .text_content()
            .contains("not a final reply")
    );
}

#[tokio::test]
async fn invalid_workflow_titles_can_be_corrected_before_submission() {
    let mut invalid_calls = vec![json!({"tool":"lookup","arguments":{"id":2}})];
    for title in [
        Value::Null,
        json!(""),
        json!(" \t\n\u{3000}"),
        json!("检".repeat(201)),
    ] {
        invalid_calls.push(json!({"tool":"lookup","arguments":{"id":2},"title":title}));
    }
    for invalid in invalid_calls {
        let backend = backend();
        backend.ready.store(true, Ordering::SeqCst);
        let valid = json!({"tool":"lookup","arguments":{"id":1},"title":"检查 RDP 会话状态"});
        let model = model(vec![
            call(
                "invalid",
                "run_tools",
                json!({"calls":[valid.clone(), invalid]}),
            ),
            call("corrected", "run_tools", json!({"calls":[valid]})),
            Message::assistant("checked"),
        ]);
        let output = Agent::new(model.clone())
            .tool(Lookup)
            .with_task_backend(backend.clone())
            .run("检查会话")
            .await
            .unwrap();
        assert_eq!(output.final_text, "checked");
        // The first batch must not partially submit its valid call.
        assert_eq!(backend.submitted.load(Ordering::SeqCst), 1);
        assert!(model.requests.lock().unwrap()[1].messages.iter()
            .flat_map(|message| &message.content)
            .any(|content| matches!(content, Content::ToolResult { call_id, is_error: true, .. } if call_id == "invalid")));
    }
}

#[tokio::test]
async fn malformed_nested_tool_never_reaches_backend() {
    let backend = backend();
    backend.ready.store(true, Ordering::SeqCst);
    let model = model(vec![
        call(
            "batch",
            "run_tools",
            json!({"calls":[{"tool":"lookup","arguments":{"id":"bad"},"title":"查询记录"}]}),
        ),
        Message::assistant("invalid"),
    ]);
    Runtime::new()
        .run(
            &Agent::new(model.clone())
                .tool(Lookup)
                .with_task_backend(backend.clone()),
            RunRequest::new(vec![Message::user("work")]),
        )
        .await
        .unwrap();
    assert_eq!(backend.submitted.load(Ordering::SeqCst), 0);
    assert!(
        model.requests.lock().unwrap()[1]
            .messages
            .iter()
            .flat_map(|m| &m.content)
            .any(|c| matches!(c, Content::ToolResult { is_error: true, .. }))
    );
}

#[tokio::test]
async fn nested_tools_are_charged_before_submission() {
    let backend = backend();
    let model = model(vec![call(
        "batch",
        "run_tools",
        json!({"calls":[{"tool":"lookup","arguments":{"id":1},"title":"查询记录 1"}]}),
    )]);
    let result = Runtime::new()
        .run(
            &Agent::new(model)
                .tool(Lookup)
                .with_task_backend(backend.clone())
                .with_limits(RunLimits {
                    max_tool_calls: 1,
                    ..Default::default()
                }),
            RunRequest::new(vec![Message::user("work")]),
        )
        .await;
    assert!(matches!(result, Err(AgentError::MaxToolCallsExceeded(1))));
    assert_eq!(backend.submitted.load(Ordering::SeqCst), 0);
}

struct DenyLookup;
#[async_trait]
impl Policy for DenyLookup {
    async fn authorize(
        &self,
        _: &ToolContext,
        spec: &ToolSpec,
        _: &Value,
    ) -> Result<(), PolicyError> {
        if spec.name == "lookup" {
            Err(PolicyError::new("denied"))
        } else {
            Ok(())
        }
    }
}
#[tokio::test]
async fn task_wrapper_cannot_bypass_original_tool_policy() {
    let backend = backend();
    let model = model(vec![call(
        "batch",
        "run_tools",
        json!({"calls":[{"tool":"lookup","arguments":{"id":1},"title":"查询记录 1"}]}),
    )]);
    let result = Runtime::new()
        .run(
            &Agent::new(model)
                .tool(Lookup)
                .with_policy(Arc::new(DenyLookup))
                .with_task_backend(backend.clone()),
            RunRequest::new(vec![Message::user("work")]),
        )
        .await;
    assert!(matches!(result, Err(AgentError::Policy(_))));
    assert_eq!(backend.submitted.load(Ordering::SeqCst), 0);
}
