//! Claude turns, session tracking, streamed events, and interruption.
use std::{collections::HashMap, sync::Arc, time::Duration};

use anyhow::anyhow;
use serde_json::{json, Value};

use crate::{
    api::assistant::{
        persistence::persist_message, tool_details::{save_tool_detail, ToolDetail}, Message, ToolCall,
    },
    assistant_job, assistant_store, claude, codex,
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
    let mut turn = claude::start_turn(&abs, record.claude_session_id.as_deref(), record.claude_provider.as_deref(), record.claude_model.as_deref(), record.claude_reasoning_level.as_deref(), &prompt).await?;
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
