use super::{Manager, TaskRecord};
use anyhow::Context;
use async_trait::async_trait;
use g::{Tool, ToolBehavior, ToolError, ToolSpec};
use serde_json::{json, Value};
use std::{path::PathBuf, process::Stdio, time::Duration};
use tokio::{io::AsyncReadExt, process::Command};
use tokio_util::sync::CancellationToken;

pub struct CommandTool;
#[async_trait]
impl Tool for CommandTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec { name:"command".into(), description:"Run a noninteractive shell command in the agent workspace, including codex/claude CLI, builds and tests. Submit via run_tools, never invoke directly. Output is available in get_tasks. Default execution timeout: 2 hours; queue has no timeout. Commands run concurrently regardless of cwd. Set the same resource_key only for operations that must not overlap on a shared resource (for example writes to the same repository); cwd is not a lock. Commands must keep descendants in their process group; do not daemonize.".into(),
            input_schema:json!({"type":"object","properties":{
                "command":{"type":"string","minLength":1,"maxLength":64000},
                "cwd":{"type":"string","description":"Workspace-relative directory, or an absolute directory inside WORKSPACE_ROOT; defaults to workspace root"},
                "resource_key":{"type":"string","minLength":1,"maxLength":255,"description":"Optional explicit mutex name shared across conversations, e.g. repo:firefly or deploy:production. Only identical keys serialize while tasks execute. Omit for independent commands; do not use cwd or the workspace root as a blanket lock."},
                "execution_timeout_secs":{"type":"integer","minimum":1}
            },"required":["command"],"additionalProperties":false}),
            behavior:ToolBehavior { read_only:false, idempotent:false, parallel_safe:true } }
    }
    fn background_allowed(&self) -> bool {
        true
    }
    async fn call(&self, _: g::ToolContext, _: Value) -> Result<Value, ToolError> {
        Err(ToolError::new(
            "Use run_tools with tool=command so execution is tracked and cancellable",
        ))
    }
}

pub async fn resolve_cwd(args: &Value) -> anyhow::Result<PathBuf> {
    let root = PathBuf::from(&crate::config::get().workspace_root);
    tokio::fs::create_dir_all(&root).await?;
    let root = tokio::fs::canonicalize(root).await?;
    let path = PathBuf::from(args["cwd"].as_str().unwrap_or(""));
    let cwd = tokio::fs::canonicalize(if path.is_absolute() { path } else { root.join(path) })
        .await
        .context("工作目录不存在")?;
    anyhow::ensure!(
        cwd.starts_with(&root) && cwd.is_dir(),
        "命令工作目录必须位于 WORKSPACE_ROOT 内"
    );
    Ok(cwd)
}

/// A shell's cwd says nothing about which resources its command will access.
/// Mutual exclusion is opt-in and independent of execution paths.
pub(super) fn resource_key(args: &Value) -> anyhow::Result<Option<String>> {
    let Some(value) = args.get("resource_key") else { return Ok(None); };
    let key = value.as_str().context("resource_key 必须是字符串")?;
    anyhow::ensure!(!key.trim().is_empty() && key.chars().count() <= 255, "resource_key 必须为 1–255 个字符，且不能全为空白");
    anyhow::ensure!(key == key.trim(), "resource_key 不能包含首尾空白");
    Ok(Some(key.to_owned()))
}

pub(super) async fn execute(
    manager: &Manager,
    task: &TaskRecord,
    token: CancellationToken,
) -> (&'static str, Option<Value>, Option<String>) {
    match run(manager, task, token).await {
        Ok((status, result)) => (status, Some(result), None),
        Err(error) => ("failed", None, Some(format!("{error:#}"))),
    }
}

async fn run(manager: &Manager, task: &TaskRecord, token: CancellationToken) -> anyhow::Result<(&'static str, Value)> {
    let command = task.arguments["command"].as_str().context("缺少 command")?;
    let cwd = PathBuf::from(task.arguments["cwd"].as_str().context("缺少 cwd")?);
    // The canonical execution path is fixed at submission, independently of
    // the optional resource mutex.
    anyhow::ensure!(tokio::fs::canonicalize(&cwd).await? == cwd, "任务工作目录已发生变化");
    if token.is_cancelled() {
        return Ok(("cancelled", json!({"message":"启动前取消"})));
    }
    let mut child = Command::new("sh")
        .arg("-c")
        .arg(command)
        .current_dir(&cwd)
        .env("CI", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .kill_on_drop(true)
        .spawn()
        .context("启动命令失败")?;
    let pid = child.id().context("缺少命令进程 ID")?;
    let group = crate::process_control::ProcessGroup::new(pid);
    let mut stdout = child.stdout.take().unwrap();
    let mut stderr = child.stderr.take().unwrap();
    let mut out_buffer = [0_u8; 4096];
    let mut err_buffer = [0_u8; 4096];
    let mut out_pending = Vec::new();
    let mut err_pending = Vec::new();
    let mut out_open = true;
    let mut err_open = true;
    let mut saved = 0usize;
    let cap = super::setting("ASSISTANT_TASK_OUTPUT_BYTES", 16 * 1024 * 1024);
    let remaining = task
        .deadline_at
        .unwrap_or(super::now())
        .saturating_sub(super::now())
        .max(0) as u64;
    let deadline = tokio::time::Instant::now() + Duration::from_millis(remaining);
    let mut exit = None;
    let status = loop {
        if exit.is_some() && !out_open && !err_open {
            break "finished";
        }
        tokio::select! {
            biased;
            _ = token.cancelled() => break "cancelled",
            _ = tokio::time::sleep_until(deadline) => break "timed_out",
            result = child.wait(), if exit.is_none() => { exit = Some(result?); },
            read = stdout.read(&mut out_buffer), if out_open => {
                let size = read?; out_open = size != 0;
                let text = decode(&mut out_pending, &out_buffer[..size], size == 0);
                save_output(manager, &task.id, "stdout", text, &mut saved, cap).await?;
            },
            read = stderr.read(&mut err_buffer), if err_open => {
                let size = read?; err_open = size != 0;
                let text = decode(&mut err_pending, &err_buffer[..size], size == 0);
                save_output(manager, &task.id, "stderr", text, &mut saved, cap).await?;
            },
        }
    };
    if status != "finished" {
        // Signal only this task's dedicated group, never the shared Codex service.
        unsafe {
            libc::killpg(pid as i32, libc::SIGTERM);
        }
        let grace = Duration::from_secs(super::setting("ASSISTANT_COMMAND_CANCEL_GRACE_SECS", 3) as u64);
        // Descendants may ignore TERM even when their shell has exited.
        tokio::time::sleep(grace).await;
        group.kill();
        let waited = tokio::time::timeout(Duration::from_secs(5), child.wait()).await;
        if !matches!(waited, Ok(Ok(_))) {
            return Ok(("unknown", json!({"reason":status,"message":"停止尚未确认"})));
        }
        return Ok((
            status,
            json!({"reason":status,"message":"命令进程组已停止；已产生的修改不会撤销"}),
        ));
    }
    group.kill();
    let exit = exit.context("缺少退出状态")?;
    Ok((
        if exit.success() { "succeeded" } else { "failed" },
        json!({"exit_code":exit.code(),"output_bytes_saved":saved.min(cap),"output_truncated":saved>cap}),
    ))
}

fn decode(pending: &mut Vec<u8>, bytes: &[u8], eof: bool) -> String {
    pending.extend_from_slice(bytes);
    if eof {
        let text = String::from_utf8_lossy(pending).into_owned();
        pending.clear();
        return text;
    }
    let count = match std::str::from_utf8(pending) {
        Ok(_) => pending.len(),
        Err(e) if e.error_len().is_none() => e.valid_up_to(),
        Err(_) => pending.len(),
    };
    let text = String::from_utf8_lossy(&pending[..count]).into_owned();
    pending.drain(..count);
    text
}

async fn save_output(
    manager: &Manager,
    id: &str,
    stream: &str,
    mut text: String,
    saved: &mut usize,
    cap: usize,
) -> anyhow::Result<()> {
    if text.is_empty() || *saved > cap {
        return Ok(());
    }
    let truncated = text.len() > cap.saturating_sub(*saved);
    crate::utils::truncate_to_char_boundary(&mut text, cap.saturating_sub(*saved));
    *saved += text.len();
    if !text.is_empty() {
        manager
            .event(id, "output", json!({"stream":stream,"text":text}))
            .await?;
    }
    if truncated {
        *saved = cap + 1;
        manager
            .event(
                id,
                "output",
                json!({"stream":"notice","text":"输出保存达到上限，后续输出继续读取但不保存。"}),
            )
            .await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn resource_mutexes_are_explicit_and_validated() {
        assert_eq!(resource_key(&json!({"cwd":"/workspace"})).unwrap(), None);
        assert_eq!(resource_key(&json!({"resource_key":"repo:firefly"})).unwrap(), Some("repo:firefly".into()));
        for key in [json!(null), json!(42), json!(""), json!(" "), json!(" repo:firefly"), json!("repo:firefly "), json!("x".repeat(256))] {
            assert!(resource_key(&json!({"resource_key":key})).is_err());
        }
        assert!(resource_key(&json!({"resource_key":"中".repeat(255)})).is_ok());
        let spec = CommandTool.spec();
        assert!(jsonschema::is_valid(&spec.input_schema, &json!({"command":"pwd","resource_key":"repo:firefly"})));
    }

    #[test]
    fn utf8_survives_chunk_boundaries() {
        let mut bytes = Vec::new();
        let source = "中文输出".as_bytes();
        let mut result = String::new();
        for byte in source {
            result.push_str(&decode(&mut bytes, &[*byte], false));
        }
        result.push_str(&decode(&mut bytes, &[], true));
        assert_eq!(result, "中文输出");
    }
}
