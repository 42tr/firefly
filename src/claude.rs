use std::path::Path;

use anyhow::Context;
use serde_json::Value;
use tokio::process::Command;

use crate::config;

pub use firefly_claude_transport::{session_id, session_title, text_delta};

mod settings;
mod sessions;
pub use sessions::acquire;
#[cfg(test)]
pub use sessions::Lease;
pub use settings::{cli_defaults, effective_model, normalize_reasoning_level, providers, REASONING_LEVELS};

pub async fn normalize_provider(provider: Option<String>) -> anyhow::Result<Option<String>> {
    let provider = provider.map(|p| p.trim().to_owned()).filter(|p| !p.is_empty());
    if crate::cli_profiles::resolve(provider.as_deref()).await?.is_some() { return Ok(provider) }
    if let Some(name) = provider.as_deref() {
        if crate::agent_settings::Settings::load(crate::db::pool()).await?.profiles.iter()
            .any(|profile| profile.agent == "claude" && (profile.id == name || profile.name == name)) {
            return Ok(provider);
        }
    }
    settings::normalize_provider(provider)
}

// The settings snapshot must outlive the child, including its tool subprocesses.
pub struct ClaudeTurn {
    transport: firefly_claude_transport::ClaudeTurn,
    _settings_file: Option<tempfile::NamedTempFile>,
}

impl ClaudeTurn {
    pub(crate) fn is_running(&mut self) -> anyhow::Result<bool> { self.transport.is_running() }
    pub async fn next_event(&mut self) -> anyhow::Result<Option<Value>> {
        self.transport.next_event().await
    }

    pub async fn interrupt(&mut self) -> anyhow::Result<()> {
        self.transport.interrupt().await
    }

    async fn send_prompt(&mut self, prompt: &str) -> anyhow::Result<String> {
        let id = uuid::Uuid::new_v4().to_string();
        self.transport.send(&serde_json::json!({
            "type":"user", "uuid":id, "session_id":"", "parent_tool_use_id":null,
            "message":{"role":"user","content":prompt},
        })).await?;
        Ok(id)
    }
}

/// The host keeps streaming input open until all work and its follow-up finish.
static CLAUDE_APPEND_SYSTEM_PROMPT: &str = include_str!("../prompts/claude.md");

/// 构建 claude CLI 参数。
fn claude_args(cfg: &config::Config, session_id: Option<&str>, model: &str, reasoning_level: Option<&str>) -> Vec<String> {
    let mut args = vec![
        "--print".to_string(),
        "--verbose".to_string(),
        "--output-format".to_string(),
        "stream-json".to_string(),
        "--input-format".to_string(),
        "stream-json".to_string(),
        "--replay-user-messages".to_string(),
        "--include-partial-messages".to_string(),
        "--permission-mode".to_string(),
        cfg.claude_permission_mode.clone(),
        "--append-system-prompt".to_string(),
        CLAUDE_APPEND_SYSTEM_PROMPT.trim().to_string(),
    ];
    if !cfg.claude_allowed_tools.trim().is_empty() {
        args.push("--allowedTools".to_string());
        args.push(cfg.claude_allowed_tools.trim().to_string());
    }
    // Always specify a model so resuming a session cannot retain a previous
    // per-conversation override after the user restores the default.
    args.push("--model".to_string());
    args.push(model.to_owned());
    if let Some(level) = reasoning_level {
        args.extend(["--effort".to_owned(), level.to_owned()]);
    }
    if let Some(session_id) = session_id {
        args.push("--resume".to_string());
        args.push(session_id.to_string());
    }
    args
}

pub async fn prepare_turn(cwd: &Path, session_id: Option<&str>, provider: Option<&str>, model: Option<&str>, reasoning_level: Option<&str>) -> anyhow::Result<PreparedTurn> {
    if let Some(profile) = crate::cli_profiles::resolve(provider).await? {
        return prepare_turn_with_model_profile(cwd, session_id, &profile, model, reasoning_level).await;
    }
    let cfg = config::get();
    let profile = settings::resolve_provider(provider)?;
    let model = effective_model(&cfg, profile.as_ref(), model)?;
    let effort = reasoning_level.map(str::to_owned)
        .or_else(|| profile.as_ref().and_then(|profile| profile.default_reasoning_level.clone()))
        .or_else(|| settings::default_reasoning_level_for_model(&model));
    prepare_configured(&cfg, cwd, session_id, profile.as_ref(), &model, effort)
}

pub async fn prepare_turn_with_model_profile(
    cwd: &Path, session_id: Option<&str>, profile: &crate::agent_settings::ModelProfile,
    model: Option<&str>, reasoning_level: Option<&str>,
) -> anyhow::Result<PreparedTurn> {
    anyhow::ensure!(profile.supports("claude"), "Claude API 配置协议不匹配");
    let cfg = config::get();
    let mut profile = profile.clone();
    if let Some(model) = model.filter(|model| !model.is_empty()) { profile.model = model.to_owned(); }
    let native = if profile.protocol == "anthropic" { settings::provider_from_model_profile(&profile)? }
        else { settings::managed_provider(&profile) };
    let effort = profile.reasoning_level_for_protocol(&profile.model, reasoning_level, Some(REASONING_LEVELS), "anthropic")
        .or_else(|| (profile.protocol == "anthropic").then(|| native.default_reasoning_level.clone()).flatten());
    prepare_configured(&cfg, cwd, session_id, Some(&native), &profile.model, effort)
}

fn prepare_configured(
    cfg: &config::Config, cwd: &Path, session_id: Option<&str>, profile: Option<&settings::ClaudeProvider>,
    model: &str, effort: Option<String>,
) -> anyhow::Result<PreparedTurn> {
    let effort = normalize_reasoning_level(effort)?;
    let mut command = Command::new("claude");
    let settings_file = settings::configure_command_for_model(&mut command, profile, Some(model), effort.as_deref())?;
    command.current_dir(cwd).args(claude_args(cfg, session_id, model, effort.as_deref()));
    PreparedTurn::new(command, settings_file)
}

pub struct PreparedTurn {
    command: Command,
    settings_file: Option<tempfile::NamedTempFile>,
    identity: [u8; 32],
}

impl PreparedTurn {
    #[cfg(test)]
    pub(crate) fn for_test(command: Command) -> Self { Self::new(command, None).unwrap() }
    fn new(command: Command, settings_file: Option<tempfile::NamedTempFile>) -> anyhow::Result<Self> {
        use sha2::{Digest, Sha256};
        let mut hash = Sha256::new();
        let mut add = |value: &[u8]| { hash.update(value.len().to_le_bytes()); hash.update(value); };
        let mut skip = false;
        for arg in command.as_std().get_args() {
            if skip { skip = false; continue; }
            if arg == "--settings" || arg == "--resume" { skip = true; continue; }
            add(arg.to_string_lossy().as_bytes());
        }
        if let Some(cwd) = command.as_std().get_current_dir() {
            add(cwd.to_string_lossy().as_bytes());
            // A changed local/native setting must take effect on the next job.
            for parent in cwd.ancestors() {
                for name in ["settings.json", "settings.local.json"] {
                    if let Ok(bytes) = std::fs::read(parent.join(".claude").join(name)) { add(&bytes); }
                }
            }
        }
        let native_dir = std::env::var_os("CLAUDE_CONFIG_DIR").map(std::path::PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|home| std::path::PathBuf::from(home).join(".claude")));
        if let Some(path) = native_dir {
            if let Ok(bytes) = std::fs::read(path.join("settings.json")) { add(&bytes); }
        }
        for (key, value) in command.as_std().get_envs() {
            add(key.to_string_lossy().as_bytes());
            if let Some(value) = value { add(value.to_string_lossy().as_bytes()); }
        }
        if let Some(file) = &settings_file { add(&std::fs::read(file.path())?); }
        Ok(Self { command, settings_file, identity: hash.finalize().into() })
    }

    fn spawn(self) -> anyhow::Result<ClaudeTurn> {
        let transport = firefly_claude_transport::ClaudeTurn::spawn_streaming(self.command)
            .context("启动 Claude CLI 失败，请确认 Claude Code 已安装并完成登录")?;
        Ok(ClaudeTurn { transport, _settings_file: self.settings_file })
    }
}

#[cfg(test)]
fn spawn_turn(command: Command, settings_file: Option<tempfile::NamedTempFile>) -> anyhow::Result<ClaudeTurn> {
    let transport = firefly_claude_transport::ClaudeTurn::spawn(command)
        .context("启动 Claude CLI 失败，请确认 Claude Code 已安装并完成登录")?;
    Ok(ClaudeTurn { transport, _settings_file: settings_file })
}

pub fn normalize_model(model: Option<String>) -> anyhow::Result<Option<String>> {
    let model = model.map(|value| value.trim().to_owned()).filter(|value| !value.is_empty());
    if let Some(model) = &model {
        anyhow::ensure!(model.len() <= 256 && !model.starts_with('-')
            && !model.chars().any(|value| value.is_whitespace() || value.is_control()), "Claude 模型名称格式无效");
    }
    Ok(model)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::claude_args;

    #[tokio::test]
    async fn settings_snapshot_outlives_spawn_and_is_removed_with_turn() {
        let mut command = tokio::process::Command::new("python3");
        command.env_clear().args(["-u", "-c", r#"
import json, os, sys
with open(sys.argv[sys.argv.index('--settings') + 1]) as file:
    settings = json.load(file)
print(json.dumps({'effort': settings['env']['CLAUDE_CODE_EFFORT_LEVEL'],
                  'process_effort': os.environ['CLAUDE_CODE_EFFORT_LEVEL']}), flush=True)
"#]);
        let file = super::settings::configure_command(&mut command, None, Some("max")).unwrap();
        let path = file.as_ref().unwrap().path().to_path_buf();
        let mut turn = super::spawn_turn(command, file).unwrap();
        let event = tokio::time::timeout(std::time::Duration::from_secs(5), turn.next_event())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(event, json!({"effort":"max", "process_effort":"max"}));
        assert!(tokio::time::timeout(std::time::Duration::from_secs(5), turn.next_event())
            .await
            .unwrap()
            .unwrap()
            .is_none());
        turn.interrupt().await.unwrap();
        assert!(path.exists());
        drop(turn);
        assert!(!path.exists());
    }

    #[tokio::test]
    async fn failed_spawn_removes_settings_snapshot() {
        let directory = tempfile::tempdir().unwrap();
        let mut command = tokio::process::Command::new(directory.path().join("missing-cli"));
        let file = super::settings::configure_command(&mut command, None, Some("max")).unwrap();
        let path = file.as_ref().unwrap().path().to_path_buf();
        let error = super::spawn_turn(command, file).err().expect("missing CLI must fail");
        assert!(error.to_string().contains("启动 Claude CLI 失败，请确认 Claude Code 已安装并完成登录"));
        assert!(!path.exists());
    }

    #[test]
    fn persistent_input_has_no_positional_prompt() {
        crate::config::init_test_config();
        let args = claude_args(&crate::config::get(), Some("session-1"), "default", None);
        let system = args.iter().position(|arg| arg == "--append-system-prompt").unwrap();
        assert!(args[system + 1].contains("persistent streaming input"));
        assert!(args.windows(2).any(|pair| pair == ["--input-format", "stream-json"]));
        assert!(args.iter().any(|arg| arg == "--replay-user-messages"));
    }

    #[test]
    fn conversation_models_override_global_models_on_new_and_resumed_turns() {
        let cfg: crate::config::Config = serde_json::from_value(json!({
            "jwt_secret":"test", "auth_password":"test", "claude_model":"global-model",
            "claude_models":" sonnet,opus,custom-model,opus "
        })).unwrap();
        for session in [None,Some("same-session")] {
            for expected in ["opus","global-model"] {
                let args = claude_args(&cfg,session,expected,None);
                let model = args.iter().position(|arg| arg == "--model").unwrap();
                assert_eq!(args[model+1],expected);
                assert_eq!(args.iter().any(|arg| arg == "--resume"),session.is_some());
                assert!(!args.iter().any(|arg| arg == "question"));
            }
        }
        assert_eq!(super::cli_defaults(&cfg).unwrap().models,["global-model","sonnet","opus","custom-model"]);
    }

    #[test]
    fn claude_effort_is_explicit_on_new_and_resumed_turns_and_default_omits_flag() {
        crate::config::init_test_config();
        for session in [None, Some("same-session")] {
            let args = claude_args(&crate::config::get(), session, "opus", Some("max"));
            let effort = args.iter().position(|arg| arg == "--effort").unwrap();
            assert_eq!(args[effort + 1], "max");
            let defaults = claude_args(&crate::config::get(), session, "default", None);
            assert!(!defaults.iter().any(|arg| arg == "--effort"));
        }
    }
}
