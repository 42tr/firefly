//! Claude turns, session tracking, streamed events, and interruption.
use std::{collections::HashMap, sync::Arc, time::Duration};

use anyhow::anyhow;
use serde_json::{json, Value};

use crate::{
    api::assistant::{
        persistence::persist_message, tool_details::{save_tool_detail, ToolDetail}, Message, ToolCall,
    },
    agent_settings, assistant_job, assistant_store, claude, codex,
};

const CLAUDE_IDLE_TIMEOUT: Duration = Duration::from_secs(30 * 60);

pub(in crate::api::assistant) async fn run_claude_turn(
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
    let settings = agent_settings::Settings::load(crate::db::pool()).await?;
    let profile_id = match record.claude_provider.as_deref() {
        Some(provider) if provider.starts_with("api-") => Some(provider.trim_start_matches("api-")),
        Some(provider) => settings
            .profiles
            .iter()
            .find(|profile| {
                (profile.id == provider || profile.name == provider)
                    && profile.agent == "claude"
                    && profile.protocol == "anthropic"
            })
            .map(|profile| profile.id.as_str()),
        None => settings.claude_api_profile.as_deref(),
    };
    let profile = profile_id.and_then(|id| settings.profiles.iter().find(|profile| profile.id == id));
    let prepared = match profile {
        Some(profile) => claude::prepare_turn_with_model_profile(
            &abs,
            record.claude_session_id.as_deref(),
            profile,
            record.claude_model.as_deref(),
            record.claude_reasoning_level.as_deref(),
        ).await?,
        None => claude::prepare_turn(
            &abs,
            record.claude_session_id.as_deref(),
            record.claude_provider.as_deref(),
            record.claude_model.as_deref(),
            record.claude_reasoning_level.as_deref(),
        ).await?,
    };
    if job.cancellation_token().is_cancelled() { return Ok(()); }
    let mut session = claude::acquire(user_id, conversation_id, prepared, &prompt).await?;
    let prompt_id = session.prompt_id.clone();
    let (_seen_session, result) = run_claude_events(session.turn(), record, user_id, conversation_id, &job, &prompt_id).await;
    let cleanup = session.release(result.is_ok() && !job.cancellation_token().is_cancelled()).await;
    result.and(cleanup)
}

async fn run_claude_events(
    turn: &mut claude::ClaudeTurn, record: &assistant_store::ConversationRecord, user_id: &str, conversation_id: i64,
    job: &Arc<assistant_job::GenerationJob>, prompt_id: &str,
) -> (Option<String>, anyhow::Result<()>) {
    let cancellation = job.cancellation_token();
    let mut stored_session = record.claude_session_id.is_some();
    let mut seen_session: Option<String> = None;
    let mut tool_names = HashMap::<String, String>::new();
    let mut activity = firefly_claude_transport::Activity::default();
    let mut acknowledged = false;
    let mut state_deadline = None;

    loop {
        let event = tokio::select! {
            _ = cancellation.cancelled() => {
                return (seen_session, turn.interrupt().await);
            }
            event = turn.next_event() => match event {
                Ok(event) => event,
                Err(err) => return (seen_session, Err(err)),
            },
            _ = tokio::time::sleep_until(state_deadline.unwrap_or_else(|| tokio::time::Instant::now() + CLAUDE_IDLE_TIMEOUT)), if state_deadline.is_some() => {
                return (seen_session, Err(anyhow!("Claude 未发送会话状态事件，无法安全确认后台任务已结束；请升级 Claude Code 后继续此会话")));
            }
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
            return (seen_session, Err(anyhow!("Claude 会话在工作完成前已断开，子任务结果可能尚未送达；请恢复会话并检查实际执行结果")));
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
        if let Some(title) = claude::session_title(&event) {
            if let Err(err) = crate::api::assistant::persist_native_conversation_title(
                conversation_id, user_id, title, job,
            )
            .await
            {
                return (seen_session, Err(err));
            }
        }
        // A reused process can still have buffered idle/ambient events from the
        // previous job. Only this prompt's acknowledgment opens the new turn.
        if !acknowledged && (event["type"] == "user" && event["uuid"] == prompt_id
            || event["user_message_uuid"] == prompt_id) {
            acknowledged = true;
            activity.begin_prompt();
        }
        if !acknowledged {
            // Claude reports running before echoing the input. Remember that
            // state support is present, without accepting a previous job's idle.
            if event["subtype"] == "session_state_changed" { activity.observe(&event); }
            continue;
        }
        activity.observe(&event);
        if activity.missing_session_state() {
            state_deadline.get_or_insert_with(|| tokio::time::Instant::now() + Duration::from_secs(15));
        } else { state_deadline = None; }
        if let Err(error) = job.set_runtime_activity(activity.snapshot()).await {
            return (seen_session, Err(error));
        }
        if event["type"] == "system" {
            if let Err(error) = persist_claude_task(&event, conversation_id, job).await {
                return (seen_session, Err(error));
            }
        }
        // Child output is not a final answer from the main agent.
        if !event["parent_tool_use_id"].is_null() || !event["agent_id"].is_null() { continue; }
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
                        if let Err(error) = save_tool_detail(conversation_id, Some(job.id), ToolDetail {
                            call_id:&id, parent_id:"", name:&name, input:None,
                            output:Some(&json!(output)), status:if is_error { "error" } else { "completed" },
                        }).await { return (seen_session, Err(error)); }
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
                if event["is_error"] == true || event["subtype"].as_str().is_some_and(|s| s.starts_with("error")) {
                    let errors = event["errors"].as_array().map(|errors| errors.iter().filter_map(Value::as_str).collect::<Vec<_>>().join("\n"));
                    return (seen_session, Err(anyhow!("{}", event["result"].as_str().or(errors.as_deref()).unwrap_or("Claude 执行失败"))));
                }
                job.emit("assistant_turn_done", json!({"agent":"claude"}));
            }
            _ => {}
        }
        if activity.complete() { return (seen_session, Ok(())); }
    }
}

async fn persist_claude_task(event: &Value, conversation_id: i64, job: &Arc<assistant_job::GenerationJob>) -> anyhow::Result<()> {
    let subtype = event["subtype"].as_str().unwrap_or_default();
    if event["ambient"] == true { return Ok(()); }
    if subtype == "background_tasks_changed" {
        for task in event["tasks"].as_array().into_iter().flatten().filter(|task| task["ambient"] != true) {
            save_claude_task(task, conversation_id, job, "running").await?;
        }
    } else if subtype == "task_started" && event["is_backgrounded"] == true {
        save_claude_task(event, conversation_id, job, "running").await?;
    } else if subtype == "task_notification" {
        let status = match event["status"].as_str() { Some("completed") => "completed", Some("stopped") => "cancelled", _ => "error" };
        save_claude_task(event, conversation_id, job, status).await?;
    }
    Ok(())
}

async fn save_claude_task(task: &Value, conversation_id: i64, job: &Arc<assistant_job::GenerationJob>, status: &str) -> anyhow::Result<()> {
    let Some(id) = task["task_id"].as_str() else { return Ok(()) };
    let id = format!("claude-task:{id}:{}", task["run_id"].as_str().unwrap_or_default());
    let input = json!({"task":task["description"]});
    // Keep a task's original name/input on notifications that only have its ID.
    let existing: Option<String> = sqlx::query_scalar("SELECT name FROM assistant_tool_details WHERE conversation_id=?1 AND job_id=?2 AND call_id=?3 AND parent_id=''")
        .bind(conversation_id).bind(job.id.to_string()).bind(&id).fetch_optional(crate::db::pool()).await?;
    let name = existing.as_deref().unwrap_or(if task["task_type"] == "local_agent" { "handoff_to_claude" } else { "claude_background_task" });
    save_tool_detail(conversation_id, Some(job.id), ToolDetail {
        call_id:&id, parent_id:"", name,
        input:task["description"].is_string().then_some(&input),
        output:(status != "running").then_some(task), status,
    }).await?;
    Ok(())
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

#[cfg(test)]
#[path = "claude_tests.rs"]
mod tests;
