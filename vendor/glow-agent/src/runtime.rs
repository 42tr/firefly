use std::{
    collections::HashMap,
    future::Future,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use async_trait::async_trait;
use serde_json::{Value, json};
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::{
    Agent, AgentError, Content, EventSink, Message, ModelEvent, ModelEventSink, ModelRequest,
    ModelResponse, PolicyDenial, PolicyError, Role, RunEvent, Tool, ToolContext, ToolOrigin,
    ToolSpec, Usage,
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum InstructionsMode {
    #[default]
    Compose,
    Preserve,
}

#[derive(Clone, Debug)]
pub struct RunRequest {
    pub messages: Vec<Message>,
    pub cancellation_token: CancellationToken,
    pub extensions: Arc<crate::ExtensionConfig>,
    pub scope: crate::InvocationScope,
    pub selected_skills: Vec<String>,
    pub instructions_mode: InstructionsMode,
}

impl RunRequest {
    pub fn new(messages: Vec<Message>) -> Self {
        Self {
            messages,
            cancellation_token: CancellationToken::new(),
            extensions: Arc::new(crate::ExtensionConfig::default()),
            scope: crate::InvocationScope::default(),
            selected_skills: Vec::new(),
            instructions_mode: InstructionsMode::Compose,
        }
    }

    pub fn with_extensions(mut self, config: Arc<crate::ExtensionConfig>) -> Self {
        self.extensions = config;
        self
    }
    pub fn with_scope(mut self, scope: crate::InvocationScope) -> Self {
        self.scope = scope;
        self
    }
    pub fn with_selected_skills(
        mut self,
        names: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        self.selected_skills = names.into_iter().map(Into::into).collect();
        self
    }
    pub fn with_instructions_mode(mut self, mode: InstructionsMode) -> Self {
        self.instructions_mode = mode;
        self
    }

    pub fn with_cancellation_token(mut self, cancellation_token: CancellationToken) -> Self {
        self.cancellation_token = cancellation_token;
        self
    }
}

#[derive(Clone, Debug)]
pub struct RunOutput {
    pub run_id: Uuid,
    pub messages: Vec<Message>,
    pub conversation_messages: Vec<Message>,
    pub context_manifest: crate::ContextManifest,
    pub final_text: String,
    pub turns: usize,
    pub tool_calls: usize,
    pub usage: Usage,
}

#[derive(Clone, Debug, Default)]
pub struct Runtime {
    #[cfg(feature = "mcp")]
    mcp: Option<Arc<crate::mcp::McpManager>>,
}

impl Runtime {
    pub fn new() -> Self {
        Self::default()
    }

    #[cfg(feature = "mcp")]
    pub fn with_mcp_manager(mut self, manager: Arc<crate::mcp::McpManager>) -> Self {
        self.mcp = Some(manager);
        self
    }

    /// Stream a full request, including its extension configuration. Dropping the
    /// stream cancels this run without cancelling the caller's shared token. A
    /// successful run ends with `RunEvent::Finished` carrying the `RunOutput`.
    pub fn stream_run(
        &self,
        agent: &Agent,
        mut request: RunRequest,
    ) -> impl futures_util::Stream<Item = Result<RunEvent, AgentError>> + Send + Unpin + 'static + use<>
    {
        let (sender, receiver) = tokio::sync::mpsc::channel(64);
        let token = request.cancellation_token.child_token();
        request.cancellation_token = token.clone();
        let mut agent = agent.clone();
        agent.event_sink = Arc::new(crate::agent::StreamEventSink {
            sender: sender.clone(),
            downstream: agent.event_sink.clone(),
            cancellation_token: token.clone(),
        });
        let runtime = self.clone();
        tokio::spawn(async move {
            let last = runtime
                .run(&agent, request)
                .await
                .map(|output| RunEvent::Finished {
                    output: Box::new(output),
                });
            let _ = sender.send(last).await;
        });
        crate::agent::CancelOnDropStream {
            inner: tokio_stream::wrappers::ReceiverStream::new(receiver),
            token,
        }
    }

    /// Warm up exactly the request configuration; never invokes the model or tools.
    pub async fn warmup(
        &self,
        agent: &Agent,
        mut request: RunRequest,
    ) -> Result<crate::WarmupReport, AgentError> {
        let token = begin(agent, &mut request)?;
        guarded(agent, &token, async {
            let prepared = self.prepare(agent, &request, None).await?;
            Ok(crate::WarmupReport {
                tools: prepared.agent.tool_specs(),
                diagnostics: prepared.diagnostics,
            })
        })
        .await
    }

    pub async fn run(&self, agent: &Agent, request: RunRequest) -> Result<RunOutput, AgentError> {
        self.run_with(agent, request, None).await
    }

    /// `mcp_tools` lets handoff children reuse the tools their parent already discovered
    /// for the same extension configuration and scope.
    async fn run_with(
        &self,
        agent: &Agent,
        mut request: RunRequest,
        mcp_tools: Option<McpTools>,
    ) -> Result<RunOutput, AgentError> {
        let token = begin(agent, &mut request)?;
        guarded(agent, &token, self.run_inner(agent, request, mcp_tools)).await
    }

    async fn run_inner(
        &self,
        agent: &Agent,
        mut request: RunRequest,
        mcp_tools: Option<McpTools>,
    ) -> Result<RunOutput, AgentError> {
        let prepared = self.prepare(agent, &request, mcp_tools).await?;
        // `RunContext` borrows the request, so move the input messages out first.
        let mut input = std::mem::take(&mut request.messages);
        let tool_specs: Arc<[ToolSpec]> = prepared.agent.tool_specs().into();
        let run = RunContext {
            runtime: self,
            agent: &prepared.agent,
            run_id: Uuid::new_v4(),
            request: &request,
            spec_index: tool_specs
                .iter()
                .enumerate()
                .map(|(index, spec)| (spec.name.clone(), index))
                .collect(),
            tool_specs: tool_specs.clone(),
            mcp_tools: prepared.mcp_tools.clone(),
        };

        let mut history = match request.instructions_mode {
            InstructionsMode::Compose => run.compose_prefix(&prepared.skills).await?,
            InstructionsMode::Preserve => Vec::new(),
        };
        let injected = history.len();
        if agent.task_backend.is_some() {
            history.push(Message::system(crate::TASK_INSTRUCTION));
        }
        let run_id = run.run_id;
        let agent = run.agent;
        history.append(&mut input);
        let mut history = Arc::new(history);
        let mut usage = Usage::default();
        let mut tool_calls = 0;

        tracing::info!(%run_id, "agent run started");
        run.emit(RunEvent::Started { run_id }).await;

        let mut turn = 0usize;
        loop {
            turn = turn.saturating_add(1);
            tracing::debug!(%run_id, turn, "requesting model response");
            run.emit(RunEvent::ModelStarted { run_id, turn }).await;
            let response = run
                .call_model(turn, ModelRequest::new(history.clone(), tool_specs.clone()))
                .await?;
            if response.message.role != Role::Assistant {
                return Err(AgentError::InvalidModelResponse(response.message.role));
            }
            usage.add(response.usage);
            run.emit(RunEvent::ModelCompleted {
                run_id,
                turn,
                message: response.message.clone(),
                usage: response.usage,
            })
            .await;

            let calls = tool_calls_of(&response.message);
            let final_text = response.message.text_content();
            // Cheap unless a model kept its request alive; then this copies once.
            let messages = Arc::make_mut(&mut history);
            messages.push(response.message);

            if calls.is_empty() {
                if let Some(backend) = &agent.task_backend {
                    if !backend
                        .ready_to_finish()
                        .await
                        .map_err(|e| crate::extensions::invalid(&e.message))?
                    {
                        let blockers = backend
                            .finish_blockers()
                            .await
                            .map_err(|e| crate::extensions::invalid(&e.message))?;
                        messages.push(Message::system(format!(
                            "Tasks remain attached or required results have not been received. Handle only these blockers using their exact task_ids: wait_tools/get_tasks to receive results, detach_tools for authorized background work, or cancel_tools for unnecessary attached tasks. Preserve already detached sessions needed for user confirmation; do not clean up unrelated historical tasks. If has_more is true, handle this batch then inspect remaining blockers. Your preceding text is a progress update, not a final reply. Completion blockers: {blockers}"
                        )));
                        continue;
                    }
                }
                let messages = Arc::try_unwrap(history).unwrap_or_else(|shared| (*shared).clone());
                return Ok(run
                    .finish(
                        messages, injected, &prepared, final_text, turn, tool_calls, usage,
                    )
                    .await);
            }

            tracing::debug!(%run_id, turn, tool_calls = calls.len(), "model requested tools");
            let charged = calls
                .iter()
                .map(|call| {
                    1 + if agent.task_backend.is_some() && call.name == "run_tools" {
                        call.arguments
                            .get("calls")
                            .and_then(Value::as_array)
                            .map_or(0, Vec::len)
                    } else {
                        0
                    }
                })
                .sum::<usize>();
            if tool_calls.saturating_add(charged) > agent.limits.max_tool_calls {
                tracing::warn!(
                    %run_id,
                    limit = agent.limits.max_tool_calls,
                    "maximum tool call limit exceeded"
                );
                return Err(AgentError::MaxToolCallsExceeded(
                    agent.limits.max_tool_calls,
                ));
            }
            tool_calls += charged;
            run.execute_calls(calls, &mut usage, messages).await?;
        }
    }
}

/// Validate the request and give it a child token owned by this run.
fn begin(agent: &Agent, request: &mut RunRequest) -> Result<CancellationToken, AgentError> {
    agent.validate()?;
    request.extensions.validate()?;
    let token = request.cancellation_token.child_token();
    request.cancellation_token = token.clone();
    Ok(token)
}

/// Run `future` under the agent's timeout, stopping early on cancellation. The token
/// is cancelled when this returns, which stops anything the run left behind.
async fn guarded<T>(
    agent: &Agent,
    token: &CancellationToken,
    future: impl Future<Output = Result<T, AgentError>>,
) -> Result<T, AgentError> {
    let _guard = token.clone().drop_guard();
    tokio::select! {
        biased;
        _ = token.cancelled() => {
            tracing::warn!("agent run cancelled");
            Err(AgentError::Cancelled)
        },
        result = timeout(agent.limits.timeout, future) => result.unwrap_or_else(|_| {
            tracing::warn!(timeout_ms = agent.limits.timeout.as_millis(), "agent run timed out");
            Err(AgentError::Timeout)
        }),
    }
}

#[derive(Clone)]
struct ToolCall {
    id: String,
    name: String,
    arguments: Value,
}

fn tool_calls_of(message: &Message) -> Vec<ToolCall> {
    message
        .content
        .iter()
        .filter_map(|content| match content {
            Content::ToolCall {
                id,
                name,
                arguments,
            } => Some(ToolCall {
                id: id.clone(),
                name: name.clone(),
                arguments: arguments.clone(),
            }),
            _ => None,
        })
        .collect()
}

/// Per-run state shared by the helpers below.
struct RunContext<'a> {
    runtime: &'a Runtime,
    agent: &'a Agent,
    run_id: Uuid,
    request: &'a RunRequest,
    tool_specs: Arc<[ToolSpec]>,
    spec_index: HashMap<String, usize>,
    mcp_tools: McpTools,
}

impl RunContext<'_> {
    fn tool_context(&self) -> ToolContext {
        ToolContext {
            run_id: self.run_id,
            cancellation_token: self.request.cancellation_token.clone(),
        }
    }

    async fn emit(&self, event: RunEvent) {
        self.agent.event_sink.emit(event).await;
    }

    fn spec(&self, name: &str) -> Option<&ToolSpec> {
        self.spec_index
            .get(name)
            .map(|&index| &self.tool_specs[index])
    }

    fn is_parallel_tool(&self, name: &str) -> bool {
        self.agent.tools.contains_key(name)
            && self
                .spec(name)
                .is_some_and(|spec| spec.behavior.parallel_safe)
    }

    /// `Ok(None)` if allowed, `Ok(Some(result))` if denied and the denial goes back to
    /// the model, `Err` if denied and the run must stop.
    async fn authorize(
        &self,
        spec: &ToolSpec,
        arguments: &Value,
        origin: &ToolOrigin,
    ) -> Result<Option<Value>, AgentError> {
        let decision = self
            .agent
            .policy
            .authorize_extension(
                &self.tool_context(),
                spec,
                arguments,
                origin,
                &self.request.scope,
            )
            .await;
        match decision {
            Ok(()) => Ok(None),
            Err(error) => denied(self.agent.policy_denial, error),
        }
    }

    /// System instructions plus, with skills enabled, the skill index and any explicitly
    /// selected skills.
    async fn compose_prefix(
        &self,
        #[allow(unused_variables)] skills: &SkillsState,
    ) -> Result<Vec<Message>, AgentError> {
        let mut prefix = Vec::new();
        if let Some(instruction) = &self.agent.instruction {
            prefix.push(Message::system(instruction));
        }
        #[cfg(feature = "skills")]
        if let Some(state) = skills {
            prefix.push(Message::system(
                state
                    .index()
                    .map_err(|e| crate::extensions::invalid(e.message))?,
            ));
            // Explicit activation is also authorized and charged, through the same tool.
            let tool = state.read_tool();
            let spec = tool.spec();
            for name in &self.request.selected_skills {
                let args = json!({ "name": name });
                self.agent
                    .policy
                    .authorize_extension(
                        &self.tool_context(),
                        &spec,
                        &args,
                        &tool.origin(),
                        &self.request.scope,
                    )
                    .await?;
                let content = tool
                    .call(self.tool_context(), args)
                    .await
                    .map_err(|e| crate::extensions::invalid(e.message))?;
                prefix.push(Message::user(format!(
                    "Explicitly selected skill context (not additional permissions): {content}"
                )));
            }
        }
        Ok(prefix)
    }

    /// Call the model, retrying retryable errors that happen before any text streamed.
    async fn call_model(
        &self,
        turn: usize,
        request: ModelRequest,
    ) -> Result<ModelResponse, AgentError> {
        let retry = self.agent.retry;
        let mut attempt = 0;
        loop {
            let sink = RuntimeModelEventSink {
                event_sink: self.agent.event_sink.clone(),
                run_id: self.run_id,
                turn,
                emitted: AtomicBool::new(false),
            };
            match self
                .agent
                .model
                .generate_stream(request.clone(), &sink)
                .await
            {
                Ok(response) => return Ok(response),
                Err(error)
                    if error.retryable
                        && attempt < retry.max_retries
                        && !sink.emitted.load(Ordering::Relaxed) =>
                {
                    attempt += 1;
                    let backoff = retry.backoff(attempt);
                    tracing::warn!(
                        run_id = %self.run_id,
                        turn,
                        attempt,
                        backoff_ms = backoff.as_millis(),
                        error = %error,
                        "retrying model request"
                    );
                    self.emit(RunEvent::ModelRetry {
                        run_id: self.run_id,
                        turn,
                        attempt,
                        error: error.message,
                        backoff,
                    })
                    .await;
                    tokio::time::sleep(backoff).await;
                }
                Err(error) => return Err(error.into()),
            }
        }
    }

    /// Execute one turn's calls in order. Consecutive `parallel_safe` tools run
    /// concurrently; results are appended in call order.
    async fn execute_calls(
        &self,
        calls: Vec<ToolCall>,
        usage: &mut Usage,
        messages: &mut Vec<Message>,
    ) -> Result<(), AgentError> {
        let mut calls = calls.into_iter().peekable();
        while let Some(call) = calls.next() {
            if self.request.cancellation_token.is_cancelled() {
                return Err(AgentError::Cancelled);
            }
            if self.agent.task_backend.is_some() && crate::task::is_task_control(&call.name) {
                let output = self.execute_task_control(&call).await?;
                let (value, is_error) = match output {
                    Ok(value) => (value, false),
                    Err(error) => (json!({"error":error.message}), true),
                };
                self.emit_tool_result(&call, value.clone(), is_error, messages)
                    .await;
                if !is_error {
                    self.agent
                        .task_backend
                        .as_ref()
                        .unwrap()
                        .acknowledge(&value)
                        .await
                        .map_err(|e| crate::extensions::invalid(&e.message))?;
                }
                continue;
            }
            if let Some(child) = self.agent.handoff_by_tool_name(&call.name) {
                self.execute_handoff(child, call, usage, messages).await?;
                continue;
            }
            let Some(tool) = self.agent.tools.get(&call.name) else {
                tracing::warn!(
                    run_id = %self.run_id,
                    call_id = %call.id,
                    tool = %call.name,
                    "model requested unknown tool"
                );
                let result = json!({ "error": format!("unknown tool: {}", call.name) });
                self.emit_tool_result(&call, result, true, messages).await;
                continue;
            };
            if self.agent.task_backend.is_some() {
                let mut batch = vec![(call, tool.clone())];
                while let Some(next) = calls.next_if(|next| {
                    !crate::task::is_task_control(&next.name)
                        && self.agent.handoff_by_tool_name(&next.name).is_none()
                        && self.agent.tools.contains_key(&next.name)
                }) {
                    let tool = self.agent.tools[&next.name].clone();
                    batch.push((next, tool));
                }
                self.execute_direct_tasks(batch, messages).await?;
                continue;
            }
            let parallel = self.is_parallel_tool(&call.name);
            let mut batch = vec![(call, tool.clone())];
            if parallel {
                while let Some(next) = calls.next_if(|next| self.is_parallel_tool(&next.name)) {
                    let tool = self.agent.tools[&next.name].clone();
                    batch.push((next, tool));
                }
            }
            self.execute_tools(batch, messages).await?;
        }
        Ok(())
    }

    async fn execute_task_control(
        &self,
        call: &ToolCall,
    ) -> Result<Result<Value, crate::ToolError>, AgentError> {
        let backend = self.agent.task_backend.as_ref().expect("task backend");
        let legacy_spec;
        let spec = if call.name == "run_tools" {
            legacy_spec = crate::task::legacy_run_tools_spec();
            &legacy_spec
        } else {
            self.spec(&call.name).expect("task control spec")
        };
        if let Some(result) = self
            .authorize(spec, &call.arguments, &ToolOrigin::Local)
            .await?
        {
            return Ok(Err(crate::ToolError::new(result.to_string())));
        }
        let validate = |spec: &ToolSpec, args: &Value| -> Result<(), crate::ToolError> {
            let validator = jsonschema::validator_for(&spec.input_schema)
                .map_err(|e| crate::ToolError::new(e.to_string()))?;
            validator
                .validate(args)
                .map_err(|e| crate::ToolError::new(e.to_string()))
        };
        if let Err(e) = validate(spec, &call.arguments) {
            return Ok(Err(e));
        }
        if call.name != "run_tools" {
            return Ok(backend.control(&call.name, call.arguments.clone()).await);
        }
        let requests: Vec<crate::TaskCall> =
            match serde_json::from_value(call.arguments["calls"].clone()) {
                Ok(calls) => calls,
                Err(e) => return Ok(Err(crate::ToolError::new(e.to_string()))),
            };
        let mut submissions = Vec::new();
        for (index, request) in requests.into_iter().enumerate() {
            if request
                .title
                .as_deref()
                .unwrap_or_default()
                .trim()
                .is_empty()
            {
                return Ok(Err(crate::ToolError::new(
                    "Every task must include a nonblank title describing its action and target",
                )));
            }
            let Some(tool) = self.agent.tools.get(&request.tool) else {
                return Ok(Err(crate::ToolError::new(format!(
                    "Unknown task tool: {}",
                    request.tool
                ))));
            };
            if crate::task::is_task_control(&request.tool) {
                return Ok(Err(crate::ToolError::new(
                    "Recursive task controls are forbidden",
                )));
            }
            let spec = self.spec(&request.tool).expect("registered tool");
            if let Err(e) = validate(spec, &request.arguments) {
                return Ok(Err(e));
            }
            if let Some(result) = self
                .authorize(spec, &request.arguments, &tool.origin())
                .await?
            {
                return Ok(Err(crate::ToolError::new(result.to_string())));
            }
            submissions.push(crate::TaskSubmission {
                run_id: self.run_id,
                call_id: call.id.clone(),
                index,
                request,
                tool: tool.clone(),
            });
        }
        let ids = match backend.submit(submissions).await {
            Ok(ids) => ids,
            Err(e) => return Ok(Err(e)),
        };
        let mode = call.arguments["yield_when"].as_str().unwrap_or("any");
        let operation = if mode == "none" {
            "get_tasks"
        } else {
            "wait_tools"
        };
        Ok(backend
            .control(
                operation,
                json!({"task_ids":ids,"yield_when":mode,
            "wait_timeout_secs":call.arguments["wait_timeout_secs"].as_u64().unwrap_or(30)}),
            )
            .await)
    }

    async fn execute_direct_tasks(
        &self,
        batch: Vec<(ToolCall, Arc<dyn Tool>)>,
        messages: &mut Vec<Message>,
    ) -> Result<(), AgentError> {
        let backend = self.agent.task_backend.as_ref().expect("task backend");
        let all_calls = batch.clone();
        let mut results = vec![None; batch.len()];
        let mut pending = Vec::new();
        let mut acknowledgement = None;
        for (index, (call, tool)) in batch.into_iter().enumerate() {
            let spec = self.spec(&call.name).expect("registered tool spec");
            if let Some(result) = self.authorize(spec, &call.arguments, &tool.origin()).await? {
                results[index] = Some((result, true));
                continue;
            }
            let validator = jsonschema::validator_for(&spec.input_schema)
                .map_err(|e| crate::extensions::invalid(e.to_string()))?;
            if let Err(error) = validator.validate(&call.arguments) {
                results[index] = Some((json!({"error": error.to_string()}), true));
                continue;
            }
            self.emit(RunEvent::ToolStarted {
                run_id: self.run_id,
                call_id: call.id.clone(),
                name: call.name.clone(),
            })
            .await;
            pending.push((index, call, tool));
        }

        if !pending.is_empty() {
            let submissions = pending
                .iter()
                .map(|(index, call, tool)| crate::TaskSubmission {
                    run_id: self.run_id,
                    call_id: call.id.clone(),
                    index: *index,
                    request: crate::TaskCall {
                        tool: call.name.clone(),
                        arguments: call.arguments.clone(),
                        title: None,
                        parent_task_id: None,
                        execution_timeout_secs: None,
                    },
                    tool: tool.clone(),
                })
                .collect();
            match backend.submit(submissions).await {
                Ok(ids) => {
                    // shortcut: direct calls wait 30s; use wait_tools for longer tasks.
                    let snapshot = backend
                        .control(
                            "wait_tools",
                            json!({"task_ids":ids,"yield_when":"all","wait_timeout_secs":30}),
                        )
                        .await;
                    match snapshot {
                        Ok(snapshot) => {
                            for ((index, _, _), task_id) in pending.iter().zip(ids.iter()) {
                                let entry = ["completed", "pending"]
                                    .iter()
                                    .find_map(|key| {
                                        snapshot[*key]
                                            .as_array()?
                                            .iter()
                                            .find(|task| {
                                                task["id"].as_str() == Some(task_id.as_str())
                                                    || task["task_id"].as_str() == Some(task_id.as_str())
                                            })
                                            .cloned()
                                    })
                                    .unwrap_or_else(|| snapshot.clone());
                                results[*index] = Some((entry, false));
                            }
                            acknowledgement = Some(snapshot);
                        }
                        Err(error) => {
                            for (index, _, _) in &pending {
                                results[*index] = Some((json!({"error": error.message.clone()}), true));
                            }
                        }
                    }
                }
                Err(error) => {
                    for (index, _, _) in &pending {
                        results[*index] = Some((json!({"error": error.message.clone()}), true));
                    }
                }
            }
        }
        for (index, _, _) in &pending {
            if results[*index].is_none() {
                results[*index] = Some((
                    json!({"error":"task backend returned an incomplete submission result"}),
                    true,
                ));
            }
        }

        for (index, (call, _)) in all_calls.into_iter().enumerate() {
            if let Some((value, is_error)) = results[index].take() {
                self.emit_tool_result(&call, value, is_error, messages).await;
            }
        }
        if let Some(snapshot) = acknowledgement {
            backend
                .acknowledge(&snapshot)
                .await
                .map_err(|e| crate::extensions::invalid(&e.message))?;
        }
        Ok(())
    }

    async fn execute_tools(
        &self,
        batch: Vec<(ToolCall, Arc<dyn Tool>)>,
        messages: &mut Vec<Message>,
    ) -> Result<(), AgentError> {
        let run_id = self.run_id;
        let mut results: Vec<Option<(Value, bool)>> = Vec::with_capacity(batch.len());
        let mut allowed = Vec::with_capacity(batch.len());
        for (index, (call, tool)) in batch.iter().enumerate() {
            tracing::debug!(%run_id, call_id = %call.id, tool = %call.name, arguments = %call.arguments, "authorizing tool call");
            let spec = self.spec(&call.name).expect("registered tool spec");
            if let Some(result) = self
                .authorize(spec, &call.arguments, &tool.origin())
                .await?
            {
                results.push(Some((result, true)));
                continue;
            }
            tracing::info!(%run_id, call_id = %call.id, tool = %call.name, "tool call started");
            self.emit(RunEvent::ToolStarted {
                run_id,
                call_id: call.id.clone(),
                name: call.name.clone(),
            })
            .await;
            results.push(None);
            allowed.push(index);
        }

        let outputs = futures_util::future::join_all(allowed.iter().map(|&index| {
            let (call, tool) = &batch[index];
            tool.call_output(self.tool_context(), call.arguments.clone())
        }))
        .await;
        for (index, output) in allowed.into_iter().zip(outputs) {
            let call = &batch[index].0;
            results[index] = Some(match output {
                Ok(output) => {
                    tracing::info!(%run_id, call_id = %call.id, tool = %call.name, "tool call completed");
                    (output.value, output.is_error)
                }
                Err(error) => {
                    tracing::warn!(%run_id, call_id = %call.id, tool = %call.name, error = %error, "tool call failed");
                    (json!({ "error": error.message }), true)
                }
            });
        }

        for ((call, _), result) in batch.iter().zip(results) {
            let (value, is_error) = result.expect("every call has a result");
            self.emit_tool_result(call, value, is_error, messages).await;
        }
        Ok(())
    }

    async fn execute_handoff(
        &self,
        child: &Arc<Agent>,
        call: ToolCall,
        usage: &mut Usage,
        messages: &mut Vec<Message>,
    ) -> Result<(), AgentError> {
        let run_id = self.run_id;
        let child_name = child.display_name();
        let spec = self.spec(&call.name).expect("handoff spec");
        let origin = ToolOrigin::Handoff {
            agent: child_name.to_owned(),
        };
        if let Some(result) = self.authorize(spec, &call.arguments, &origin).await? {
            self.emit_handoff_result(&call, child_name, None, result, true, messages)
                .await;
            return Ok(());
        }
        let Some(task) = call.arguments.get("task").and_then(Value::as_str) else {
            let result = json!({ "error": "handoff requires a string `task` argument" });
            self.emit_handoff_result(&call, child_name, None, result, true, messages)
                .await;
            return Ok(());
        };

        tracing::info!(%run_id, call_id = %call.id, agent = child_name, "handoff started");
        tracing::debug!(%run_id, call_id = %call.id, agent = child_name, task, "handoff task");
        self.emit(RunEvent::HandoffStarted {
            run_id,
            call_id: call.id.clone(),
            agent: child_name.into(),
        })
        .await;

        let mut child_agent = child.as_ref().clone();
        if let Some(backend) = &child_agent.task_backend {
            if let Some(scoped) = backend.for_handoff(child_name, &call.id) {
                child_agent.task_backend = Some(scoped);
            }
        }
        child_agent.event_sink = self.agent.event_sink.clone();
        child_agent.policy = Arc::new(crate::extensions::PolicyIntersection(
            self.agent.policy.clone(),
            child_agent.policy.clone(),
        ));
        let child_request = RunRequest::new(vec![Message::user(task)])
            .with_cancellation_token(self.request.cancellation_token.clone())
            .with_extensions(self.request.extensions.clone())
            .with_scope(self.request.scope.clone());
        let child_run =
            self.runtime
                .run_with(&child_agent, child_request, Some(self.mcp_tools.clone()));
        match Box::pin(child_run).await {
            Ok(output) => {
                usage.add(output.usage);
                tracing::info!(%run_id, call_id = %call.id, child_run_id = %output.run_id, agent = child_name, "handoff completed");
                let result = json!({ "agent": child_name, "response": output.final_text });
                self.emit_handoff_result(
                    &call,
                    child_name,
                    Some(output.run_id),
                    result,
                    false,
                    messages,
                )
                .await;
            }
            Err(AgentError::Cancelled) => return Err(AgentError::Cancelled),
            Err(error) => {
                tracing::warn!(%run_id, call_id = %call.id, agent = child_name, error = %error, "handoff failed");
                let result = json!({ "agent": child_name, "error": error.to_string() });
                self.emit_handoff_result(&call, child_name, None, result, true, messages)
                    .await;
            }
        }
        Ok(())
    }

    async fn emit_tool_result(
        &self,
        call: &ToolCall,
        result: Value,
        is_error: bool,
        messages: &mut Vec<Message>,
    ) {
        self.emit(RunEvent::ToolCompleted {
            run_id: self.run_id,
            call_id: call.id.clone(),
            name: call.name.clone(),
            result: result.clone(),
            is_error,
        })
        .await;
        push_tool_result(&call.id, result, is_error, messages);
    }

    async fn emit_handoff_result(
        &self,
        call: &ToolCall,
        child_name: &str,
        child_run_id: Option<Uuid>,
        result: Value,
        is_error: bool,
        messages: &mut Vec<Message>,
    ) {
        self.emit(RunEvent::HandoffCompleted {
            run_id: self.run_id,
            call_id: call.id.clone(),
            agent: child_name.to_owned(),
            child_run_id,
            is_error,
        })
        .await;
        push_tool_result(&call.id, result, is_error, messages);
    }

    #[allow(clippy::too_many_arguments)]
    async fn finish(
        &self,
        messages: Vec<Message>,
        injected: usize,
        prepared: &Prepared,
        final_text: String,
        turns: usize,
        tool_calls: usize,
        usage: Usage,
    ) -> RunOutput {
        let run_id = self.run_id;
        #[allow(unused_mut)] // Mutable only with skills enabled.
        let mut context_manifest = crate::ContextManifest {
            diagnostics: prepared.diagnostics.clone(),
            ..Default::default()
        };
        #[cfg(feature = "skills")]
        if let Some(state) = &prepared.skills {
            context_manifest.active_skills = state.active();
        }
        self.emit(RunEvent::Completed {
            run_id,
            turns,
            tool_calls,
            usage,
        })
        .await;
        tracing::info!(
            %run_id,
            turns,
            tool_calls,
            input_tokens = usage.input_tokens,
            output_tokens = usage.output_tokens,
            "agent run completed"
        );
        RunOutput {
            run_id,
            conversation_messages: messages[injected..].to_vec(),
            context_manifest,
            messages,
            final_text,
            turns,
            tool_calls,
            usage,
        }
    }
}

fn denied(mode: PolicyDenial, error: PolicyError) -> Result<Option<Value>, AgentError> {
    match mode {
        PolicyDenial::Abort => Err(error.into()),
        PolicyDenial::ReportToModel => Ok(Some(json!({
            "error": { "code": "policy_denied", "message": error.message }
        }))),
    }
}

struct RuntimeModelEventSink {
    event_sink: Arc<dyn EventSink>,
    run_id: Uuid,
    turn: usize,
    emitted: AtomicBool,
}

#[async_trait]
impl ModelEventSink for RuntimeModelEventSink {
    async fn emit(&self, event: ModelEvent) {
        match event {
            ModelEvent::TextDelta { text } => {
                self.emitted.store(true, Ordering::Relaxed);
                self.event_sink
                    .emit(RunEvent::TextDelta {
                        run_id: self.run_id,
                        turn: self.turn,
                        text,
                    })
                    .await;
            }
        }
    }
}

fn push_tool_result(call_id: &str, result: Value, is_error: bool, messages: &mut Vec<Message>) {
    messages.push(Message::new(
        Role::Tool,
        vec![Content::ToolResult {
            call_id: call_id.to_owned(),
            result,
            is_error,
        }],
    ));
}

#[cfg(feature = "skills")]
type SkillsState = Option<Arc<crate::skills::SkillState>>;
#[cfg(not(feature = "skills"))]
type SkillsState = ();

/// MCP tools discovered for one request, shared with handoff children.
#[derive(Clone, Default)]
struct McpTools {
    #[cfg(feature = "mcp")]
    tools: Arc<[Arc<dyn Tool>]>,
    #[cfg(feature = "mcp")]
    diagnostics: Arc<[String]>,
}

struct Prepared {
    agent: Agent,
    diagnostics: Vec<String>,
    skills: SkillsState,
    mcp_tools: McpTools,
}

impl Runtime {
    async fn prepare(
        &self,
        agent: &Agent,
        request: &RunRequest,
        #[allow(unused_variables)] mcp_tools: Option<McpTools>,
    ) -> Result<Prepared, AgentError> {
        #[allow(unused_mut)] // Feature-dependent registration.
        let mut prepared = agent.clone();
        #[allow(unused_mut)]
        let mut diagnostics = Vec::new();
        #[cfg(feature = "mcp")]
        let mcp_tools = {
            let mcp_tools = match mcp_tools {
                Some(tools) => tools,
                None => self.discover_mcp(request).await?,
            };
            for tool in mcp_tools.tools.iter() {
                crate::extensions::register(&mut prepared, tool.clone())?;
            }
            diagnostics.extend(mcp_tools.diagnostics.iter().cloned());
            mcp_tools
        };
        #[cfg(not(feature = "mcp"))]
        let mcp_tools = McpTools::default();

        #[cfg(feature = "skills")]
        let skills = {
            let origins: Vec<_> = prepared.tools.values().map(|t| t.origin()).collect();
            let mut available = Vec::new();
            for skill in &request.extensions.skills {
                if skill.required_tools.iter().all(|r| origins.contains(r)) {
                    available.push(skill.clone());
                } else if request.selected_skills.contains(&skill.name) {
                    return Err(crate::extensions::invalid(
                        "selected skill has unavailable required tools",
                    ));
                } else {
                    diagnostics.push(format!(
                        "skill {} unavailable: missing required tools",
                        skill.name
                    ));
                }
            }
            for name in &request.selected_skills {
                if !available.iter().any(|s| &s.name == name) {
                    return Err(crate::extensions::invalid("selected skill not found"));
                }
            }
            if available.is_empty() {
                None
            } else {
                let state = crate::skills::SkillState::new(available);
                crate::extensions::register(&mut prepared, state.list_tool())?;
                crate::extensions::register(&mut prepared, state.read_tool())?;
                Some(state)
            }
        };
        #[cfg(not(feature = "skills"))]
        if !request.selected_skills.is_empty() {
            return Err(crate::extensions::invalid("skills feature is disabled"));
        }
        #[cfg(not(feature = "skills"))]
        let skills = ();
        Ok(Prepared {
            agent: prepared,
            diagnostics,
            skills,
            mcp_tools,
        })
    }

    #[cfg(feature = "mcp")]
    async fn discover_mcp(&self, request: &RunRequest) -> Result<McpTools, AgentError> {
        let mut tools = Vec::new();
        let mut diagnostics = Vec::new();
        for server in &request.extensions.mcp {
            let manager = self.mcp.as_ref().ok_or_else(|| {
                crate::extensions::invalid("MCP configuration requires a manager")
            })?;
            if manager.is_closed() {
                return Err(crate::extensions::unavailable("manager_closed"));
            }
            match manager.tools(server, &request.scope).await {
                Ok(server_tools) => tools.extend(server_tools),
                Err(error) if !server.required && !manager.is_closed() => {
                    diagnostics.push(format!("MCP {} unavailable: {error}", server.id))
                }
                Err(error) => return Err(error),
            }
        }
        Ok(McpTools {
            tools: tools.into(),
            diagnostics: diagnostics.into(),
        })
    }
}
