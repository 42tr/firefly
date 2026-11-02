use std::{collections::HashMap, sync::Arc, time::Duration};

use crate::{
    AgentError, AllowAll, EventSink, IntoPrompt, IntoTool, Model, NoopEventSink, Policy, RunEvent,
    RunOutput, RunRequest, Runtime, Tool, ToolBehavior, ToolSpec,
};
use serde_json::json;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tokio_util::sync::CancellationToken;

#[derive(Clone, Copy, Debug)]
pub struct RunLimits {
    pub max_tool_calls: usize,
    pub timeout: Duration,
}

impl Default for RunLimits {
    fn default() -> Self {
        Self {
            max_tool_calls: 32,
            timeout: Duration::from_secs(120),
        }
    }
}

/// Retries for model errors marked `retryable` (network failures, 429, 5xx).
/// A request is only retried if it had not streamed any text yet.
#[derive(Clone, Copy, Debug)]
pub struct RetryPolicy {
    pub max_retries: u32,
    pub initial_backoff: Duration,
    pub max_backoff: Duration,
}

impl RetryPolicy {
    pub fn none() -> Self {
        Self {
            max_retries: 0,
            ..Self::default()
        }
    }

    pub(crate) fn backoff(&self, attempt: u32) -> Duration {
        let factor = 2u32.saturating_pow(attempt.saturating_sub(1));
        self.initial_backoff
            .saturating_mul(factor)
            .min(self.max_backoff)
    }
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_retries: 2,
            initial_backoff: Duration::from_millis(500),
            max_backoff: Duration::from_secs(8),
        }
    }
}

/// What the runtime does when the policy rejects a model-requested tool call.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PolicyDenial {
    /// Fail the run with `AgentError::Policy`.
    #[default]
    Abort,
    /// Return the rejection to the model as an error tool result and continue.
    ReportToModel,
}

#[derive(Clone)]
pub(crate) struct Handoff {
    /// `None` for unnamed agents, which `validate` rejects.
    pub(crate) tool_name: Option<String>,
    pub(crate) agent: Arc<Agent>,
}

#[derive(Clone)]
pub struct Agent {
    pub(crate) task_backend: Option<Arc<dyn crate::TaskBackend>>,
    pub(crate) model: Arc<dyn Model>,
    pub(crate) tools: HashMap<String, Arc<dyn Tool>>,
    pub(crate) policy: Arc<dyn Policy>,
    pub(crate) event_sink: Arc<dyn EventSink>,
    pub(crate) limits: RunLimits,
    pub(crate) instruction: Option<String>,
    pub(crate) retry: RetryPolicy,
    pub(crate) policy_denial: PolicyDenial,
    name: Option<String>,
    description: Option<String>,
    handoffs: Vec<Handoff>,
}

impl Agent {
    pub fn new(model: Arc<dyn Model>) -> Self {
        Self {
            task_backend: None,
            model,
            tools: HashMap::new(),
            policy: Arc::new(AllowAll),
            event_sink: Arc::new(NoopEventSink),
            limits: RunLimits::default(),
            instruction: None,
            retry: RetryPolicy::default(),
            policy_denial: PolicyDenial::default(),
            name: None,
            description: None,
            handoffs: Vec::new(),
        }
    }

    pub fn name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    pub fn description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }

    pub fn instruction(mut self, instruction: impl Into<String>) -> Self {
        self.instruction = Some(instruction.into());
        self
    }

    pub fn tool(mut self, tool: impl IntoTool) -> Self {
        let tool = tool.into_tool();
        self.tools.insert(tool.spec().name, tool);
        self
    }

    pub fn tools<I>(mut self, tools: I) -> Self
    where
        I: IntoIterator,
        I::Item: IntoTool,
    {
        for tool in tools {
            let tool = tool.into_tool();
            self.tools.insert(tool.spec().name, tool);
        }
        self
    }

    pub fn handoff<I>(mut self, agents: I) -> Self
    where
        I: IntoIterator<Item = Agent>,
    {
        self.handoffs
            .extend(agents.into_iter().map(|agent| Handoff {
                tool_name: agent.name.as_deref().map(handoff_tool_name),
                agent: Arc::new(agent),
            }));
        self
    }

    pub fn register_tool(&mut self, tool: Arc<dyn Tool>) -> Result<(), AgentError> {
        let name = tool.spec().name;
        self.register_tool_named(name, tool)
    }

    pub(crate) fn register_tool_named(
        &mut self,
        name: String,
        tool: Arc<dyn Tool>,
    ) -> Result<(), AgentError> {
        if self.tools.contains_key(&name)
            || (self.task_backend.is_some() && crate::task::is_task_control(&name))
        {
            return Err(AgentError::DuplicateTool(name));
        }
        self.tools.insert(name, tool);
        Ok(())
    }

    pub fn with_policy(mut self, policy: Arc<dyn Policy>) -> Self {
        self.policy = policy;
        self
    }

    pub fn with_task_backend(mut self, backend: Arc<dyn crate::TaskBackend>) -> Self {
        self.task_backend = Some(backend);
        self
    }

    pub fn with_event_sink(mut self, event_sink: Arc<dyn EventSink>) -> Self {
        self.event_sink = event_sink;
        self
    }

    pub fn with_limits(mut self, limits: RunLimits) -> Self {
        self.limits = limits;
        self
    }

    pub fn with_retry_policy(mut self, retry: RetryPolicy) -> Self {
        self.retry = retry;
        self
    }

    pub fn on_policy_denial(mut self, mode: PolicyDenial) -> Self {
        self.policy_denial = mode;
        self
    }

    pub fn tool_specs(&self) -> Vec<ToolSpec> {
        let mut specs: Vec<_> = self.tools.values().map(|tool| tool.spec()).collect();
        if self.task_backend.is_some() {
            specs.extend(crate::task::task_specs());
        }
        specs.extend(self.handoffs.iter().filter_map(|handoff| {
            let tool_name = handoff.tool_name.clone()?;
            let agent = &handoff.agent;
            let name = agent.name.as_deref()?;
            Some(ToolSpec {
                name: tool_name,
                description: agent
                    .description
                    .clone()
                    .unwrap_or_else(|| format!("Hand off the task to the {name} agent")),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "task": {
                            "type": "string",
                            "description": "The task and relevant context for the agent"
                        }
                    },
                    "required": ["task"],
                    "additionalProperties": false
                }),
                behavior: ToolBehavior::default(),
            })
        }));
        specs.sort_by(|left, right| left.name.cmp(&right.name));
        specs
    }

    pub async fn run(&self, prompt: impl IntoPrompt) -> Result<RunOutput, AgentError> {
        Runtime::new()
            .run(self, RunRequest::new(vec![prompt.into_prompt()]))
            .await
    }

    pub fn stream_run(
        &self,
        prompt: impl IntoPrompt,
    ) -> impl futures_util::Stream<Item = Result<RunEvent, AgentError>> + Send + Unpin + 'static
    {
        Runtime::new().stream_run(self, RunRequest::new(vec![prompt.into_prompt()]))
    }

    pub(crate) fn handoff_by_tool_name(&self, tool_name: &str) -> Option<&Arc<Agent>> {
        self.handoffs
            .iter()
            .find(|handoff| handoff.tool_name.as_deref() == Some(tool_name))
            .map(|handoff| &handoff.agent)
    }

    pub(crate) fn display_name(&self) -> &str {
        self.name.as_deref().unwrap_or("agent")
    }

    pub(crate) fn validate(&self) -> Result<(), AgentError> {
        if self.task_backend.is_some()
            && self
                .tools
                .keys()
                .any(|name| crate::task::is_task_control(name))
        {
            return Err(AgentError::InvalidConfiguration(
                "Task control names are reserved".into(),
            ));
        }
        let mut names = std::collections::HashSet::new();
        for Handoff { agent, .. } in &self.handoffs {
            let name = agent.name.as_deref().ok_or_else(|| {
                AgentError::InvalidConfiguration("handoff agents must have a name".into())
            })?;
            if name.trim().is_empty() {
                return Err(AgentError::InvalidConfiguration(
                    "handoff agent name cannot be empty".into(),
                ));
            }
            let tool_name = handoff_tool_name(name);
            if !names.insert(tool_name.clone()) {
                return Err(AgentError::InvalidConfiguration(format!(
                    "duplicate handoff name `{name}`"
                )));
            }
            if self.tools.contains_key(&tool_name) {
                return Err(AgentError::InvalidConfiguration(format!(
                    "handoff tool `{tool_name}` conflicts with a registered tool"
                )));
            }
            agent.validate()?;
        }
        Ok(())
    }
}

pub(crate) struct StreamEventSink {
    pub(crate) sender: mpsc::Sender<Result<RunEvent, AgentError>>,
    pub(crate) downstream: Arc<dyn EventSink>,
    pub(crate) cancellation_token: CancellationToken,
}

#[async_trait::async_trait]
impl EventSink for StreamEventSink {
    async fn emit(&self, event: RunEvent) {
        self.downstream.emit(event.clone()).await;
        if self.sender.send(Ok(event)).await.is_err() {
            self.cancellation_token.cancel();
        }
    }
}

fn handoff_tool_name(name: &str) -> String {
    let name: String = name
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '_' | '-') {
                character.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect();
    format!("handoff_to_{name}")
}

pub(crate) struct CancelOnDropStream {
    pub(crate) inner: ReceiverStream<Result<RunEvent, AgentError>>,
    pub(crate) token: CancellationToken,
}
impl Drop for CancelOnDropStream {
    fn drop(&mut self) {
        self.token.cancel();
    }
}
impl futures_util::Stream for CancelOnDropStream {
    type Item = Result<RunEvent, AgentError>;
    fn poll_next(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        std::pin::Pin::new(&mut self.inner).poll_next(cx)
    }
}
