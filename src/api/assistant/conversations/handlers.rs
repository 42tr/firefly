//! Conversation lifecycle, list cursors, and monotonic read acknowledgements.
use std::collections::HashSet;

use axum::{extract::{Path, Query}, Json};
use serde_json::{json, Value};
use tokio::fs;
use tracing::warn;

use super::{
    presentation::to_summary, prepare_conversation, system_prompt_for_agent,
    MarkConversationReadRequest,
};
use crate::{
    api::{
        assistant::{
            ConversationListCursor, ConversationListQuery, ConversationListResponse,
            ConversationSummary, CreateConversationRequest, UpdateConversationRequest,
        },
        auth::Session, error::AppError,
    },
    assistant_job, assistant_store,
};

pub async fn create_conversation(
    auth_user: Session, Json(payload): Json<CreateConversationRequest>,
) -> Result<Json<ConversationSummary>, AppError> {
    let settings = prepare_conversation(&payload).await?;
    let system_prompt = system_prompt_for_agent(&settings.agent).await?;
    let workspace_id = (settings.workspace_mode == "parallel").then(uuid::Uuid::new_v4);
    let actual_cwd = workspace_id.map(crate::assistant_workspace::destination);
    let record = assistant_store::create_conversation(auth_user.storage_scope(), crate::assistant_store::NewConversation {
        system_prompt: &system_prompt,
        title: payload.title.as_deref(),
        agent: &settings.agent,
        codex_cwd: actual_cwd.as_deref().or(settings.cwd.as_deref()),
        provider: settings.provider.as_deref(),
        model: settings.model.as_deref(),
        reasoning_level: settings.reasoning_level.as_deref(),
        parallel: workspace_id.map(|id| (settings.cwd.as_deref().unwrap_or(""), id)),
    }).await.map_err(AppError::internal_server_error)?;
    Ok(Json(to_summary(record, false)))
}

pub async fn list_conversations(
    auth_user: Session, Query(query): Query<ConversationListQuery>,
) -> Result<Json<ConversationListResponse>, AppError> {
    let before = match (query.before_pinned, query.before_at, query.before_id) {
        (pinned, Some(at), Some(id)) if id > 0 => Some((pinned.unwrap_or(false), at, id)),
        (None, None, None) => None,
        _ => return Err(AppError::bad_request("分页游标无效")),
    };
    let limit = query.limit.unwrap_or(20).clamp(1, 100);
    let (mut records, active_conversation_ids, selected) = tokio::try_join!(
        assistant_store::list_workspace_conversations(
            crate::db::pool(),
            auth_user.storage_scope(),
            query.archived,
            limit + 1,
            before,
            &query.search,
        ),
        assistant_store::list_active_generation_conversation_ids(auth_user.storage_scope()),
        async {
            match query.selected_id {
                Some(id) => assistant_store::get_conversation_for_read(id, auth_user.storage_scope()).await,
                None => Ok(None),
            }
        },
    )
    .map_err(AppError::internal_server_error)?;
    let has_more = records.len() > limit as usize;
    records.truncate(limit as usize);
    let next_cursor = records
        .last()
        .filter(|_| has_more)
        .map(|record| ConversationListCursor { before_pinned: record.is_pinned, before_at: record.last_message_at, before_id: record.id });
    let active_conversation_ids = active_conversation_ids.into_iter().collect::<HashSet<_>>();
    let summary = |record: assistant_store::ConversationRecord| {
        let is_running = active_conversation_ids.contains(&record.id);
        to_summary(record, is_running)
    };
    let conversations = records.into_iter().map(summary).collect();
    let selected_conversation = selected.map(summary);
    Ok(Json(ConversationListResponse { conversations, next_cursor, selected_conversation }))
}

pub async fn mark_conversation_read(
    auth_user: Session, Path(conversation_id): Path<i64>, Json(payload): Json<MarkConversationReadRequest>,
) -> Result<Json<Value>, AppError> {
    if payload.message_id <= 0 {
        return Err(AppError::bad_request("已读消息位置无效"));
    }
    // A delayed request can acknowledge only the messages that device saw.
    // Reads never move backwards or change the conversation's activity time.
    let position: Option<i64> = sqlx::query_scalar(
        "UPDATE assistant_conversations SET read_message_id = MAX(read_message_id, ?3)
         WHERE id = ?1 AND user_id = ?2 AND EXISTS (SELECT 1 FROM assistant_messages WHERE id = ?3 AND conversation_id = ?1)
         RETURNING read_message_id",
    )
    .bind(conversation_id)
    .bind(auth_user.storage_scope())
    .bind(payload.message_id)
    .fetch_optional(crate::db::pool())
    .await
    .map_err(AppError::internal_server_error)?;
    let Some(position) = position else {
        assistant_store::get_conversation_for_read(conversation_id, auth_user.storage_scope())
            .await
            .map_err(AppError::internal_server_error)?
            .ok_or(AppError::not_found("Conversation not found"))?;
        return Err(AppError::bad_request("已读消息位置不属于该对话"));
    };
    crate::assistant_events::notify(Some(auth_user.storage_scope()), "conversations");
    Ok(Json(json!({"read_message_id": position})))
}

pub async fn update_conversation(
    auth_user: Session, Path(conversation_id): Path<i64>, Json(payload): Json<UpdateConversationRequest>,
) -> Result<Json<Value>, AppError> {
    let user_id = auth_user.storage_scope();

    if payload.title.is_none() && payload.is_pinned.is_none() && payload.is_archived.is_none() {
        return Err(AppError::bad_request("No updates specified"));
    }

    if let Some(archived) = payload.is_archived {
        let found = assistant_store::set_conversation_archived(conversation_id, user_id, archived)
            .await
            .map_err(|err| {
                if err.is::<assistant_store::ConversationArchivalBlocked>() { AppError::conflict(err) }
                else { AppError::internal_server_error(err) }
            })?;
        if !found { return Err(AppError::not_found("Conversation not found")); }
    }

    if let Some(title) = payload.title.as_deref() {
        assistant_store::rename_conversation(conversation_id, user_id, title)
            .await
            .map_err(AppError::internal_server_error)?;
    }

    if let Some(pinned) = payload.is_pinned {
        let found = assistant_store::set_conversation_pinned(conversation_id, user_id, pinned)
            .await
            .map_err(AppError::internal_server_error)?;
        if !found { return Err(AppError::not_found("Conversation not found")); }
    }

    Ok(Json(json!({ "ok": true })))
}

pub async fn delete_conversation(
    auth_user: Session, Path(conversation_id): Path<i64>,
) -> Result<Json<Value>, AppError> {
    let user_id = auth_user.storage_scope();

    if assistant_job::global_registry()
        .find_active_by_conversation(conversation_id)
        .is_some_and(|job| job.user_id == user_id)
        || assistant_store::find_active_generation(conversation_id, user_id)
            .await
            .map_err(AppError::internal_server_error)?
            .is_some()
    {
        return Err(AppError::conflict("当前对话仍有生成任务，暂时无法删除"));
    }

    let storage_paths = assistant_store::delete_conversation(conversation_id, user_id)
        .await
        .map_err(|err| {
            if err.is::<assistant_store::ConversationDeletionBlocked>() {
                AppError::conflict(err)
            } else {
                AppError::internal_server_error(err)
            }
        })?
        .ok_or_else(|| AppError::not_found("对话不存在或无权删除"))?;

    for storage_path in storage_paths {
        if let Err(err) = fs::remove_file(&storage_path).await {
            if err.kind() != std::io::ErrorKind::NotFound {
                warn!("failed to remove assistant attachment {}: {}", storage_path, err);
            }
        }
    }

    Ok(Json(json!({ "ok": true })))
}
