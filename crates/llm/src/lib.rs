//! Model calls, provider limits, and per-run context compaction.
//!
//! Callers supply a shared HTTP client and, optionally, an initialized SQLite
//! pool for durable provider limits. Database migrations belong to the caller.

mod agent_model;
mod context_model;
mod model_limits;
mod tool_arguments;
pub use agent_model::AgentModel;
pub use context_model::ContextModel;
pub use model_limits::ModelBudget;

#[cfg(test)]
fn test_client() -> reqwest::Client {
    reqwest::Client::builder().no_proxy().build().unwrap()
}

pub fn normalize_bearer_secret(raw: &str) -> String {
    let trimmed = raw.trim();
    trimmed
        .strip_prefix("Bearer ")
        .or_else(|| trimmed.strip_prefix("bearer "))
        .map(|rest| rest.trim().to_string())
        .unwrap_or_else(|| trimmed.to_string())
}
