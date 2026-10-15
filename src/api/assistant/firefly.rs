use super::*;
use crate::db;
use sqlx::{query, query_scalar};

pub(super) async fn inherited_context(conversation_id: i64, user_id: &str) -> anyhow::Result<Vec<Message>> {
    let value: Option<Value> =
        query_scalar("SELECT context FROM assistant_firefly_branches WHERE conversation_id = ?1 AND user_id = ?2")
            .bind(conversation_id)
            .bind(user_id)
            .fetch_optional(db::pool())
            .await?;
    Ok(serde_json::from_value(value.unwrap_or_else(|| json!([])))?)
}

/// Snapshot in execution order, including ancestors, stopping at the selected reply.
async fn snapshot(user_id: &str, target: i64) -> Result<(Vec<Message>, String), AppError> {
    let config = AssistantConfig::shared().await?;
    snapshot_with_config(user_id, target, &config).await
}

pub(super) async fn snapshot_with_config(user_id: &str, target: i64, config: &AssistantConfig) -> Result<(Vec<Message>, String), AppError> {
    let location: Option<(i64, Option<uuid::Uuid>, String)> = sqlx::query_as(
        "SELECT m.conversation_id, m.generation_job_id, m.content FROM assistant_messages m
         JOIN assistant_conversations c ON c.id = m.conversation_id
         LEFT JOIN assistant_generation_jobs j ON j.id = m.generation_job_id
         WHERE m.id = ?1 AND c.user_id = ?2 AND c.agent = 'firefly'
           AND m.role = 'assistant' AND trim(m.content) <> ''
           AND (m.tool_calls IS NULL OR m.tool_calls = '[]')
           AND (m.generation_job_id IS NULL OR j.status = 'completed')",
    )
    .bind(target)
    .bind(user_id)
    .fetch_optional(db::pool())
    .await
    .map_err(AppError::internal_server_error)?;
    let (conversation_id, job_id, preview) =
        location.ok_or(AppError::bad_request("引用消息不可用，请选择已完成的流萤回复"))?;
    let mut context = inherited_context(conversation_id, user_id).await.map_err(AppError::internal_server_error)?;
    let history =
        assistant_store::fetch_generation_context_messages(conversation_id, user_id, job_id.unwrap_or_default())
            .await
            .map_err(AppError::internal_server_error)?;
    let end = history.iter().position(|m| m.id == target).ok_or(AppError::bad_request("引用消息不可用"))?;
    let mut history = history;
    history.truncate(end + 1);
    if let Some(summary) = super::turn_context::restore(&mut history) {
        context = vec![summary];
    }
    let ids: Vec<_> = history.iter().map(|m| m.id).collect();
    let attachments = assistant_store::fetch_attachments_for_messages(conversation_id, user_id, &ids)
        .await
        .map_err(AppError::internal_server_error)?;
    let mut grouped: HashMap<i64, Vec<assistant_store::AttachmentRecord>> = HashMap::new();
    for attachment in attachments {
        grouped.entry(attachment.message_id).or_default().push(attachment);
    }
    for record in history {
        let files = grouped.remove(&record.id).unwrap_or_default();
        context.push(message_from_record(record, files).map_err(AppError::internal_server_error)?);
    }
    // Large branch snapshots use the same summary policy; keep no raw tool
    // transcript in the inherited context once it has been summarized.
    let budget = config.budget.resolve().await.map_err(AppError::bad_request)?;
    if serde_json::to_vec(&context).map_err(AppError::internal_server_error)?.len() > budget {
        let latest_user = context.iter().rev().find(|m| m.role == Role::User).cloned();
        let mut input = Vec::new();
        for message in &context {
            if let Some(message) = super::inputs::model_message(message).await.map_err(AppError::internal_server_error)? {
                input.push(message);
            }
        }
        let model = llm::ContextModel::with_budget(config.model.clone(), config.budget.clone());
        let (prepared, _) = model.prepare(g::ModelRequest::new(input, vec![]), true).await
            .map_err(AppError::bad_request)?;
        context = prepared.messages.iter().filter(|m| m.role == GRole::Assistant)
            .map(|m| Message::assistant(m.text_content(), None)).collect();
        if let Some(user) = latest_user { context.push(user); }
    }
    Ok((context, preview.chars().take(160).collect()))
}

#[derive(Deserialize)]
pub struct SendQuery {
    request_id: uuid::Uuid,
    reply_to_message_id: Option<i64>,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct Submission {
    conversation_id: i64,
    job_id: uuid::Uuid,
    message_id: i64,
}

pub async fn send(
    auth: Session, Query(args): Query<SendQuery>, mut multipart: Multipart,
) -> Result<Json<Submission>, AppError> {
    let prompt = crate::agent_settings::Settings::load(db::pool()).await?.firefly.instruction;
    let parsed = parse_chat_multipart(&mut multipart).await?;
    let submitted = enqueue(auth.storage_scope(), &args, parsed, &prompt, config::get().assistant_enable_tools).await?;
    kick_conversation_queue(submitted.conversation_id, auth.storage_scope()).await.map_err(AppError::internal_server_error)?;
    Ok(Json(submitted))
}

async fn enqueue(
    user_id: &str, args: &SendQuery, parsed: ParsedChatPayload, prompt: &str, default_enable_tools: bool,
) -> Result<Submission, AppError> {
    enqueue_with_config(user_id, args, parsed, prompt, default_enable_tools, None).await
}

async fn enqueue_with_config(
    user_id: &str, args: &SendQuery, parsed: ParsedChatPayload, prompt: &str, default_enable_tools: bool,
    snapshot_config: Option<&AssistantConfig>,
) -> Result<Submission, AppError> {
    let mut cleanup = AttachmentCleanup::new(parsed.attachments);
    let content = parsed.message.trim();
    if content.is_empty() && cleanup.entries().is_empty() {
        return Err(AppError::bad_request("消息或附件不能为空"));
    }
    let existing: Option<(i64, uuid::Uuid, i64)> = sqlx::query_as(
        "SELECT b.conversation_id, m.generation_job_id, m.id FROM assistant_firefly_branches b
         JOIN assistant_messages m ON m.conversation_id = b.conversation_id AND m.role = 'user'
         WHERE b.user_id = ?1 AND b.request_id = ?2 ORDER BY m.id LIMIT 1",
    )
    .bind(user_id)
    .bind(args.request_id)
    .fetch_optional(db::pool())
    .await
    .map_err(AppError::internal_server_error)?;
    if let Some((conversation_id, job_id, message_id)) = existing {
        return Ok(Submission { conversation_id, job_id, message_id });
    }
    let (context, preview) = if let Some(target) = args.reply_to_message_id {
        let (context, preview) = match snapshot_config {
            Some(config) => snapshot_with_config(user_id, target, config).await?,
            None => snapshot(user_id, target).await?,
        };
        (context, Some(preview))
    } else {
        (Vec::new(), None)
    };
    // Snapshot reads finish before taking the writer connection. Otherwise a
    // burst of submissions can exhaust the pool while waiting for this writer.
    let mut tx = db::pool().begin_with("BEGIN IMMEDIATE").await.map_err(AppError::internal_server_error)?;
    let existing: Option<(i64, uuid::Uuid, i64)> = sqlx::query_as(
        "SELECT b.conversation_id, m.generation_job_id, m.id FROM assistant_firefly_branches b
         JOIN assistant_messages m ON m.conversation_id = b.conversation_id AND m.role = 'user'
         WHERE b.user_id = ?1 AND b.request_id = ?2 ORDER BY m.id LIMIT 1",
    )
    .bind(user_id)
    .bind(args.request_id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(AppError::internal_server_error)?;
    if let Some((conversation_id, job_id, message_id)) = existing {
        tx.commit().await.map_err(AppError::internal_server_error)?;
        return Ok(Submission { conversation_id, job_id, message_id });
    }
    let queue_limit = config::try_get().map_or(32, |cfg| cfg.assistant_max_queued_jobs);
    assistant_store::enforce_submission_capacity(&mut tx, user_id, queue_limit)
        .await.map_err(|error| match error.downcast_ref::<assistant_store::SubmissionConflict>() {
            Some(assistant_store::SubmissionConflict::QueueFull) => AppError::too_many_requests(error, 5),
            _ => AppError::internal_server_error(error),
        })?;
    if let Some(target) = args.reply_to_message_id {
        let valid: bool = query_scalar("SELECT EXISTS(SELECT 1 FROM assistant_messages m JOIN assistant_conversations c ON c.id=m.conversation_id LEFT JOIN assistant_generation_jobs j ON j.id=m.generation_job_id WHERE m.id=?1 AND c.user_id=?2 AND c.agent='firefly' AND (m.generation_job_id IS NULL OR j.status='completed'))")
            .bind(target).bind(user_id).fetch_one(&mut *tx).await.map_err(AppError::internal_server_error)?;
        if !valid {
            return Err(AppError::bad_request("引用消息已发生变化，请重新选择"));
        }
    }
    let conversation_id: i64 = query_scalar(
        "INSERT INTO assistant_conversations (user_id, title, system_prompt, agent) VALUES (?1, '流萤', ?2, 'firefly') RETURNING id",
    ).bind(user_id).bind(prompt).fetch_one(&mut *tx).await.map_err(AppError::internal_server_error)?;
    query("INSERT INTO assistant_firefly_branches (conversation_id, user_id, request_id, reply_to_message_id, reply_preview, context) VALUES (?1,?2,?3,?4,?5,?6)")
        .bind(conversation_id).bind(user_id).bind(args.request_id).bind(args.reply_to_message_id).bind(preview)
        .bind(serde_json::to_value(context).map_err(AppError::internal_server_error)?)
        .execute(&mut *tx).await.map_err(AppError::internal_server_error)?;
    let job_id = uuid::Uuid::new_v4();
    query("INSERT INTO assistant_generation_jobs (id, conversation_id, user_id, status, enable_tools) VALUES (?1,?2,?3,'queued',?4)")
        .bind(job_id).bind(conversation_id).bind(user_id).bind(parsed.enable_tools.unwrap_or(default_enable_tools))
        .execute(&mut *tx).await.map_err(AppError::internal_server_error)?;
    let message_id: i64 = query_scalar("INSERT INTO assistant_messages (conversation_id, role, content, generation_job_id) VALUES (?1,'user',?2,?3) RETURNING id")
        .bind(conversation_id).bind(content).bind(job_id).fetch_one(&mut *tx).await.map_err(AppError::internal_server_error)?;
    for file in cleanup.entries() {
        query("INSERT INTO assistant_message_attachments (message_id, file_name, content_type, file_size, storage_path) VALUES (?1,?2,?3,?4,?5)")
            .bind(message_id).bind(&file.file_name).bind(&file.content_type).bind(file.file_size).bind(&file.storage_path)
            .execute(&mut *tx).await.map_err(AppError::internal_server_error)?;
    }
    query("UPDATE assistant_conversations SET message_count = 1, last_message_at = CAST(unixepoch('subsec') * 1000000 AS INTEGER), updated_at = CAST(unixepoch('subsec') * 1000000 AS INTEGER) WHERE id = ?1")
        .bind(conversation_id).execute(&mut *tx).await.map_err(AppError::internal_server_error)?;
    tx.commit().await.map_err(AppError::internal_server_error)?;
    crate::assistant_events::notify(Some(user_id), "conversations");
    cleanup.disarm();
    Ok(Submission { conversation_id, job_id, message_id })
}

#[derive(Deserialize)]
pub struct FeedQuery {
    before: Option<i64>,
    turn_id: Option<i64>,
    message_id: Option<i64>,
    limit: Option<i64>,
}

pub async fn list(auth: Session, Query(args): Query<FeedQuery>) -> Result<Json<Value>, AppError> {
    let limit = args.limit.unwrap_or(5).clamp(1, 100);
    let mut turns = load_feed(
        db::pool(),
        auth.storage_scope(),
        args.before,
        args.turn_id,
        args.message_id,
        limit + 1,
    )
    .await
    .map_err(AppError::internal_server_error)?;
    let has_more = turns.len() > limit as usize;
    turns.truncate(limit as usize);
    for turn in &mut turns {
        if let Some(messages) = turn["messages"].as_array_mut() {
            let records = std::mem::take(messages)
                .into_iter()
                .map(|mut value| {
                    let attachments = value["attachments"].take();
                    let record = serde_json::from_value(value).map_err(AppError::internal_server_error)?;
                    let mut response = to_message_response(record, Vec::new());
                    response.attachments =
                        serde_json::from_value(attachments).map_err(AppError::internal_server_error)?;
                    Ok(response)
                })
                .collect::<Result<Vec<_>, AppError>>()?;
            *messages = summarize_process_messages(records)
                .into_iter()
                .map(serde_json::to_value)
                .collect::<Result<_, _>>()
                .map_err(AppError::internal_server_error)?;
        }
    }

    let before = turns.last().and_then(|t| t["id"].as_i64());
    turns.reverse();
    Ok(Json(json!({ "turns": turns, "has_more": has_more, "next_before": before })))
}

#[derive(sqlx::FromRow)]
struct FeedTurn {
    id: i64,
    conversation_id: i64,
    generation_job_id: Option<uuid::Uuid>,
    status: Option<String>,
    error_message: Option<String>,
    reply_id: Option<i64>,
    reply_preview: Option<String>,
}

#[derive(sqlx::FromRow)]
struct FeedMessage {
    turn_id: i64,
    #[sqlx(flatten)]
    message: assistant_store::MessageRecord,
}

// Three bounded queries in one read snapshot; UUID/time/JSON encoding belongs
// to the Rust API layer, not database-specific JSON aggregation functions.
async fn load_feed(
    pool: &sqlx::SqlitePool, user: &str, before: Option<i64>, turn_id: Option<i64>, message_id: Option<i64>,
    limit: i64,
) -> anyhow::Result<Vec<Value>> {
    let mut tx = pool.begin().await?;
    let turns = sqlx::query_as::<_, FeedTurn>(include_str!("firefly_feed.sql"))
        .bind(user)
        .bind(before)
        .bind(turn_id)
        .bind(message_id)
        .bind(limit)
        .fetch_all(&mut *tx)
        .await?;
    let ids: Vec<i64> = turns.iter().map(|turn| turn.id).collect();
    let messages = sqlx::query_as::<_, FeedMessage>(
        "SELECT t.id AS turn_id, m.*, j.status AS processing_status, j.error_message AS processing_error
         FROM assistant_messages t JOIN assistant_messages m ON m.conversation_id = t.conversation_id
         LEFT JOIN assistant_generation_jobs j ON j.id = t.generation_job_id
         WHERE t.id IN (SELECT value FROM json_each(?1)) AND (
             m.generation_job_id = t.generation_job_id OR
             (t.generation_job_id IS NULL AND m.generation_job_id IS NULL AND m.id >= t.id
              AND NOT EXISTS (SELECT 1 FROM assistant_messages next_u WHERE next_u.conversation_id = t.conversation_id
                AND next_u.role = 'user' AND next_u.id > t.id AND next_u.id <= m.id)))
         ORDER BY t.id, m.id",
    )
    .bind(sqlx::types::Json(&ids))
    .fetch_all(&mut *tx)
    .await?;
    let message_ids: Vec<i64> = messages.iter().map(|row| row.message.id).collect();
    let attachments = sqlx::query_as::<_, assistant_store::AttachmentRecord>(
        "SELECT * FROM assistant_message_attachments WHERE message_id IN (SELECT value FROM json_each(?1)) ORDER BY id",
    )
    .bind(sqlx::types::Json(&message_ids))
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;
    let mut files: HashMap<i64, Vec<Value>> = HashMap::new();
    for file in attachments {
        files.entry(file.message_id).or_default().push(serde_json::to_value(file)?);
    }
    let mut grouped: HashMap<i64, Vec<Value>> = HashMap::new();
    for row in messages {
        let attached = files.remove(&row.message.id).unwrap_or_default();
        let mut message = serde_json::to_value(row.message)?;
        message["attachments"] = json!(attached);
        grouped.entry(row.turn_id).or_default().push(message);
    }
    Ok(turns.into_iter().map(|turn| json!({
        "id": turn.id, "conversation_id": turn.conversation_id, "job_id": turn.generation_job_id,
        "status": turn.status.as_deref().unwrap_or("completed"), "error": turn.error_message,
        "reply": turn.reply_preview.map(|content| json!({"id": turn.reply_id, "content": content})),
        "messages": grouped.remove(&turn.id).unwrap_or_default()
    })).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot_config() -> AssistantConfig {
        struct Summary;
        #[async_trait]
        impl Model for Summary {
            async fn generate(&self, _: g::ModelRequest) -> Result<g::ModelResponse, g::ModelError> {
                Ok(g::ModelResponse::new(GMessage::assistant("任务摘要")))
            }
        }
        AssistantConfig { model: Arc::new(Summary),
            budget: llm::ModelBudget::fixed(32768), system_prompt: String::new(), agent_settings: None }
    }

    async fn snapshot(user_id: &str, target: i64) -> Result<(Vec<Message>, String), AppError> {
        snapshot_with_config(user_id, target, &snapshot_config()).await
    }

    async fn enqueue(user_id: &str, args: &SendQuery, parsed: ParsedChatPayload, prompt: &str,
        default_enable_tools: bool) -> Result<Submission, AppError> {
        enqueue_with_config(user_id, args, parsed, prompt, default_enable_tools, Some(&snapshot_config())).await
    }

    #[tokio::test]
    async fn db_tests_large_snapshot_summarizes_without_preserving_raw_results() {
        let pool = crate::db::test_pool().await;
        db::set_test_pool(pool.clone());
        let conversation: i64 = query_scalar("INSERT INTO assistant_conversations (user_id, system_prompt, agent) VALUES ('owner', '', 'firefly') RETURNING id")
            .fetch_one(&pool).await.unwrap();
        query("INSERT INTO assistant_messages (conversation_id, role, content) VALUES (?1, 'user', '查看项目列表')")
            .bind(conversation).execute(&pool).await.unwrap();
        let answer: i64 = query_scalar("INSERT INTO assistant_messages (conversation_id, role, content) VALUES (?1, 'assistant', ?2) RETURNING id")
            .bind(conversation).bind("详细查询结果".repeat(10000)).fetch_one(&pool).await.unwrap();
        let (context, _) = snapshot("owner", answer).await.unwrap();
        assert_eq!(context.len(), 2);
        assert!(context[0].content.contains("任务摘要"));
        assert!(!context[0].content.contains("详细查询结果"));
        assert_eq!(context[1].content, "查看项目列表");
    }

    #[tokio::test]
    async fn db_tests_firefly_capacity_preserves_replay_and_rolls_back_rejection() {
        let pool = crate::db::test_pool().await;
        db::set_test_pool(pool.clone());
        let limit = config::try_get().map_or(32, |cfg| cfg.assistant_max_queued_jobs);
        let request = SendQuery { request_id: uuid::Uuid::new_v4(), reply_to_message_id: None };
        let payload = || ParsedChatPayload {
            message: "capacity test".into(), conversation_id: None, enable_tools: None, attachments: vec![],
        };
        let first = enqueue("capacity-owner", &request, payload(), "", false).await.unwrap();
        for _ in 1..limit {
            query("INSERT INTO assistant_generation_jobs (id, conversation_id, user_id, status) VALUES (?1, ?2, 'capacity-owner', 'queued')")
                .bind(uuid::Uuid::new_v4()).bind(first.conversation_id).execute(&pool).await.unwrap();
        }
        assert_eq!(enqueue("capacity-owner", &request, payload(), "", false).await.unwrap(), first);
        let count_before: i64 = query_scalar("SELECT count(*) FROM assistant_conversations")
            .fetch_one(&pool).await.unwrap();
        let error = enqueue("capacity-owner", &SendQuery {
            request_id: uuid::Uuid::new_v4(), reply_to_message_id: None,
        }, payload(), "", false).await.unwrap_err();
        let response = axum::response::IntoResponse::into_response(error);
        assert_eq!(response.status(), axum::http::StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(response.headers()["retry-after"], "5");
        let count_after: i64 = query_scalar("SELECT count(*) FROM assistant_conversations")
            .fetch_one(&pool).await.unwrap();
        assert_eq!(count_before, count_after);
        enqueue("another-owner", &SendQuery { request_id: uuid::Uuid::new_v4(), reply_to_message_id: None },
            payload(), "", false).await.unwrap();
    }

    #[tokio::test]
    async fn db_tests_firefly_branch_isolation_and_feed() {
        let pool = crate::db::test_pool().await;
        db::set_test_pool(pool.clone());
        let a: i64 = query_scalar("INSERT INTO assistant_conversations (user_id, system_prompt, agent) VALUES ('owner', '', 'firefly') RETURNING id")
            .fetch_one(&pool).await.unwrap();
        let question: i64 = query_scalar("INSERT INTO assistant_messages (conversation_id, role, content) VALUES (?1, 'user', 'A question') RETURNING id")
            .bind(a).fetch_one(&pool).await.unwrap();
        query("INSERT INTO assistant_message_attachments (message_id, file_name, content_type, file_size, storage_path) VALUES (?1, 'report.csv', 'text/csv', 10, '/test/report.csv')")
            .bind(question).execute(&pool).await.unwrap();
        let answer: i64 = query_scalar("INSERT INTO assistant_messages (conversation_id, role, content) VALUES (?1, 'assistant', 'A answer') RETURNING id")
            .bind(a).fetch_one(&pool).await.unwrap();
        let (b_context, _) = snapshot("owner", answer).await.unwrap();
        assert_eq!(b_context.len(), 2);
        assert_eq!(b_context[0].attachments[0].file_name, "report.csv");
        // A later legacy turn must never become part of a branch taken at A.
        query("INSERT INTO assistant_messages (conversation_id, role, content) VALUES (?1, 'user', 'later question'), (?1, 'assistant', 'later answer')")
            .bind(a).execute(&pool).await.unwrap();
        let (c_context, _) = snapshot("owner", answer).await.unwrap();
        assert_eq!(serde_json::to_value(&b_context).unwrap(), serde_json::to_value(&c_context).unwrap());
        assert!(snapshot("other", answer).await.is_err());
        assert!(snapshot("owner", question).await.is_err());
        assert!(snapshot("owner", i64::MAX).await.is_err());
        let b: i64 = query_scalar("INSERT INTO assistant_conversations (user_id, system_prompt, agent) VALUES ('owner', '', 'firefly') RETURNING id")
            .fetch_one(&pool).await.unwrap();
        query("INSERT INTO assistant_firefly_branches (conversation_id, user_id, request_id, reply_to_message_id, reply_preview, context) VALUES (?1, 'owner', ?2, ?3, 'A answer', ?4)")
            .bind(b).bind(uuid::Uuid::new_v4()).bind(answer).bind(serde_json::to_value(&b_context).unwrap()).execute(&pool).await.unwrap();
        query("INSERT INTO assistant_messages (conversation_id, role, content) VALUES (?1, 'user', 'B question')")
            .bind(b)
            .execute(&pool)
            .await
            .unwrap();
        let b_answer: i64 = query_scalar("INSERT INTO assistant_messages (conversation_id, role, content) VALUES (?1, 'assistant', 'B answer') RETURNING id")
            .bind(b).fetch_one(&pool).await.unwrap();
        let (d_context, _) = snapshot("owner", b_answer).await.unwrap();
        assert_eq!(
            d_context.iter().map(|m| m.content.as_str()).collect::<Vec<_>>(),
            ["A question", "A answer", "B question", "B answer"]
        );
        let (c_again, _) = snapshot("owner", answer).await.unwrap();
        assert_eq!(c_again.len(), 2);
        // The feed contains original records once; snapshots never duplicate them.
        let feed = |user: &'static str, before: Option<i64>, message: Option<i64>| {
            load_feed(&pool, user, before, None, message, 20)
        };
        let all = feed("owner", None, None).await.unwrap();
        assert_eq!(all.len(), 3);
        assert_eq!(all[0]["messages"].as_array().unwrap().len(), 2);
        assert_eq!(all[0]["reply"]["id"], answer);
        assert_eq!(all[1]["reply"]["id"], answer);
        let older = feed("owner", all[0]["id"].as_i64(), None).await.unwrap();
        assert_eq!(older.len(), 2);
        let located = feed("owner", None, Some(answer)).await.unwrap();
        assert_eq!(located.len(), 1);
        assert_eq!(located[0]["id"], question);
        assert!(feed("other", None, None).await.unwrap().is_empty());
        // Atomic submissions are idempotent and always own a new conversation.
        let request = SendQuery { request_id: uuid::Uuid::new_v4(), reply_to_message_id: Some(answer) };
        let payload = || ParsedChatPayload {
            message: "branch C".into(),
            conversation_id: None,
            enable_tools: None,
            attachments: vec![],
        };
        let (left, right) = tokio::join!(
            enqueue("owner", &request, payload(), "", true),
            enqueue("owner", &request, payload(), "", true)
        );
        let submitted = left.unwrap();
        assert_eq!(submitted, right.unwrap());
        assert_ne!(submitted.conversation_id, a);
        let count: i64 = query_scalar("SELECT count(*) FROM assistant_messages WHERE conversation_id = ?1")
            .bind(submitted.conversation_id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(count, 1);
        assert_eq!(inherited_context(submitted.conversation_id, "owner").await.unwrap().len(), 2);
        let independent = enqueue(
            "owner",
            &SendQuery { request_id: uuid::Uuid::new_v4(), reply_to_message_id: None },
            payload(),
            "",
            true,
        )
        .await
        .unwrap();
        assert_ne!(independent.conversation_id, submitted.conversation_id);
        assert!(inherited_context(independent.conversation_id, "owner").await.unwrap().is_empty());
        let before_count: i64 =
            query_scalar("SELECT count(*) FROM assistant_conversations").fetch_one(&pool).await.unwrap();
        assert!(enqueue(
            "other",
            &SendQuery { request_id: uuid::Uuid::new_v4(), reply_to_message_id: Some(answer) },
            payload(),
            "",
            true
        )
        .await
        .is_err());
        let after_count: i64 =
            query_scalar("SELECT count(*) FROM assistant_conversations").fetch_one(&pool).await.unwrap();
        assert_eq!(before_count, after_count);

        assert!(snapshot("owner", answer).await.is_ok());
        assert_eq!(feed("owner", None, None).await.unwrap().len(), 5);

        // Legacy queue: B can be submitted before A's reply is persisted.
        // Snapshot and reference order must follow turns, not raw record IDs.
        let queued: i64 = query_scalar("INSERT INTO assistant_conversations (user_id, system_prompt, agent) VALUES ('owner', '', 'firefly') RETURNING id")
            .fetch_one(&pool).await.unwrap();
        let first_job = uuid::Uuid::new_v4();
        let second_job = uuid::Uuid::new_v4();
        query("INSERT INTO assistant_generation_jobs (id, conversation_id, user_id, status, created_at) VALUES (?1,?3,'owner','completed',1789084800000000), (?2,?3,'owner','completed',1789084801000000)")
            .bind(first_job).bind(second_job).bind(queued).execute(&pool).await.unwrap();
        query("INSERT INTO assistant_messages (conversation_id, role, content, generation_job_id) VALUES (?1,'user','queued A',?2), (?1,'user','queued B',?3)")
            .bind(queued).bind(first_job).bind(second_job).execute(&pool).await.unwrap();
        query("INSERT INTO assistant_messages (conversation_id, role, content, generation_job_id, tool_calls) VALUES (?1,'assistant','',?2,?3)")
            .bind(queued).bind(first_job).bind(json!([{"id":"tool-a","name":"lookup","arguments":{"q":"A"}}])).execute(&pool).await.unwrap();
        query("INSERT INTO assistant_messages (conversation_id, role, content, generation_job_id, tool_call_id) VALUES (?1,'tool','tool A result',?2,'tool-a')")
            .bind(queued).bind(first_job).execute(&pool).await.unwrap();
        let queued_a: i64 = query_scalar("INSERT INTO assistant_messages (conversation_id, role, content, generation_job_id) VALUES (?1,'assistant','queued A answer',?2) RETURNING id")
            .bind(queued).bind(first_job).fetch_one(&pool).await.unwrap();
        query("INSERT INTO assistant_messages (conversation_id, role, content, generation_job_id) VALUES (?1,'assistant','queued B answer',?2)")
            .bind(queued).bind(second_job).execute(&pool).await.unwrap();
        let (queued_context, _) = snapshot("owner", queued_a).await.unwrap();
        assert_eq!(
            queued_context.iter().map(|m| m.content.as_str()).collect::<Vec<_>>(),
            ["queued A", "", "tool A result", "queued A answer"]
        );
        assert_eq!(queued_context[1].tool_calls.as_ref().unwrap()[0].id, "tool-a");
        assert_eq!(queued_context[2].tool_call_id.as_deref(), Some("tool-a"));
        let queued_feed = feed("owner", None, None).await.unwrap();
        assert_eq!(queued_feed[0]["reply"]["id"], queued_a);
        assert_eq!(queued_feed[1]["messages"].as_array().unwrap().len(), 4);
        query("UPDATE assistant_generation_jobs SET status = 'running' WHERE id = ?1")
            .bind(first_job)
            .execute(&pool)
            .await
            .unwrap();
        assert!(snapshot("owner", queued_a).await.is_err());
        query("UPDATE assistant_conversations SET agent = 'codex' WHERE id = ?1")
            .bind(queued)
            .execute(&pool)
            .await
            .unwrap();
        assert!(feed("owner", None, Some(queued_a)).await.unwrap().is_empty());
        enqueue(
            "owner",
            &SendQuery { request_id: uuid::Uuid::new_v4(), reply_to_message_id: None },
            payload(),
            "",
            true,
        )
        .await
        .unwrap();
        let Json(page) = list(Session::for_storage("owner"), Query(serde_json::from_value(json!({})).unwrap()))
            .await
            .unwrap();
        assert_eq!(page["turns"].as_array().unwrap().len(), 5);
        assert_eq!(page["has_more"], true);
        assert!(page["turns"].as_array().unwrap().iter().all(|turn| turn.get("is_archived").is_none()));
        let Json(older) = list(
            Session::for_storage("owner"),
            Query(serde_json::from_value(json!({ "before": page["next_before"] })).unwrap()),
        )
        .await
        .unwrap();
        assert_eq!(older["turns"].as_array().unwrap().len(), 1);
        assert_eq!(older["has_more"], false);
    }
}
