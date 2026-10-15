use std::{path::Path, process::Stdio, time::Duration};

use anyhow::{anyhow, Context};
use serde_json::Value;
use tokio::{
    io::{AsyncBufReadExt, BufReader, Lines},
    process::{Child, ChildStdout, Command},
};
use tracing::warn;

use crate::config;

mod settings;
pub use settings::{default_provider, effective_model, normalize_provider, normalize_reasoning_level, providers, resolve_provider, REASONING_LEVELS};

pub struct ClaudeTurn {
    child: Child,
    stdout: Lines<BufReader<ChildStdout>>,
    process_group: crate::process_control::ProcessGroup,
    stopped: bool,
    _settings_file: Option<tempfile::NamedTempFile>,
}

impl ClaudeTurn {
    pub async fn next_event(&mut self) -> anyhow::Result<Option<Value>> {
        while let Some(line) = self.stdout.next_line().await.map_err(|error| {
            if error.kind() == std::io::ErrorKind::InvalidData {
                anyhow::Error::new(error).context("Claude CLI 消息不是有效 UTF-8")
            } else {
                anyhow::Error::new(error).context("Claude 传输中断")
            }
        })? {
            if line.trim().is_empty() {
                continue;
            }
            return serde_json::from_str(&line)
                .with_context(|| format!("无法解析 Claude CLI 消息: {}", line))
                .map(Some);
        }

        let status = self.child.wait().await.context("等待 Claude CLI 退出失败")?;
        if status.success() {
            Ok(None)
        } else {
            Err(anyhow!("Claude CLI 异常退出: {}", status))
        }
    }

    pub async fn interrupt(&mut self) -> anyhow::Result<()> {
        if self.stopped { return Ok(()); }
        drop(self.child.stdin.take());
        self.process_group.kill();
        tokio::time::timeout(Duration::from_secs(5), self.child.wait())
            .await
            .context("等待 Claude CLI 退出超时")?
            .context("等待 Claude CLI 退出失败")?;
        self.stopped = true;
        Ok(())
    }
}

/// `--print` 单次执行：turn 结束即退出，后台任务被终止、完成通知不会送达，需在提示词中明确告知。
static CLAUDE_APPEND_SYSTEM_PROMPT: &str = include_str!("../prompts/claude.md");

/// 构建 claude CLI 参数。
fn claude_args(cfg: &config::Config, session_id: Option<&str>, model: Option<&str>, reasoning_level: Option<&str>, prompt: &str) -> Vec<String> {
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
    args.push(model.map(str::trim).filter(|value| !value.is_empty())
        .map(str::to_owned).unwrap_or_else(|| default_model(cfg)));
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
    let profile = settings::resolve_provider(&cfg, provider)?;
    let model = effective_model(&cfg, profile.as_ref(), model);
    let reasoning_level = normalize_reasoning_level(reasoning_level.map(str::to_owned))?
        .or(normalize_reasoning_level(Some(cfg.claude_reasoning_level.clone()))?);
    let mut command = Command::new("claude");
    let settings_file = settings::configure_command(&mut command, profile.as_ref(), reasoning_level.as_deref())?;
    command
        .current_dir(cwd)
        .args(claude_args(&cfg, session_id, Some(&model), reasoning_level.as_deref(), prompt))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    let mut turn = spawn_turn(command, "启动 Claude CLI 失败，请确认 Claude Code 已安装并完成登录").await?;
    turn._settings_file = settings_file;
    Ok(turn)
}

pub fn default_model(cfg: &config::Config) -> String {
    if !cfg.claude_model.trim().is_empty() { return cfg.claude_model.trim().to_owned(); }
    std::env::var("ANTHROPIC_MODEL").ok().map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty()).unwrap_or_else(|| "default".to_owned())
}

pub fn model_choices(cfg: &config::Config) -> Vec<String> {
    let configured = cfg.claude_models.trim();
    let values = if configured.is_empty() { "sonnet,opus,haiku" } else { configured };
    let mut models = Vec::new();
    for value in values.split(',').map(str::trim).filter(|value| !value.is_empty()) {
        if !models.iter().any(|model| model == value) { models.push(value.to_owned()); }
    }
    let default = default_model(cfg);
    if default != "default" && !models.contains(&default) { models.insert(0,default); }
    models
}

pub fn normalize_model(model: Option<String>) -> anyhow::Result<Option<String>> {
    let model = model.map(|value| value.trim().to_owned()).filter(|value| !value.is_empty());
    if let Some(model) = &model {
        anyhow::ensure!(model.len() <= 256 && !model.starts_with('-')
            && !model.chars().any(|value| value.is_whitespace() || value.is_control()), "Claude 模型名称格式无效");
    }
    Ok(model)
}

async fn spawn_turn(mut command: Command, spawn_error_context: &'static str) -> anyhow::Result<ClaudeTurn> {
    let mut child = command.process_group(0).kill_on_drop(true).spawn().context(spawn_error_context)?;
    let process_group = crate::process_control::ProcessGroup::new(child.id().context("Claude 缺少进程编号")?);
    let stdout = child.stdout.take().ok_or_else(|| anyhow!("Claude CLI 缺少 stdout"))?;
    let stderr = child.stderr.take().ok_or_else(|| anyhow!("Claude CLI 缺少 stderr"))?;
    tokio::spawn(async move {
        let mut lines = BufReader::new(stderr).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            if !line.trim().is_empty() {
                warn!("claude: {}", line);
            }
        }
    });

    Ok(ClaudeTurn { child, stdout: BufReader::new(stdout).lines(), process_group, stopped: false, _settings_file: None })
}

pub fn session_id(event: &Value) -> Option<&str> {
    (event["type"].as_str() == Some("system") && event["subtype"].as_str() == Some("init"))
        .then(|| event["session_id"].as_str())
        .flatten()
}

pub fn text_delta(event: &Value) -> Option<&str> {
    (event["type"].as_str() == Some("stream_event")
        && event["event"]["type"].as_str() == Some("content_block_delta")
        && event["event"]["delta"]["type"].as_str() == Some("text_delta"))
    .then(|| event["event"]["delta"]["text"].as_str())
    .flatten()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{claude_args, session_id, text_delta};

    #[tokio::test]
    async fn interrupt_stops_local_tool_descendants() {
        let mut command = tokio::process::Command::new("python3");
        command.args(["-u", "-c", "import json,subprocess,time\np=subprocess.Popen(['sleep','60'])\nprint(json.dumps({'pid':p.pid}),flush=True)\ntime.sleep(60)"])
            .stdin(std::process::Stdio::null()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped());
        let mut turn = super::spawn_turn(command, "mock CLI").await.unwrap();
        let event = tokio::time::timeout(std::time::Duration::from_secs(2), turn.next_event()).await.unwrap().unwrap().unwrap();
        let pid = event["pid"].as_u64().unwrap();
        turn.interrupt().await.unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                let state = std::fs::read_to_string(format!("/proc/{pid}/stat"));
                if state.as_ref().is_err() || state.unwrap().split(") ").nth(1).unwrap().starts_with('Z') { break; }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        }).await.unwrap();
        turn.interrupt().await.unwrap();
    }

    #[tokio::test]
    async fn invalid_utf8_is_a_protocol_error() {
        let mut command = tokio::process::Command::new("python3");
        command.args(["-u", "-c", "import sys;sys.stdout.buffer.write(b'\\xff\\n');sys.stdout.flush()"])
            .stdin(std::process::Stdio::null()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped());
        let mut turn = super::spawn_turn(command, "mock CLI").await.unwrap();
        let error = turn.next_event().await.unwrap_err();
        assert!(format!("{error:#}").contains("UTF-8"));
        turn.interrupt().await.unwrap();
    }

    #[test]
    fn one_shot_prompt_precedes_positional_prompt() {
        crate::config::init_test_config();
        let args = claude_args(&crate::config::get(), Some("session-1"), None, None, "--not-a-flag");
        let system = args.iter().position(|arg| arg == "--append-system-prompt").unwrap();
        assert!(args[system + 1].contains("no task-completion notification"));
        assert_eq!(&args[args.len() - 2..], ["--", "--not-a-flag"]);
    }

    #[test]
    fn extracts_init_session_id() {
        let event = json!({ "type": "system", "subtype": "init", "session_id": "session-1" });
        assert_eq!(session_id(&event), Some("session-1"));
    }

    #[test]
    fn conversation_models_override_global_models_on_new_and_resumed_turns() {
        let cfg: crate::config::Config = serde_json::from_value(json!({
            "jwt_secret":"test", "auth_password":"test", "claude_model":"global-model",
            "claude_models":" sonnet,opus,custom-model,opus "
        })).unwrap();
        for session in [None,Some("same-session")] {
            for (selection,expected) in [(Some(" opus "),"opus"),(None,"global-model")] {
                let args = claude_args(&cfg,session,selection,None,"question");
                let model = args.iter().position(|arg| arg == "--model").unwrap();
                assert_eq!(args[model+1],expected);
                assert_eq!(args.iter().any(|arg| arg == "--resume"),session.is_some());
                assert_eq!(args.last().unwrap(),"question");
            }
        }
        assert_eq!(super::model_choices(&cfg),["global-model","sonnet","opus","custom-model"]);
    }

    #[test]
    fn claude_effort_is_explicit_on_new_and_resumed_turns_and_default_omits_flag() {
        crate::config::init_test_config();
        for session in [None, Some("same-session")] {
            let args = claude_args(&crate::config::get(), session, Some("opus"), Some("max"), "question");
            let effort = args.iter().position(|arg| arg == "--effort").unwrap();
            assert_eq!(args[effort + 1], "max");
            let defaults = claude_args(&crate::config::get(), session, None, None, "question");
            assert!(!defaults.iter().any(|arg| arg == "--effort"));
        }
    }

    #[test]
    fn extracts_streamed_text_delta() {
        let event = json!({
            "type": "stream_event",
            "event": { "type": "content_block_delta", "delta": { "type": "text_delta", "text": "hello" } }
        });
        assert_eq!(text_delta(&event), Some("hello"));
    }
}
