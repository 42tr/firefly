//! UI-managed model profiles shared by the two native CLI adapters.
use anyhow::{ensure, Context};
use serde_json::{json, Value};
use crate::{agent_settings::{ModelProfile, Settings}, db};

pub const PREFIX: &str = "firefly:";
pub fn profile_id(provider: &str) -> Option<&str> { provider.strip_prefix(PREFIX).or_else(|| provider.strip_prefix("api-")) }

pub async fn resolve(provider: Option<&str>) -> anyhow::Result<Option<ModelProfile>> {
    let Some(id) = provider.and_then(profile_id) else { return Ok(None) };
    let mut profile = Settings::load(db::pool()).await?.profiles.into_iter().find(|p| p.id == id)
        .context("所选界面模型配置已删除，请重新选择 API 平台")?;
    ensure!(profile.default_model().is_some(), "请先在 Agent 配置中为此平台获取或填写模型");
    if profile.model.trim().is_empty() { profile.model = profile.default_model().unwrap().to_owned(); }
    Ok(Some(profile))
}

pub async fn listing(runtime: &str) -> anyhow::Result<Vec<Value>> {
    Ok(Settings::load(db::pool()).await?.profiles.into_iter().filter(|p| p.default_model().is_some() && p.supports(runtime))
        .map(|p| {
            let protocol = if runtime == "claude" { "anthropic" } else { &p.protocol };
            json!({"name": format!("{PREFIX}{}", p.id), "display_name": p.name,
            "aliases": if p.agent == "claude" { vec![format!("api-{}",p.id),p.id.clone(),p.name.clone()] } else { vec![format!("api-{}",p.id)] },
            "source":"ui", "base_url":p.base_url, "models":p.model_ids(),
            "default_model":p.default_model(),
            "default_reasoning_level":p.default_model().and_then(|model| p.reasoning_level_for_protocol(model, None, None, protocol)),
            "model_reasoning":p.models.iter().map(|model| {
                let (levels, default) = model.reasoning_options(protocol);
                json!({"model":model.id, "reasoning_levels":levels, "default_reasoning_level":default})
            }).collect::<Vec<_>>()})
        }).collect())
}

pub fn claude_url(profile: &ModelProfile) -> String {
    if let Some(url) = profile.claude_base_url.as_deref().filter(|v| !v.is_empty()) { return url.trim_end_matches('/').into() }
    profile.base_url.trim_end_matches('/').strip_suffix("/v1")
        .unwrap_or(profile.base_url.trim_end_matches('/')).to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    fn profile() -> ModelProfile { ModelProfile {
        id:"kimi".into(), name:"Kimi".into(), base_url:"https://api.kimi.com/coding/v1/".into(),
        claude_base_url:None, model:"test-model".into(), api_key:Some("private-test-key".into()),
        models:vec![], context_tokens:Some(32768), _key_configured:false, ..Default::default() }}
    #[test]
    fn endpoints_are_protocol_specific_and_can_be_overridden() {
        let mut p = profile();
        assert_eq!(claude_url(&p), "https://api.kimi.com/coding");
        p.claude_base_url = Some("https://api.kimi.com/anthropic/".into());
        assert_eq!(claude_url(&p), "https://api.kimi.com/anthropic");
        assert_eq!(profile_id("firefly:kimi"), Some("kimi"));
        assert_eq!(profile_id("openai"), None);
    }
    #[tokio::test]
    async fn protocol_capabilities_persist_and_reach_each_runtime_without_crossing_defaults() {
        use crate::agent_settings::{ProfileModel, ReasoningOptions};
        let pool = crate::db::test_pool().await; crate::db::set_test_pool(pool.clone());
        let mut settings = Settings::load(&pool).await.unwrap();
        let mut p = profile();
        let mut model = ProfileModel::from("test-model");
        model.reasoning_by_protocol.insert("responses".into(), ReasoningOptions {
            reasoning_levels: vec!["none".into(), "high".into(), "max".into()], default_reasoning_level: Some("high".into()),
        });
        model.reasoning_by_protocol.insert("anthropic".into(), ReasoningOptions {
            reasoning_levels: vec!["high".into(), "max".into()], default_reasoning_level: Some("max".into()),
        });
        p.models = vec![model];
        settings.profiles.push(p); settings.firefly.model_profile = Some("kimi".into());
        settings.save(&pool).await.unwrap().unwrap();
        let codex = listing("codex").await.unwrap();
        let claude = listing("claude").await.unwrap();
        assert_eq!(codex[0]["model_reasoning"][0]["reasoning_levels"], json!(["none", "high", "max"]));
        assert_eq!(codex[0]["default_reasoning_level"], "high");
        assert_eq!(claude[0]["model_reasoning"][0]["reasoning_levels"], json!(["high", "max"]));
        assert_eq!(claude[0]["default_reasoning_level"], "max");
        let loaded = Settings::load(&pool).await.unwrap();
        assert_eq!(loaded.reasoning_level(&loaded.firefly).unwrap().as_deref(), Some("high"));
        let profile = &loaded.profiles[0];
        assert_eq!(profile.reasoning_level_for_protocol("test-model", Some("none"), None, "anthropic").as_deref(), Some("max"));
        let resolved = crate::codex::resolve_conversation_settings(std::path::Path::new("."), Some("firefly:kimi"), Some("test-model"), Some("max")).await.unwrap();
        assert_eq!(resolved.reasoning_level, "max");
        let mut invalid = loaded;
        invalid.profiles[0].models[0].reasoning_by_protocol.get_mut("anthropic").unwrap().default_reasoning_level = Some("low".into());
        assert!(invalid.validate().is_err());
    }
    #[tokio::test]
    async fn catalog_never_exposes_keys_and_deleted_profiles_do_not_fall_back() {
        let pool = crate::db::test_pool().await; crate::db::set_test_pool(pool.clone());
        let mut settings = Settings::load(&pool).await.unwrap();
        let mut p = profile();
        p.models = vec![
            crate::agent_settings::ProfileModel { id:"test-model".into(), context_length:Some(65536), input_budget:Some(60000),
                reasoning_levels:Some(vec!["low".into(),"high".into(),"max".into()]), default_reasoning_level:Some("high".into()), ..Default::default() },
            crate::agent_settings::ProfileModel { id:"second-model".into(), context_length:Some(32768), input_budget:Some(30000), ..Default::default() },
        ];
        settings.profiles.push(p); settings.firefly.model_profile=Some("kimi".into());
        let saved = settings.save(&pool).await.unwrap().unwrap();
        let catalog = listing("codex").await.unwrap();
        assert_eq!(catalog[0]["name"], "firefly:kimi");
        assert_eq!(catalog[0]["models"], json!(["test-model", "second-model"]));
        assert_eq!(catalog[0]["model_reasoning"][0]["reasoning_levels"], json!(["low", "high", "max"]));
        assert_eq!(catalog[0]["default_reasoning_level"], "high");
        let codex = crate::codex::resolve_conversation_settings(std::path::Path::new("."), Some("firefly:kimi"), Some("test-model"), Some("medium")).await.unwrap();
        assert_eq!(codex.reasoning_level, "high");
        let loaded = Settings::load(&pool).await.unwrap();
        assert_eq!(loaded.profiles[0].models[1].context_length, Some(32768));
        assert!(!serde_json::to_string(&catalog).unwrap().contains("private-test-key"));
        assert_eq!(resolve(Some("firefly:kimi")).await.unwrap().unwrap().api_key.as_deref(), Some("private-test-key"));
        assert_eq!(resolve(Some("api-kimi")).await.unwrap().unwrap().id, "kimi");
        let mut removed = saved; removed.profiles.clear(); removed.firefly.model_profile=None;
        removed.save(&pool).await.unwrap().unwrap();
        assert!(resolve(Some("firefly:kimi")).await.is_err());
        assert!(resolve(Some("openai")).await.unwrap().is_none());
    }
}
