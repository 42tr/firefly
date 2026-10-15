use std::{
    ffi::OsString,
    io::Read,
    path::{Path, PathBuf},
};

use anyhow::{anyhow, ensure, Context};
use serde::Serialize;
use serde_json::{Map, Value};
use tokio::process::Command;

use crate::config::Config;

pub const REASONING_LEVELS: &[&str] = &["low", "medium", "high", "xhigh", "max"];
const MAX_SETTINGS_BYTES: u64 = 2 * 1024 * 1024;
const MODEL_ENV_KEYS: &[&str] = &[
    "ANTHROPIC_MODEL",
    "ANTHROPIC_DEFAULT_OPUS_MODEL",
    "ANTHROPIC_DEFAULT_SONNET_MODEL",
    "ANTHROPIC_DEFAULT_HAIKU_MODEL",
    "ANTHROPIC_DEFAULT_FABLE_MODEL",
    "ANTHROPIC_CUSTOM_MODEL_OPTION",
];

/// Native Claude settings are private: only an explicit projection reaches the API.
#[derive(Clone)]
pub struct NativeSettings(Value);

impl Default for NativeSettings {
    fn default() -> Self {
        Self(serde_json::json!({}))
    }
}

impl NativeSettings {
    fn load(path: &Path) -> anyhow::Result<Self> {
        ensure!(
            std::fs::metadata(path)
                .context("无法读取 Claude settings 文件")?
                .is_file(),
            "Claude settings 须为普通文件"
        );
        let file = std::fs::File::open(path).context("无法读取 Claude settings 文件")?;
        ensure!(file.metadata()?.is_file(), "Claude settings 须为普通文件");
        let mut bytes = Vec::new();
        file.take(MAX_SETTINGS_BYTES + 1).read_to_end(&mut bytes)?;
        ensure!(
            bytes.len() as u64 <= MAX_SETTINGS_BYTES,
            "Claude settings 文件超过 2 MiB"
        );
        // Do not attach serde's typed value errors, which can contain credentials.
        let value: Value =
            serde_json::from_slice(&bytes).map_err(|_| anyhow!("Claude settings JSON 格式无效"))?;
        ensure!(value.is_object(), "Claude settings 须为 JSON 对象");
        if let Some(env) = value.get("env") {
            let env = env
                .as_object()
                .ok_or_else(|| anyhow!("Claude settings.env 须为对象"))?;
            ensure!(
                env.iter().all(|(key, value)| !key.contains(['=', '\0'])
                    && value.as_str().is_some_and(|value| !value.contains('\0'))),
                "Claude settings.env 须包含有效的字符串环境变量"
            );
        }
        for key in ["model", "effortLevel", "apiKeyHelper"] {
            ensure!(
                value.get(key).is_none_or(Value::is_string),
                "Claude settings.{key} 须为字符串"
            );
        }
        if let Some(models) = value.get("availableModels") {
            ensure!(
                models
                    .as_array()
                    .is_some_and(|models| models.iter().all(Value::is_string)),
                "Claude settings.availableModels 须为字符串数组"
            );
        }
        let settings = Self(value);
        for model in settings.models() {
            super::normalize_model(Some(model))?;
        }
        if let Some(url) = settings.env_value("ANTHROPIC_BASE_URL") {
            let url = reqwest::Url::parse(url).map_err(|_| anyhow!("Claude API 地址无效"))?;
            ensure!(
                matches!(url.scheme(), "https" | "http")
                    && url.host_str().is_some()
                    && url.username().is_empty()
                    && url.password().is_none()
                    && url.query().is_none()
                    && url.fragment().is_none(),
                "Claude API 地址须为无凭据的 HTTP(S) 地址"
            );
        }
        Ok(settings)
    }

    fn env_value(&self, key: &str) -> Option<&str> {
        self.0
            .get("env")?
            .get(key)?
            .as_str()
            .map(str::trim)
            .filter(|value| !value.is_empty())
    }

    fn model(&self, inherited_model: Option<&str>) -> Option<String> {
        let inherited_model = if self
            .0
            .get("env")
            .and_then(|env| env.get("ANTHROPIC_MODEL"))
            .is_some()
        {
            None
        } else {
            inherited_model
        };
        self.env_value("ANTHROPIC_MODEL")
            .or(inherited_model)
            .or_else(|| self.0.get("model").and_then(Value::as_str))
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
    }

    fn models(&self) -> Vec<String> {
        let mut models = Vec::new();
        if let Some(model) = self.model(None) {
            models.push(model);
        }
        if let Some(values) = self.0.get("availableModels").and_then(Value::as_array) {
            models.extend(values.iter().filter_map(Value::as_str).map(str::to_owned));
        }
        models.extend(
            MODEL_ENV_KEYS
                .iter()
                .filter_map(|key| self.env_value(key))
                .map(str::to_owned),
        );
        let mut seen = std::collections::HashSet::new();
        models
            .retain(|model| !model.is_empty() && model != "default" && seen.insert(model.clone()));
        models
    }

    fn reasoning_level(&self) -> Option<String> {
        self.env_value("CLAUDE_CODE_EFFORT_LEVEL")
            .or_else(|| self.0.get("effortLevel").and_then(Value::as_str))
            .filter(|level| REASONING_LEVELS.contains(level))
            .map(str::to_owned)
    }
}

#[derive(Clone, Serialize)]
pub struct ClaudeProvider {
    pub name: String,
    pub base_url: Option<String>,
    pub models: Vec<String>,
    pub default_model: Option<String>,
    pub default_reasoning_level: Option<String>,
    #[serde(skip_serializing)]
    settings: NativeSettings,
}

pub struct ProviderListing {
    pub providers: Vec<ClaudeProvider>,
    pub warnings: Vec<String>,
}

fn config_dir_from(explicit: Option<OsString>, home: Option<OsString>) -> Option<PathBuf> {
    explicit
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            home.filter(|home| !home.is_empty())
                .map(|home| PathBuf::from(home).join(".claude"))
        })
}

pub fn config_dir() -> Option<PathBuf> {
    config_dir_from(
        std::env::var_os("CLAUDE_CONFIG_DIR"),
        std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")),
    )
}

fn read_cli_settings_at(root: Option<&Path>) -> anyhow::Result<NativeSettings> {
    let Some(path) = root.map(|root| root.join("settings.json")) else {
        return Ok(NativeSettings::default());
    };
    match std::fs::metadata(&path) {
        Ok(_) => NativeSettings::load(&path).context("Claude 默认 settings.json 无效"),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(NativeSettings::default()),
        Err(error) => Err(error).context("无法读取 Claude 默认 settings.json"),
    }
}

pub struct CliDefaults {
    pub model: String,
    pub models: Vec<String>,
    pub reasoning_level: Option<String>,
}

fn cli_defaults_from(
    cfg: &Config,
    settings: &NativeSettings,
    inherited_model: Option<&str>,
) -> CliDefaults {
    let model = (!cfg.claude_model.trim().is_empty())
        .then(|| cfg.claude_model.trim().to_owned())
        .or_else(|| settings.model(inherited_model))
        .unwrap_or_else(|| "default".into());
    let mut models = if cfg.claude_models.trim().is_empty() {
        settings.models()
    } else {
        cfg.claude_models
            .split(',')
            .map(str::trim)
            .filter(|model| !model.is_empty())
            .map(str::to_owned)
            .collect()
    };
    if models.is_empty() {
        models = vec!["sonnet".into(), "opus".into(), "haiku".into()];
    }
    if model != "default" && !models.contains(&model) {
        models.insert(0, model.clone());
    }
    let mut seen = std::collections::HashSet::new();
    models.retain(|model| seen.insert(model.clone()));
    CliDefaults {
        model,
        models,
        reasoning_level: settings.reasoning_level(),
    }
}

pub fn cli_defaults(cfg: &Config) -> anyhow::Result<CliDefaults> {
    let settings = read_cli_settings_at(config_dir().as_deref())?;
    let inherited_model = std::env::var("ANTHROPIC_MODEL").ok();
    Ok(cli_defaults_from(
        cfg,
        &settings,
        inherited_model
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty()),
    ))
}

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name != "."
        && name != ".."
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_.-".contains(&b))
}

fn read_provider(root: &Path, name: &str) -> anyhow::Result<ClaudeProvider> {
    ensure!(
        valid_name(name) && name != "cli",
        "Claude 平台名称格式无效（cli 为保留名称）"
    );
    let settings = NativeSettings::load(&root.join("providers").join(format!("{name}.json")))
        .with_context(|| format!("Claude 平台 {name} 的 settings 文件无效或不存在"))?;
    let base_url = settings.env_value("ANTHROPIC_BASE_URL").map(str::to_owned);
    ensure!(
        base_url.is_some()
            || [
                "CLAUDE_CODE_USE_BEDROCK",
                "CLAUDE_CODE_USE_VERTEX",
                "CLAUDE_CODE_USE_FOUNDRY"
            ]
            .iter()
            .any(|key| settings
                .env_value(key)
                .is_some_and(|value| matches!(value, "1" | "true"))),
        "Claude 平台 {name} 须在原生 env 中指定 API 地址或云平台"
    );
    Ok(ClaudeProvider {
        name: name.to_owned(),
        base_url,
        models: settings.models(),
        default_model: settings.model(None),
        default_reasoning_level: settings.reasoning_level(),
        settings,
    })
}

fn providers_at(root: Option<&Path>) -> anyhow::Result<ProviderListing> {
    let mut listing = ProviderListing {
        providers: Vec::new(),
        warnings: Vec::new(),
    };
    let Some(root) = root else {
        return Ok(listing);
    };
    let entries = match std::fs::read_dir(root.join("providers")) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(listing),
        Err(error) => return Err(error).context("无法读取 Claude providers 目录"),
    };
    let mut names = Vec::new();
    for entry in entries {
        let path = entry?.path();
        if path.extension().and_then(|value| value.to_str()) != Some("json") || !path.is_file() {
            continue;
        }
        if let Some(name) = path.file_stem().and_then(|value| value.to_str()) {
            names.push(name.to_owned());
        }
    }
    names.sort();
    for name in names {
        match read_provider(root, &name) {
            Ok(profile) => listing.providers.push(profile),
            Err(error) => listing.warnings.push(format!("{name}.json: {error:#}")),
        }
    }
    Ok(listing)
}

pub fn providers() -> anyhow::Result<ProviderListing> {
    providers_at(config_dir().as_deref())
}

pub fn resolve_provider(provider: Option<&str>) -> anyhow::Result<Option<ClaudeProvider>> {
    let name = provider.filter(|value| !value.is_empty()).unwrap_or("cli");
    if name == "cli" {
        return Ok(None);
    }
    ensure!(valid_name(name), "Claude 平台名称格式无效");
    let root = config_dir().ok_or_else(|| anyhow!("无法定位 Claude 配置目录"))?;
    read_provider(&root, name).map(Some)
}

pub fn normalize_provider(provider: Option<String>) -> anyhow::Result<Option<String>> {
    let provider = provider
        .map(|name| name.trim().to_owned())
        .filter(|name| !name.is_empty());
    ensure!(
        provider.as_deref().is_none_or(valid_name),
        "Claude 平台名称格式无效"
    );
    resolve_provider(provider.as_deref())?;
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
    profile: Option<&ClaudeProvider>,
    model: Option<&str>,
) -> anyhow::Result<String> {
    if let Some(model) = model.map(str::trim).filter(|value| !value.is_empty()) {
        return Ok(model.to_owned());
    }
    match profile {
        Some(profile) => Ok(profile
            .default_model
            .clone()
            .unwrap_or_else(|| "default".into())),
        None => Ok(cli_defaults(cfg)?.model),
    }
}

/// Compose one native settings snapshot for this turn. Claude evaluates native
/// apiKeyHelper itself; listing profiles never executes commands or exposes env.
pub fn configure_command(
    command: &mut Command,
    profile: Option<&ClaudeProvider>,
    reasoning_level: Option<&str>,
) -> anyhow::Result<Option<tempfile::NamedTempFile>> {
    use std::io::Write;
    let mut settings = profile
        .map(|profile| profile.settings.0.clone())
        .unwrap_or_else(|| serde_json::json!({}));
    let mut env = Map::new();
    if let Some(profile) = profile {
        for key in [
            "ANTHROPIC_API_KEY",
            "ANTHROPIC_AUTH_TOKEN",
            "CLAUDE_CODE_OAUTH_TOKEN",
            "CLAUDE_CODE_OAUTH_TOKEN_FILE_DESCRIPTOR",
            "ANTHROPIC_BASE_URL",
            "ANTHROPIC_CUSTOM_HEADERS",
            "ANTHROPIC_PROFILE",
            "ANTHROPIC_FEDERATION_RULE_ID",
            "CLAUDE_CODE_SUBAGENT_MODEL",
        ]
        .into_iter()
        .chain(MODEL_ENV_KEYS.iter().copied())
        {
            env.insert(key.into(), "".into());
        }
        for key in [
            "CLAUDE_CODE_USE_BEDROCK",
            "CLAUDE_CODE_USE_VERTEX",
            "CLAUDE_CODE_USE_FOUNDRY",
        ] {
            env.insert(key.into(), "0".into());
        }
        // Clear inherited env effort so native effortLevel/modelSettings can
        // resolve the default; an explicit choice below still takes priority.
        env.insert("CLAUDE_CODE_EFFORT_LEVEL".into(), "".into());
        if let Some(native_env) = profile.settings.0.get("env").and_then(Value::as_object) {
            env.extend(native_env.clone());
        }
        if settings.get("apiKeyHelper").is_none() {
            settings["apiKeyHelper"] = "".into();
        }
    }
    if let Some(level) = reasoning_level {
        env.insert("CLAUDE_CODE_EFFORT_LEVEL".into(), level.into());
    }
    if env.is_empty() {
        return Ok(None);
    }
    for (name, value) in &env {
        command.env(name, value.as_str().context("Claude env 值须为字符串")?);
    }
    settings["env"] = env.into();
    let mut file = tempfile::NamedTempFile::new().context("创建 Claude 本轮临时配置失败")?;
    serde_json::to_writer(&mut file, &settings)?;
    file.flush()?;
    command.arg("--settings").arg(file.path());
    Ok(Some(file))
}

#[cfg(test)]
#[path = "settings_tests.rs"]
mod tests;
