//! Conversation/message projections and compact process presentation.
use std::collections::HashMap;

use serde_json::{json, Value};

use crate::{
    api::assistant::{
        ConversationMessageResponse, ConversationSummary, HistoryMessage, Message,
        MessageAttachment, MessageAttachmentResponse, ProcessSummary, Role, ToolCall,
    },
    assistant_job, assistant_store,
};

pub(super) fn summarize_tool_record(mut message: ConversationMessageResponse) -> ConversationMessageResponse {
    if let Some(calls) = message.tool_calls.as_mut().and_then(Value::as_array_mut) {
        for call in calls {
            if let Some(map) = call.as_object_mut() { map.remove("arguments"); }
        }
        message.detail_omitted = true;
    }
    if message.role == "tool" {
        let presentation = if !message.is_error && tool_message_is_result(&message) {
            serde_json::from_str::<Value>(&message.content).ok().and_then(|value| assistant_job::tool_presentation(&value))
        } else { None };
        message.content = presentation.map(|value| value.to_string()).unwrap_or_default();
        message.detail_omitted = true;
        if let Some(metadata) = message.metadata.as_mut().and_then(Value::as_object_mut) {
            metadata.retain(|key, _| matches!(key.as_str(), "tool_name" | "agent" | "steps"));
            if let Some(steps) = metadata.get_mut("steps").and_then(Value::as_array_mut) {
                for step in steps {
                    if let Some(map) = step.as_object_mut() {
                        map.retain(|key, _| matches!(key.as_str(), "id" | "name" | "status"));
                    }
                }
            }
        }
    }
    message
}

pub(super) fn to_summary(record: assistant_store::ConversationRecord, is_running: bool) -> ConversationSummary {
    let status = if is_running {
        "running"
    } else {
        "completed"
    };
    ConversationSummary {
        id: record.id,
        title: record.title,
        agent: record.agent,
        cwd: record.codex_cwd,
        workspace_mode: record.workspace_mode,
        source_cwd: record.source_cwd,
        workspace_state: record.workspace_state,
        workspace_error: record.workspace_error,
        codex_provider: record.codex_provider,
        codex_model: record.codex_model,
        codex_reasoning_level: record.codex_reasoning_level,
        claude_model: record.claude_model,
        claude_provider: record.claude_provider,
        claude_reasoning_level: record.claude_reasoning_level,
        message_count: record.message_count,
        read_message_id: record.read_message_id,
        last_message_id: record.last_message_id,
        is_pinned: record.is_pinned,
        is_archived: record.is_archived,
        created_at: record.created_at,
        updated_at: record.updated_at,
        last_message_at: record.last_message_at,
        status,
    }
}

pub(in crate::api::assistant) fn to_message_response(
    record: assistant_store::MessageRecord, attachments: Vec<assistant_store::AttachmentRecord>,
) -> ConversationMessageResponse {
    ConversationMessageResponse {
        id: record.id,
        conversation_id: record.conversation_id,
        role: record.role,
        content: record.content,
        tool_call_id: record.tool_call_id,
        tool_calls: record.tool_calls,
        metadata: record.metadata,
        is_error: record.is_error,
        created_at: record.created_at,
        generation_job_id: record.generation_job_id,
        processing_status: record.processing_status,
        processing_error: record.processing_error,
        processing_started_at: record.processing_started_at,
        processing_completed_at: record.processing_completed_at,
        detail_omitted: false,
        attachments: attachments.into_iter().map(map_attachment_record).collect(),
    }
}

/// History contains one timing summary per contiguous process range. Full records
/// are returned only by the process endpoint, including for legacy turns without a job.
pub(in crate::api::assistant) fn summarize_process_messages(messages: Vec<ConversationMessageResponse>) -> Vec<HistoryMessage> {
    // A queued user message may be interleaved with another job's output. Keep
    // job boundaries independent; jobless history uses the latest user record.
    let mut legacy_turn = 0;
    let mut keys = Vec::with_capacity(messages.len());
    let mut last_tool = HashMap::new();
    let mut last_reply = HashMap::new();
    let messages: Vec<_> = messages.into_iter().filter(|message| {
        message.role != "assistant" || !message.content.trim().is_empty()
            || message.tool_calls.as_ref().and_then(Value::as_array).is_some_and(|calls| !calls.is_empty())
            || !message.attachments.is_empty()
    }).collect();
    for message in &messages {
        if message.role == "user" && message.generation_job_id.is_none() {
            legacy_turn = message.id;
        }
        let key = (message.generation_job_id, if message.generation_job_id.is_none() { legacy_turn } else { 0 });
        let has_tool_calls =
            message.tool_calls.as_ref().and_then(Value::as_array).is_some_and(|calls| !calls.is_empty());
        if message.role == "tool" || has_tool_calls {
            last_tool.insert(key, message.id);
        }
        // Codex may persist delayed tool results when finalizing a turn, after
        // its final answer. Keep the last reply visible, matching the frontend
        // timeline, rather than treating everything before that tool as process.
        if message.role == "assistant"
            && !has_tool_calls
            && !message.content.trim().is_empty()
            && !tool_content_is_chart_payload(&message.content)
        {
            last_reply.insert(key, message.id);
        }
        keys.push(key);
    }
    let mut result = Vec::new();
    let mut pending: Option<ProcessSummary> = None;
    let mut pending_key = None;
    for (mut message, key) in messages.into_iter().zip(keys) {
        let is_process = match message.role.as_str() {
            "tool" => !tool_message_is_result(&message),
            "assistant" => {
                last_tool.get(&key).is_some_and(|id| message.id <= *id)
                    && last_reply.get(&key) != Some(&message.id)
                    && !tool_content_is_chart_payload(&message.content)
            }
            _ => false,
        };
        if !is_process || pending_key != Some(key) {
            if let Some(summary) = pending.take() {
                result.push(HistoryMessage::Process(summary));
            }
        }
        if !is_process {
            if message.role == "tool" {
                let presentation = serde_json::from_str::<Value>(&message.content).ok()
                    .and_then(|value| assistant_job::tool_presentation(&value));
                let name = message.metadata.as_ref().and_then(|m| m["tool_name"].as_str()).unwrap_or("").to_string();
                message.content = presentation.map(|value| value.to_string()).unwrap_or_default();
                message.metadata = Some(json!({"tool_name":name}));
                message.tool_calls = None;
            }
            result.push(HistoryMessage::Message(message));
            continue;
        }
        if let Some(summary) = pending.as_mut() {
            summary.end_id = message.id;
            summary.started_at = summary.started_at.min(message.created_at);
            summary.completed_at = summary.completed_at.max(message.created_at);
        } else {
            pending = Some(ProcessSummary {
                id: message.id,
                conversation_id: message.conversation_id,
                role: "process",
                generation_job_id: message.generation_job_id,
                created_at: message.created_at,
                started_at: message.created_at,
                completed_at: message.created_at,
                start_id: message.id,
                end_id: message.id,
                status: message.processing_status.unwrap_or_else(|| "completed".into()),
                processing_started_at: message.processing_started_at,
                processing_completed_at: message.processing_completed_at,
            });
            pending_key = Some(key);
        }
    }
    if let Some(summary) = pending {
        result.push(HistoryMessage::Process(summary));
    }
    result
}

fn tool_message_is_result(message: &ConversationMessageResponse) -> bool {
    let name = message.metadata.as_ref().and_then(|m| m.get("tool_name")).and_then(Value::as_str).unwrap_or("");
    tool_content_is_chart_payload(&message.content)
        || (name == "image_generation" && !message.is_error && serde_json::from_str::<Value>(&message.content)
            .is_ok_and(|result| result["type"] == "image" && result["failure"].is_null() && result["error"].is_null()
                && result["image_url"].as_str().is_some_and(|url| url.starts_with("/api/assistant/attachments/by-file/"))))
}

/// 与前端 extractEchartsFromTool 的判定保持一致：工具结果是 echarts 图表负载
/// （会渲染成结果气泡）时不剥离内容。
fn tool_content_is_chart_payload(content: &str) -> bool {
    let Ok(value) = serde_json::from_str::<Value>(content) else {
        return false;
    };
    let result = value.get("result").unwrap_or(&value);
    (result.get("type").and_then(Value::as_str) == Some("echarts") && result.get("option").is_some())
        || result.get("echarts_option").is_some()
        || (result.get("option").is_some() && result.get("series").is_some())
}

pub(in crate::api::assistant) fn map_attachment_record(record: assistant_store::AttachmentRecord) -> MessageAttachmentResponse {
    MessageAttachmentResponse {
        id: record.id,
        file_name: record.file_name,
        content_type: record.content_type,
        file_size: record.file_size,
    }
}

pub(in crate::api::assistant) fn message_from_record(
    record: assistant_store::MessageRecord, attachments: Vec<assistant_store::AttachmentRecord>,
) -> anyhow::Result<Message> {
    let attachment_list = attachments.into_iter().map(MessageAttachment::from).collect::<Vec<_>>();
    match record.role.as_str() {
        "user" => {
            let mut msg = Message::new(Role::User, record.content);
            msg.attachments = attachment_list;
            Ok(msg)
        }
        "assistant" => {
            let tool_calls = if let Some(value) = record.tool_calls {
                let calls: Vec<assistant_store::StoredToolCall> = serde_json::from_value(value)?;
                if calls.is_empty() {
                    None
                } else {
                    Some(
                        calls
                            .into_iter()
                            .map(|call| ToolCall { id: call.id, name: call.name, arguments: call.arguments })
                            .collect(),
                    )
                }
            } else {
                None
            };
            let mut msg = Message::assistant(record.content, tool_calls);
            msg.attachments = attachment_list;
            Ok(msg)
        }
        "tool" => {
            let tool_call_id = record.tool_call_id.unwrap_or_default();
            let mut msg = Message::tool(record.content, tool_call_id, record.is_error);
            msg.attachments = attachment_list;
            Ok(msg)
        }
        other => Err(anyhow::anyhow!("Unknown role in history: {}", other)),
    }
}
