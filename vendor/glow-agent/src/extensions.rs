//! Explicit, request-scoped extensions. This module never reads configuration files.
#[cfg(any(feature = "skills", feature = "mcp"))]
use crate::Tool;
use crate::{AgentError, ToolContext, ToolSpec};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, sync::Arc};

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InvocationScope {
    pub id: String,
    #[serde(default)]
    pub attributes: BTreeMap<String, String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ToolOrigin {
    Local,
    Mcp {
        server_id: String,
        tool_name: String,
    },
    Skills,
    /// A handoff to the named child agent.
    Handoff {
        agent: String,
    },
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ExtensionConfig {
    #[cfg(feature = "mcp")]
    pub mcp: Vec<crate::mcp::McpServerConfig>,
    #[cfg(feature = "skills")]
    pub skills: Vec<crate::skills::SkillDefinition>,
}
impl ExtensionConfig {
    pub fn validate(&self) -> Result<(), AgentError> {
        #[cfg(feature = "mcp")]
        {
            let mut names = std::collections::HashSet::new();
            if self.mcp.len() > 128 {
                return Err(invalid("too many MCP servers"));
            }
            for server in &self.mcp {
                server.validate()?;
                if !names.insert(&server.id) {
                    return Err(invalid("duplicate MCP server id"));
                }
            }
        }
        #[cfg(feature = "skills")]
        crate::skills::validate(&self.skills)?;
        Ok(())
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ContextManifest {
    pub active_skills: BTreeMap<String, String>,
    pub diagnostics: Vec<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct WarmupReport {
    pub tools: Vec<ToolSpec>,
    pub diagnostics: Vec<String>,
}

pub(crate) fn invalid(message: impl Into<String>) -> AgentError {
    AgentError::InvalidConfiguration(message.into())
}

#[cfg(feature = "mcp")]
pub(crate) fn unavailable(message: impl Into<String>) -> AgentError {
    AgentError::Extension(message.into())
}

#[cfg(any(feature = "skills", feature = "mcp"))]
pub(crate) fn fingerprint(value: &impl Serialize) -> String {
    use sha2::{Digest, Sha256};
    // Only serializable owned configuration types reach this function.
    format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(value).expect("configuration serialization"))
    )
}

pub(crate) struct PolicyIntersection(pub Arc<dyn crate::Policy>, pub Arc<dyn crate::Policy>);
#[async_trait::async_trait]
impl crate::Policy for PolicyIntersection {
    async fn authorize(
        &self,
        ctx: &ToolContext,
        spec: &ToolSpec,
        args: &serde_json::Value,
    ) -> Result<(), crate::PolicyError> {
        self.0.authorize(ctx, spec, args).await?;
        self.1.authorize(ctx, spec, args).await
    }
    async fn authorize_extension(
        &self,
        ctx: &ToolContext,
        spec: &ToolSpec,
        args: &serde_json::Value,
        origin: &ToolOrigin,
        scope: &InvocationScope,
    ) -> Result<(), crate::PolicyError> {
        self.0
            .authorize_extension(ctx, spec, args, origin, scope)
            .await?;
        self.1
            .authorize_extension(ctx, spec, args, origin, scope)
            .await
    }
}

#[cfg(any(feature = "skills", feature = "mcp"))]
pub(crate) fn register(agent: &mut crate::Agent, tool: Arc<dyn Tool>) -> Result<(), AgentError> {
    let name = tool.spec().name;
    // `register_tool` rejects local collisions; handoff names live in a separate list.
    if agent.handoff_by_tool_name(&name).is_some() {
        return Err(AgentError::DuplicateTool(name));
    }
    agent.register_tool_named(name, tool)
}
