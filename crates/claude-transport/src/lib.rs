//! Claude CLI subprocess ownership and newline-delimited JSON events.
//!
//! The caller supplies the executable, arguments, working directory, and
//! environment through a [`tokio::process::Command`]. Application configuration,
//! provider selection, prompts, and settings-file lifetimes remain with the caller.

use std::{process::Stdio, time::Duration};

use anyhow::{anyhow, Context};
use serde_json::Value;
use tokio::{
    io::{AsyncBufReadExt, BufReader, Lines},
    process::{Child, ChildStdout, Command},
};
use tracing::warn;

pub struct ClaudeTurn {
    child: Child,
    stdout: Lines<BufReader<ChildStdout>>,
    process_group: g::process_control::ProcessGroup,
    stopped: bool,
}

impl ClaudeTurn {
    /// Start a configured command inside a Tokio runtime, in its own process
    /// group. Stdin is closed;
    /// stdout carries JSON events and stderr is drained into tracing warnings.
    /// Interrupting or dropping the turn also stops shell/tool descendants.
    pub fn spawn(mut command: Command) -> anyhow::Result<Self> {
        let mut child = command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0)
            .kill_on_drop(true)
            .spawn()
            .context("启动 Claude CLI 失败")?;
        let process_group =
            g::process_control::ProcessGroup::new(child.id().context("Claude 缺少进程编号")?);
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow!("Claude CLI 缺少 stdout"))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| anyhow!("Claude CLI 缺少 stderr"))?;
        tokio::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            loop {
                match lines.next_line().await {
                    Ok(Some(line)) => {
                        if !line.trim().is_empty() {
                            warn!("claude: {}", line);
                        }
                    }
                    Ok(None) => break,
                    Err(error) => {
                        warn!("claude: 读取 stderr 失败: {error}");
                        break;
                    }
                }
            }
        });

        Ok(Self {
            child,
            stdout: BufReader::new(stdout).lines(),
            process_group,
            stopped: false,
        })
    }

    /// Read the next nonempty JSON line. EOF succeeds only when the CLI exits
    /// successfully. This method can be cancelled without losing a partial line.
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

        let status = self
            .child
            .wait()
            .await
            .context("等待 Claude CLI 退出失败")?;
        if status.success() {
            // The child is reaped and exited cleanly: stop tracking its group so
            // a later Drop cannot signal a recycled process-group ID.
            self.process_group.disarm();
            self.stopped = true;
            Ok(None)
        } else {
            Err(anyhow!("Claude CLI 异常退出: {}", status))
        }
    }

    pub async fn interrupt(&mut self) -> anyhow::Result<()> {
        if self.stopped {
            return Ok(());
        }
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

pub fn session_id(event: &Value) -> Option<&str> {
    (event["type"].as_str() == Some("system") && event["subtype"].as_str() == Some("init"))
        .then(|| event["session_id"].as_str())
        .flatten()
}

/// Claude Code's native session title update emitted in stream-json mode.
pub fn session_title(event: &Value) -> Option<&str> {
    (event["type"].as_str() == Some("system")
        && event["subtype"].as_str() == Some("session_title_changed"))
        .then(|| event["title"].as_str())
        .flatten()
}

pub fn text_delta(event: &Value) -> Option<&str> {
    (event["type"].as_str() == Some("stream_event")
        && event["event"]["type"].as_str() == Some("content_block_delta")
        && event["event"]["delta"]["type"].as_str() == Some("text_delta"))
    .then(|| event["event"]["delta"]["text"].as_str())
    .flatten()
}
