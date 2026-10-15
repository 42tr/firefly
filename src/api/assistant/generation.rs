use super::*;

#[cfg(test)]
#[path = "generation/workflow_tests.rs"]
mod workflow_tests;

pub(super) async fn run_conversation(
    config: &AssistantConfig, conversation: &mut Conversation, conversation_id: i64, enable_tools: bool,
    job: Arc<assistant_job::GenerationJob>, tool_context: mcp::ToolContext,
) -> anyhow::Result<()> {
    let settings = match &config.agent_settings {
        Some(settings) => settings.clone(),
        None => Arc::new(crate::agent_settings::Settings::load(crate::db::pool()).await?),
    };
    let enable_tools = enable_tools && crate::config::get().assistant_enable_tools;
    let mut system_prompt = config.system_prompt.clone();
    let task_backend = enable_tools.then(|| crate::assistant_tasks::Backend::new(
        tool_context.user_id.clone(), conversation_id, job.id));
    let _task_guard = task_backend.as_ref().map(|backend| crate::assistant_tasks::RunGuard(backend.clone()));
    let tools = if enable_tools { crate::agent_settings::available_tools(tool_context.clone()).await }
        else { Default::default() };
    let (children, unavailable) = if let Some(backend) = &task_backend {
        settings.child_agents(&tools, backend.clone())
    } else { (Vec::new(), Vec::new()) };
    if enable_tools {
        // Apply only host instructions relevant to the selected root tools.
        let mut root = settings.firefly.clone();
        root.instruction.clear();
        system_prompt.push_str(&crate::agent_settings::tool_instruction(&root));
        system_prompt.push_str(&crate::agent_settings::routing_instruction(&children));
        if !unavailable.is_empty() {
            system_prompt.push_str("\n以下子 agent 暂不可用，相关请求需告知用户检查 Agent 配置：\n");
            system_prompt.push_str(&unavailable.join("\n"));
        }
    }
    let outstanding_tasks = if let Some(backend) = &task_backend {
        !g::TaskBackend::ready_to_finish(backend.as_ref()).await?
    } else { false };
    if enable_tools {
        let attention = crate::assistant_tasks::global()
            .attention_summary(&tool_context.user_id, conversation_id, &job.id.to_string()).await?;
        append_task_attention(&mut system_prompt, &attention);
    }
    let mut messages = Vec::with_capacity(conversation.messages.len() + 1);
    messages.push(GMessage::system(system_prompt));
    for message in &conversation.messages {
        if let Some(message) = super::inputs::model_message(message).await? {
            messages.push(message);
        }
    }
    // 正常回合以 user 消息结尾；重启恢复的回合可能以 tool 结果结尾
    //（中断的工具调用已补写占位结果），两种结尾都可以直接续跑。
    if !matches!(messages.last(), Some(message) if matches!(message.role, GRole::User | GRole::Tool)
        || (outstanding_tasks && message.role == GRole::Assistant)) {
        return Err(anyhow!("last message is not user or tool"));
    }

    let (event_tx, mut event_rx) = mpsc::channel(64);
    // The summary state belongs to this run, never to the process-wide model.
    let model = Arc::new(llm::ContextModel::with_budget(config.model.clone(), config.budget.clone()));
    let mut agent = Agent::new(model.clone())
        .with_event_sink(Arc::new(AssistantRunEventSink { sender: event_tx }))
        .with_limits(RunLimits {
            max_tool_calls: usize::MAX,
            timeout: Duration::from_secs(settings.firefly.timeout_secs),
        });
    if task_backend.is_some() {
        agent = agent
            .tools(crate::agent_settings::selected_tools(&settings.firefly, &tools))
            .handoff(children);
    }

    if let Some(backend) = &task_backend { agent = agent.with_task_backend(backend.clone()); }

    let cancellation = job.cancellation_token();
    // Inline tools belong to this worker. Host-owned tasks are tracked by the
    // backend; RunGuard cancels attached tasks while preserving detached work.
    let runtime = Runtime::new();
    let run = runtime.run(&agent, RunRequest::new(messages).with_cancellation_token(cancellation.clone()));
    tokio::pin!(run);
    let mut handoff_state = HandoffState::default();
    loop {
        let next_event = tokio::select! {
            biased;
            _ = cancellation.cancelled() => return Ok(()),
            result = &mut run => {
                result?;
                return Ok(());
            }
            event = event_rx.recv() => event,
        };
        let Some((event, acknowledged)) = next_event else {
            return Err(anyhow!("assistant event channel closed before completion"));
        };
        handle_run_event(event, &mut handoff_state, conversation, conversation_id, &job).await?;
        let _ = acknowledged.send(());
    }
}

fn append_task_attention(prompt: &mut String, attention: &Value) {
    if attention["total"].as_u64().unwrap_or(0) == 0 { return; }
    prompt.push_str("\n以下仅列出需关注的持久化任务概况。仅当与当前请求相关，或需领取本轮未接收结果时，使用 get_tasks 指定 task_ids 查询；不要读取整段历史，也不要为收尾取消已转入后台且仍需供用户确认的会话。unknown 表示结果未知，先核实实际状态，不要直接重跑。has_more 为 true 时可用 get_tasks({}) 分批领取剩余结果。\n");
    prompt.push_str(&attention.to_string());
}

const CODEX_CAPACITY_RETRY_DELAY: Duration = Duration::from_secs(10);

pub(super) async fn run_codex_turn(
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

pub(super) fn is_text_attachment(attachment: &MessageAttachment) -> bool {
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

fn is_codex_capacity_error(message: &str) -> bool {
    message.to_ascii_lowercase().contains("selected model is at capacity")
}

async fn consume_codex_turn(
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

const CLAUDE_IDLE_TIMEOUT: Duration = Duration::from_secs(30 * 60);
const CODEX_TURN_STATUS_INTERVAL: Duration = Duration::from_secs(120);
const CODEX_FINISHED_TURN_GRACE: Duration = Duration::from_secs(10);

fn codex_turn_outcome(turn: &Value, event_error: Option<&str>, capacity_error: Option<&str>) -> anyhow::Result<()> {
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
async fn persist_codex_tool_result(
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
            match super::attachments::save_codex_image(&result, thread_id, &storage_dir).await {
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

pub(super) async fn run_claude_turn(
    record: &assistant_store::ConversationRecord, user_id: &str, conversation_id: i64, user_message: &Message,
    job: Arc<assistant_job::GenerationJob>,
) -> anyhow::Result<()> {
    let cwd_relative = record.codex_cwd.as_deref().ok_or_else(|| anyhow!("Claude 对话缺少工作目录"))?;
    let (abs, _display) = codex::resolve_workspace_path(Some(cwd_relative)).await?;

    let mut prompt = user_message.content.clone();
    if !user_message.attachments.is_empty() {
        let base_dir = std::env::current_dir()?;
        prompt.push_str("\n\n以下附件可按路径直接读取：");
        for attachment in &user_message.attachments {
            let path = std::path::Path::new(&attachment.storage_path);
            let path = if path.is_absolute() { path.to_path_buf() } else { base_dir.join(path) };
            prompt.push_str(&format!("\n- {}: {}", attachment.file_name, path.display()));
        }
    }

    if job.cancellation_token().is_cancelled() { return Ok(()); }
    let mut turn = claude::start_turn(&abs, record.claude_session_id.as_deref(), record.claude_model.as_deref(), &prompt).await?;
    let (_seen_session, result) = run_claude_events(&mut turn, record, user_id, conversation_id, &job).await;
    turn.interrupt().await?;
    result
}

async fn run_claude_events(
    turn: &mut claude::ClaudeTurn, record: &assistant_store::ConversationRecord, user_id: &str, conversation_id: i64,
    job: &Arc<assistant_job::GenerationJob>,
) -> (Option<String>, anyhow::Result<()>) {
    let cancellation = job.cancellation_token();
    let mut stored_session = record.claude_session_id.is_some();
    let mut seen_session: Option<String> = None;
    let mut tool_names = HashMap::<String, String>::new();

    loop {
        let event = tokio::select! {
            _ = cancellation.cancelled() => {
                return (seen_session, turn.interrupt().await);
            }
            event = turn.next_event() => match event {
                Ok(event) => event,
                Err(err) => return (seen_session, Err(err)),
            },
            // A hung CLI (network stall, stuck tool) otherwise keeps the job running and
            // blocks the conversation queue until the user cancels.
            _ = tokio::time::sleep(CLAUDE_IDLE_TIMEOUT) => {
                if let Err(err) = turn.interrupt().await {
                    return (seen_session, Err(err));
                }
                return (
                    seen_session,
                    Err(anyhow!("Claude 超过 {} 分钟没有任何输出，已停止", CLAUDE_IDLE_TIMEOUT.as_secs() / 60)),
                );
            }
        };
        let Some(event) = event else {
            return (seen_session, Err(anyhow!("Claude 事件流在返回结果前已断开")));
        };

        if !stored_session {
            if let Some(session_id) = claude::session_id(&event) {
                if let Err(err) = assistant_store::set_claude_session_id(conversation_id, user_id, session_id).await {
                    return (seen_session, Err(err));
                }
                stored_session = true;
                seen_session = Some(session_id.to_string());
            }
        }
        if let Some(delta) = claude::text_delta(&event).filter(|delta| !delta.is_empty()) {
            job.emit("assistant_delta", json!({ "delta": delta }));
            continue;
        }

        match event["type"].as_str().unwrap_or_default() {
            "assistant" => {
                if event.get("error").is_some() {
                    continue;
                }
                let mut text_parts = Vec::new();
                let mut tool_calls = Vec::new();
                if let Some(contents) = event["message"]["content"].as_array() {
                    for content in contents {
                        match content["type"].as_str() {
                            Some("text") => {
                                if let Some(text) = content["text"].as_str().filter(|text| !text.trim().is_empty()) {
                                    text_parts.push(text.to_string());
                                }
                            }
                            Some("tool_use") => {
                                let id = content["id"].as_str().unwrap_or("claude-tool").to_string();
                                let name = content["name"].as_str().unwrap_or("claude_tool").to_string();
                                let arguments = content.get("input").cloned().unwrap_or(Value::Null);
                                tool_names.insert(id.clone(), name.clone());
                                if let Err(err) = save_tool_detail(conversation_id, Some(job.id), ToolDetail {
                                    call_id: &id,
                                    parent_id: "",
                                    name: &name,
                                    input: Some(&arguments),
                                    output: None,
                                    status: "running",
                                }).await {
                                    return (seen_session, Err(err));
                                }
                                job.emit(
                                    "tool_call",
                                    json!({ "id": id, "name": name, "arguments": arguments, "agent": "claude" }),
                                );
                                tool_calls.push(ToolCall { id, name, arguments });
                            }
                            _ => {}
                        }
                    }
                }
                let content = text_parts.join("\n").trim().to_string();
                if !content.is_empty() || !tool_calls.is_empty() {
                    let assistant = Message::assistant(content, (!tool_calls.is_empty()).then_some(tool_calls));
                    if let Err(err) =
                        persist_message(conversation_id, &assistant, Some(json!({ "agent": "claude" })), Some(job.id))
                            .await
                    {
                        return (seen_session, Err(err));
                    }
                }
            }
            "user" => {
                if let Some(contents) = event["message"]["content"].as_array() {
                    for content in contents.iter().filter(|content| content["type"].as_str() == Some("tool_result")) {
                        let id = content["tool_use_id"].as_str().unwrap_or("claude-tool").to_string();
                        let name = tool_names.get(&id).cloned().unwrap_or_else(|| "claude_tool".to_string());
                        let output = claude_tool_result_content(content.get("content").unwrap_or(&Value::Null));
                        let is_error = content["is_error"].as_bool().unwrap_or(false);
                        let tool_message = Message::tool(output.clone(), id.clone(), is_error);
                        if let Err(err) = persist_message(
                            conversation_id,
                            &tool_message,
                            Some(json!({ "tool_name": name, "agent": "claude" })),
                            Some(job.id),
                        )
                        .await
                        {
                            return (seen_session, Err(err));
                        }
                        job.emit(
                            "tool_result",
                            json!({ "id": id, "name": name, "result": output, "is_error": is_error, "agent": "claude" }),
                        );
                    }
                }
            }
            "result" => {
                if event["is_error"].as_bool().unwrap_or(false) {
                    return (seen_session, Err(anyhow!("{}", event["result"].as_str().unwrap_or("Claude 执行失败"))));
                }
                return (seen_session, Ok(()));
            }
            _ => {}
        }
    }
}

fn claude_tool_result_content(content: &Value) -> String {
    if let Some(text) = content.as_str() {
        return text.to_string();
    }
    if let Some(blocks) = content.as_array() {
        let text = blocks.iter().filter_map(|block| block["text"].as_str()).collect::<Vec<_>>().join("\n");
        if !text.is_empty() {
            return text;
        }
    }
    serde_json::to_string(content).unwrap_or_else(|_| content.to_string())
}

fn codex_tool_payload(item: &Value, completed: bool) -> Option<Value> {
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

#[derive(Default)]
struct HandoffState {
    active: Option<ActiveHandoff>,
}

struct ActiveHandoff {
    call_id: String,
    agent: String,
    final_text: String,
    tools: Vec<Value>,
    usage: g::Usage,
}

struct AssistantRunEventSink {
    sender: mpsc::Sender<(RunEvent, oneshot::Sender<()>)>,
}

#[async_trait]
impl GEventSink for AssistantRunEventSink {
    async fn emit(&self, event: RunEvent) {
        let (acknowledged, receiver) = oneshot::channel();
        if self.sender.send((event, acknowledged)).await.is_ok() {
            let _ = receiver.await;
        }
    }
}

#[derive(Serialize)]
struct ToolResultEvent<'a> {
    id: &'a str,
    name: &'a str,
    result: &'a Value,
    is_error: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    message_id: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    agent: Option<&'a str>,
}

async fn handle_run_event(
    event: RunEvent, handoff_state: &mut HandoffState, conversation: &mut Conversation, conversation_id: i64,
    job: &assistant_job::GenerationJob,
) -> anyhow::Result<()> {
    if let RunEvent::HandoffStarted { call_id, agent, .. } = &event {
        save_tool_detail(conversation_id, Some(job.id), ToolDetail {
            call_id,
            parent_id: "",
            name: &format!("handoff_to_{agent}"),
            input: None,
            output: None,
            status: "running",
        }).await?;
        handoff_state.active = Some(ActiveHandoff {
            call_id: call_id.clone(),
            agent: agent.clone(),
            final_text: String::new(),
            tools: Vec::new(),
            usage: g::Usage::default(),
        });
        job.emit("handoff_started", json!({ "id": call_id, "agent": agent }));
        return Ok(());
    }
    if let RunEvent::ToolStarted { call_id, name, .. } = &event {
        let parent = handoff_state.active.as_ref().map_or("", |active| active.call_id.as_str());
        save_tool_detail(conversation_id, Some(job.id), ToolDetail {
            call_id,
            parent_id: parent,
            name,
            input: None,
            output: None,
            status: "running",
        }).await?;
        return Ok(());
    }
    if let RunEvent::HandoffCompleted { call_id, agent, child_run_id, is_error, .. } = &event {
        let active = handoff_state.active.take();
        let (final_text, tools, usage) = active
            .filter(|active| active.call_id == *call_id && active.agent == *agent)
            .map(|active| (active.final_text, active.tools, active.usage))
            .unwrap_or_else(|| (String::new(), Vec::new(), g::Usage::default()));
        let result = if *is_error {
            json!({ "agent": agent, "status": "failed", "child_run_id": child_run_id })
        } else {
            json!({ "agent": agent, "response": final_text, "child_run_id": child_run_id })
        };
        return persist_tool_result(
            conversation,
            conversation_id,
            job,
            call_id.clone(),
            format!("handoff_to_{}", agent),
            result,
            *is_error,
            Some(json!({ "agent": agent, "steps": tools, "usage": usage })),
        )
        .await;
    }
    if let Some(active) = handoff_state.active.as_mut() {
        match &event {
            RunEvent::ModelCompleted { message, usage, .. } => {
                active.usage.add(*usage);
                if !message.content.iter().any(|content| matches!(content, GContent::ToolCall { .. })) {
                    active.final_text = message.text_content();
                }
                for content in &message.content {
                    if let GContent::ToolCall { id, name, arguments } = content {
                        save_tool_detail(conversation_id, Some(job.id), ToolDetail {
                            call_id: id,
                            parent_id: &active.call_id,
                            name,
                            input: Some(arguments),
                            output: None,
                            status: "queued",
                        }).await?;
                        active.tools.push(json!({
                            "id": id,
                            "name": name,
                            "input": arguments,
                            "output": null,
                            "status": "running"
                        }));
                        job.emit(
                            "tool_call",
                            json!({
                                "id": id,
                                "name": name,
                                "arguments": arguments,
                                "agent": active.agent,
                                "parent_id": active.call_id,
                            }),
                        );
                    }
                }
            }
            RunEvent::ToolCompleted { call_id, name, result, is_error, .. } => {
                save_tool_detail(conversation_id, Some(job.id), ToolDetail {
                    call_id,
                    parent_id: &active.call_id,
                    name,
                    input: None,
                    output: Some(result),
                    status: if *is_error { "error" } else { "completed" },
                }).await?;
                if let Some(step) = active.tools.iter_mut().rev().find(|step| step["id"] == *call_id) {
                    step["output"] = result.clone();
                    step["status"] = if *is_error { json!("error") } else { json!("completed") };
                }
                job.emit("tool_result", json!({"id":call_id,"name":name,"is_error":is_error,
                    "agent":active.agent,"parent_id":active.call_id}));
            }
            _ => {}
        }
        return Ok(());
    }

    match event {
        RunEvent::TextDelta { text, .. } => {
            job.emit("assistant_delta", json!({ "delta": text }));
        }
        RunEvent::ModelCompleted { message, usage, .. } => {
            let content = message.text_content();
            let tool_calls = message
                .content
                .iter()
                .filter_map(|item| match item {
                    GContent::ToolCall { id, name, arguments } => {
                        Some(ToolCall { id: id.clone(), name: name.clone(), arguments: arguments.clone() })
                    }
                    _ => None,
                })
                .collect::<Vec<_>>();
            for call in &tool_calls {
                save_tool_detail(conversation_id, Some(job.id), ToolDetail {
                    call_id: &call.id,
                    parent_id: "",
                    name: &call.name,
                    input: Some(&call.arguments),
                    output: None,
                    status: "queued",
                }).await?;
                job.emit("tool_call", json!({ "id": call.id, "name": call.name, "arguments": call.arguments }));
            }
            if !content.is_empty() || !tool_calls.is_empty() {
                let assistant =
                    Message::assistant(content, if tool_calls.is_empty() { None } else { Some(tool_calls) });
                conversation.add_message(assistant.clone());
                persist_message(conversation_id, &assistant, Some(json!({"usage": usage})), Some(job.id)).await?;
            }
        }
        RunEvent::ToolCompleted { call_id, name, result, is_error, .. } => {
            persist_tool_result(conversation, conversation_id, job, call_id, name, result, is_error, None).await?;
        }
        _ => {}
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn persist_tool_result(
    conversation: &mut Conversation, conversation_id: i64, job: &assistant_job::GenerationJob, call_id: String,
    call_name: String, result: Value, is_error: bool, extra_metadata: Option<Value>,
) -> anyhow::Result<()> {
    let (result_value, attachments_meta) = match result {
        Value::Object(mut obj) => {
            let mut attachments =
                obj.remove("generated_attachments").and_then(|a| a.as_array().cloned()).unwrap_or_default();
            if let Some(response) = obj.remove("response") {
                attachments.extend(obj.remove("attachments").and_then(|a| a.as_array().cloned()).unwrap_or_default());
                (response, attachments)
            } else {
                obj.remove("generated_attachments");
                (Value::Object(obj), attachments)
            }
        }
        other => (other, Vec::new()),
    };
    let attachments = attachments_meta
        .into_iter()
        .filter_map(|value| serde_json::from_value::<mcp::ToolAttachment>(value).ok().map(MessageAttachment::from))
        .collect::<Vec<_>>();
    let mut tool_message = Message::tool(
        serde_json::to_string(&result_value).unwrap_or_else(|_| result_value.to_string()),
        call_id.clone(),
        is_error,
    );
    tool_message.attachments = attachments;
    conversation.add_message(tool_message.clone());

    let mut metadata = json!({ "tool_name": call_name });
    if let Some(Value::Object(extra)) = extra_metadata {
        if let Value::Object(metadata_object) = &mut metadata {
            metadata_object.extend(extra);
        }
    }
    let stored_record = persist_message(conversation_id, &tool_message, Some(metadata), Some(job.id)).await?;
    let stored_attachments = if let Some(last) = conversation.messages.last_mut() {
        persist_message_attachments(stored_record.id, last).await?
    } else {
        Vec::new()
    };
    let attachment_payload = stored_attachments.into_iter().map(map_attachment_record).collect::<Vec<_>>();
    let mut event_body = serde_json::to_value(ToolResultEvent {
        id: &call_id, name: &call_name, result: &result_value, is_error,
        message_id: Some(stored_record.id), agent: None,
    })?;
    if !attachment_payload.is_empty() {
        event_body["attachments"] = json!(attachment_payload);
    }
    job.emit("tool_result", event_body);
    Ok(())
}

pub(super) fn to_g_message(message: &Message) -> Option<GMessage> {
    match message.role {
        Role::User => {
            let text = if message.attachments.is_empty() {
                message.content.clone()
            } else {
                format_content_with_attachment_hint(&message.content, &message.attachments)
            };
            Some(GMessage::user(text))
        }
        Role::Assistant => {
            let mut items = Vec::new();
            if !message.content.is_empty() {
                items.push(GContent::text(message.content.clone()));
            }
            if let Some(calls) = &message.tool_calls {
                for call in calls {
                    items.push(GContent::ToolCall {
                        id: call.id.clone(),
                        name: call.name.clone(),
                        arguments: call.arguments.clone(),
                    });
                }
            }
            if items.is_empty() {
                return None;
            }
            Some(GMessage::new(GRole::Assistant, items))
        }
        Role::Tool => {
            let result =
                serde_json::from_str(&message.content).unwrap_or_else(|_| Value::String(message.content.clone()));
            let result = crate::assistant_tasks::bounded_snapshot_history(result);
            Some(GMessage::new(
                GRole::Tool,
                vec![GContent::ToolResult {
                    call_id: message.tool_call_id.clone().unwrap_or_default(),
                    result,
                    is_error: message.is_error,
                }],
            ))
        }
    }
}

fn format_content_with_attachment_hint(content: &str, attachments: &[MessageAttachment]) -> String {
    if attachments.is_empty() {
        return content.to_string();
    }

    let mut desc = attachments
        .iter()
        .map(|att| {
            let id_label = att.id.map(|id| format!("#{}", id)).unwrap_or_else(|| "-".to_string());
            format!("附件 {} · {} ({})", id_label, att.file_name, human_readable_size(att.file_size))
        })
        .collect::<Vec<_>>();
    desc.sort();
    let hint = format!(
        "[附件提示] 用户随消息上传了 {} 个文件：{}。\n请根据附件内容回答；需要处理 PDF 时可使用合并附件工具。",
        attachments.len(),
        desc.join("；")
    );

    if content.trim().is_empty() {
        hint
    } else {
        format!("{content}\n\n{hint}")
    }
}

fn human_readable_size(bytes: i64) -> String {
    const KB: i64 = 1024;
    const MB: i64 = KB * 1024;
    const GB: i64 = MB * 1024;

    if bytes >= GB {
        format!("{:.2} GB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.2} MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.1} KB", bytes as f64 / KB as f64)
    } else {
        format!("{bytes} B")
    }
}

#[cfg(test)]
mod event_contract_tests {
    use super::*;

    #[test]
    fn task_history_is_bounded_for_model_context_without_changing_the_transcript() {
        let tasks: Vec<_> = (0..438).map(|index| json!({
            "id":format!("t-{index}"),"task_id":format!("t-{index}"),"status":"succeeded",
            "title":"旧任务","result":{"exit_code":0},
            "recent_output":[{"data":{"text":"历史输出".repeat(1000)}}]
        })).collect();
        let original = json!({"completed":tasks,"pending":[]}).to_string();
        let message = Message::tool(original.clone(), "old-query".into(), false);
        let modeled = to_g_message(&message).unwrap();
        let GContent::ToolResult { result, call_id, is_error } = &modeled.content[0] else { panic!("missing tool result"); };
        assert_eq!(call_id, "old-query");
        assert!(!is_error);
        assert_eq!(result["historical_snapshot_truncated"], true);
        assert_eq!(result["completed"].as_array().unwrap().len(), 32);
        assert_eq!(result["omitted_completed"], 406);
        assert!(result.to_string().len() < 64 * 1024);
        assert_eq!(message.content, original);
        let unrelated = json!({"completed":[{"document":"x".repeat(100000)}],"pending":[]});
        assert_eq!(crate::assistant_tasks::bounded_snapshot_history(unrelated.clone()), unrelated);
        let empty_arrays = json!({"completed":[],"pending":[],"document":"x".repeat(100000)});
        assert_eq!(crate::assistant_tasks::bounded_snapshot_history(empty_arrays.clone()), empty_arrays);
    }

    #[test]
    fn task_startup_prompt_only_mentions_tasks_needing_attention() {
        let mut prompt = "base".to_owned();
        append_task_attention(&mut prompt, &json!({"total":0,"tasks":[]}));
        assert_eq!(prompt, "base");
        append_task_attention(&mut prompt, &json!({"total":1,"tasks":[{"id":"needed","status":"unknown"}]}));
        assert!(prompt.contains("needed"));
        assert!(prompt.contains("指定 task_ids"));
        assert!(!prompt.contains("先用 get_tasks"));
    }

    #[tokio::test]
    async fn codex_image_events_persist_visible_attachments_without_duplicate_base64() {
        use base64::Engine;
        crate::config::init_test_config();
        for only_terminal_item in [false, true] {
            let pool = crate::db::test_pool().await;
            crate::db::set_test_pool(pool.clone());
            let id: i64 = sqlx::query_scalar("INSERT INTO assistant_conversations (user_id, system_prompt, agent) VALUES ('owner','','codex') RETURNING id")
                .fetch_one(&pool).await.unwrap();
            let job = Arc::new(assistant_job::GenerationJob::new(uuid::Uuid::new_v4(), id, "owner".into()));
            sqlx::query("INSERT INTO assistant_generation_jobs (id,conversation_id,user_id,status) VALUES (?1,?2,'owner','running')")
                .bind(job.id).bind(id).execute(&pool).await.unwrap();
            let mut png = std::io::Cursor::new(Vec::new());
            image::DynamicImage::new_rgba8(1, 1).write_to(&mut png, image::ImageFormat::Png).unwrap();
            let encoded = base64::engine::general_purpose::STANDARD.encode(png.get_ref());
            let item = json!({"type":"imageGeneration", "id":"drawing", "status":"completed",
                "savedPath":"/root/.codex/generated_images/thread/drawing.png", "result":encoded});
            let (tx, rx) = tokio::sync::broadcast::channel(16);
            if !only_terminal_item {
                tx.send(json!({"method":"item/started", "params":{"threadId":"thread", "turnId":"turn",
                    "item":{"type":"imageGeneration","id":"drawing","status":"inProgress"}}})).unwrap();
                for _ in 0..2 {
                    tx.send(json!({"method":"item/completed", "params":{"threadId":"thread", "turnId":"turn", "item":item}})).unwrap();
                }
            }
            tx.send(json!({"method":"turn/completed", "params":{"threadId":"thread",
                "turn":{"id":"turn","status":"completed","items":[item]}}})).unwrap();
            drop(tx);
            consume_codex_turn("thread", "turn", rx, id, &job).await.unwrap();
            let rows: Vec<(String, Value)> = sqlx::query_as("SELECT content,metadata FROM assistant_messages WHERE conversation_id=?1 AND role='tool'")
                .bind(id).fetch_all(&pool).await.unwrap();
            assert_eq!(rows.len(), 1);
            let output: Value = serde_json::from_str(&rows[0].0).unwrap();
            assert!(output["image_url"].as_str().unwrap().starts_with("/api/assistant/attachments/by-file/"));
            assert!(output.get("result").is_none());
            assert_eq!(rows[0].1["tool_name"], "image_generation");
            let attachments: Vec<(String, String)> = sqlx::query_as("SELECT storage_path,content_type FROM assistant_message_attachments")
                .fetch_all(&pool).await.unwrap();
            assert_eq!(attachments.len(), 1);
            assert_eq!(attachments[0].1, "image/png");
            assert_eq!(fs::read(&attachments[0].0).await.unwrap(), *png.get_ref());
            fs::remove_file(&attachments[0].0).await.unwrap();
            let log = job.event_log.lock().unwrap();
            let results: Vec<_> = log.values().filter(|event| event.event == "tool_result").collect();
            assert_eq!(results.len(), 1);
            assert_eq!(results[0].data["attachments"][0]["content_type"], "image/png");
            assert!(results[0].data["message_id"].as_i64().is_some());
            assert!(!results[0].data.to_string().contains(&encoded));
        }
    }

    #[tokio::test]
    async fn codex_image_failures_and_missing_files_are_visible_errors() {
        crate::config::init_test_config();
        let pool = crate::db::test_pool().await;
        crate::db::set_test_pool(pool.clone());
        let id: i64 = sqlx::query_scalar("INSERT INTO assistant_conversations (user_id,system_prompt) VALUES ('owner','') RETURNING id")
            .fetch_one(&pool).await.unwrap();
        let job = Arc::new(assistant_job::GenerationJob::new(uuid::Uuid::new_v4(), id, "owner".into()));
        sqlx::query("INSERT INTO assistant_generation_jobs (id,conversation_id,user_id,status) VALUES (?1,?2,'owner','running')")
            .bind(job.id).bind(id).execute(&pool).await.unwrap();
        for failure in [json!({"code":"quota_exceeded"}), Value::Null] {
            let item = json!({"type":"imageGeneration","id":uuid::Uuid::new_v4(),"status":"completed",
                "savedPath":null,"result":"","failure":failure});
            persist_codex_tool_result("thread", id, &job, codex_tool_payload(&item, true).unwrap()).await.unwrap();
        }
        let results: Vec<bool> = sqlx::query_scalar("SELECT is_error FROM assistant_messages WHERE role='tool'")
            .fetch_all(&pool).await.unwrap();
        assert_eq!(results, [true, true]);
        let count: i64 = sqlx::query_scalar("SELECT count(*) FROM assistant_message_attachments").fetch_one(&pool).await.unwrap();
        assert_eq!(count, 0);
    }

    #[tokio::test]
    async fn persisted_tool_error_and_usage_match_live_events() {
        let pool = crate::db::test_pool().await;
        crate::db::set_test_pool(pool.clone());
        let id: i64 = sqlx::query_scalar("INSERT INTO assistant_conversations (user_id, system_prompt, agent) VALUES ('owner','','firefly') RETURNING id")
            .fetch_one(&pool).await.unwrap();
        let job = assistant_job::GenerationJob::new(uuid::Uuid::new_v4(), id, "owner".into());
        sqlx::query("INSERT INTO assistant_generation_jobs (id,conversation_id,user_id,status) VALUES (?1,?2,'owner','running')")
            .bind(job.id).bind(id).execute(&pool).await.unwrap();
        let mut conversation = Conversation::new();
        let mut handoff = HandoffState::default();
        handle_run_event(RunEvent::ModelCompleted { run_id: job.id, turn: 1,
            message: GMessage::new(GRole::Assistant, vec![GContent::ToolCall { id: "call".into(), name: "todos".into(), arguments: json!({"action":"list"}) }]),
            usage: g::Usage { input_tokens: 12, output_tokens: 3, ..Default::default() } }, &mut handoff, &mut conversation, id, &job).await.unwrap();
        handle_run_event(RunEvent::ToolCompleted { run_id: job.id, call_id: "call".into(), name: "todos".into(), result: json!({"error":"unavailable"}), is_error: true },
            &mut handoff, &mut conversation, id, &job).await.unwrap();
        let (is_error,): (bool,) = sqlx::query_as("SELECT is_error FROM assistant_messages WHERE conversation_id=?1 AND role='tool'")
            .bind(id).fetch_one(&pool).await.unwrap();
        assert!(is_error);
        let metadata: Value = sqlx::query_scalar("SELECT metadata FROM assistant_messages WHERE conversation_id=?1 AND role='assistant'")
            .bind(id).fetch_one(&pool).await.unwrap();
        assert_eq!(metadata["usage"]["input_tokens"], 12);
        let log = job.event_log.lock().unwrap();
        let result = log.values().find(|event| event.event == "tool_result").unwrap();
        assert_eq!(result.data["is_error"], true);
        assert!(result.data.get("result").is_none());
        assert!(result.data["message_id"].as_i64().is_some());
    }
}

#[cfg(test)]
mod codex_retry_tests {
    use super::*;

    const CAPACITY_ERROR: &str = "Selected model is at capacity. Please try a different model.";

    async fn consume(notifications: Vec<Value>) -> anyhow::Result<()> {
        let job = Arc::new(assistant_job::GenerationJob::new(uuid::Uuid::new_v4(), 1, "test".into()));
        let (tx, rx) = tokio::sync::broadcast::channel(16);
        for notification in notifications {
            tx.send(notification).unwrap();
        }
        drop(tx);
        consume_codex_turn("thread", "turn", rx, 1, &job).await
    }

    fn error(message: &str, will_retry: bool) -> Value {
        json!({"method": "error", "params": {
            "threadId": "thread", "turnId": "turn", "willRetry": will_retry,
            "error": {"message": message}
        }})
    }

    fn completed(status: &str, message: Option<&str>) -> Value {
        json!({"method": "turn/completed", "params": {
            "threadId": "thread", "turn": {"id": "turn", "status": status,
                "error": message.map(|message| json!({"message": message}))}
        }})
    }

    #[test]
    fn polled_turn_outcome_matches_completion_event() {
        assert!(codex_turn_outcome(&json!({"status": "completed"}), None, None).is_ok());
        let interrupted = codex_turn_outcome(&json!({"status": "interrupted"}), None, None).unwrap_err();
        assert_eq!(interrupted.to_string(), "Codex turn 已中断");
        let failed = json!({"status": "failed", "error": {"message": "boom"}});
        assert_eq!(codex_turn_outcome(&failed, Some("event"), None).unwrap_err().to_string(), "boom");
        let bare = json!({"status": "failed"});
        assert_eq!(codex_turn_outcome(&bare, None, Some(CAPACITY_ERROR)).unwrap_err().to_string(), CAPACITY_ERROR);
    }

    #[test]
    fn capacity_detection_is_specific_and_case_insensitive() {
        assert_eq!(CODEX_CAPACITY_RETRY_DELAY, Duration::from_secs(10));
        assert!(is_codex_capacity_error(CAPACITY_ERROR));
        assert!(is_codex_capacity_error("SELECTED MODEL IS AT CAPACITY"));
        for message in ["quota exceeded", "invalid API key", "Codex 事件流已断开"] {
            assert!(!is_codex_capacity_error(message));
        }
    }

    #[tokio::test]
    async fn capacity_error_waits_for_failed_turn_and_preserves_message() {
        let err = consume(vec![error(CAPACITY_ERROR, false), completed("failed", None)]).await.unwrap_err();
        assert_eq!(err.to_string(), CAPACITY_ERROR);
    }

    #[tokio::test]
    async fn internal_retry_and_stale_completion_keep_current_turn_alive() {
        let mut stale = completed("failed", Some("old failure"));
        stale["params"]["turn"]["id"] = json!("previous-turn");
        consume(vec![error(CAPACITY_ERROR, true), stale, completed("completed", None)]).await.unwrap();
    }

    #[tokio::test]
    async fn interrupted_turn_does_not_retry_an_earlier_capacity_error() {
        let err = consume(vec![error(CAPACITY_ERROR, false), completed("interrupted", None)]).await.unwrap_err();
        assert_eq!(err.to_string(), "Codex turn 已中断");
        assert!(!is_codex_capacity_error(&err.to_string()));
    }
}
