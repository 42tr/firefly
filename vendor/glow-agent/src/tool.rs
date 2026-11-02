use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::ToolError;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolBehavior {
    pub read_only: bool,
    pub idempotent: bool,
    pub parallel_safe: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
    pub behavior: ToolBehavior,
}

#[derive(Clone, Debug)]
pub struct ToolContext {
    pub run_id: Uuid,
    pub cancellation_token: CancellationToken,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ToolOutput {
    pub value: Value,
    pub is_error: bool,
}

#[async_trait]
pub trait Tool: Send + Sync {
    fn spec(&self) -> ToolSpec;

    /// Detaching a mutating tool requires an explicit host/tool opt-in.
    fn background_allowed(&self) -> bool {
        let behavior = self.spec().behavior;
        behavior.read_only && behavior.parallel_safe
    }

    fn origin(&self) -> crate::ToolOrigin {
        crate::ToolOrigin::Local
    }

    async fn call(&self, context: ToolContext, input: Value) -> Result<Value, ToolError>;

    /// Override to return a structured tool error without changing existing tools.
    async fn call_output(
        &self,
        context: ToolContext,
        input: Value,
    ) -> Result<ToolOutput, ToolError> {
        self.call(context, input).await.map(|value| ToolOutput {
            value,
            is_error: false,
        })
    }
}

pub trait IntoTool {
    fn into_tool(self) -> Arc<dyn Tool>;
}

impl<T> IntoTool for T
where
    T: Tool + 'static,
{
    fn into_tool(self) -> Arc<dyn Tool> {
        Arc::new(self)
    }
}

impl IntoTool for Arc<dyn Tool> {
    fn into_tool(self) -> Arc<dyn Tool> {
        self
    }
}
