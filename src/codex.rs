//! Codex provider, workspace, settings, and app-server helpers.
mod models;
mod protocol;
mod settings;
mod workspace;

#[allow(unused_imports)]
pub use models::{
    hydrate_provider_models, list_model_providers, list_official_models, ModelProvider,
    ModelProviderListing, OfficialModel,
};
pub use protocol::{
    interrupt_turn, list_turn_items, notification_matches, notification_method, notification_params,
    read_turn, resume_thread, start_thread, start_turn,
};
#[allow(unused_imports)]
pub use settings::{resolve_conversation_settings, ConversationSettings};
pub use workspace::{
    create_workspace_directory, list_workspace, resolve_workspace_file, resolve_workspace_path,
    workspace_root, WorkspaceEntry, WorkspaceListing,
};
pub(crate) use workspace::validate_relative_path;

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
