use super::*;
use assistant_store::schedules::ExecutionSettings;

#[cfg(test)]
mod codex_settings_tests;
#[cfg(test)]
mod claude_settings_tests;

pub(super) async fn prepare_conversation(payload: &CreateConversationRequest) -> Result<ExecutionSettings, AppError> {
    let agent = payload.agent.as_deref().unwrap_or("firefly");
    if !matches!(agent, "firefly" | "codex" | "claude") {
        return Err(AppError::bad_request("agent 必须是 firefly、codex 或 claude"));
    }
    let mode = payload.workspace_mode.as_deref().unwrap_or("shared");
    if !matches!(mode, "shared" | "parallel") || (mode == "parallel" && agent == "firefly") {
        return Err(AppError::bad_request("workspace_mode 必须是 shared 或 parallel；并行模式仅支持 Codex / Claude"));
    }
    let workspace_cwd = if matches!(agent, "codex" | "claude") {
        let (_abs, display) = codex::resolve_workspace_path(payload.cwd.as_deref())
            .await
            .map_err(|err| AppError::bad_request(err.to_string()))?;
        Some(display)
    } else {
        None
    };
    if mode == "shared" {
        let raw = workspace_cwd.as_deref().unwrap_or("");
        if raw.split('/').next() == Some(".assistant-parallel") {
            return Err(AppError::bad_request("副本目录仅供所属会话使用；从副本新建工作请开启并行模式"));
        }
    }
    let mut codex_provider = payload.provider.as_deref().map(str::trim).filter(|provider| !provider.is_empty());
    let mut codex_model = payload.model.as_deref().map(str::trim).filter(|model| !model.is_empty());
    let codex_reasoning_level = if agent == "codex" {
        payload.reasoning_level.as_deref().map(str::trim).filter(|level| !level.is_empty())
    } else {
        None
    };
    if agent == "codex" {
        validate_codex_selection(codex_provider, codex_model)?;
    } else {
        codex_provider = None;
        codex_model = None;
    }
    let model = if agent == "claude" {
        claude::normalize_model(payload.model.clone()).map_err(AppError::bad_request)?
    } else { codex_model.map(str::to_string) };
    Ok(ExecutionSettings {
        agent: agent.into(), workspace_mode: mode.into(), cwd: workspace_cwd,
        provider: codex_provider.map(str::to_string), model,
        reasoning_level: codex_reasoning_level.map(str::to_string),
    })
}

fn validate_codex_selection(provider: Option<&str>, model: Option<&str>) -> Result<(), AppError> {
    if provider.is_some_and(|provider| provider.len() > 256) || model.is_some_and(|model| model.len() > 256) {
        return Err(AppError::bad_request("平台或模型名称过长"));
    }
    let Some(provider) = provider.filter(|provider| *provider != "openai") else { return Ok(()) };
    let listing = codex::list_model_providers();
    let known = listing.providers.iter().find(|item| item.name == provider)
        .ok_or_else(|| AppError::bad_request(format!("未知的 Codex API 平台: {provider}")))?;
    if !known.models.is_empty() && model.is_some_and(|model| !known.models.iter().any(|item| item == model)) {
        return Err(AppError::bad_request(format!("模型 {} 不在 API 平台 {provider} 的可选列表中", model.unwrap())));
    }
    Ok(())
}

pub(super) async fn system_prompt_for_agent(agent: &str) -> Result<String, AppError> {
    if agent == "firefly" { Ok(crate::agent_settings::Settings::load(crate::db::pool()).await?.firefly.instruction) }
    else { Ok(config::get().assistant_system_prompt.clone()) }
}

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
        codex_provider: settings.provider.as_deref(),
        model: settings.model.as_deref(),
        codex_reasoning_level: settings.reasoning_level.as_deref(),
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

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MarkConversationReadRequest {
    pub message_id: i64,
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

#[derive(Debug, Deserialize)]
pub struct ProcessDetailQuery {
    pub job_id: Option<uuid::Uuid>,
    pub start_id: Option<i64>,
    pub end_id: Option<i64>,
}

#[derive(Debug, Serialize)]
pub struct ProcessDetailResponse {
    pub conversation_id: i64,
    pub job_id: Option<uuid::Uuid>,
    pub messages: Vec<ConversationMessageResponse>,
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

fn summarize_tool_record(mut message: ConversationMessageResponse) -> ConversationMessageResponse {
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

pub async fn update_codex_settings(
    auth_user: Session, Path(conversation_id): Path<i64>, Json(payload): Json<CodexSettingsRequest>,
) -> Result<Json<ConversationSummary>, AppError> {
    let user_id = auth_user.storage_scope();
    assistant_store::get_conversation_for_read(conversation_id, user_id).await
        .map_err(AppError::internal_server_error)?.ok_or(AppError::not_found("Conversation not found"))?;
    let normalize = |value: Option<String>| value.map(|value| value.trim().to_owned()).filter(|value| !value.is_empty());
    let provider = normalize(payload.provider);
    let model = normalize(payload.model);
    let reasoning_level = normalize(payload.reasoning_level);
    validate_codex_selection(provider.as_deref(), model.as_deref())?;
    // Reasoning effort is an extensible string in the Codex protocol. Don't reject future/custom levels.
    if reasoning_level.as_deref().is_some_and(|level| level.len() > 64 || level.chars().any(char::is_whitespace)) {
        return Err(AppError::bad_request("推理级别格式无效"));
    }
    let record = assistant_store::set_codex_settings(
        conversation_id, user_id, provider.as_deref(), model.as_deref(), reasoning_level.as_deref(),
    ).await.map_err(|err| match err.downcast_ref::<assistant_store::ConversationSettingsError>() {
        Some(assistant_store::ConversationSettingsError::NotCodex) => AppError::bad_request(err),
        Some(_) => AppError::conflict(err),
        None => AppError::internal_server_error(err),
    })?.ok_or(AppError::not_found("Conversation not found"))?;
    let is_running = assistant_store::list_active_generation_conversation_ids(user_id).await
        .map_err(AppError::internal_server_error)?.contains(&conversation_id);
    Ok(Json(to_summary(record, is_running)))
}

pub async fn update_claude_settings(
    auth_user: Session, Path(conversation_id): Path<i64>, Json(payload): Json<ClaudeSettingsRequest>,
) -> Result<Json<ConversationSummary>, AppError> {
    let user_id = auth_user.storage_scope();
    assistant_store::get_conversation_for_read(conversation_id,user_id).await
        .map_err(AppError::internal_server_error)?.ok_or(AppError::not_found("Conversation not found"))?;
    let model = claude::normalize_model(payload.model).map_err(AppError::bad_request)?;
    let record = assistant_store::set_claude_settings(conversation_id,user_id,model.as_deref()).await
        .map_err(|error| match error.downcast_ref::<assistant_store::ConversationSettingsError>() {
            Some(assistant_store::ConversationSettingsError::NotClaude) => AppError::bad_request(error),
            Some(_) => AppError::conflict(error),
            None => AppError::internal_server_error(error),
        })?.ok_or(AppError::not_found("Conversation not found"))?;
    let running = assistant_store::list_active_generation_conversation_ids(user_id).await
        .map_err(AppError::internal_server_error)?.contains(&conversation_id);
    Ok(Json(to_summary(record,running)))
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

fn to_summary(record: assistant_store::ConversationRecord, is_running: bool) -> ConversationSummary {
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

pub(super) fn to_message_response(
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
pub(super) fn summarize_process_messages(messages: Vec<ConversationMessageResponse>) -> Vec<HistoryMessage> {
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

#[cfg(test)]
mod process_summary_tests {
    use super::*;

    fn message(id: i64, role: &str, content: &str, job: Option<uuid::Uuid>) -> ConversationMessageResponse {
        ConversationMessageResponse {
            id,
            conversation_id: 1,
            role: role.into(),
            content: content.into(),
            tool_call_id: None,
            tool_calls: None,
            metadata: None,
            is_error: false,
            created_at: chrono::DateTime::from_timestamp(id, 0).unwrap(),
            generation_job_id: job,
            processing_status: Some("completed".into()),
            processing_error: None,
            processing_started_at: None,
            processing_completed_at: None,
            detail_omitted: false,
            attachments: Vec::new(),
        }
    }

    #[test]
    fn empty_presentation_records_do_not_create_process_blocks_or_hide_replies() {
        let job = Some(uuid::Uuid::new_v4());
        let history = serde_json::to_value(summarize_process_messages(vec![
            message(1, "user", "question", job),
            message(2, "assistant", "", job),
            message(3, "assistant", "answer", job),
        ])).unwrap();
        assert_eq!(history.as_array().unwrap().len(), 2);
        assert_eq!(history[0]["content"], "question");
        assert_eq!(history[1]["content"], "answer");
    }

    #[test]
    fn process_steps_contain_no_tool_inputs_outputs_or_nested_diagnostics() {
        let mut call = message(1, "assistant", "", None);
        call.tool_calls = Some(json!([{"id":"parent","name":"handoff_to_programmer","arguments":{"task":"private input"}}]));
        let mut result = message(2, "tool", "private output", None);
        result.metadata = Some(json!({"tool_name":"handoff_to_programmer","agent":"programmer","tool_input":"private input",
            "steps":[{"id":"child","name":"command","input":"private input","output":"private output","status":"completed"}]}));
        let payload = json!([summarize_tool_record(call), summarize_tool_record(result)]);
        assert!(!payload.to_string().contains("private"));
        assert!(payload[0]["tool_calls"][0].get("arguments").is_none());
        assert_eq!(payload[1]["metadata"]["steps"][0]["id"], "child");
        assert_eq!(payload[1]["content"], "");
        assert_eq!(payload[1]["detail_omitted"], true);
    }

    #[tokio::test]
    async fn db_tests_history_process_is_lazy_and_scoped() {
        let pool = crate::db::test_pool().await;
        crate::db::set_test_pool(pool.clone());
        let conversation: i64 = sqlx::query_scalar(
            "INSERT INTO assistant_conversations (user_id, system_prompt) VALUES ('owner', '') RETURNING id",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        let mut ids = Vec::new();
        for (role, content) in [
            ("user", "question"),
            ("assistant", "intermediate"),
            ("tool", "private tool output"),
            ("assistant", "answer"),
        ] {
            let id: i64 = sqlx::query_scalar(
                "INSERT INTO assistant_messages (conversation_id, role, content) VALUES (?1, ?2, ?3) RETURNING id",
            )
            .bind(conversation)
            .bind(role)
            .bind(content)
            .fetch_one(&pool)
            .await
            .unwrap();
            ids.push(id);
        }
        let Json(page) = get_conversation_messages(
            Session::for_storage("owner"),
            Path(conversation),
            Query(MessageListQuery { limit: Some(20), before: None }),
        )
        .await
        .unwrap();
        let payload = serde_json::to_value(page).unwrap();
        assert_eq!(payload["messages"].as_array().unwrap().len(), 3);
        assert_eq!(payload["messages"][1]["role"], "process");
        assert!(!payload.to_string().contains("private tool output"));
        let range = || Query(ProcessDetailQuery { job_id: None, start_id: Some(ids[1]), end_id: Some(ids[2]) });
        let Json(details) =
            get_conversation_process(Session::for_storage("owner"), Path(conversation), range()).await.unwrap();
        assert_eq!(
            details.messages.iter().map(|m| m.content.as_str()).collect::<Vec<_>>(),
            ["intermediate", ""]
        );
        assert!(get_conversation_process(Session::for_storage("other"), Path(conversation), range())
            .await
            .is_err());
        assert!(get_conversation_process(
            Session::for_storage("owner"),
            Path(conversation),
            Query(ProcessDetailQuery { job_id: None, start_id: None, end_id: None }),
        )
        .await
        .is_err());
        // Even a one-bubble budget returns the complete turn after compaction.
        let Json(newest) = get_conversation_messages(
            Session::for_storage("owner"),
            Path(conversation),
            Query(MessageListQuery { limit: Some(1), before: None }),
        )
        .await
        .unwrap();
        assert_eq!(newest.next_before, None);
        assert!(!newest.has_more);
        let newest = serde_json::to_value(newest).unwrap();
        assert_eq!(newest["messages"][0]["content"], "question");
        assert_eq!(newest["messages"][2]["content"], "answer");
    }

    #[tokio::test]
    async fn db_tests_long_history_page_keeps_question_and_compact_process_together() {
        for job in [None, Some(uuid::Uuid::new_v4())] {
            let pool = crate::db::test_pool().await;
            crate::db::set_test_pool(pool.clone());
            let conversation: i64 = sqlx::query_scalar(
                "INSERT INTO assistant_conversations (user_id, system_prompt) VALUES ('owner', '') RETURNING id",
            )
            .fetch_one(&pool)
            .await
            .unwrap();
            if let Some(job) = job {
                sqlx::query("INSERT INTO assistant_generation_jobs (id, conversation_id, user_id, status) VALUES (?1, ?2, 'owner', 'completed')")
                    .bind(job).bind(conversation).execute(&pool).await.unwrap();
            }
            for (role, content) in [("user", "older question"), ("assistant", "older answer")] {
                sqlx::query("INSERT INTO assistant_messages (conversation_id, role, content) VALUES (?1, ?2, ?3)")
                    .bind(conversation)
                    .bind(role)
                    .bind(content)
                    .execute(&pool)
                    .await
                    .unwrap();
            }
            let question_id: i64 = sqlx::query_scalar("INSERT INTO assistant_messages (conversation_id, role, content, generation_job_id) VALUES (?1, 'user', 'long task question', ?2) RETURNING id")
                .bind(conversation).bind(job).fetch_one(&pool).await.unwrap();
            for _ in 0..30 {
                for (role, content) in [("assistant", "intermediate explanation"), ("tool", "private tool output")] {
                    sqlx::query("INSERT INTO assistant_messages (conversation_id, role, content, generation_job_id) VALUES (?1, ?2, ?3, ?4)")
                        .bind(conversation).bind(role).bind(content).bind(job).execute(&pool).await.unwrap();
                }
            }
            sqlx::query("INSERT INTO assistant_messages (conversation_id, role, content, generation_job_id) VALUES (?1, 'assistant', 'final answer', ?2)")
                .bind(conversation).bind(job).execute(&pool).await.unwrap();
            let Json(page) = get_conversation_messages(
                Session::for_storage("owner"),
                Path(conversation),
                Query(MessageListQuery { limit: Some(20), before: None }),
            )
            .await
            .unwrap();
            assert_eq!(page.next_before, Some(question_id));
            assert!(page.has_more);
            let payload = serde_json::to_value(page).unwrap();
            let messages = payload["messages"].as_array().unwrap();
            assert_eq!(messages.len(), 3);
            assert_eq!(messages[0]["content"], "long task question");
            assert_eq!(messages[1]["role"], "process");
            assert_eq!(messages[1]["start_id"], question_id + 1);
            assert_eq!(messages[1]["end_id"], question_id + 60);
            assert_eq!(messages[2]["content"], "final answer");
            assert!(!payload.to_string().contains("private tool output"));
            assert!(!payload.to_string().contains("intermediate explanation"));
            let Json(older) = get_conversation_messages(
                Session::for_storage("owner"),
                Path(conversation),
                Query(MessageListQuery { limit: Some(20), before: Some(question_id) }),
            )
            .await
            .unwrap();
            assert!(!older.has_more);
            let older = serde_json::to_value(older).unwrap();
            assert_eq!(older["messages"][0]["content"], "older question");
            assert_eq!(older["messages"][1]["content"], "older answer");
            assert_eq!(older["messages"].as_array().unwrap().len(), 2);
        }
    }

    #[test]
    fn history_returns_only_timing_and_results_even_for_large_running_or_legacy_processes() {
        for job in [Some(uuid::Uuid::new_v4()), None] {
            let mut records =
                vec![message(1, "user", "question", job), message(2, "assistant", "intermediate text", job)];
            for id in 3..103 {
                let mut record = message(id, "tool", &"large private tool output".repeat(1000), job);
                record.metadata = Some(json!({"tool_input": "large input", "steps": ["handoff details"]}));
                record.processing_status = Some("running".into());
                records.push(record);
            }
            records.push(message(103, "assistant", "final answer", job));
            let result = serde_json::to_value(summarize_process_messages(records)).unwrap();
            let rows = result.as_array().unwrap();
            assert_eq!(rows.len(), 3);
            assert_eq!(rows[0]["content"], "question");
            assert_eq!(rows[1]["role"], "process");
            assert_eq!(rows[1]["start_id"], 2);
            assert_eq!(rows[1]["end_id"], 102);
            assert_eq!(rows[1]["started_at"], "1970-01-01T00:00:02Z");
            assert_eq!(rows[1]["completed_at"], "1970-01-01T00:01:42Z");
            assert_eq!(rows[2]["content"], "final answer");
            let payload = result.to_string();
            assert!(payload.len() < 1500);
            for omitted in ["large private", "large input", "handoff details", "intermediate text"] {
                assert!(!payload.contains(omitted));
            }
        }
    }

    #[test]
    fn final_reply_survives_tool_results_persisted_after_it() {
        for job in [Some(uuid::Uuid::new_v4()), None] {
            let mut call = message(3, "assistant", "", job);
            call.tool_calls = Some(json!([{"id": "command", "name": "command", "arguments": {}}]));
            let mut empty_call = message(6, "assistant", "  ", job);
            empty_call.tool_calls = Some(json!([{"id": "cleanup", "name": "command", "arguments": {}}]));
            let rows = serde_json::to_value(summarize_process_messages(vec![
                message(1, "user", "question", job),
                message(2, "assistant", "working on it", job),
                call,
                message(4, "assistant", "final answer", job),
                message(5, "tool", "delayed command result", job),
                empty_call,
                message(7, "tool", "cleanup result", job),
            ]))
            .unwrap();
            assert_eq!(rows.as_array().unwrap().len(), 4);
            assert_eq!(rows[1]["role"], "process");
            assert_eq!(rows[1]["start_id"], 2);
            assert_eq!(rows[1]["end_id"], 3);
            assert_eq!(rows[2]["id"], 4);
            assert_eq!(rows[2]["content"], "final answer");
            assert_eq!(rows[3]["role"], "process");
            assert_eq!(rows[3]["start_id"], 5);
            assert_eq!(rows[3]["end_id"], 7);
            assert!(!rows.to_string().contains("working on it"));
            assert!(!rows.to_string().contains("delayed command result"));
        }
    }

    #[test]
    fn final_replies_are_preserved_per_job_and_legacy_turn() {
        let a = Some(uuid::Uuid::new_v4());
        let b = Some(uuid::Uuid::new_v4());
        let rows = summarize_process_messages(vec![
            message(1, "user", "question A", a),
            message(2, "user", "queued question B", b),
            message(3, "assistant", "answer A", a),
            message(4, "tool", "late result A", a),
            message(5, "assistant", "answer B", b),
            message(6, "tool", "late result B", b),
            message(7, "user", "legacy question A", None),
            message(8, "assistant", "legacy answer A", None),
            message(9, "tool", "late legacy result A", None),
            message(10, "user", "legacy question B", None),
            message(11, "assistant", "legacy answer B", None),
            message(12, "tool", "late legacy result B", None),
        ]);
        let rows = serde_json::to_value(rows).unwrap();
        let replies: Vec<_> = rows
            .as_array()
            .unwrap()
            .iter()
            .filter(|row| row["role"] == "assistant")
            .map(|row| row["content"].as_str().unwrap())
            .collect();
        assert_eq!(replies, ["answer A", "answer B", "legacy answer A", "legacy answer B"]);
    }

    #[test]
    fn result_cards_and_turn_boundaries_survive_compaction() {
        let job = Some(uuid::Uuid::new_v4());
        let mut call = message(2, "assistant", "", job);
        call.tool_calls = Some(json!([{"id": "tool", "name": "parse_bill_text", "arguments": {"text": "input"}}]));
        let mut bill = message(3, "tool", "bill result", job);
        bill.metadata = Some(json!({"tool_name": "parse_bill_text"}));
        let chart = message(4, "tool", r#"{"type":"echarts","option":{}}"#, job);
        let mut image = message(5, "tool", "image result", job);
        image.metadata = Some(json!({"tool_name": "image_generation"}));
        let rows = serde_json::to_value(summarize_process_messages(vec![
            message(1, "user", "question", job),
            call,
            bill,
            chart,
            image,
            message(6, "assistant", "answer", job),
            message(7, "user", "legacy question", None),
            message(8, "tool", "legacy output", None),
            message(9, "assistant", "legacy answer", None),
        ]))
        .unwrap();
        assert_eq!(rows[1]["role"], "process");
        assert_eq!(rows[1]["end_id"], 3);
        assert!(!rows.to_string().contains("bill result"));
        assert!(rows[2]["content"].as_str().unwrap().contains("echarts"));
        assert_eq!(rows[3]["role"], "process");
        assert!(!rows.to_string().contains("image result"));
        assert_eq!(rows[4]["content"], "answer");
        assert_eq!(rows[6]["role"], "process");
        assert_eq!(rows[6]["start_id"], 8);
        assert!(rows[6]["generation_job_id"].is_null());
    }

    #[test]
    fn generated_images_survive_history_compaction_between_tool_steps() {
        let job = Some(uuid::Uuid::new_v4());
        let mut image = message(3, "tool", r#"{"type":"image","image_url":"/api/assistant/attachments/by-file/1"}"#, job);
        image.metadata = Some(json!({"tool_name":"image_generation"}));
        let mut failed = message(4, "tool", &image.content, job);
        failed.metadata = image.metadata.clone();
        failed.is_error = true;
        let rows = serde_json::to_value(summarize_process_messages(vec![
            message(1,"user","draw",job), message(2,"assistant","drawing",job), image, failed,
            message(5,"tool","cleanup",job), message(6,"assistant","choose an image",job),
        ])).unwrap();
        assert_eq!(rows[1]["role"], "process");
        assert_eq!(rows[2]["id"], 3);
        assert!(rows[2]["content"].as_str().unwrap().contains("image_url"));
        assert_eq!(rows[3]["role"], "process");
        assert_eq!(rows[3]["start_id"], 4);
        assert_eq!(rows[4]["content"], "choose an image");
    }
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

pub(super) fn map_attachment_record(record: assistant_store::AttachmentRecord) -> MessageAttachmentResponse {
    MessageAttachmentResponse {
        id: record.id,
        file_name: record.file_name,
        content_type: record.content_type,
        file_size: record.file_size,
    }
}

pub(super) fn message_from_record(
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

#[cfg(test)]
mod list_tests {
    use super::*;

    #[tokio::test]
    async fn db_tests_conversation_reads_sync_and_never_acknowledge_newer_messages() {
        let pool = crate::db::test_pool().await;
        crate::db::set_test_pool(pool.clone());
        let conversation = assistant_store::create_conversation("owner", crate::assistant_store::NewConversation {
            system_prompt: "",
            title: None,
            agent: "firefly",
            codex_cwd: None,
            codex_provider: None,
            model: None,
            codex_reasoning_level: None,
            parallel: None,
        })
            .await.unwrap();
        let append = || assistant_store::append_message(conversation.id, assistant_store::StoredMessage {
            role: "assistant".into(), content: "reply".into(), tool_call_id: None, tool_calls: None,
            metadata: None, is_error: false, generation_job_id: None,
        });
        let first = append().await.unwrap();
        let before = page("owner", json!({"selected_id": conversation.id})).await.selected_conversation.unwrap();
        assert_eq!(before.last_message_id, first.id);
        assert_eq!(before.read_message_id, 0);
        let request = |id| Json(MarkConversationReadRequest { message_id: id });
        let mut events = crate::assistant_events::changes().subscribe();
        let saved = mark_conversation_read(Session::for_storage("owner"), Path(conversation.id), request(first.id))
            .await.unwrap().0;
        assert_eq!(saved["read_message_id"], first.id);
        let event = events.try_recv().unwrap();
        assert_eq!(event.domain, "conversations");
        assert!(event.visible_to("owner"));
        assert!(!event.visible_to("other"));
        // Another device/reconnection gets the same read cursor from either list path.
        let after = page("owner", json!({"selected_id": conversation.id})).await;
        assert_eq!(after.conversations[0].read_message_id, first.id);
        assert_eq!(after.selected_conversation.unwrap().read_message_id, first.id);
        assert_eq!(after.conversations[0].updated_at, before.updated_at);
        assert_eq!(after.conversations[0].last_message_at, before.last_message_at);

        let second = append().await.unwrap();
        let _ = mark_conversation_read(Session::for_storage("owner"), Path(conversation.id), request(first.id)).await.unwrap();
        let unread = page("owner", json!({})).await.conversations.remove(0);
        assert_eq!(unread.last_message_id, second.id);
        assert_eq!(unread.read_message_id, first.id);
        let _ = mark_conversation_read(Session::for_storage("owner"), Path(conversation.id), request(second.id)).await.unwrap();
        // A late read from an older device never moves the cursor backwards.
        let saved = mark_conversation_read(Session::for_storage("owner"), Path(conversation.id), request(first.id)).await.unwrap().0;
        assert_eq!(saved["read_message_id"], second.id);
        let denied = mark_conversation_read(Session::for_storage("other"), Path(conversation.id), request(second.id)).await.unwrap_err();
        assert_eq!(denied.status(), StatusCode::NOT_FOUND);
        for invalid in [0, -1, second.id + 1] {
            let denied = mark_conversation_read(Session::for_storage("owner"), Path(conversation.id), request(invalid)).await.unwrap_err();
            assert_eq!(denied.status(), StatusCode::BAD_REQUEST);
        }
        // Deleting a queued record and reusing the message count cannot hide a new reply.
        sqlx::query("DELETE FROM assistant_messages WHERE id = ?1").bind(second.id).execute(&pool).await.unwrap();
        sqlx::query("UPDATE assistant_conversations SET message_count = message_count - 1 WHERE id = ?1")
            .bind(conversation.id).execute(&pool).await.unwrap();
        let third = append().await.unwrap();
        let unread = page("owner", json!({})).await.conversations.remove(0);
        assert_eq!(unread.message_count, 2);
        assert_eq!(unread.last_message_id, third.id);
        assert!(unread.last_message_id > unread.read_message_id);
    }

    async fn page(user_id: &str, params: Value) -> ConversationListResponse {
        list_conversations(Session::for_storage(user_id), Query(serde_json::from_value(params).unwrap()))
            .await
            .unwrap()
            .0
    }

    #[tokio::test]
    async fn db_tests_legacy_firefly_source_survives_parent_deletion_and_checks_ownership() {
        let pool = crate::db::test_pool().await;
        crate::db::set_test_pool(pool.clone());
        let parent: i64 = sqlx::query_scalar("INSERT INTO assistant_conversations(user_id, system_prompt, agent) VALUES ('owner', '', 'firefly') RETURNING id")
            .fetch_one(&pool).await.unwrap();
        let message: i64 = sqlx::query_scalar("INSERT INTO assistant_messages(conversation_id, role, content) VALUES (?1, 'assistant', 'source answer') RETURNING id")
            .bind(parent).fetch_one(&pool).await.unwrap();
        let child: i64 = sqlx::query_scalar("INSERT INTO assistant_conversations(user_id, system_prompt, agent) VALUES ('owner', '', 'firefly') RETURNING id")
            .fetch_one(&pool).await.unwrap();
        sqlx::query("INSERT INTO assistant_firefly_branches(conversation_id, user_id, request_id, reply_to_message_id, reply_preview) VALUES (?1, 'owner', ?2, ?3, 'source answer')")
            .bind(child).bind(uuid::Uuid::new_v4()).bind(message).execute(&pool).await.unwrap();
        let query = || Query(MessageListQuery { limit: None, before: None });
        let source = get_conversation_messages(Session::for_storage("owner"), Path(child), query()).await.unwrap().0.source_reply.unwrap();
        assert_eq!(source.conversation_id, Some(parent));
        assert_eq!(source.message_id, Some(message));
        assert_eq!(get_conversation_messages(Session::for_storage("other"), Path(child), query()).await.unwrap_err().status(), StatusCode::NOT_FOUND);
        sqlx::query("DELETE FROM assistant_conversations WHERE id=?1").bind(parent).execute(&pool).await.unwrap();
        let source = get_conversation_messages(Session::for_storage("owner"), Path(child), query()).await.unwrap().0.source_reply.unwrap();
        assert_eq!(source.preview, "source answer");
        assert!(source.message_id.is_none());
        assert!(source.conversation_id.is_none());
    }

    #[tokio::test]
    async fn db_tests_conversation_list_paginates_and_searches_all_assistants() {
        let pool = crate::db::test_pool().await;
        crate::db::set_test_pool(pool.clone());
        let at = chrono::Utc::now().timestamp_micros();
        let oldest: i64 = sqlx::query_scalar(
            "INSERT INTO assistant_conversations (user_id, agent, system_prompt, title, codex_cwd, message_count, last_message_at)
             VALUES ('owner', 'codex', '', '最早的工作区 Ä', 'project_100%', 1, ?1) RETURNING id",
        ).bind(at).fetch_one(&pool).await.unwrap();
        sqlx::query(
            "WITH RECURSIVE seq(n) AS (SELECT 1 UNION ALL SELECT n+1 FROM seq WHERE n < 220)
             INSERT INTO assistant_conversations (user_id, agent, system_prompt, message_count, last_message_at)
             SELECT 'owner', 'firefly', '', 1, ?1 FROM seq",
        ).bind(at + 1).execute(&pool).await.unwrap();
        let first = page("owner", json!({})).await;
        assert_eq!(first.conversations.len(), 20);
        assert!(first.conversations.iter().all(|row| row.agent == "firefly"));
        assert!(first.next_cursor.is_some());

        // More than 200 workspaces with identical timestamps must all be reachable.
        sqlx::query(
            "WITH RECURSIVE seq(n) AS (SELECT 1 UNION ALL SELECT n+1 FROM seq WHERE n < 204)
             INSERT INTO assistant_conversations (user_id, agent, system_prompt, message_count, last_message_at)
             SELECT 'owner', 'claude', '', 1, ?1 FROM seq",
        ).bind(at).execute(&pool).await.unwrap();
        sqlx::query(
            "INSERT INTO assistant_conversations (user_id, agent, system_prompt, message_count) VALUES
             ('other', 'codex', '', 1), ('owner', 'codex', '', 0)",
        ).execute(&pool).await.unwrap();
        let mut params = json!({"limit": 20});
        let mut ids = Vec::new();
        loop {
            let result = page("owner", params).await;
            assert!(result.conversations.len() <= 20);
            ids.extend(result.conversations.into_iter().map(|row| row.id));
            let Some(cursor) = result.next_cursor else { break };
            params = serde_json::to_value(cursor).unwrap();
            params["limit"] = json!(20);
        }
        assert_eq!(ids.len(), 425);
        assert_eq!(ids.iter().copied().collect::<HashSet<_>>().len(), 425);
        assert_eq!(ids.last(), Some(&oldest));
        for search in [" 最早的工作区 ä ", "PROJECT_100%"] {
            let found = page("owner", json!({"search": search})).await;
            assert_eq!(found.conversations.len(), 1);
            assert_eq!(found.conversations[0].id, oldest);
        }
        assert!(page("owner", json!({"search": "project_100%missing"})).await.conversations.is_empty());
        let selected = page("owner", json!({"search": "not found", "selected_id": oldest})).await;
        assert!(selected.conversations.is_empty());
        assert_eq!(selected.selected_conversation.unwrap().id, oldest);
        assert!(page("other", json!({"selected_id": oldest})).await.selected_conversation.is_none());
        assert_eq!(page("owner", json!({"selected_id": oldest + 1})).await.selected_conversation.unwrap().agent, "firefly");
        assert!(page("owner", json!({"search": "firefly"})).await.conversations.iter().all(|row| row.agent == "firefly"));
        assert_eq!(page("owner", json!({"limit": 100})).await.conversations.len(), 100);
    }

    #[test]
    fn conversation_updates_accept_archival_fields() {
        assert_eq!(serde_json::from_value::<UpdateConversationRequest>(json!({"is_archived": true})).unwrap().is_archived, Some(true));
        assert_eq!(serde_json::from_value::<UpdateConversationRequest>(json!({"title": "renamed", "is_archived": false})).unwrap().is_archived, Some(false));
        let update: UpdateConversationRequest = serde_json::from_value(json!({"title": "renamed"})).unwrap();
        assert_eq!(update.title.as_deref(), Some("renamed"));
    }

    #[tokio::test]
    async fn db_tests_archival_lists_history_ownership_and_busy_jobs() {
        let pool = crate::db::test_pool().await;
        crate::db::set_test_pool(pool.clone());
        let at = chrono::Utc::now().timestamp_micros();
        for id in 1..=4 {
            sqlx::query("INSERT INTO assistant_conversations(id,user_id,system_prompt,message_count,last_message_at,is_pinned) VALUES (?1,'owner','',1,?2,?3)")
                .bind(id).bind(at).bind(id == 2).execute(&pool).await.unwrap();
            sqlx::query("INSERT INTO assistant_messages(conversation_id,role,content) VALUES (?1,'user','history')")
                .bind(id).execute(&pool).await.unwrap();
        }
        let denied = update_conversation(Session::for_storage("other"), Path(2),
            Json(serde_json::from_value(json!({"is_archived":true})).unwrap())).await.unwrap_err();
        assert_eq!(denied.status(), StatusCode::NOT_FOUND);
        for id in 1..=3 {
            let _ = update_conversation(Session::for_storage("owner"), Path(id),
                Json(serde_json::from_value(json!({"is_archived":true})).unwrap())).await.unwrap();
        }
        let normal = page("owner", json!({"selected_id":2})).await;
        assert_eq!(normal.conversations.iter().map(|row| row.id).collect::<Vec<_>>(), vec![4]);
        assert!(normal.selected_conversation.unwrap().is_archived);
        assert!(assistant_store::get_conversation(2, "owner").await.unwrap().is_none());
        let record = assistant_store::get_conversation_for_read(2, "owner").await.unwrap().unwrap();
        assert!(record.is_pinned);
        assert_eq!(record.last_message_at.timestamp_micros(), at);
        assert!(assistant_store::set_conversation_archived(2, "owner", true).await.unwrap());
        assert_eq!(assistant_store::get_conversation_for_read(2, "owner").await.unwrap().unwrap().updated_at, record.updated_at);
        let archived = page("owner", json!({"archived":true,"limit":2})).await;
        assert_eq!(archived.conversations.iter().map(|row| row.id).collect::<Vec<_>>(), vec![2,3]);
        let mut params = serde_json::to_value(archived.next_cursor.unwrap()).unwrap();
        params["archived"] = json!(true);
        assert_eq!(page("owner", params).await.conversations[0].id, 1);
        assert!(page("other", json!({"archived":true})).await.conversations.is_empty());
        let Json(history) = get_conversation_messages(Session::for_storage("owner"), Path(2),
            Query(serde_json::from_value(json!({})).unwrap())).await.unwrap();
        assert!(!history.messages.is_empty());
        let _ = update_conversation(Session::for_storage("owner"), Path(2),
            Json(serde_json::from_value(json!({"is_archived":false})).unwrap())).await.unwrap();
        assert_eq!(page("owner", json!({})).await.conversations[0].id, 2);
        assert!(assistant_store::get_conversation(2, "owner").await.unwrap().is_some());
        for status in ["queued", "running"] {
            let job = uuid::Uuid::new_v4();
            sqlx::query("INSERT INTO assistant_generation_jobs(id,conversation_id,user_id,status) VALUES (?1,4,'owner',?2)")
                .bind(job).bind(status).execute(&pool).await.unwrap();
            let error = update_conversation(Session::for_storage("owner"), Path(4),
                Json(serde_json::from_value(json!({"is_archived":true})).unwrap())).await.unwrap_err();
            assert_eq!(error.status(), StatusCode::CONFLICT);
            assert!(!assistant_store::get_conversation_for_read(4, "owner").await.unwrap().unwrap().is_archived);
            sqlx::query("DELETE FROM assistant_generation_jobs WHERE id=?1").bind(job).execute(&pool).await.unwrap();
        }
        assert!(assistant_store::set_conversation_archived(4, "owner", true).await.unwrap());
    }

    #[tokio::test]
    async fn db_tests_pinned_conversations_paginate_search_and_enforce_ownership() {
        let pool = crate::db::test_pool().await;
        crate::db::set_test_pool(pool.clone());
        let at = chrono::Utc::now().timestamp_micros();
        sqlx::query(
            "WITH RECURSIVE seq(n) AS (SELECT 1 UNION ALL SELECT n+1 FROM seq WHERE n < 45)
             INSERT INTO assistant_conversations (id, user_id, system_prompt, title, message_count, last_message_at, is_pinned)
             SELECT n, 'owner', '', '工作 ' || n, 1, ?1 + n, n <= 25 FROM seq",
        ).bind(at).execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO assistant_conversations (user_id, system_prompt, message_count, is_pinned)
                     VALUES ('other', '', 1, TRUE), ('owner', '', 0, TRUE)")
            .execute(&pool).await.unwrap();
        let first = page("owner", json!({"limit": 8})).await;
        assert_eq!(first.conversations[0].id, 25);
        assert!(first.conversations.iter().all(|row| row.is_pinned));
        let first_cursor = first.next_cursor.unwrap();
        assert!(first_cursor.before_pinned);
        let mut params = json!({"limit": 8});
        let mut ids = Vec::new();
        loop {
            let result = page("owner", params).await;
            ids.extend(result.conversations.into_iter().map(|row| row.id));
            let Some(cursor) = result.next_cursor else { break };
            params = serde_json::to_value(cursor).unwrap();
            params["limit"] = json!(8);
        }
        let expected: Vec<i64> = (1..=25).rev().chain((26..=45).rev()).collect();
        assert_eq!(ids, expected);
        let found = page("owner", json!({"search": "工作 4"})).await;
        assert_eq!(found.conversations[0].id, 4);
        assert_eq!(found.conversations[1].id, 45);
        sqlx::query("DELETE FROM assistant_conversations WHERE id = ?1")
            .bind(first_cursor.before_id).execute(&pool).await.unwrap();
        let next = page("owner", serde_json::to_value(first_cursor).unwrap()).await;
        assert_eq!(next.conversations[0].id, 17);

        let denied = update_conversation(Session::for_storage("other"), Path(45),
            Json(serde_json::from_value(json!({"is_pinned":true})).unwrap())).await.unwrap_err();
        assert_eq!(denied.status(), StatusCode::NOT_FOUND);
        assert!(!assistant_store::get_conversation_for_read(45, "owner").await.unwrap().unwrap().is_pinned);
        sqlx::query("UPDATE assistant_conversations SET updated_at = 123 WHERE id = 45")
            .execute(&pool).await.unwrap();
        let _ = update_conversation(Session::for_storage("owner"), Path(45),
            Json(serde_json::from_value(json!({"is_pinned":true})).unwrap())).await.unwrap();
        assert_eq!(page("owner", json!({})).await.conversations[0].id, 45);
        let pinned = assistant_store::get_conversation_for_read(45, "owner").await.unwrap().unwrap();
        assert!(pinned.is_pinned);
        assert!(pinned.updated_at.timestamp_micros() > 123);
        assert_eq!(pinned.last_message_at.timestamp_micros(), at + 45);
        assert!(assistant_store::set_conversation_pinned(45, "owner", true).await.unwrap());
        assert_eq!(assistant_store::get_conversation_for_read(45, "owner").await.unwrap().unwrap().updated_at, pinned.updated_at);
        let _ = update_conversation(Session::for_storage("owner"), Path(45),
            Json(serde_json::from_value(json!({"is_pinned":false,"title":"重新命名"})).unwrap())).await.unwrap();
        assert_eq!(page("owner", json!({})).await.conversations[0].id, 25);
        let found = page("owner", json!({"search":"重新命名"})).await;
        assert_eq!(found.conversations.len(), 1);
        assert!(!found.conversations[0].is_pinned);
        let missing = update_conversation(Session::for_storage("owner"), Path(i64::MAX),
            Json(serde_json::from_value(json!({"is_pinned":true})).unwrap())).await.unwrap_err();
        assert_eq!(missing.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn db_tests_workspace_list_cursor_and_selected_record_lifecycle() {
        let pool = crate::db::test_pool().await;
        crate::db::set_test_pool(pool.clone());
        let at = chrono::Utc::now().timestamp_micros();
        sqlx::query(
            "INSERT INTO assistant_conversations (id, user_id, agent, system_prompt, message_count, last_message_at)
             VALUES (1, 'owner', 'codex', '', 1, ?1), (2, 'owner', 'codex', '', 1, ?1), (3, 'owner', 'codex', '', 1, ?1)",
        ).bind(at).execute(&pool).await.unwrap();
        let cursor = page("owner", json!({"limit": 2})).await.next_cursor.unwrap();
        assert_eq!(cursor.before_id, 2);
        sqlx::query("DELETE FROM assistant_conversations WHERE id = 2").execute(&pool).await.unwrap();
        let next = page("owner", serde_json::to_value(cursor).unwrap()).await;
        assert_eq!(next.conversations[0].id, 1);
        assert!(next.next_cursor.is_none());
        let selected = page("owner", json!({"selected_id": 1})).await;
        assert!(selected.conversations.iter().any(|row| row.id == 1));
        assert_eq!(selected.selected_conversation.unwrap().id, 1);
        sqlx::query("DELETE FROM assistant_conversations WHERE id = 1").execute(&pool).await.unwrap();
        assert!(page("owner", json!({"selected_id": 1})).await.selected_conversation.is_none());
        let invalid = list_conversations(
            Session::for_storage("owner"), Query(serde_json::from_value(json!({"before_id": 3})).unwrap()),
        ).await.unwrap_err();
        assert_eq!(invalid.status(), StatusCode::BAD_REQUEST);
    }
}
