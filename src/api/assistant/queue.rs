use super::*;
use super::goal_judge;
use futures_util::FutureExt;
use tracing::Instrument;
use sha2::{Digest, Sha256};
use std::{
    panic::AssertUnwindSafe,
    sync::{
        atomic::{AtomicBool, Ordering},
        Mutex, OnceLock,
    },
};
use tokio::sync::{Notify, OwnedSemaphorePermit, Semaphore};

struct QueueScheduler {
    global: Arc<Semaphore>,
    conversations: Mutex<HashSet<i64>>,
    wake: Notify,
    started: AtomicBool,
}

impl QueueScheduler {
    fn new(global: usize) -> Self {
        Self {
            global: Arc::new(Semaphore::new(global.clamp(1, Semaphore::MAX_PERMITS))),
            conversations: Mutex::new(HashSet::new()),
            wake: Notify::new(),
            started: AtomicBool::new(false),
        }
    }

    async fn acquire(&self) -> OwnedSemaphorePermit {
        self.global.clone().acquire_owned().await.expect("scheduler semaphore remains open")
    }

    fn schedule(&'static self, conversation_id: i64, user_id: String) {
        if !self.conversations.lock().unwrap_or_else(|e| e.into_inner()).insert(conversation_id) {
            return;
        }
        tokio::spawn(async move {
            struct Reservation(&'static QueueScheduler, i64);
            impl Drop for Reservation {
                fn drop(&mut self) {
                    self.0.conversations.lock().unwrap_or_else(|e| e.into_inner()).remove(&self.1);
                }
            }
            let _reservation = Reservation(self, conversation_id);
            run_queue_worker(conversation_id, user_id).await;
        });
    }

    fn start(&'static self) {
        if self.started.swap(true, Ordering::SeqCst) {
            return;
        }
        tokio::spawn(async move {
            loop {
                match assistant_store::list_queued_generation_conversations().await {
                    Ok(conversations) => {
                        for (id, user) in conversations {
                            self.schedule(id, user);
                        }
                    }
                    Err(err) => warn!("failed to reconcile assistant queue: {err:#}"),
                }
                tokio::select! {
                    _ = self.wake.notified() => {},
                    _ = tokio::time::sleep(Duration::from_secs(2)) => {},
                }
            }
        });
    }
}

fn queue_scheduler() -> &'static QueueScheduler {
    static SCHEDULER: OnceLock<QueueScheduler> = OnceLock::new();
    SCHEDULER.get_or_init(|| {
        let cfg = config::get();
        QueueScheduler::new(cfg.assistant_max_concurrent_jobs)
    })
}

fn submission_key(headers: &HeaderMap) -> Result<Option<uuid::Uuid>, AppError> {
    headers
        .get("idempotency-key")
        .map(|value| {
            value
                .to_str()
                .ok()
                .and_then(|value| uuid::Uuid::parse_str(value).ok())
                .ok_or_else(|| AppError::bad_request("Idempotency-Key 必须是 UUID"))
        })
        .transpose()
}

fn submission_fingerprint(content: &str, enable_tools: bool, attachments: &[PendingAttachment]) -> String {
    let files: Vec<_> =
        attachments.iter().map(|file| (&file.file_name, &file.content_type, file.file_size, &file.sha256)).collect();
    let payload = serde_json::to_vec(&(content, enable_tools, files)).expect("serializable submission");
    format!("{:x}", Sha256::digest(payload))
}

pub async fn chat_stream(
    auth_user: Session, request: axum::http::Request<Body>,
) -> Result<Sse<ReceiverStream<Result<Event, Infallible>>>, AppError> {
    let enqueued = enqueue_turn(auth_user.storage_scope(), None, request).await?;
    let job_id = enqueued.job_id;
    let conversation_id_for_event = enqueued.message.conversation_id;
    let (initial_tx, initial_rx) = mpsc::channel(64);
    let job = assistant_job::global_registry().get(job_id);

    tokio::spawn(async move {
        let _ = initial_tx
            .send(Ok(Event::default().event("job_started").data(
                serde_json::to_string(&json!({
                    "job_id": job_id,
                    "conversation_id": conversation_id_for_event,
                    "execution_id": job.as_ref().map(|job| job.execution_id),
                    "queued": job.is_none(),
                }))
                .unwrap_or_else(|_| "{}".to_string()),
            )))
            .await;
        if let Some(job) = job {
            let mut stream_rx = assistant_job::stream_for_job(job, 0);
            while let Some(event) = stream_rx.recv().await {
                if initial_tx.send(event).await.is_err() {
                    break;
                }
            }
        } else {
            let _ = initial_tx.send(Ok(Event::default().event("message_queued").data("{\"queued\":true}"))).await;
        }
    });

    Ok(Sse::new(ReceiverStream::new(initial_rx))
        .keep_alive(KeepAlive::new().interval(Duration::from_secs(15)).text("keep-alive")))
}

pub async fn enqueue_conversation_message(
    auth_user: Session, Path(conversation_id): Path<i64>, request: axum::http::Request<Body>,
) -> Result<Json<EnqueueMessageResponse>, AppError> {
    let enqueued = enqueue_turn(auth_user.storage_scope(), Some(conversation_id), request).await?;
    Ok(Json(enqueued))
}

async fn parse_json_chat_request(request: axum::http::Request<Body>) -> Result<ChatRequest, AppError> {
    let (_, body) = request.into_parts();
    let mut stream = body.into_data_stream();
    let mut bytes = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|err| AppError::bad_request(err.to_string()))?;
        if bytes.len().saturating_add(chunk.len()) > MAX_CHAT_JSON_BYTES {
            return Err(AppError::new(StatusCode::PAYLOAD_TOO_LARGE, "消息请求过大，最多支持 100000 个字符"));
        }
        bytes.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&bytes).map_err(|err| AppError::bad_request(err.to_string()))
}

async fn enqueue_turn(
    user_id: &str, conversation_id_override: Option<i64>, request: axum::http::Request<Body>,
) -> Result<EnqueueMessageResponse, AppError> {
    let request_id = submission_key(request.headers())?;
    let content_type = request.headers().get(header::CONTENT_TYPE).and_then(|value| value.to_str().ok()).unwrap_or("");
    let parsed = if is_multipart_content_type(content_type) {
        ensure_payload_size_limit(request.headers())?;
        let mut multipart =
            Multipart::from_request(request, &()).await.map_err(|err| AppError::bad_request(err.to_string()))?;
        parse_chat_multipart(&mut multipart).await?
    } else {
        let body = parse_json_chat_request(request).await?;
        if body.message.chars().count() > MAX_CHAT_MESSAGE_CHARS {
            return Err(AppError::new(StatusCode::PAYLOAD_TOO_LARGE, "消息内容过大，最多支持 100000 个字符"));
        }
        ParsedChatPayload {
            message: body.message,
            enable_tools: body.enable_tools,
            conversation_id: body.conversation_id,
            goal_id: body.goal_id,
            attachments: Vec::new(),
        }
    };
    let goal_id = parsed.goal_id;
    let mut attachment_guard = AttachmentCleanup::new(parsed.attachments);
    let conversation_id =
        conversation_id_override.or(parsed.conversation_id).ok_or(AppError::bad_request("Missing conversation_id"))?;
    assistant_store::get_conversation(conversation_id, user_id)
        .await
        .map_err(AppError::internal_server_error)?
        .ok_or(AppError::not_found("Conversation not found"))?;
    if let Some(goal_id) = goal_id {
        let goal = assistant_store::get_goal_by_id(goal_id, conversation_id, user_id)
            .await
            .map_err(AppError::internal_server_error)?
            .ok_or(AppError::conflict("Goal 不存在或不属于当前会话"))?;
        if matches!(goal.status.as_str(), "completed" | "cancelled") {
            return Err(AppError::conflict("Goal 已经结束，请重新开启新的 Goal"));
        }
    }

    let content = parsed.message.trim().to_string();
    if content.is_empty() && attachment_guard.entries().is_empty() {
        return Err(AppError::bad_request("消息或附件不能为空"));
    }

    let enable_tools = parsed.enable_tools.unwrap_or(config::get().assistant_enable_tools);
    let fingerprint = submission_fingerprint(&content, enable_tools, attachment_guard.entries());
    let new_attachments = attachment_guard
        .entries()
        .iter()
        .cloned()
        .map(|entry| MessageAttachment::from(entry).as_new_attachment())
        .collect::<Vec<_>>();
    let goal_metadata = goal_id.map(|goal_id| json!({
        "goal": { "goal_id": goal_id, "origin": "goal_start" }
    }));
    let submission = assistant_store::submit_queued_user_turn_with_metadata(
        conversation_id,
        user_id,
        request_id,
        &fingerprint,
        enable_tools,
        content,
        &new_attachments,
        goal_metadata,
    )
    .await
    .map_err(|err| match err.downcast_ref::<assistant_store::SubmissionConflict>() {
        Some(assistant_store::SubmissionConflict::QueueFull) => AppError::too_many_requests(err, 5),
        Some(conflict) => AppError::conflict(conflict),
        None => AppError::from(err),
    })?;
    if !submission.replayed {
        attachment_guard.disarm();
    }
    // 排队提交在存储层一定会带上 generation job，但这里不能 panic：请求路径上的
    // 断言一旦被触发就是整个进程下线。缺 job 时按内部错误返回，交给上层记录。
    let job_id = submission
        .message
        .generation_job_id
        .ok_or_else(|| AppError::internal_server_error("queued submission has no generation job"))?;

    kick_conversation_queue(conversation_id, user_id).await.map_err(AppError::internal_server_error)?;
    let job = assistant_store::get_generation_job(job_id, user_id)
        .await
        .map_err(AppError::internal_server_error)?
        .ok_or_else(|| AppError::internal_server_error("generation job disappeared"))?;
    let mut message = to_message_response(submission.message, submission.attachments);
    message.processing_status = Some(job.status.clone());
    message.processing_error = job.error_message;
    Ok(EnqueueMessageResponse { job_id, status: job.status, message })
}

pub(super) async fn kick_conversation_queue(conversation_id: i64, user_id: &str) -> anyhow::Result<()> {
    let scheduler = queue_scheduler();
    scheduler.start();
    scheduler.schedule(conversation_id, user_id.into());
    scheduler.wake.notify_one();
    Ok(())
}

async fn run_queue_worker(conversation_id: i64, user_id: String) {
    loop {
        let permits = queue_scheduler().acquire().await;
        let record = match assistant_store::claim_next_queued_generation(conversation_id, &user_id).await {
            Ok(Some(record)) => record,
            Ok(None) => return,
            Err(err) => {
                drop(permits);
                warn!(conversation_id, "failed to claim assistant turn; retrying: {err:#}");
                tokio::time::sleep(Duration::from_secs(2)).await;
                continue;
            }
        };
        let job = assistant_job::register_claimed_job(&record);
        match assistant_store::generation_requires_snapshot(record.id).await {
            Ok(required) => job.snapshot_required.store(required, Ordering::SeqCst),
            Err(err) => {
                warn!(job_id = %record.id, "failed to check replay history; using snapshots: {err:#}")
            }
        }
        let result = AssertUnwindSafe(run_queued_generation(&record, job.clone())
            .instrument(tracing::info_span!("assistant_generation", job_id = %job.id)))
            .catch_unwind()
            .await
            .unwrap_or_else(|_| Err(anyhow!("assistant worker panicked")));
        if job.cancellation_token().is_cancelled() {
            job.cancel().await;
        } else {
            match result {
                Ok(()) => {
                    job.complete().await;
                    if let Err(err) = maybe_continue_goal(&record, &job).await {
                        warn!(job_id = %job.id, "goal completion check failed: {err:#}");
                    }
                }
                Err(err) => {
                    // 完整错误链只进服务端日志；推给前端的仅保留外层消息并截断，
                    // 避免 SQL 细节、文件路径等内部信息通过 SSE 泄露。
                    warn!("assistant generation failed (job {}): {:#}", job.id, err);
                    job.fail(safe_generation_error(&err)).await;
                }
            }
        }
        assistant_job::cleanup_job(job.id, job.execution_id);
        drop(permits);
    }
}

async fn maybe_continue_goal(
    record: &assistant_store::GenerationJobRecord,
    job: &Arc<assistant_job::GenerationJob>,
) -> anyhow::Result<()> {
    let Some(user_message) = assistant_store::fetch_generation_user_message(job.id, record.conversation_id, &record.user_id).await? else { return Ok(()) };
    let Some(goal_id) = user_message.metadata.as_ref()
        .and_then(|metadata| metadata.get("goal"))
        .and_then(|goal| goal.get("goal_id"))
        .and_then(serde_json::Value::as_i64) else { return Ok(()) };
    let Some(goal) = assistant_store::get_goal_by_id(goal_id, record.conversation_id, &record.user_id).await? else { return Ok(()) };
    if !matches!(goal.status.as_str(), "running" | "checking" | "continuing") || !goal.auto_continue { return Ok(()) }
    if chrono::Utc::now().signed_duration_since(goal.created_at).num_seconds() >= goal.max_runtime_secs {
        let _ = assistant_store::set_goal_status(record.conversation_id, goal_id, &record.user_id, "limit_reached").await?;
        return Ok(())
    }
    let config = match AssistantConfig::shared().await {
        Ok(config) => config,
        Err(err) => {
            let failure = serde_json::json!({"status":"failed","summary":"无法准备 Goal 检查模型"});
            assistant_store::apply_goal_check(goal_id, record.conversation_id, &record.user_id, "failed", &failure, None).await?;
            return Err(anyhow!("assistant config error: {err:?}"));
        }
    };
    let _ = assistant_store::set_goal_status(record.conversation_id, goal_id, &record.user_id, "checking").await?;
    let judge = match goal_judge::check(config.as_ref(), &goal, job.id, &record.user_id).await {
        Ok(result) => result,
        Err(err) => {
            let failure = serde_json::json!({"status":"failed","summary":err.to_string()});
            assistant_store::apply_goal_check(goal_id, record.conversation_id, &record.user_id, "failed", &failure, None).await?;
            return Err(err);
        }
    };
    let result = goal_judge::as_value(&judge);
    if !judge.stages.is_empty() {
        let active_index = judge.current_stage_id.as_ref()
            .and_then(|id| judge.stages.iter().position(|item| item.id.as_ref() == Some(id)))
            .or_else(|| judge.stages.iter().position(|item| item.status.as_deref() == Some("active")))
            .or_else(|| judge.stages.iter().position(|item| item.status.as_deref() != Some("completed")))
            .unwrap_or(0);
        let stages = judge.stages.iter().enumerate().map(|(index, stage)| (
            stage.id.clone().unwrap_or_default(), stage.title.clone(), stage.summary.clone(),
            stage.status.clone().unwrap_or_else(|| if index < active_index { "completed".into() } else if index == active_index { "active".into() } else { "pending".into() }),
        )).collect::<Vec<_>>();
        assistant_store::replace_goal_stages(goal_id, record.conversation_id, &record.user_id, &stages, judge.current_stage_id.as_deref()).await?;
    }
    let mut final_status = match judge.status.as_str() {
        "completed" => "completed",
        "continue" => "continuing",
        _ => "needs_review",
    };
    let updated = assistant_store::apply_goal_check(
        goal_id, record.conversation_id, &record.user_id, final_status, &result, judge.current_stage_id.as_deref(),
    ).await?;
    let Some(updated) = updated else { return Ok(()) };
    if final_status == "continuing" && updated.turn_count >= updated.max_turns {
        final_status = "limit_reached";
        let _ = assistant_store::set_goal_status(record.conversation_id, goal_id, &record.user_id, final_status).await?;
        return Ok(());
    }
    if final_status != "continuing" || !updated.auto_continue { return Ok(()) }
    let continuation = judge.next_prompt.as_deref().unwrap_or("请继续完成原始目标，先检查仍未完成的工作，再开始下一阶段。").trim().to_string();
    let fingerprint = format!("{:x}", Sha256::digest(serde_json::to_vec(&(goal_id, updated.turn_count, &continuation))?));
    let metadata = serde_json::json!({"goal": {"goal_id": goal_id, "origin": "goal_continue", "turn_index": updated.turn_count + 1}});
    let submission = assistant_store::submit_queued_user_turn_with_metadata(
        record.conversation_id, &record.user_id, None, &fingerprint, record.enable_tools, continuation, &[], Some(metadata),
    ).await?;
    assistant_store::set_goal_status(record.conversation_id, goal_id, &record.user_id, "running").await?;
    kick_conversation_queue(record.conversation_id, &record.user_id).await?;
    tracing::info!(goal_id, job_id = %submission.message.generation_job_id.unwrap_or_default(), "queued Goal continuation");
    Ok(())
}

async fn run_queued_generation(
    record: &assistant_store::GenerationJobRecord, job: Arc<assistant_job::GenerationJob>,
) -> anyhow::Result<()> {
    let mut conversation_record = assistant_store::get_conversation(record.conversation_id, &record.user_id)
        .await?
        .ok_or_else(|| anyhow!("Conversation not found"))?;
    if conversation_record.agent == "codex" {
        if let Some(settings) = &record.codex_settings {
            settings.apply_to(&mut conversation_record);
        }
    }
    if conversation_record.agent == "claude" {
        if let Some(settings) = &record.claude_settings {
            settings.apply_to(&mut conversation_record);
        }
    }
    let user_record = assistant_store::fetch_generation_user_message(job.id, record.conversation_id, &record.user_id)
        .await?
        .ok_or_else(|| anyhow!("Queued user message not found"))?;
    // A cancellation can arrive after the database claim but before this
    // execution is registered. Consume its durable intent before any work.
    if user_record
        .metadata
        .as_ref()
        .and_then(|metadata| metadata.get("cancellation_requested"))
        .and_then(Value::as_bool)
        == Some(true)
    {
        job.cancellation_token().cancel();
    }
    if job.cancellation_token().is_cancelled() {
        return Ok(());
    }
    let attachments =
        assistant_store::fetch_attachments_for_messages(record.conversation_id, &record.user_id, &[user_record.id])
            .await?;
    let user_message_id = user_record.id;
    let user_message = message_from_record(user_record, attachments)?;

    // Claude emits a native session title event. Codex app-server exposes a
    // name field for explicit renames, but does not auto-promote its internal
    // preview into that user-facing name, so keep the fallback there.
    if conversation_record.agent != "claude" {
        spawn_conversation_title(&conversation_record, user_message_id, &user_message, job.clone());
    }

    crate::assistant_workspace::ensure_ready(&conversation_record, &job).await?;
    if job.cancellation_token().is_cancelled() {
        return Ok(());
    }

    if conversation_record.agent == "codex" {
        run_codex_turn(&conversation_record, &record.user_id, record.conversation_id, &user_message, job).await?;
        return Ok(());
    }
    if conversation_record.agent == "claude" {
        run_claude_turn(&conversation_record, &record.user_id, record.conversation_id, &user_message, job).await?;
        return Ok(());
    }

    let config = AssistantConfig::shared().await.map_err(|err| anyhow!("assistant config error: {err:?}"))?;
    let mut conversation = load_generation_conversation(
        record.conversation_id,
        &record.user_id,
        record.id,
    )
    .await?;
    let tool_context = mcp::ToolContext { user_id: record.user_id.clone() };
    run_conversation(
        config.as_ref(),
        &mut conversation,
        record.conversation_id,
        record.enable_tools,
        job,
        tool_context,
    )
    .await?;
    Ok(())
}

pub async fn resume_queued_generations() -> anyhow::Result<()> {
    for (conversation_id, user_id) in assistant_store::list_queued_generation_conversations().await? {
        kick_conversation_queue(conversation_id, &user_id).await?;
    }
    Ok(())
}

pub async fn get_active_generation(
    auth_user: Session, Path(conversation_id): Path<i64>,
) -> Result<Json<Value>, AppError> {
    let user_id = auth_user.storage_scope().to_owned();

    // 校验对话归属
    let _ = assistant_store::get_conversation_for_read(conversation_id, &user_id)
        .await
        .map_err(AppError::internal_server_error)?
        .ok_or(AppError::not_found("Conversation not found"))?;

    // 优先查内存中的 running 任务
    if let Some(job) = assistant_job::global_registry().find_active_by_conversation(conversation_id) {
        if job.user_id == user_id {
            let last_seq = job.seq_counter.load(std::sync::atomic::Ordering::SeqCst);
            return Ok(Json(json!({
                "active": true,
                "job_id": job.id,
                "status": job.status_name(),
                "last_event_seq": last_seq,
                "execution_id": job.execution_id,
            })));
        }
    }

    // 内存没有则查 DB（running）
    if let Some(record) = assistant_store::find_active_generation(conversation_id, &user_id)
        .await
        .map_err(AppError::internal_server_error)?
    {
        return Ok(Json(json!({
            "active": true,
            "job_id": record.id,
            "status": record.status,
            "last_event_seq": 0,
            "execution_id": null,
        })));
    }

    Ok(Json(json!({ "active": false })))
}

pub async fn resume_job_stream(
    auth_user: Session, Path(job_id): Path<uuid::Uuid>,
    axum::extract::Query(params): axum::extract::Query<HashMap<String, String>>,
) -> Result<Sse<ReceiverStream<Result<Event, Infallible>>>, AppError> {
    let user_id = auth_user.storage_scope().to_owned();

    let start_seq = params.get("after_seq").and_then(|v| v.parse::<u64>().ok()).unwrap_or(0);
    let execution_id = params.get("execution_id").and_then(|id| uuid::Uuid::parse_str(id).ok());
    let stream_rx = if let Some(job) = assistant_job::global_registry().get(job_id) {
        if job.user_id != user_id {
            return Err(AppError::not_found("生成任务不存在或无权访问"));
        }
        assistant_job::stream_for_job_from(job, start_seq, execution_id)
    } else {
        let record = assistant_store::get_generation_job(job_id, &user_id)
            .await
            .map_err(AppError::internal_server_error)?
            .ok_or_else(|| AppError::not_found("生成任务不存在或无权访问"))?;
        let (tx, rx) = mpsc::channel(1);
        let _ = tx
            .send(Ok(assistant_job::stream_reset_event(job_id, record.conversation_id, None, "job_not_in_memory")))
            .await;
        rx
    };
    Ok(Sse::new(ReceiverStream::new(stream_rx))
        .keep_alive(KeepAlive::new().interval(Duration::from_secs(15)).text("keep-alive")))
}

pub async fn cancel_job(auth_user: Session, Path(job_id): Path<uuid::Uuid>) -> Result<Json<Value>, AppError> {
    let user_id = auth_user.storage_scope().to_owned();
    let mut job = assistant_job::global_registry().get(job_id).filter(|job| !job.is_done());
    if job.is_none() {
        if let Some((conversation_id, paths)) = assistant_store::delete_queued_generation(job_id, &user_id)
            .await
            .map_err(AppError::internal_server_error)?
        {
            for path in paths {
                if let Err(err) = fs::remove_file(&path).await {
                    if err.kind() != std::io::ErrorKind::NotFound {
                        warn!("failed to remove queued assistant attachment {}: {}", path, err);
                    }
                }
            }
            kick_conversation_queue(conversation_id, &user_id).await.map_err(AppError::internal_server_error)?;
            return Ok(Json(json!({ "ok": true, "job_id": job_id, "queued": true })));
        }
        // The worker may have claimed the queued turn during the deletion
        // transaction. Recheck memory rather than report a spurious 404.
        job = assistant_job::global_registry().get(job_id).filter(|job| !job.is_done());
        if job.is_none() {
            let record = assistant_store::get_generation_job(job_id, &user_id)
                .await
                .map_err(AppError::internal_server_error)?
                .ok_or_else(|| AppError::not_found("生成任务不存在或无权访问"))?;
            if matches!(record.status.as_str(), "completed" | "failed") {
                return Ok(Json(json!({ "ok": true, "job_id": job_id, "status": record.status })));
            }
            if let Err(err) = assistant_store::request_generation_cancellation(job_id, &user_id).await {
                // Completion can also win the race before cancellation commits.
                let latest = assistant_store::get_generation_job(job_id, &user_id)
                    .await
                    .map_err(AppError::internal_server_error)?;
                if let Some(latest) = latest.filter(|record| matches!(record.status.as_str(), "completed" | "failed")) {
                    return Ok(Json(json!({ "ok": true, "job_id": job_id, "status": latest.status })));
                }
                return Err(AppError::internal_server_error(err));
            }
            job = assistant_job::global_registry().get(job_id);
            if job.is_none() {
                crate::assistant_events::notify(Some(&user_id), "conversations");
                return Ok(Json(json!({ "ok": true, "job_id": job_id, "status": "cancelling" })));
            }
        }
    }
    let job = job.expect("registered cancellation path has a job");

    if job.user_id != user_id {
        return Err(AppError::not_found("生成任务不存在或无权访问"));
    }

    job.request_cancellation().await.map_err(AppError::internal_server_error)?;
    crate::assistant_events::notify(Some(&user_id), "conversations");
    Ok(Json(json!({ "ok": true, "job_id": job_id, "status": job.status_name() })))
}

async fn load_generation_conversation(
    conversation_id: i64, user_id: &str, current_job_id: uuid::Uuid,
) -> anyhow::Result<Conversation> {
    let mut conversation = Conversation::new();
    conversation.messages = firefly::inherited_context(conversation_id, user_id).await?;
    let persisted_summary = assistant_store::fetch_generation_context_summary(conversation_id, user_id).await?;
    let mut history = assistant_store::fetch_generation_context_messages_from(
        conversation_id,
        user_id,
        current_job_id,
        persisted_summary.as_ref().map(|(message_id, _)| *message_id),
    ).await?;
    if let Some((_, summary)) = persisted_summary {
        conversation.messages = vec![Message::assistant(summary, None)];
    } else if let Some(summary) = super::turn_context::restore(&mut history) {
        conversation.messages = vec![summary];
    }
    let message_ids = history.iter().map(|message| message.id).collect::<Vec<_>>();
    let attachment_rows =
        assistant_store::fetch_attachments_for_messages(conversation_id, user_id, &message_ids).await?;
    let mut grouped: HashMap<i64, Vec<assistant_store::AttachmentRecord>> = HashMap::new();
    for attachment in attachment_rows {
        grouped.entry(attachment.message_id).or_default().push(attachment);
    }
    for record in history {
        let attachments = grouped.remove(&record.id).unwrap_or_default();
        conversation.add_message(message_from_record(record, attachments)?);
    }
    Ok(conversation)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct PlainReplyModel {
        requests: std::sync::Mutex<Vec<g::ModelRequest>>,
    }

    #[async_trait]
    impl Model for PlainReplyModel {
        async fn generate(&self, request: g::ModelRequest) -> Result<g::ModelResponse, g::ModelError> {
            assert!(!request.messages[0].text_content().starts_with("你负责压缩任务上下文"));
            self.requests.lock().unwrap().push(request);
            Ok(g::ModelResponse::new(GMessage::assistant("plain reply")))
        }
    }

    #[tokio::test]
    async fn completed_turns_do_not_enqueue_summaries_and_next_turn_keeps_original_history() {
        crate::config::init_test_config();
        let pool = crate::db::test_pool().await;
        crate::db::set_test_pool(pool.clone());
        let conversation_id: i64 = sqlx::query_scalar("INSERT INTO assistant_conversations(user_id,system_prompt,agent) VALUES ('owner','','firefly') RETURNING id")
            .fetch_one(&pool).await.unwrap();
        let model = Arc::new(PlainReplyModel { requests: Default::default() });
        let config = AssistantConfig {
            model: model.clone(),
            budget: llm::ModelBudget::fixed(100000),
            system_prompt: "plain assistant instruction".into(),
            agent_settings: None,
        };
        for question in ["first question", "follow-up question"] {
            let id = uuid::Uuid::new_v4();
            assistant_store::create_queued_user_turn(id, conversation_id, "owner", false, question.into(), &[]).await.unwrap();
            assistant_store::claim_next_queued_generation(conversation_id, "owner").await.unwrap().unwrap();
            let mut conversation = load_generation_conversation(conversation_id, "owner", id).await.unwrap();
            let job = Arc::new(assistant_job::GenerationJob::new(id, conversation_id, "owner".into()));
            run_conversation(&config, &mut conversation, conversation_id, false, job.clone(),
                mcp::ToolContext { user_id: "owner".into() }).await.unwrap();
            job.complete().await;
            assert!(job.is_done());
            assert_eq!(assistant_store::get_generation_job(id, "owner").await.unwrap().unwrap().status, "completed");
            let retired_tables: i64 = sqlx::query_scalar("SELECT count(*) FROM sqlite_master WHERE type='table' AND name='assistant_postprocess_jobs'")
                .fetch_one(&pool).await.unwrap();
            assert_eq!(retired_tables, 0);
            let summaries: i64 = sqlx::query_scalar("SELECT count(*) FROM assistant_messages WHERE json_type(metadata,'$.context_summary') IS NOT NULL")
                .fetch_one(&pool).await.unwrap();
            assert_eq!(summaries, 0);
        }
        let requests = model.requests.lock().unwrap();
        assert_eq!(requests.len(), 2, "one model request per short turn, without completion summaries");
        let history: Vec<_> = requests[1].messages.iter().map(GMessage::text_content).collect();
        assert_eq!(history, ["plain assistant instruction", "first question", "plain reply", "follow-up question"]);
    }


    #[tokio::test]
    async fn db_tests_firefly_continuation_retains_branch_context_and_excludes_future_turns() {
        let pool = crate::db::test_pool().await;
        crate::db::set_test_pool(pool.clone());
        let conversation: i64 = sqlx::query_scalar(
            "INSERT INTO assistant_conversations(user_id, agent, system_prompt) VALUES ('owner', 'firefly', 'system') RETURNING id",
        ).fetch_one(&pool).await.unwrap();
        let inherited = vec![Message {
            role: Role::User, content: "inherited question".into(), tool_calls: None,
            tool_call_id: None, is_error: false, attachments: vec![],
        }, Message {
            role: Role::Assistant, content: "inherited answer".into(), tool_calls: None,
            tool_call_id: None, is_error: false, attachments: vec![],
        }];
        sqlx::query("INSERT INTO assistant_firefly_branches (conversation_id, user_id, request_id, context) VALUES (?1, 'owner', ?2, ?3)")
            .bind(conversation).bind(uuid::Uuid::new_v4()).bind(serde_json::to_value(inherited).unwrap())
            .execute(&pool).await.unwrap();
        let first = uuid::Uuid::from_u128(3);
        assistant_store::create_queued_user_turn(first, conversation, "owner", true, "first question".into(), &[]).await.unwrap();
        assistant_store::claim_next_queued_generation(conversation, "owner").await.unwrap().unwrap();
        assistant_store::append_message(conversation, assistant_store::StoredMessage {
            role: "assistant".into(), content: "first answer".into(), tool_call_id: None,
            tool_calls: None, metadata: None, is_error: false, generation_job_id: Some(first),
        }).await.unwrap();
        sqlx::query("UPDATE assistant_generation_jobs SET status='completed' WHERE id=?1")
            .bind(first).execute(&pool).await.unwrap();
        let second = uuid::Uuid::from_u128(2);
        assistant_store::create_queued_user_turn(second, conversation, "owner", true, "follow-up".into(), &[]).await.unwrap();
        let future = uuid::Uuid::from_u128(1);
        assistant_store::create_queued_user_turn(future, conversation, "owner", true, "future question".into(), &[]).await.unwrap();
        // Fast submissions can share SQLite's millisecond timestamp. Reverse
        // UUID order must neither reorder the queue nor the completed history.
        sqlx::query("UPDATE assistant_generation_jobs SET created_at=1000000 WHERE conversation_id=?1")
            .bind(conversation).execute(&pool).await.unwrap();
        let claimed = assistant_store::claim_next_queued_generation(conversation, "owner").await.unwrap().unwrap();
        assert_eq!(claimed.id, second);
        let context = load_generation_conversation(conversation, "owner", second).await.unwrap();
        assert_eq!(context.messages.iter().map(|message| message.content.as_str()).collect::<Vec<_>>(),
            vec!["inherited question", "inherited answer", "first question", "first answer", "follow-up"]);
        assert!(load_generation_conversation(conversation, "other", second).await.unwrap().messages.is_empty());

        // The completed summary already includes inherited branch context;
        // neither ancestors nor raw messages should be replayed beside it.
        sqlx::query("UPDATE assistant_messages SET metadata=?1 WHERE generation_job_id=?2 AND role='assistant'")
            .bind(json!({"context_summary":{"version":1,"content":"祖先与第一轮的摘要"}}))
            .bind(first).execute(&pool).await.unwrap();
        let context = load_generation_conversation(conversation, "owner", second).await.unwrap();
        assert_eq!(context.messages.iter().map(|message| message.content.as_str()).collect::<Vec<_>>(),
            vec!["祖先与第一轮的摘要", "follow-up"]);
    }

    #[tokio::test]
    async fn cancel_between_database_claim_and_registration_stops_before_execution() {
        let pool = crate::db::test_pool().await;
        crate::db::set_test_pool(pool.clone());
        let conversation: i64 = sqlx::query_scalar(
            "INSERT INTO assistant_conversations(user_id,system_prompt) VALUES ('owner','') RETURNING id",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        let id = uuid::Uuid::new_v4();
        assistant_store::create_queued_user_turn(id, conversation, "owner", true, "hello".into(), &[]).await.unwrap();
        let record = assistant_store::claim_next_queued_generation(conversation, "owner").await.unwrap().unwrap();
        assert!(assistant_job::global_registry().get(id).is_none());
        let response = cancel_job(Session::for_storage("owner"), Path(id)).await.unwrap();
        assert_eq!(response.0["status"], "cancelling");
        assert_eq!(assistant_store::get_generation_job(id, "owner").await.unwrap().unwrap().status, "running");
        let forbidden = cancel_job(Session::for_storage("other"), Path(id)).await.unwrap_err();
        assert_eq!(forbidden.status(), StatusCode::NOT_FOUND);
        let job = Arc::new(assistant_job::GenerationJob::new(id, conversation, "owner".into()));
        // No model/config/workspace setup is needed: the worker exits first.
        run_queued_generation(&record, job.clone()).await.unwrap();
        assert!(job.cancellation_token().is_cancelled());
        job.complete().await;
        assert_eq!(assistant_store::get_generation_job(id, "owner").await.unwrap().unwrap().status, "failed");
    }

    #[tokio::test]
    async fn scheduler_limits_concurrent_jobs_and_releases_slots() {
        let scheduler = QueueScheduler::new(2);
        let first = scheduler.acquire().await;
        let second = scheduler.acquire().await;
        let waiting = scheduler.acquire();
        tokio::pin!(waiting);
        assert!(tokio::time::timeout(Duration::from_millis(20), &mut waiting).await.is_err());
        drop(first);
        let next = tokio::time::timeout(Duration::from_secs(1), &mut waiting).await.unwrap();
        drop((second, next));
        assert_eq!(scheduler.global.available_permits(), 2);
    }
}
