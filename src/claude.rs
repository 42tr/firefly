use std::path::Path;

use anyhow::Context;
use serde_json::Value;
use tokio::process::Command;

use crate::config;

pub use firefly_claude_transport::{session_id, session_title, text_delta};

mod settings;
pub use settings::{cli_defaults, effective_model, normalize_provider, normalize_reasoning_level, providers, REASONING_LEVELS};

// The settings snapshot must outlive the child, including its tool subprocesses.
pub struct ClaudeTurn {
    transport: firefly_claude_transport::ClaudeTurn,
    _settings_file: Option<tempfile::NamedTempFile>,
}

impl ClaudeTurn {
    pub async fn next_event(&mut self) -> anyhow::Result<Option<Value>> {
        self.transport.next_event().await
    }

    pub async fn interrupt(&mut self) -> anyhow::Result<()> {
        self.transport.interrupt().await
    }
}

/// `--print` 单次执行：turn 结束即退出，后台任务被终止、完成通知不会送达，需在提示词中明确告知。
static CLAUDE_APPEND_SYSTEM_PROMPT: &str = include_str!("../prompts/claude.md");

/// 构建 claude CLI 参数。
fn claude_args(cfg: &config::Config, session_id: Option<&str>, model: &str, reasoning_level: Option<&str>, prompt: &str) -> Vec<String> {
    let mut args = vec![
        "--print".to_string(),
        "--verbose".to_string(),
        "--output-format".to_string(),
        "stream-json".to_string(),
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
    // `--allowedTools` accepts multiple values, so terminate option parsing
    // explicitly before passing the positional prompt.
    args.push("--".to_string());
    args.push(prompt.to_string());
    args
}

pub async fn start_turn(cwd: &Path, session_id: Option<&str>, provider: Option<&str>, model: Option<&str>, reasoning_level: Option<&str>, prompt: &str) -> anyhow::Result<ClaudeTurn> {
    let cfg = config::get();
    let profile = settings::resolve_provider(provider)?;
    let model = effective_model(&cfg, profile.as_ref(), model)?;
    let reasoning_level = normalize_reasoning_level(reasoning_level.map(str::to_owned))?;
    let mut command = Command::new("claude");
    let settings_file = settings::configure_command(&mut command, profile.as_ref(), reasoning_level.as_deref())?;
    command
        .current_dir(cwd)
        .args(claude_args(&cfg, session_id, &model, reasoning_level.as_deref(), prompt));
    spawn_turn(command, settings_file)
}

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
    fn one_shot_prompt_precedes_positional_prompt() {
        crate::config::init_test_config();
        let args = claude_args(&crate::config::get(), Some("session-1"), "default", None, "--not-a-flag");
        let system = args.iter().position(|arg| arg == "--append-system-prompt").unwrap();
        assert!(args[system + 1].contains("no task-completion notification"));
        assert_eq!(&args[args.len() - 2..], ["--", "--not-a-flag"]);
    }

    #[test]
    fn conversation_models_override_global_models_on_new_and_resumed_turns() {
        let cfg: crate::config::Config = serde_json::from_value(json!({
            "jwt_secret":"test", "auth_password":"test", "claude_model":"global-model",
            "claude_models":" sonnet,opus,custom-model,opus "
        })).unwrap();
        for session in [None,Some("same-session")] {
            for expected in ["opus","global-model"] {
                let args = claude_args(&cfg,session,expected,None,"question");
                let model = args.iter().position(|arg| arg == "--model").unwrap();
                assert_eq!(args[model+1],expected);
                assert_eq!(args.iter().any(|arg| arg == "--resume"),session.is_some());
                assert_eq!(args.last().unwrap(),"question");
            }
        }
        assert_eq!(super::cli_defaults(&cfg).unwrap().models,["global-model","sonnet","opus","custom-model"]);
    }

    #[test]
    fn claude_effort_is_explicit_on_new_and_resumed_turns_and_default_omits_flag() {
        crate::config::init_test_config();
        for session in [None, Some("same-session")] {
            let args = claude_args(&crate::config::get(), session, "opus", Some("max"), "question");
            let effort = args.iter().position(|arg| arg == "--effort").unwrap();
            assert_eq!(args[effort + 1], "max");
            let defaults = claude_args(&crate::config::get(), session, "default", None, "question");
            assert!(!defaults.iter().any(|arg| arg == "--effort"));
        }
    }
}
