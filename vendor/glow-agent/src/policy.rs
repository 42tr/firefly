use async_trait::async_trait;
use serde_json::Value;

use crate::{PolicyError, ToolContext, ToolSpec};

#[async_trait]
pub trait Policy: Send + Sync {
    async fn authorize(
        &self,
        context: &ToolContext,
        tool: &ToolSpec,
        arguments: &Value,
    ) -> Result<(), PolicyError>;

    /// Stable origin and host identity for request-scoped tools. Existing policies
    /// remain effective through the default implementation.
    async fn authorize_extension(
        &self,
        context: &ToolContext,
        tool: &ToolSpec,
        arguments: &Value,
        _origin: &crate::ToolOrigin,
        _scope: &crate::InvocationScope,
    ) -> Result<(), PolicyError> {
        self.authorize(context, tool, arguments).await
    }
}

#[derive(Debug, Default)]
pub struct AllowAll;

#[async_trait]
impl Policy for AllowAll {
    async fn authorize(
        &self,
        _context: &ToolContext,
        _tool: &ToolSpec,
        _arguments: &Value,
    ) -> Result<(), PolicyError> {
        Ok(())
    }
}
