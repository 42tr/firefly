mod agent_model;
mod context_model;
mod model_limits;
mod tool_arguments;
pub use agent_model::AgentModel;
pub use context_model::ContextModel;
pub use model_limits::ModelBudget;

pub fn normalize_bearer_secret(raw: &str) -> String {
    let trimmed = raw.trim();
    trimmed
        .strip_prefix("Bearer ")
        .or_else(|| trimmed.strip_prefix("bearer "))
        .map(|rest| rest.trim().to_string())
        .unwrap_or_else(|| trimmed.to_string())
}
