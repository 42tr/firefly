//! Firefly event persistence, handoff state, and model-message conversion.
use async_trait::async_trait;
use g::{Content as GContent, EventSink as GEventSink, Message as GMessage, Role as GRole, RunEvent};
use serde::Serialize;
use serde_json::{json, Value};
use tokio::sync::{mpsc, oneshot};

use crate::{
    api::assistant::{
        conversations::map_attachment_record,
        persistence::{persist_message, persist_message_attachments},
        tool_details::{save_tool_detail, ToolDetail},
        Conversation, Message, MessageAttachment, Role, ToolCall,
    },
    assistant_job, mcp,
};

#[derive(Default)]
pub(super) struct HandoffState {
    active: Option<ActiveHandoff>,
}

struct ActiveHandoff {
    call_id: String,
    agent: String,
    final_text: String,
    tools: Vec<Value>,
    usage: g::Usage,
}

pub(super) struct AssistantRunEventSink {
    pub(super) sender: mpsc::Sender<(RunEvent, oneshot::Sender<()>)>,
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

pub(super) async fn handle_run_event(
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

pub(in crate::api::assistant) fn to_g_message(message: &Message) -> Option<GMessage> {
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
