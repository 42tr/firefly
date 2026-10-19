//! Ownership-checked history pages and lazily loaded process details.
use std::collections::HashMap;

use axum::{extract::{Path, Query}, Json};
use serde_json::{json, Value};

use super::{
    presentation::{summarize_process_messages, summarize_tool_record, to_message_response},
    ProcessDetailQuery, ProcessDetailResponse,
};
use crate::{
    api::{
        assistant::{
            ConversationSourceReply, MessageListQuery, MessageListResponse,
            MAX_MESSAGE_PAGE_SIZE, MESSAGE_PAGE_SIZE,
        },
        auth::Session, error::AppError,
    },
    assistant_store,
};

pub async fn get_conversation_messages(
    auth_user: Session, Path(conversation_id): Path<i64>, Query(query): Query<MessageListQuery>,
) -> Result<Json<MessageListResponse>, AppError> {
    let user_id = auth_user.storage_scope();
    let limit = query.limit.unwrap_or(MESSAGE_PAGE_SIZE).clamp(1, MAX_MESSAGE_PAGE_SIZE);
    let before_id = query.before.filter(|id| *id > 0);
    // 分页查询自带 user_id 归属校验，None 即会话不存在或不属于当前用户
    let page = assistant_store::fetch_messages_page(conversation_id, user_id, limit, before_id)
        .await
        .map_err(AppError::internal_server_error)?
        .ok_or(AppError::not_found("Conversation not found"))?;
    let source_reply = sqlx::query_as::<_, ConversationSourceReply>(
        "SELECT b.reply_to_message_id AS message_id, c.id AS conversation_id, b.reply_preview AS preview
         FROM assistant_firefly_branches b
         LEFT JOIN assistant_messages m ON m.id = b.reply_to_message_id
         LEFT JOIN assistant_conversations c ON c.id = m.conversation_id AND c.user_id = b.user_id
         WHERE b.conversation_id = ?1 AND b.user_id = ?2 AND b.reply_preview IS NOT NULL",
    )
    .bind(conversation_id)
    .bind(user_id)
    .fetch_optional(crate::db::pool())
    .await
    .map_err(AppError::internal_server_error)?;
    let message_ids = page.messages.iter().map(|record| record.id).collect::<Vec<_>>();
    let attachment_rows = assistant_store::fetch_attachments_for_messages(conversation_id, user_id, &message_ids)
        .await
        .map_err(AppError::internal_server_error)?;
    let mut attachment_map: HashMap<i64, Vec<assistant_store::AttachmentRecord>> = HashMap::new();
    for attachment in attachment_rows {
        attachment_map.entry(attachment.message_id).or_default().push(attachment);
    }
    let next_before = if page.has_more { page.messages.first().map(|record| record.id) } else { None };
    let messages = page
        .messages
        .into_iter()
        .map(|record| {
            let attachments = attachment_map.remove(&record.id).unwrap_or_default();
            to_message_response(record, attachments)
        })
        .collect();
    Ok(Json(MessageListResponse {
        conversation_id,
        messages: summarize_process_messages(messages),
        has_more: page.has_more,
        next_before,
        source_reply,
    }))
}

/// Return step summaries; individual tool I/O is loaded through tool-detail.
pub async fn get_conversation_process(
    auth_user: Session, Path(conversation_id): Path<i64>, Query(query): Query<ProcessDetailQuery>,
) -> Result<Json<ProcessDetailResponse>, AppError> {
    match (query.start_id, query.end_id) {
        (Some(start), Some(end)) if start > 0 && end >= start => {}
        (None, None) if query.job_id.is_some() => {}
        _ => return Err(AppError::bad_request("请指定有效的执行过程范围")),
    }
    let user_id = auth_user.storage_scope();
    assistant_store::get_conversation_for_read(conversation_id, user_id)
        .await
        .map_err(AppError::internal_server_error)?
        .ok_or(AppError::not_found("Conversation not found"))?;

    let records = assistant_store::fetch_job_process_messages(
        conversation_id,
        user_id,
        query.job_id,
        query.start_id,
        query.end_id,
    )
    .await
    .map_err(AppError::internal_server_error)?;
    let message_ids = records.iter().map(|record| record.id).collect::<Vec<_>>();
    let attachment_rows = assistant_store::fetch_attachments_for_messages(conversation_id, user_id, &message_ids)
        .await
        .map_err(AppError::internal_server_error)?;
    let mut attachment_map: HashMap<i64, Vec<assistant_store::AttachmentRecord>> = HashMap::new();
    for attachment in attachment_rows {
        attachment_map.entry(attachment.message_id).or_default().push(attachment);
    }
    let mut messages: Vec<_> = records
        .into_iter()
        .map(|record| {
            let attachments = attachment_map.remove(&record.id).unwrap_or_default();
            summarize_tool_record(to_message_response(record, attachments))
        })
        .collect();
    let child_steps: Vec<(String, String, String, String)> = sqlx::query_as(
        "SELECT parent_id,call_id,name,status FROM assistant_tool_details WHERE conversation_id=?1 AND job_id=?2 AND parent_id!='' ORDER BY rowid",
    ).bind(conversation_id).bind(query.job_id.map(|id| id.to_string()).unwrap_or_default())
        .fetch_all(crate::db::pool()).await.map_err(AppError::internal_server_error)?;
    for message in &mut messages {
        if let Some(calls) = message.tool_calls.as_mut().and_then(Value::as_array_mut) {
            for call in calls {
                let steps: Vec<Value> = child_steps.iter().filter(|(parent,_,_,_)| call["id"] == *parent)
                    .map(|(_,id,name,status)| json!({"id":id,"name":name,"status":status})).collect();
                if !steps.is_empty() { call["steps"] = json!(steps); }
            }
        }
    }
    Ok(Json(ProcessDetailResponse { conversation_id, job_id: query.job_id, messages }))
}
