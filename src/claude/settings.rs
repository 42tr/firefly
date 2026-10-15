use anyhow::{anyhow, ensure, Context};
use serde::{Deserialize, Serialize};
use tokio::process::Command;

use crate::config::Config;

pub const REASONING_LEVELS: &[&str] = &["low", "medium", "high", "xhigh", "max"];

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ClaudeProvider {
    pub name: String,
    pub base_url: String,
    #[serde(default)]
    pub models: Vec<String>,
    #[serde(default)]
    pub default_model: Option<String>,
    #[serde(default, skip_serializing)]
    pub api_key_env: Option<String>,
    #[serde(default, skip_serializing)]
    pub auth_token_env: Option<String>,
}

pub fn providers(cfg: &Config) -> anyhow::Result<Vec<ClaudeProvider>> {
    if cfg.claude_providers.trim().is_empty() {
        return Ok(Vec::new());
    }
    let profiles: Vec<ClaudeProvider> = serde_json::from_str(&cfg.claude_providers)
        .map_err(|_| anyhow!("CLAUDE_PROVIDERS 必须是平台配置的 JSON 数组，请检查配置字段"))?;
    let mut names = std::collections::HashSet::new();
    for profile in &profiles {
        ensure!(
            !profile.name.is_empty()
                && profile.name.len() <= 64
                && profile.name != "cli"
                && profile
                    .name
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"_.-".contains(&b)),
            "Claude 平台名称格式无效（cli 为保留名称）"
        );
        ensure!(
            names.insert(&profile.name),
            "Claude 平台名称重复: {}",
            profile.name
        );
        let url = reqwest::Url::parse(&profile.base_url)
            .map_err(|_| anyhow!("Claude 平台 {} 的 API 地址无效", profile.name))?;
        ensure!(
            matches!(url.scheme(), "https" | "http")
                && url.host_str().is_some()
                && url.username().is_empty()
                && url.password().is_none()
                && url.query().is_none()
                && url.fragment().is_none(),
            "Claude 平台 {} 的 API 地址须为无凭据的 HTTP(S) 地址",
            profile.name
        );
        ensure!(
            profile.api_key_env.is_some() ^ profile.auth_token_env.is_some(),
            "Claude 平台 {} 须配置 api_key_env 或 auth_token_env 之一",
            profile.name
        );
        for name in [&profile.api_key_env, &profile.auth_token_env]
            .into_iter()
            .flatten()
        {
            ensure!(
                !name.is_empty()
                    && !name.as_bytes()[0].is_ascii_digit()
                    && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_'),
                "Claude 平台密钥环境变量名称无效"
            );
        }
        for model in profile.models.iter().chain(profile.default_model.iter()) {
            super::normalize_model(Some(model.clone()))?;
            ensure!(
                !model.trim().is_empty() && model.trim() == model,
                "Claude 平台模型名称不可为空或包含首尾空格"
            );
        }
    }
    Ok(profiles)
}

pub fn default_provider(cfg: &Config) -> &str {
    let name = cfg.claude_provider.trim();
    if name.is_empty() {
        "cli"
    } else {
        name
    }
}

pub fn resolve_provider(
    cfg: &Config,
    provider: Option<&str>,
) -> anyhow::Result<Option<ClaudeProvider>> {
    let name = provider
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| default_provider(cfg));
    if name == "cli" {
        return Ok(None);
    }
    providers(cfg)?
        .into_iter()
        .find(|profile| profile.name == name)
        .map(Some)
        .ok_or_else(|| anyhow!("未知的 Claude API 平台: {name}"))
}

pub fn normalize_provider(
    cfg: &Config,
    provider: Option<String>,
) -> anyhow::Result<Option<String>> {
    let provider = provider
        .map(|name| name.trim().to_owned())
        .filter(|name| !name.is_empty());
    ensure!(
        provider.as_deref().is_none_or(|name| name.len() <= 64
            && name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"_.-".contains(&b))),
        "Claude 平台名称格式无效"
    );
    resolve_provider(cfg, provider.as_deref())?;
    Ok(provider)
}

pub fn normalize_reasoning_level(level: Option<String>) -> anyhow::Result<Option<String>> {
    let level = level
        .map(|level| level.trim().to_owned())
        .filter(|level| !level.is_empty());
    ensure!(
        level
            .as_deref()
            .is_none_or(|level| REASONING_LEVELS.contains(&level)),
        "Claude 推理级别须为 low、medium、high、xhigh 或 max"
    );
    Ok(level)
}

pub fn effective_model(
    cfg: &Config,
    provider: Option<&ClaudeProvider>,
    model: Option<&str>,
) -> String {
    model
        .map(str::trim)
        .filter(|model| !model.is_empty())
        .map(str::to_owned)
        .or_else(|| {
            provider.and_then(|profile| {
                profile
                    .default_model
                    .clone()
                    .or_else(|| profile.models.first().cloned())
            })
        })
        .unwrap_or_else(|| super::default_model(cfg))
}

/// Scope endpoint and credentials to this child; never mutate the server environment.
pub fn configure_command(
    command: &mut Command,
    profile: Option<&ClaudeProvider>,
    reasoning_level: Option<&str>,
) -> anyhow::Result<Option<tempfile::NamedTempFile>> {
    use std::io::Write;

    let mut env = serde_json::Map::new();
    if let Some(profile) = profile {
        let (target, source) = if let Some(source) = &profile.api_key_env {
            ("ANTHROPIC_API_KEY", source)
        } else {
            (
                "ANTHROPIC_AUTH_TOKEN",
                profile
                    .auth_token_env
                    .as_ref()
                    .context("Claude 平台缺少密钥配置")?,
            )
        };
        let secret = std::env::var(source)
            .ok()
            .filter(|value| !value.trim().is_empty())
            .with_context(|| {
                format!(
                    "Claude 平台 {} 的密钥环境变量 {source} 未设置",
                    profile.name
                )
            })?;
        for name in [
            "ANTHROPIC_API_KEY",
            "ANTHROPIC_AUTH_TOKEN",
            "CLAUDE_CODE_OAUTH_TOKEN",
            "CLAUDE_CODE_OAUTH_TOKEN_FILE_DESCRIPTOR",
            "CLAUDE_CODE_USE_BEDROCK",
            "CLAUDE_CODE_USE_VERTEX",
            "CLAUDE_CODE_USE_FOUNDRY",
            "ANTHROPIC_CUSTOM_HEADERS",
            "ANTHROPIC_DEFAULT_OPUS_MODEL",
            "ANTHROPIC_DEFAULT_SONNET_MODEL",
            "ANTHROPIC_DEFAULT_HAIKU_MODEL",
        ] {
            command.env_remove(name);
            env.insert(name.into(), "".into());
        }
        for name in [
            "CLAUDE_CODE_USE_BEDROCK",
            "CLAUDE_CODE_USE_VERTEX",
            "CLAUDE_CODE_USE_FOUNDRY",
        ] {
            command.env(name, "0");
            env.insert(name.into(), "0".into());
        }
        command
            .env("ANTHROPIC_BASE_URL", &profile.base_url)
            .env(target, &secret);
        env.insert("ANTHROPIC_BASE_URL".into(), profile.base_url.clone().into());
        env.insert(target.into(), secret.into());
    }
    // The CLI environment has priority over --effort. Match it to the explicit
    // selection, including when settings.env would override the inherited env.
    if let Some(level) = reasoning_level {
        command.env("CLAUDE_CODE_EFFORT_LEVEL", level);
        env.insert("CLAUDE_CODE_EFFORT_LEVEL".into(), level.into());
    }
    if env.is_empty() {
        return Ok(None);
    }
    // Claude loads settings.env after the inherited process environment. An
    // invocation-scoped settings file overrides user/project platform settings
    // as well, while keeping credentials out of argv and off the public API.
    let mut file = tempfile::NamedTempFile::new().context("创建 Claude 平台临时配置失败")?;
    serde_json::to_writer(&mut file, &serde_json::json!({"env": env}))?;
    file.flush()?;
    command.arg("--settings").arg(file.path());
    Ok(Some(file))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn cfg(profiles: serde_json::Value) -> Config {
        serde_json::from_value(json!({"jwt_secret":"test", "auth_password":"test",
            "claude_model":"global-model", "claude_providers":profiles.to_string()}))
        .unwrap()
    }

    fn profile() -> serde_json::Value {
        json!({"name":"relay", "base_url":"https://claude.example/api", "models":["relay-opus", "relay-sonnet"],
            "default_model":"relay-sonnet", "api_key_env":"FIREFLY_CLAUDE_TEST_KEY"})
    }

    #[test]
    fn claude_provider_selection_and_catalog_do_not_expose_credentials() {
        let mut config = cfg(json!([profile()]));
        assert!(resolve_provider(&config, None).unwrap().is_none());
        config.claude_provider = "relay".into();
        let selected = resolve_provider(&config, None).unwrap().unwrap();
        assert_eq!(
            effective_model(&config, Some(&selected), None),
            "relay-sonnet"
        );
        assert_eq!(
            effective_model(&config, Some(&selected), Some(" custom ")),
            "custom"
        );
        assert!(resolve_provider(&config, Some("cli")).unwrap().is_none());
        assert!(resolve_provider(&config, Some("missing")).is_err());
        assert_eq!(
            normalize_provider(&config, Some(" relay ".into()))
                .unwrap()
                .as_deref(),
            Some("relay")
        );
        let public = serde_json::to_string(&providers(&config).unwrap()).unwrap();
        assert!(!public.contains("api_key_env") && !public.contains("FIREFLY_CLAUDE_TEST_KEY"));
    }

    #[test]
    fn claude_provider_configuration_rejects_invalid_profiles_and_effort() {
        for (field, value) in [
            ("name", json!("cli")),
            ("base_url", json!("file:///tmp/key")),
            ("base_url", json!("https://user:secret@example.com")),
            ("api_key_env", json!("BAD=KEY")),
            ("models", json!(["--help"])),
        ] {
            let mut bad = profile();
            bad[field] = value;
            assert!(providers(&cfg(json!([bad]))).is_err(), "{field}");
        }
        assert!(providers(&cfg(json!([profile(), profile()]))).is_err());
        for level in REASONING_LEVELS {
            assert_eq!(
                normalize_reasoning_level(Some(format!(" {level} ")))
                    .unwrap()
                    .as_deref(),
                Some(*level)
            );
        }
        assert!(normalize_reasoning_level(Some("ultra".into())).is_err());
        assert_eq!(normalize_reasoning_level(Some(" ".into())).unwrap(), None);
    }

    #[test]
    fn claude_platform_and_effort_are_scoped_to_child_settings_and_removed_afterward() {
        std::env::set_var("FIREFLY_CLAUDE_TEST_KEY", "test-only-credential");
        let profile = providers(&cfg(json!([profile()]))).unwrap().remove(0);
        let mut command = Command::new("claude");
        let file = configure_command(&mut command, Some(&profile), Some("max"))
            .unwrap()
            .unwrap();
        let settings: serde_json::Value = serde_json::from_reader(file.reopen().unwrap()).unwrap();
        assert_eq!(
            settings["env"]["ANTHROPIC_BASE_URL"],
            "https://claude.example/api"
        );
        assert_eq!(settings["env"]["ANTHROPIC_API_KEY"], "test-only-credential");
        assert_eq!(settings["env"]["ANTHROPIC_AUTH_TOKEN"], "");
        assert_eq!(settings["env"]["CLAUDE_CODE_USE_BEDROCK"], "0");
        assert_eq!(settings["env"]["CLAUDE_CODE_EFFORT_LEVEL"], "max");
        let args: Vec<_> = command
            .as_std()
            .get_args()
            .map(|value| value.to_string_lossy())
            .collect();
        assert_eq!(args[0], "--settings");
        assert!(!args.iter().any(|arg| arg.contains("test-only-credential")));
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            file.as_file().metadata().unwrap().permissions().mode() & 0o777,
            0o600
        );
        let path = file.path().to_owned();
        drop(file);
        assert!(!path.exists());

        let mut command = Command::new("claude");
        assert!(configure_command(&mut command, None, None)
            .unwrap()
            .is_none());
        assert_eq!(command.as_std().get_envs().count(), 0);
        assert_eq!(command.as_std().get_args().count(), 0);
        let file = configure_command(&mut command, None, Some("low"))
            .unwrap()
            .unwrap();
        let settings: serde_json::Value = serde_json::from_reader(file.reopen().unwrap()).unwrap();
        assert_eq!(settings, json!({"env":{"CLAUDE_CODE_EFFORT_LEVEL":"low"}}));
    }

    #[test]
    fn claude_auth_tokens_use_bearer_auth_and_missing_keys_fail_before_launch() {
        std::env::set_var("FIREFLY_CLAUDE_TEST_TOKEN", "test-only-token");
        let mut value = profile();
        value.as_object_mut().unwrap().remove("api_key_env");
        value["auth_token_env"] = json!("FIREFLY_CLAUDE_TEST_TOKEN");
        let mut profile = providers(&cfg(json!([value]))).unwrap().remove(0);
        let mut command = Command::new("claude");
        let file = configure_command(&mut command, Some(&profile), None)
            .unwrap()
            .unwrap();
        let settings: serde_json::Value = serde_json::from_reader(file.reopen().unwrap()).unwrap();
        assert_eq!(settings["env"]["ANTHROPIC_AUTH_TOKEN"], "test-only-token");
        assert_eq!(settings["env"]["ANTHROPIC_API_KEY"], "");
        profile.auth_token_env = Some("FIREFLY_CLAUDE_UNSET_TEST_TOKEN".into());
        assert!(configure_command(&mut command, Some(&profile), None).is_err());
    }
}
