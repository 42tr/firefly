use super::*;

pub(super) async fn persist_message(
    conversation_id: i64, message: &Message, metadata: Option<Value>, generation_job_id: Option<uuid::Uuid>,
) -> anyhow::Result<assistant_store::MessageRecord> {
    if let Some(calls) = &message.tool_calls {
        for call in calls {
            save_tool_detail(conversation_id, generation_job_id, ToolDetail {
                call_id: &call.id,
                parent_id: "",
                name: &call.name,
                input: Some(&call.arguments),
                output: None,
                status: "queued",
            }).await?;
        }
    }
    if let Some(call_id) = &message.tool_call_id {
        let name = metadata.as_ref().and_then(|m| m["tool_name"].as_str()).unwrap_or("tool");
        let input = metadata.as_ref().and_then(|m| m.get("tool_input"));
        let output = serde_json::from_str(&message.content).unwrap_or(Value::String(message.content.clone()));
        save_tool_detail(conversation_id, generation_job_id, ToolDetail {
            call_id,
            parent_id: "",
            name,
            input,
            output: Some(&output),
            status: if message.is_error { "error" } else { "completed" },
        }).await?;
    }
    let role = match message.role {
        Role::User => "user",
        Role::Assistant => "assistant",
        Role::Tool => "tool",
    };

    let tool_calls = message.tool_calls.as_ref().map(|calls| {
        calls
            .iter()
            .map(|call| assistant_store::StoredToolCall {
                id: call.id.clone(),
                name: call.name.clone(),
                arguments: call.arguments.clone(),
            })
            .collect::<Vec<_>>()
    });

    let stored = assistant_store::StoredMessage {
        role: role.to_string(),
        content: message.content.clone(),
        tool_call_id: message.tool_call_id.clone(),
        tool_calls,
        metadata,
        is_error: message.is_error,
        generation_job_id,
    };

    assistant_store::append_message(conversation_id, stored).await
}

pub(super) async fn persist_message_attachments(
    message_record_id: i64, message: &mut Message,
) -> anyhow::Result<Vec<assistant_store::AttachmentRecord>> {
    if message.attachments.is_empty() {
        return Ok(Vec::new());
    }
    let new_attachments = message.attachments.iter().map(|att| att.as_new_attachment()).collect::<Vec<_>>();
    let stored = assistant_store::insert_attachments(message_record_id, &new_attachments).await?;
    message.attachments = stored.iter().cloned().map(MessageAttachment::from).collect();
    Ok(stored)
}
