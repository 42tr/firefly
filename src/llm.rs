//! Re-exports of the glow-agent model stack plus Firefly's key hygiene.
pub use g::{ContextModel, ModelBudget};
pub use g::OpenAIModel as AgentModel;

pub fn normalize_bearer_secret(raw: &str) -> String {
    let trimmed = raw.trim();
    trimmed
        .strip_prefix("Bearer ")
        .or_else(|| trimmed.strip_prefix("bearer "))
        .map(|rest| rest.trim().to_string())
        .unwrap_or_else(|| trimmed.to_string())
}
