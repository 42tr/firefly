//! Conversation-level Codex model and reasoning settings.
use std::path::Path;
use anyhow::anyhow;
use serde_json::{json, Value};

use crate::config;
use firefly_codex_transport::app_server;

/// 选择生效模型：对话绑定 > 全局 CODEX_MODEL > 交给 codex 读取 config.toml 默认。
pub(crate) fn effective_model(conversation_model: Option<&str>) -> Option<String> {
    if let Some(model) = conversation_model.map(str::trim).filter(|model| !model.is_empty()) {
        return Some(model.to_string());
    }
    let global = config::try_get().map(|config| config.codex_model.trim().to_string()).unwrap_or_default();
    if global.is_empty() {
        None
    } else {
        Some(global)
    }
}

pub struct ConversationSettings {
    pub provider: String,
    pub model: String,
    pub reasoning_level: String,
}

/// Resume inherits saved settings when an override is omitted. Resolve defaults explicitly so clearing a selection takes effect.
pub async fn resolve_conversation_settings(
    cwd: &Path, provider: Option<&str>, model: Option<&str>, reasoning_level: Option<&str>,
) -> anyhow::Result<ConversationSettings> {
    let client = app_server().await?;
    let defaults = client.request("config/read", json!({"cwd":cwd, "includeLayers":false})).await?;
    let configured = &defaults["config"];
    let provider = provider.filter(|value| !value.is_empty())
        .or(configured["model_provider"].as_str()).unwrap_or("openai").to_owned();
    let model = effective_model(model).or_else(|| configured["model"].as_str().map(ToOwned::to_owned));
    let effort = reasoning_level.filter(|value| !value.is_empty())
        .or(configured["model_reasoning_effort"].as_str());
    let catalog = if model.is_none() || effort.is_none() {
        let result = client.request("model/list", json!({"limit":100, "includeHidden":true})).await?;
        result["data"].as_array().ok_or_else(|| anyhow!("Codex 未返回默认模型信息"))?.clone()
    } else { Vec::new() };
    resolve_settings_from_sources(provider, model, effort, &catalog)
}

pub(crate) fn resolve_settings_from_sources(
    provider: String, model: Option<String>, effort: Option<&str>, catalog: &[Value],
) -> anyhow::Result<ConversationSettings> {
    let model = model.or_else(|| catalog.iter().find(|item| item["isDefault"] == true)
        .and_then(|item| item["model"].as_str()).map(ToOwned::to_owned))
        .ok_or_else(|| anyhow!("无法确定 Codex 默认模型，请在模型设置中选择模型"))?;
    let unprefixed = model.strip_prefix(&format!("{provider}/")).unwrap_or(&model);
    let info = catalog.iter().find(|item| item["model"] == model || item["model"] == unprefixed);
    let reasoning_level = effort.or_else(|| info.and_then(|item| item["defaultReasoningEffort"].as_str()))
        .unwrap_or_else(|| {
            if info.is_some_and(|item| item["supportedReasoningEfforts"].as_array().is_some_and(Vec::is_empty)) {
                "none"
            } else {
                // Use standard medium reasoning when a custom model has no catalog entry.
                "medium"
            }
        })
        .to_owned();
    Ok(ConversationSettings { provider, model, reasoning_level })
}
