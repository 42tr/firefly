//! Host-owned tasks can outlive a model turn or a run. The host is responsible
//! for persistence, independent cancellation, resource limits and recovery.
use crate::{Tool, ToolBehavior, ToolError, ToolSpec};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::sync::Arc;
use uuid::Uuid;

pub const TASK_INSTRUCTION: &str = "\nTask execution: call the registered tool directly with its typed arguments. Independent tool calls in one response may run concurrently. Results can contain pending task IDs; use wait_tools to receive results, and get_tasks with specific task_ids when progress or output is needed. Do not reread completed history unless it is relevant to the user's request. Never claim a pending task succeeded. Before finishing, receive required results and detach or cancel remaining attached tasks. command runs noninteractive CLI programs. Do not repeatedly poll without waiting. Parent task IDs denote actual subtasks, not mere dependencies.\n";

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TaskCall {
    pub tool: String,
    #[serde(default = "empty_object")]
    pub arguments: Value,
    pub title: Option<String>,
    pub parent_task_id: Option<String>,
    pub execution_timeout_secs: Option<u64>,
}
fn empty_object() -> Value {
    json!({})
}

/// Authorization has already been checked by the runtime. The backend must
/// use its own cancellation token, never the parent run's token, for execution.
pub struct TaskSubmission {
    pub run_id: Uuid,
    pub call_id: String,
    pub index: usize,
    pub request: TaskCall,
    pub tool: Arc<dyn Tool>,
}

#[async_trait]
pub trait TaskBackend: Send + Sync {
    /// Bind a child agent's explicitly configured backend to a handoff. Hosts
    /// can use the stable call ID to isolate task ownership and recovery. A
    /// backend is never inherited by children that did not configure one.
    fn for_handoff(&self, _agent: &str, _call_id: &str) -> Option<Arc<dyn TaskBackend>> {
        None
    }
    async fn submit(&self, tasks: Vec<TaskSubmission>) -> Result<Vec<String>, ToolError>;
    async fn control(&self, operation: &str, arguments: Value) -> Result<Value, ToolError>;
    /// Called after the corresponding model tool response has reached the event sink.
    async fn acknowledge(&self, _result: &Value) -> Result<(), ToolError> {
        Ok(())
    }
    /// True only when required results were delivered and no attached task is active.
    async fn ready_to_finish(&self) -> Result<bool, ToolError>;
    /// Bounded task identifiers/statuses for actionable completion feedback.
    async fn finish_blockers(&self) -> Result<Value, ToolError> {
        Ok(Value::Null)
    }
}

pub fn is_task_control(name: &str) -> bool {
    matches!(
        name,
        "run_tools" | "wait_tools" | "get_tasks" | "detach_tools" | "cancel_tools"
    )
}

pub(crate) fn task_specs() -> Vec<ToolSpec> {
    let ids = json!({"type":"array","items":{"type":"string"},"maxItems":64});
    let wait = json!({"type":"integer","minimum":0,"maximum":604800});
    [
        ("wait_tools", "Receive task results; waiting does not cancel tasks. Defaults to any result, 30 seconds.", json!({"type":"object","properties":{
            "task_ids":ids,"yield_when":{"type":"string","enum":["any","all"]},"wait_timeout_secs":wait
        },"required":["task_ids"],"additionalProperties":false})),
        ("get_tasks", "Read bounded task status/results. Prefer specific task_ids to inspect output. Without IDs, return a paged overview of active tasks, this reply's unreceived results and unacknowledged unknown tasks; completed history is excluded. Use include_history only for a relevant historical lookup, with next_offset for pagination. Default limit 32, maximum 64. Output/result previews may be truncated; consult task details or saved result files for longer content.", json!({"type":"object","properties":{
            "task_ids":ids,"include_history":{"type":"boolean","default":false},
            "include_output":{"type":"boolean","description":"Include recent output previews. Defaults to true with task_ids; false for overview queries, except required unreceived results."},
            "limit":{"type":"integer","minimum":1,"maximum":64,"default":32},
            "offset":{"type":"integer","minimum":0,"description":"Pagination offset for historical lookups. For attention queries repeat without offset after receiving results."}
        },"additionalProperties":false})),
        ("detach_tools", "Allow selected tasks to continue after this reply finishes or is stopped.", json!({"type":"object","properties":{"task_ids":ids},"required":["task_ids"],"additionalProperties":false})),
        ("cancel_tools", "Cancel selected tasks and their descendants. Cancellation does not undo side effects.", json!({"type":"object","properties":{"task_ids":ids,"reason":{"type":"string"}},"required":["task_ids"],"additionalProperties":false})),
    ].into_iter().map(|(name, description, input_schema)| ToolSpec {
        name:name.into(), description:description.into(), input_schema, behavior:ToolBehavior::default(),
    }).collect()
}

/// Kept for old conversations that still contain a run_tools call. New model
/// requests do not advertise this wrapper; direct calls use the registered
/// tool schema instead.
pub(crate) fn legacy_run_tools_spec() -> ToolSpec {
    ToolSpec {
        name: "run_tools".into(),
        description: "Legacy task wrapper; use direct tool calls instead.".into(),
        input_schema: json!({
            "type":"object", "properties": {
                "calls":{"type":"array","minItems":1,"maxItems":32,"items":{"type":"object","properties":{
                    "tool":{"type":"string"},"arguments":{"type":"object"},"title":{"type":"string","minLength":1,"maxLength":200,"pattern":"\\S"},
                    "parent_task_id":{"type":"string"},"execution_timeout_secs":{"type":"integer","minimum":1}
                },"required":["tool","arguments","title"],"additionalProperties":false}},
                "yield_when":{"type":"string","enum":["none","any","all"]},"wait_timeout_secs":{"type":"integer","minimum":0,"maximum":604800}
            },"required":["calls"],"additionalProperties":false
        }),
        behavior: ToolBehavior::default(),
    }
}
