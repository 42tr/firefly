//! Claude CLI subprocess ownership and newline-delimited JSON events.
//!
//! The caller supplies the executable, arguments, working directory, and
//! environment through a [`tokio::process::Command`]. Application configuration,
//! provider selection, prompts, and settings-file lifetimes remain with the caller.

use std::{process::Stdio, time::Duration};

use anyhow::{anyhow, Context};
use serde_json::Value;
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::{Child, Command},
    sync::mpsc,
};
use tracing::warn;

mod activity;
pub use activity::Activity;

pub struct ClaudeTurn {
    child: Child,
    events: mpsc::Receiver<anyhow::Result<Value>>,
    process_group: firefly_process_control::ProcessGroup,
    stopped: bool,
}

impl ClaudeTurn {
    /// Start a configured command inside a Tokio runtime, in its own process
    /// group. Stdin is closed;
    /// stdout carries JSON events and stderr is drained into tracing warnings.
    /// Interrupting or dropping the turn also stops shell/tool descendants.
    pub fn spawn(mut command: Command) -> anyhow::Result<Self> {
        Self::spawn_with_input(&mut command, false)
    }

    /// Keep stdin open so task notifications can trigger later assistant turns.
    pub fn spawn_streaming(mut command: Command) -> anyhow::Result<Self> {
        Self::spawn_with_input(&mut command, true)
    }

    fn spawn_with_input(command: &mut Command, streaming: bool) -> anyhow::Result<Self> {
        let mut child = command
            .stdin(if streaming { Stdio::piped() } else { Stdio::null() })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0)
            .kill_on_drop(true)
            .spawn()
            .context("启动 Claude CLI 失败")?;
        let process_group =
            firefly_process_control::ProcessGroup::new(child.id().context("Claude 缺少进程编号")?);
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

        let (sender, events) = mpsc::channel(256);
        tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            loop {
                let event = match lines.next_line().await {
                    Ok(Some(line)) if line.trim().is_empty() => continue,
                    Ok(Some(line)) => serde_json::from_str(&line)
                        .context("无法解析 Claude CLI JSON 消息"),
                    Ok(None) => break,
                    Err(error) => Err(anyhow::Error::new(error).context("Claude 传输中断或消息不是有效 UTF-8")),
                };
                let failed = event.is_err();
                if sender.send(event).await.is_err() || failed { break; }
            }
        });

        Ok(Self {
            child,
            events,
            process_group,
            stopped: false,
        })
    }

    /// Read the next nonempty JSON line. EOF succeeds only when the CLI exits
    /// successfully. This method can be cancelled without losing a partial line.
    pub async fn next_event(&mut self) -> anyhow::Result<Option<Value>> {
        if let Some(event) = self.events.recv().await { return event.map(Some); }

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

    pub fn is_running(&mut self) -> anyhow::Result<bool> {
        if self.stopped { return Ok(false); }
        if self.child.try_wait()?.is_some() {
            self.process_group.disarm();
            self.stopped = true;
            return Ok(false);
        }
        Ok(true)
    }

    pub async fn send(&mut self, message: &Value) -> anyhow::Result<()> {
        let stdin = self.child.stdin.as_mut().context("Claude 流式输入已关闭")?;
        let mut line = serde_json::to_vec(message)?;
        line.push(b'\n');
        stdin.write_all(&line).await.context("写入 Claude 流式输入失败")?;
        stdin.flush().await.context("刷新 Claude 流式输入失败")
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
