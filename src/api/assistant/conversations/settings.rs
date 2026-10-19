//! Agent/workspace validation and per-conversation model settings.
use axum::{extract::Path, Json};

use super::presentation::to_summary;
use crate::{
    api::{
        assistant::{
            ClaudeSettingsRequest, CodexSettingsRequest, ConversationSummary, CreateConversationRequest,
        },
        auth::Session, error::AppError,
    },
    assistant_store::{self, schedules::ExecutionSettings}, claude, codex, config,
};

pub(in crate::api::assistant) async fn prepare_conversation(payload: &CreateConversationRequest) -> Result<ExecutionSettings, AppError> {
    let agent = payload.agent.as_deref().unwrap_or("firefly");
    if !matches!(agent, "firefly" | "codex" | "claude") {
        return Err(AppError::bad_request("agent 必须是 firefly、codex 或 claude"));
    }
    let mode = payload.workspace_mode.as_deref().unwrap_or("shared");
    if !matches!(mode, "shared" | "parallel") || (mode == "parallel" && agent == "firefly") {
        return Err(AppError::bad_request("workspace_mode 必须是 shared 或 parallel；并行模式仅支持 Codex / Claude"));
    }
    let workspace_cwd = if matches!(agent, "codex" | "claude") {
        let (_abs, display) = codex::resolve_workspace_path(payload.cwd.as_deref())
            .await
            .map_err(|err| AppError::bad_request(err.to_string()))?;
        Some(display)
    } else {
        None
    };
    if mode == "shared" {
        let raw = workspace_cwd.as_deref().unwrap_or("");
        if raw.split('/').next() == Some(".assistant-parallel") {
            return Err(AppError::bad_request("副本目录仅供所属会话使用；从副本新建工作请开启并行模式"));
        }
    }
    let normalize = |value: &Option<String>| value.as_deref().map(str::trim).filter(|value| !value.is_empty()).map(str::to_owned);
    let (provider, model, reasoning_level) = match agent {
        "codex" => {
            let provider = normalize(&payload.provider);
            let model = normalize(&payload.model);
            validate_codex_selection(provider.as_deref(), model.as_deref())?;
            (provider, model, normalize(&payload.reasoning_level))
        }
        "claude" => (
            claude::normalize_provider(payload.provider.clone()).map_err(AppError::bad_request)?,
            claude::normalize_model(payload.model.clone()).map_err(AppError::bad_request)?,
            claude::normalize_reasoning_level(payload.reasoning_level.clone()).map_err(AppError::bad_request)?,
        ),
        _ => (None, None, None),
    };
    Ok(ExecutionSettings {
        agent: agent.into(), workspace_mode: mode.into(), cwd: workspace_cwd,
        provider, model, reasoning_level,
    })
}

fn validate_codex_selection(provider: Option<&str>, model: Option<&str>) -> Result<(), AppError> {
    if provider.is_some_and(|provider| provider.len() > 256) || model.is_some_and(|model| model.len() > 256) {
        return Err(AppError::bad_request("平台或模型名称过长"));
    }
    let Some(provider) = provider.filter(|provider| *provider != "openai") else { return Ok(()) };
    let listing = codex::list_model_providers();
    let known = listing.providers.iter().find(|item| item.name == provider)
        .ok_or_else(|| AppError::bad_request(format!("未知的 Codex API 平台: {provider}")))?;
    if !known.models.is_empty() && model.is_some_and(|model| !known.models.iter().any(|item| item == model)) {
        return Err(AppError::bad_request(format!("模型 {} 不在 API 平台 {provider} 的可选列表中", model.unwrap())));
    }
    Ok(())
}

pub(in crate::api::assistant) async fn system_prompt_for_agent(agent: &str) -> Result<String, AppError> {
    if agent == "firefly" { Ok(crate::agent_settings::Settings::load(crate::db::pool()).await?.firefly.instruction) }
    else { Ok(config::get().assistant_system_prompt.clone()) }
}

pub async fn update_codex_settings(
    auth_user: Session, Path(conversation_id): Path<i64>, Json(payload): Json<CodexSettingsRequest>,
) -> Result<Json<ConversationSummary>, AppError> {
    let user_id = auth_user.storage_scope();
    assistant_store::get_conversation_for_read(conversation_id, user_id).await
        .map_err(AppError::internal_server_error)?.ok_or(AppError::not_found("Conversation not found"))?;
    let normalize = |value: Option<String>| value.map(|value| value.trim().to_owned()).filter(|value| !value.is_empty());
    let provider = normalize(payload.provider);
    let model = normalize(payload.model);
    let reasoning_level = normalize(payload.reasoning_level);
    validate_codex_selection(provider.as_deref(), model.as_deref())?;
    // Reasoning effort is an extensible string in the Codex protocol. Don't reject future/custom levels.
    if reasoning_level.as_deref().is_some_and(|level| level.len() > 64 || level.chars().any(char::is_whitespace)) {
        return Err(AppError::bad_request("推理级别格式无效"));
    }
    let record = assistant_store::set_codex_settings(
        conversation_id, user_id, provider.as_deref(), model.as_deref(), reasoning_level.as_deref(),
    ).await.map_err(|err| match err.downcast_ref::<assistant_store::ConversationSettingsError>() {
        Some(assistant_store::ConversationSettingsError::NotCodex) => AppError::bad_request(err),
        Some(_) => AppError::conflict(err),
        None => AppError::internal_server_error(err),
    })?.ok_or(AppError::not_found("Conversation not found"))?;
    let is_running = assistant_store::list_active_generation_conversation_ids(user_id).await
        .map_err(AppError::internal_server_error)?.contains(&conversation_id);
    Ok(Json(to_summary(record, is_running)))
}

pub async fn update_claude_settings(
    auth_user: Session, Path(conversation_id): Path<i64>, Json(payload): Json<ClaudeSettingsRequest>,
) -> Result<Json<ConversationSummary>, AppError> {
    let user_id = auth_user.storage_scope();
    assistant_store::get_conversation_for_read(conversation_id,user_id).await
        .map_err(AppError::internal_server_error)?.ok_or(AppError::not_found("Conversation not found"))?;
    let provider = claude::normalize_provider(payload.provider).map_err(AppError::bad_request)?;
    let model = claude::normalize_model(payload.model).map_err(AppError::bad_request)?;
    let reasoning_level = claude::normalize_reasoning_level(payload.reasoning_level).map_err(AppError::bad_request)?;
    let record = assistant_store::set_claude_settings(conversation_id,user_id,provider.as_deref(),model.as_deref(),reasoning_level.as_deref()).await
        .map_err(|error| match error.downcast_ref::<assistant_store::ConversationSettingsError>() {
            Some(assistant_store::ConversationSettingsError::NotClaude) => AppError::bad_request(error),
            Some(_) => AppError::conflict(error),
            None => AppError::internal_server_error(error),
        })?.ok_or(AppError::not_found("Conversation not found"))?;
    let running = assistant_store::list_active_generation_conversation_ids(user_id).await
        .map_err(AppError::internal_server_error)?.contains(&conversation_id);
    Ok(Json(to_summary(record,running)))
}
