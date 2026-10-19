//! Codex turns, capacity retries, attachment inputs, and lost-event recovery.
use std::{collections::{HashMap, HashSet}, path::PathBuf, sync::Arc, time::Duration};

use anyhow::anyhow;
use serde_json::{json, Value};
use tokio::fs;
use tracing::warn;

use crate::{
    api::assistant::{
        attachments, conversations::map_attachment_record, is_codex_retrying_message,
        persistence::{persist_message, persist_message_attachments},
        AttachmentCleanup, Message, MessageAttachment, ToolCall,
    },
    assistant_job, assistant_store, codex, config,
};

pub(super) const CODEX_CAPACITY_RETRY_DELAY: Duration = Duration::from_secs(10);

pub(in crate::api::assistant) async fn run_codex_turn(
    record: &assistant_store::ConversationRecord, user_id: &str, conversation_id: i64, user_message: &Message,
    job: Arc<assistant_job::GenerationJob>,
) -> anyhow::Result<()> {
    let cwd_relative = record.codex_cwd.as_deref().ok_or_else(|| anyhow!("Codex 对话缺少工作目录"))?;
    let (cwd, _) = codex::resolve_workspace_path(Some(cwd_relative)).await?;
    let settings = codex::resolve_conversation_settings(
        &cwd, record.codex_provider.as_deref(), record.codex_model.as_deref(), record.codex_reasoning_level.as_deref(),
    ).await?;

    let thread_id = if let Some(thread_id) = record.codex_thread_id.as_deref() {
        codex::resume_thread(thread_id, &cwd, Some(&settings.provider), Some(&settings.model)).await?;
        thread_id.to_string()
    } else {
        let thread_id =
            codex::start_thread(&cwd, Some(&settings.provider), Some(&settings.model)).await?;
        assistant_store::set_codex_thread_id(conversation_id, user_id, &thread_id).await?;
        thread_id
    };

    let input = build_codex_input(user_message).await?;

    let cancellation = job.cancellation_token();
    let mut turn_input = input;
    loop {
        if cancellation.is_cancelled() {
            return Ok(());
        }

        let (result, turn_started) = match codex::start_turn(
            &thread_id,
            &cwd,
            turn_input.clone(),
            Some(&settings.reasoning_level),
        )
        .await
        {
            Ok((turn_id, events)) => {
                let result = consume_codex_turn(&thread_id, &turn_id, events, conversation_id, &job).await;
                if let Err(error) = &result {
                    // Storage/protocol errors can end consumption before the
                    // server's turn ends. Stop it before releasing the queue or
                    // retrying the prompt on this thread.
                    codex::interrupt_turn(&thread_id, &turn_id).await
                        .map_err(|stop| anyhow!("{error}; 无法确认旧 Codex 执行已停止: {stop}"))?;
                }
                (result, true)
            }
            Err(err) => (Err(err), false),
        };
        let Err(err) = result else {
            return Ok(());
        };
        if !is_codex_capacity_error(&err.to_string()) {
            // The queue records the final failure after all internal retries.
            return Err(err);
        }

        tokio::select! {
            biased;
            _ = cancellation.cancelled() => return Ok(()),
            _ = tokio::time::sleep(CODEX_CAPACITY_RETRY_DELAY) => {}
        }
        if turn_started {
            turn_input = vec![json!({
                "type": "text",
                "text": "上次请求因模型容量不足而中断，请从上次进度继续完成之前的任务，不要重复已完成的工作。"
            })];
        }
    }
}

// Storage paths are relative to the backend, not necessarily the selected workspace.
async fn build_codex_input(message: &Message) -> anyhow::Result<Vec<Value>> {
    let mut input = vec![json!({ "type": "text", "text": message.content })];
    for attachment in &message.attachments {
        let path = std::path::absolute(&attachment.storage_path)?;
        if attachment.content_type.starts_with("image/") {
            input.push(json!({ "type": "localImage", "path": path }));
        } else {
            let mut text = format!(
                "用户上传的附件：{}\n文件绝对路径：{}\n可使用文件读取工具按此路径读取完整内容。",
                attachment.file_name,
                path.display()
            );
            if is_text_attachment(attachment) {
                match codex_text_attachment_prompt(attachment, &path).await {
                    Ok(content) => text.push_str(&content),
                    Err(err) => text.push_str(&format!("\n后端读取附件预览失败：{}", err)),
                }
            }
            input.push(json!({ "type": "text", "text": text }));
        }
    }
    Ok(input)
}

const CODEX_TEXT_ATTACHMENT_LIMIT: usize = 1024 * 1024;

pub(in crate::api::assistant) fn is_text_attachment(attachment: &MessageAttachment) -> bool {
    if attachment.content_type.starts_with("text/") || attachment.content_type == "application/json" {
        return true;
    }
    let name = attachment.file_name.to_ascii_lowercase();
    let extension = name.rsplit('.').next().unwrap_or_default();
    matches!(
        extension,
        "txt"
            | "md"
            | "markdown"
            | "json"
            | "jsonl"
            | "yaml"
            | "yml"
            | "toml"
            | "ini"
            | "cfg"
            | "conf"
            | "csv"
            | "tsv"
            | "log"
            | "js"
            | "mjs"
            | "cjs"
            | "jsx"
            | "ts"
            | "tsx"
            | "vue"
            | "svelte"
            | "css"
            | "scss"
            | "less"
            | "html"
            | "htm"
            | "xml"
            | "svg"
            | "py"
            | "rs"
            | "go"
            | "java"
            | "c"
            | "h"
            | "cpp"
            | "hpp"
            | "cs"
            | "sh"
            | "bash"
            | "zsh"
            | "sql"
            | "diff"
            | "patch"
    ) || matches!(name.as_str(), "dockerfile" | "makefile" | ".gitignore" | ".env")
}

async fn codex_text_attachment_prompt(
    attachment: &MessageAttachment, path: &std::path::Path,
) -> anyhow::Result<String> {
    use anyhow::Context;
    use tokio::io::AsyncReadExt;

    // Bound the actual read, not just the stored upload size.
    let file = fs::File::open(path).await.with_context(|| format!("无法读取附件：{}", attachment.file_name))?;
    let mut bytes = Vec::new();
    file.take((CODEX_TEXT_ATTACHMENT_LIMIT + 1) as u64)
        .read_to_end(&mut bytes)
        .await
        .with_context(|| format!("无法读取附件：{}", attachment.file_name))?;
    let truncated = bytes.len() > CODEX_TEXT_ATTACHMENT_LIMIT;
    let text = String::from_utf8_lossy(&bytes[..bytes.len().min(CODEX_TEXT_ATTACHMENT_LIMIT)]);
    let suffix = if truncated { "\n[以上仅展示前 1MB，请按上述绝对路径读取完整文件]" } else { "" };
    Ok(format!("\n以下是附件文本内容：\n```text\n{}\n```{}", text, suffix))
}

pub(super) fn is_codex_capacity_error(message: &str) -> bool {
    message.to_ascii_lowercase().contains("selected model is at capacity")
}

pub(super) async fn consume_codex_turn(
    thread_id: &str, turn_id: &str, mut events: tokio::sync::broadcast::Receiver<Value>, conversation_id: i64,
    job: &Arc<assistant_job::GenerationJob>,
) -> anyhow::Result<()> {
    let cancellation = job.cancellation_token();
    let mut capacity_error = None;
    // 已收到 item/started 但尚未收到 item/completed 的工具项（id → item/started 时的负载）。
    // 广播接收方滞后（Lagged）会丢弃事件，turn 结束时需要兜底补齐 tool_result，
    // 否则前端执行过程卡片会一直停留在“正在执行”且耗时持续增长。
    let mut pending_tools: HashMap<String, Value> = HashMap::new();
    // Agent messages already persisted, so a poll-based finish only recovers missing ones.
    let mut persisted_messages: HashSet<String> = HashSet::new();
    let mut persisted_images: HashSet<String> = HashSet::new();
    // The broadcast channel may drop `turn/completed` (Lagged), leaving the job running
    // forever. Poll the turn status periodically and after a lag; once Codex reports it
    // finished, give queued events a grace period before finishing from the polled state.
    let mut status_check = tokio::time::interval_at(
        tokio::time::Instant::now() + CODEX_TURN_STATUS_INTERVAL,
        CODEX_TURN_STATUS_INTERVAL,
    );
    let mut finished_turn: Option<(tokio::time::Instant, Value)> = None;
    let result = 'turn: loop {
        let grace_deadline = finished_turn.as_ref().map(|(deadline, _)| *deadline);
        let message = tokio::select! {
            _ = cancellation.cancelled() => {
                break 'turn codex::interrupt_turn(thread_id, turn_id).await;
            }
            message = events.recv() => match message {
            Ok(message) => message,
            // The app-server keeps running when a broadcast receiver falls behind.
            // Treating this as a terminal error loses the turn while Codex may still
            // be retrying its upstream LLM request.
            Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                warn!(thread_id, turn_id, skipped, "Codex event receiver lagged; checking turn status");
                if finished_turn.is_none() {
                    finished_turn = poll_finished_codex_turn(thread_id, turn_id).await;
                }
                continue;
            }
            Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                break 'turn Err(anyhow!("Codex 事件流已断开"));
            }
            },
            _ = status_check.tick(), if finished_turn.is_none() => {
                finished_turn = poll_finished_codex_turn(thread_id, turn_id).await;
                continue;
            }
            _ = async { tokio::time::sleep_until(grace_deadline.unwrap()).await }, if grace_deadline.is_some() => {
                let (_, turn) = finished_turn.take().unwrap();
                warn!(thread_id, turn_id, "Codex turn finished without a received turn/completed event");
                recover_codex_agent_messages(thread_id, turn_id, &persisted_messages, &mut persisted_images,
                    &mut pending_tools, conversation_id, job).await;
                break 'turn codex_turn_outcome(&turn, None, capacity_error.as_deref());
            }
        };
        if codex::notification_method(&message) == Some("server/closed") {
            warn!(thread_id, turn_id, "Codex app-server closed before turn completion");
            break 'turn Err(anyhow!(
                "{}",
                codex::notification_params(&message)["message"].as_str().unwrap_or("codex app-server 连接已关闭")
            ));
        }
        if !codex::notification_matches(&message, thread_id, turn_id) {
            continue;
        }

        let method = codex::notification_method(&message).unwrap_or_default();
        let params = codex::notification_params(&message);
        // Retried turns share a thread, so a delayed event from an earlier turn
        // must not complete or mutate the current attempt.
        if params["turnId"].as_str().or_else(|| params["turn"]["id"].as_str()).is_some_and(|id| id != turn_id) {
            continue;
        }
        match method {
            "item/agentMessage/delta" => {
                if let Some(delta) = params["delta"].as_str().filter(|delta| !delta.is_empty()) {
                    job.emit("assistant_delta", json!({ "delta": delta }));
                }
            }
            "item/started" => {
                if let Some(item) = params.get("item") {
                    if let Some(tool) = codex_tool_payload(item, false) {
                        let assistant = Message::assistant(
                            String::new(),
                            Some(vec![ToolCall {
                                id: tool["id"].as_str().unwrap_or("codex-item").to_string(),
                                name: tool["name"].as_str().unwrap_or("codex_tool").to_string(),
                                arguments: tool["arguments"].clone(),
                            }]),
                        );
                        persist_message(conversation_id, &assistant, Some(json!({ "agent": "codex" })), Some(job.id))
                            .await?;
                        if let Some(id) = tool["id"].as_str() {
                            pending_tools.insert(id.to_string(), tool.clone());
                        }
                        job.emit("tool_call", tool);
                    }
                }
            }
            "item/completed" => {
                if let Some(item) = params.get("item") {
                    match item["type"].as_str() {
                        Some("agentMessage") => {
                            if let Some(id) = item["id"].as_str() {
                                persisted_messages.insert(id.to_string());
                            }
                            let content = item["text"].as_str().unwrap_or_default().trim().to_string();
                            if !content.is_empty() {
                                let assistant = Message::assistant(content, None);
                                persist_message(
                                    conversation_id,
                                    &assistant,
                                    Some(json!({ "agent": "codex" })),
                                    Some(job.id),
                                )
                                .await?;
                            }
                        }
                        Some("commandExecution") | Some("fileChange") | Some("mcpToolCall") | Some("imageView") | Some("imageGeneration") => {
                            if item["type"] == "imageGeneration" && item["id"].as_str().is_some_and(|id| persisted_images.contains(id)) {
                                continue;
                            }
                            if let Some(tool) = codex_tool_payload(item, true) {
                                if let Some(id) = tool["id"].as_str() {
                                    pending_tools.remove(id);
                                }
                                persist_codex_tool_result(thread_id, conversation_id, job, tool).await?;
                                if item["type"] == "imageGeneration" {
                                    if let Some(id) = item["id"].as_str() { persisted_images.insert(id.into()); }
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
            "turn/completed" => {
                tracing::debug!(thread_id, turn_id, "Codex turn completed");
                if let Some(items) = params["turn"]["items"].as_array() {
                    recover_codex_image_items(thread_id, items, &mut persisted_images, &mut pending_tools,
                        conversation_id, job).await;
                }
                break 'turn codex_turn_outcome(&params["turn"], params["error"]["message"].as_str(), capacity_error.as_deref());
            }
            "error" => {
                let error = params["error"]["message"].as_str().unwrap_or("Codex 执行失败");
                if params["willRetry"].as_bool() == Some(true) || is_codex_retrying_message(error) {
                    continue;
                }
                if is_codex_capacity_error(error) {
                    // Wait for the failed turn's terminal event before starting a
                    // new turn on the same thread.
                    capacity_error = Some(error.to_string());
                    continue;
                }
                break 'turn Err(anyhow!("{}", error));
            }
            _ => {}
        }
    };
    if !pending_tools.is_empty() {
        finalize_codex_pending_tools(thread_id, turn_id, pending_tools, conversation_id, job, result.is_ok()).await;
    }
    result
}

const CODEX_TURN_STATUS_INTERVAL: Duration = Duration::from_secs(120);
const CODEX_FINISHED_TURN_GRACE: Duration = Duration::from_secs(10);

pub(super) fn codex_turn_outcome(turn: &Value, event_error: Option<&str>, capacity_error: Option<&str>) -> anyhow::Result<()> {
    match turn["status"].as_str().unwrap_or_default() {
        "completed" => Ok(()),
        "interrupted" => Err(anyhow!("Codex turn 已中断")),
        _ => {
            let error = turn["error"]["message"]
                .as_str()
                .or(event_error)
                .or(capacity_error)
                .unwrap_or("Codex turn 未完成");
            Err(anyhow!("{}", error))
        }
    }
}

/// Returns the turn with a grace deadline once Codex reports it is no longer in progress.
async fn poll_finished_codex_turn(thread_id: &str, turn_id: &str) -> Option<(tokio::time::Instant, Value)> {
    match codex::read_turn(thread_id, turn_id).await {
        Ok(Some(turn)) if turn["status"].as_str().is_some_and(|status| status != "inProgress") => {
            Some((tokio::time::Instant::now() + CODEX_FINISHED_TURN_GRACE, turn))
        }
        Ok(_) => None,
        Err(err) => {
            warn!(thread_id, turn_id, "failed to read Codex turn status: {}", err);
            None
        }
    }
}

/// Persists final agent messages whose item/completed events were dropped.
async fn recover_codex_agent_messages(
    thread_id: &str, turn_id: &str, persisted: &HashSet<String>, persisted_images: &mut HashSet<String>,
    pending_tools: &mut HashMap<String, Value>, conversation_id: i64,
    job: &Arc<assistant_job::GenerationJob>,
) {
    let items = match codex::list_turn_items(thread_id, turn_id).await {
        Ok(items) => items,
        Err(err) => {
            warn!(thread_id, turn_id, "failed to list Codex turn items: {}", err);
            return;
        }
    };
    recover_codex_image_items(thread_id, &items, persisted_images, pending_tools, conversation_id, job).await;
    for item in items {
        if item["type"].as_str() != Some("agentMessage") || item["id"].as_str().is_some_and(|id| persisted.contains(id)) {
            continue;
        }
        let content = item["text"].as_str().unwrap_or_default().trim().to_string();
        if content.is_empty() {
            continue;
        }
        let assistant = Message::assistant(content, None);
        if let Err(err) =
            persist_message(conversation_id, &assistant, Some(json!({ "agent": "codex" })), Some(job.id)).await
        {
            warn!(thread_id, turn_id, "failed to persist recovered Codex message: {}", err);
        }
    }
}

async fn recover_codex_image_items(
    thread_id: &str, items: &[Value], persisted: &mut HashSet<String>, pending: &mut HashMap<String, Value>,
    conversation_id: i64, job: &Arc<assistant_job::GenerationJob>,
) {
    for item in items {
        let Some(id) = item["id"].as_str() else { continue };
        if item["type"] != "imageGeneration" || persisted.contains(id) { continue; }
        let Some(tool) = codex_tool_payload(item, true) else { continue };
        match persist_codex_tool_result(thread_id, conversation_id, job, tool).await {
            Ok(()) => { persisted.insert(id.into()); pending.remove(id); }
            Err(error) => warn!(conversation_id, "恢复 Codex 图片结果失败: {error:#}"),
        }
    }
}

/// 持久化并广播一条 codex 工具结果（item/completed 与兜底恢复共用）。
pub(super) async fn persist_codex_tool_result(
    thread_id: &str, conversation_id: i64, job: &Arc<assistant_job::GenerationJob>, mut tool: Value,
) -> anyhow::Result<()> {
    let id = tool["id"].as_str().unwrap_or("codex-item").to_string();
    let name = tool["name"].as_str().unwrap_or("codex_tool").to_string();
    let mut is_error = tool["status"].as_str().is_some_and(|status| matches!(status, "failed" | "declined" | "error"));
    let mut result = tool.get("output").cloned().unwrap_or(Value::Null);
    let mut images = Vec::new();
    if name == "image_generation" {
        is_error |= !result["failure"].is_null();
        if !is_error {
            let storage_dir = PathBuf::from(config::get().assistant_upload_dir.clone());
            match attachments::save_codex_image(&result, thread_id, &storage_dir).await {
                Ok(image) => {
                    let file_id = std::path::Path::new(&image.storage_path).file_name().unwrap().to_string_lossy();
                    result["image_url"] = json!(format!("/api/assistant/attachments/by-file/{file_id}"));
                    images.push(image);
                }
                Err(error) => {
                    is_error = true;
                    result["error"] = json!(format!("生成图片无法保存：{error}"));
                }
            }
        }
        // The attachment owns the bytes; never duplicate base64 in DB/SSE logs.
        if let Some(object) = result.as_object_mut() { object.remove("result"); }
        tool["output"] = result.clone();
    }
    let mut cleanup = AttachmentCleanup::new(images);
    let content = if result.is_null() {
        String::new()
    } else {
        serde_json::to_string(&result).unwrap_or_else(|_| result.to_string())
    };
    let mut tool_message = Message::tool(content, id.clone(), is_error);
    tool_message.attachments = cleanup.entries().iter().cloned().map(MessageAttachment::from).collect();
    let stored = persist_message(
        conversation_id,
        &tool_message,
        Some(json!({ "tool_name": name, "agent": "codex", "tool_input": tool["arguments"] })),
        Some(job.id),
    )
    .await?;
    let attachments = persist_message_attachments(stored.id, &mut tool_message).await?;
    cleanup.disarm();
    if !attachments.is_empty() {
        tool["attachments"] = json!(attachments.into_iter().map(map_attachment_record).collect::<Vec<_>>());
    }
    tool["message_id"] = json!(stored.id);
    tool["result"] = result;
    tool["is_error"] = json!(is_error);
    job.emit("tool_result", tool);
    Ok(())
}

/// turn 结束时仍没等到 item/completed 的工具项（广播滞后丢事件等）：
/// 先从 thread 历史恢复真实结果，恢复不到则按 turn 成败兜底收尾，
/// 避免工具步骤永远停留在 running。
async fn finalize_codex_pending_tools(
    thread_id: &str, turn_id: &str, pending: HashMap<String, Value>, conversation_id: i64,
    job: &Arc<assistant_job::GenerationJob>, turn_ok: bool,
) {
    warn!(thread_id, turn_id, count = pending.len(), "Codex turn 结束时仍有工具项缺少完成事件，尝试从历史恢复");
    let recovered = match codex::list_turn_items(thread_id, turn_id).await {
        Ok(items) => items,
        Err(err) => {
            warn!(thread_id, turn_id, "恢复 Codex 工具结果失败: {:#}", err);
            Vec::new()
        }
    };
    for (id, started) in pending {
        let tool = recovered
            .iter()
            .find(|item| item["id"].as_str() == Some(id.as_str()))
            .and_then(|item| codex_tool_payload(item, true))
            .unwrap_or_else(|| {
                let mut tool = started;
                tool["output"] = Value::Null;
                tool["status"] = json!(if turn_ok { "completed" } else { "failed" });
                tool
            });
        if let Err(err) = persist_codex_tool_result(thread_id, conversation_id, job, tool).await {
            warn!(conversation_id, "补齐 Codex 工具结果失败: {:#}", err);
        }
    }
}

pub(super) fn codex_tool_payload(item: &Value, completed: bool) -> Option<Value> {
    let item_type = item["type"].as_str()?;
    let (name, input, output, status) = match item_type {
        "commandExecution" => (
            "command",
            json!({ "command": item["command"], "cwd": item["cwd"] }),
            if completed {
                json!({
                    "output": item["aggregatedOutput"],
                    "exit_code": item["exitCode"],
                    "status": item["status"]
                })
            } else {
                Value::Null
            },
            item["status"].as_str().unwrap_or(if completed { "completed" } else { "running" }),
        ),
        "fileChange" => (
            "file_change",
            json!({ "changes": item["changes"] }),
            if completed { json!({ "changes": item["changes"], "status": item["status"] }) } else { Value::Null },
            item["status"].as_str().unwrap_or(if completed { "completed" } else { "running" }),
        ),
        "mcpToolCall" => (
            "mcp_tool",
            json!({ "server": item["server"], "tool": item["tool"], "arguments": item["arguments"] }),
            if completed { json!({ "result": item["result"], "error": item["error"] }) } else { Value::Null },
            item["status"].as_str().unwrap_or(if completed { "completed" } else { "running" }),
        ),
        "imageView" => (
            "image_view",
            json!({ "path": item["path"] }),
            if completed { json!({ "path": item["path"] }) } else { Value::Null },
            "completed",
        ),
        "imageGeneration" => (
            "image_generation",
            json!({ "revised_prompt": item["revisedPrompt"], "transparent_background": item["transparentBackground"] }),
            if completed { json!({ "type": "image", "saved_path": item["savedPath"], "result": item["result"],
                "status": item["status"], "failure": item["failure"] }) } else { Value::Null },
            item["status"].as_str().unwrap_or(if completed { "completed" } else { "running" }),
        ),
        _ => return None,
    };
    Some(json!({
        "id": item["id"],
        "name": name,
        "arguments": input,
        "output": output,
        "status": status
    }))
}
