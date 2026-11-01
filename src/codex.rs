//! Codex provider, workspace, settings, and app-server helpers.
mod models;
mod protocol;
mod settings;
mod workspace;

use std::sync::Arc;
use firefly_codex_transport::{CodexAppServer, RuntimeProvider};

#[allow(unused_imports)]
pub use models::{
    hydrate_provider_models, list_model_providers, list_official_models, ModelProvider,
    ModelProviderListing, OfficialModel,
};
pub use protocol::{
    interrupt_turn, list_turn_items, notification_matches, notification_method, notification_params,
    read_thread_name, read_turn, resume_thread, start_thread, start_turn,
};
#[allow(unused_imports)]
pub use settings::{resolve_conversation_settings, ConversationSettings};
pub use workspace::{
    create_workspace_directory, list_workspace, resolve_workspace_file, resolve_workspace_path,
    workspace_root, WorkspaceEntry, WorkspaceListing,
};
pub(crate) use workspace::validate_relative_path;

/// Build the Codex app-server from Firefly's database-backed API profiles.
/// Native ~/.codex/config.toml remains available when no database profiles
/// are configured, preserving existing installations during migration.
pub(crate) async fn app_server() -> anyhow::Result<Arc<CodexAppServer>> {
    let settings = crate::agent_settings::Settings::load(crate::db::pool()).await?;
    let providers = settings.profiles.iter()
        .filter(|profile| matches!(profile.agent.as_str(), "codex" | "shared"))
        .map(|profile| RuntimeProvider {
            name: format!("api-{}", profile.id),
            base_url: profile.base_url.clone(),
            api_key: profile.api_key.clone().unwrap_or_default(),
            wire_api: "responses".into(),
        })
        .collect::<Vec<_>>();
    firefly_codex_transport::app_server_with_providers(&providers).await
}

#[cfg(test)]
mod tests;

#[cfg(test)]
pub(crate) use models::{
    cached_provider_models, fetch_models_once, parse_model_providers, provider_models_cache,
    ProviderModelsEntry, PROVIDER_MODELS_ERROR_TTL,
};
#[cfg(test)]
pub(crate) use protocol::turn_params;
#[cfg(test)]
pub(crate) use settings::{effective_model, resolve_settings_from_sources};
