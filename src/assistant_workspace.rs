//! Independent, persistent working directories for parallel conversations.
use std::{process::Stdio, sync::Arc, time::Duration};

use anyhow::{anyhow, bail, Context};
use serde_json::{json, Value};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, BufReader},
    process::Command,
};

use crate::{
    assistant_job::GenerationJob,
    assistant_store::{self, ConversationRecord},
    codex,
};

pub fn destination(id: uuid::Uuid) -> String {
    format!(".assistant-parallel/{id}/work")
}


async fn request(
    record: &ConversationRecord, attempt: uuid::Uuid, op: &str,
) -> anyhow::Result<Value> {
    let source = record.source_cwd.as_deref().context("并行会话缺少来源目录")?;
    let root = codex::workspace_root().await?.to_string_lossy().into_owned();
    Ok(json!({
        "op": op, "root": root, "source": source,
        "workspace_id": record.workspace_id.context("并行会话缺少副本标识")?,
        "conversation_id": record.id,
        "attempt": attempt, "ready": record.workspace_state == "ready",
    }))
}

async fn spawn(request: &Value) -> anyhow::Result<tokio::process::Child> {
    let payload = serde_json::to_string(request)?;
    let mut command = Command::new(std::env::current_exe()?);
    command.arg("--workspace-worker").arg(payload);
    let child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .context("无法启动工作目录准备程序")?;
    Ok(child)
}

async fn cancel(request: &Value) {
    let mut request = request.clone();
    request["op"] = json!("cancel");
    let result = tokio::time::timeout(Duration::from_secs(15), async {
        let child = spawn(&request).await?;
        child.wait_with_output().await.map_err(anyhow::Error::from)
    })
    .await;
    if !matches!(result, Ok(Ok(output)) if output.status.success()) {
        tracing::warn!("无法通知工作目录准备程序取消；程序可能仍在运行，文件锁阻止重叠准备");
    }
}

async fn run(request: &Value, job: &Arc<GenerationJob>) -> anyhow::Result<Value> {
    let mut child = spawn(request).await?;
    let stdout = child.stdout.take().context("准备程序缺少 stdout")?;
    let mut stderr = child.stderr.take().context("准备程序缺少 stderr")?;
    let error_reader = tokio::spawn(async move {
        let mut output = Vec::new();
        let mut buffer = [0; 1024];
        while let Ok(count) = stderr.read(&mut buffer).await {
            if count == 0 {
                break;
            }
            if output.len() < 4096 {
                output.extend_from_slice(&buffer[..count]);
            }
        }
        String::from_utf8_lossy(&output).into_owned()
    });
    let token = job.cancellation_token();
    let read = async {
        let mut lines = BufReader::new(stdout).lines();
        let mut metadata = None;
        let mut error = None;
        while let Some(line) = lines.next_line().await? {
            let event: Value = serde_json::from_str(&line).context("工作目录准备程序返回无效数据")?;
            match event["stage"].as_str() {
                Some("ready") => metadata = Some(event["metadata"].clone()),
                Some("error") => error = event["message"].as_str().map(str::to_owned),
                _ => {
                    job.emit(
                        "workspace_progress",
                        json!({
                            "conversation_id": job.conversation_id, "workspace_state": "preparing",
                            "stage": event["stage"], "message": event["message"],
                        }),
                    );
                }
            }
        }
        let status = child.wait().await?;
        if !status.success() {
            bail!(
                "{}",
                error.unwrap_or_else(|| "工作目录准备程序退出，请检查 git 和磁盘空间".to_owned())
            );
        }
        metadata.context("工作目录准备程序没有返回完成标记")
    };
    let result = tokio::select! {
        result = read => result,
        _ = token.cancelled() => {
            cancel(request).await;
            let _ = tokio::time::timeout(Duration::from_secs(10), child.wait()).await;
            Err(anyhow!("工作目录准备已取消"))
        }
    };
    // On cancellation/protocol failure terminate the transport.
    if result.is_err() {
        let _ = child.kill().await;
    }
    if let Ok(Ok(stderr)) = tokio::time::timeout(Duration::from_secs(2), error_reader).await {
        if !stderr.trim().is_empty() {
            tracing::warn!(%stderr, "工作目录准备程序 stderr");
        }
    }
    result
}

pub async fn ensure_ready(record: &ConversationRecord, job: &Arc<GenerationJob>) -> anyhow::Result<()> {
    if record.workspace_mode != "parallel" {
        return Ok(());
    }
    if !matches!(record.workspace_state.as_str(), "pending" | "preparing" | "ready") {
        bail!("工作目录尚未就绪，请先重试目录准备");
    }
    let was_ready = record.workspace_state == "ready";
    if !was_ready {
        assistant_store::set_workspace_state(
            record.id,
            &record.user_id,
            "preparing",
            None,
            Some(&json!({"job_id": job.id})),
        )
        .await?;
        job.emit(
            "workspace_progress",
            json!({"conversation_id": record.id, "workspace_state": "preparing", "message": "等待准备独立工作目录"}),
        );
    }
    let result = async {
        let request =
            request(record, uuid::Uuid::new_v4(), if was_ready { "check" } else { "prepare" }).await?;
        run(&request, job).await
    }
    .await;
    match result {
        Ok(metadata) => {
            assistant_store::set_workspace_state(record.id, &record.user_id, "ready", None, Some(&metadata)).await?;
            job.emit("workspace_progress", json!({"conversation_id": record.id, "workspace_state": "ready", "cwd": record.codex_cwd, "message": "工作目录准备完成"}));
            Ok(())
        }
        Err(error) => {
            // A cancelled verification must never invalidate a previously ready workspace.
            let state = if was_ready && job.cancellation_token().is_cancelled() {
                "ready"
            } else if was_ready {
                "missing"
            } else if job.cancellation_token().is_cancelled() {
                "cancelled"
            } else {
                "failed"
            };
            let message = error.to_string();
            let mut metadata = record.workspace_metadata.clone().unwrap_or_else(|| json!({}));
            metadata["job_id"] = json!(job.id);
            assistant_store::set_workspace_state(record.id, &record.user_id, state, Some(&message), Some(&metadata))
                .await?;
            job.emit(
                "workspace_progress",
                json!({"conversation_id": record.id, "workspace_state": state, "workspace_error": message}),
            );
            Err(error)
        }
    }
}
