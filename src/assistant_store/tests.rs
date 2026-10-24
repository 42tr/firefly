use chrono::{DateTime, Utc};
use sqlx::{query, query_scalar};

use super::attachments::get_attachment_by_file_with_pool;
use super::conversations::{retry_workspace_with_pool, CONVERSATION_BUSY_SQL};
use super::messages::{fetch_messages_page_with_pool, PAGE_QUESTION_SQL};
use super::*;
use super::generation::{claim_next_queued_generation_with_pool, create_queued_user_turn};
use super::weekly::fetch_weekly_agent_work_with_pool;

#[test]
fn archival_checks_skip_completed_history() {
    let connection = crate::db::test_connection();
    connection.execute_batch("INSERT INTO assistant_conversations(id,user_id,system_prompt) VALUES (1,'owner','');
        WITH RECURSIVE n(i) AS (VALUES(1) UNION ALL SELECT i+1 FROM n WHERE i<20000)
        INSERT INTO assistant_generation_jobs(id,conversation_id,user_id,status)
        SELECT randomblob(16),1,'owner','completed' FROM n;
        WITH RECURSIVE n(i) AS (VALUES(1) UNION ALL SELECT i+1 FROM n WHERE i<20000)
        INSERT INTO assistant_messages(conversation_id,role,content,metadata)
        SELECT 1,'tool','output','{}' FROM n;").unwrap();
    for analyze in [false, true] {
        if analyze { connection.execute_batch("ANALYZE").unwrap(); }
        let mut statement = connection.prepare(CONVERSATION_BUSY_SQL).unwrap();
        let busy: bool = statement.query_row([1], |row| row.get(0)).unwrap();
        assert!(!busy);
        let steps = statement.get_status(rusqlite::StatementStatus::VmStep);
        assert!(steps < 100, "archival scanned history: {steps} steps");
    }
    connection.execute_batch("INSERT INTO assistant_generation_jobs(id,conversation_id,user_id,status)
        VALUES (zeroblob(16),1,'owner','queued');").unwrap();
    assert!(connection.query_row(CONVERSATION_BUSY_SQL, [1], |row| row.get::<_, bool>(0)).unwrap());
}

#[tokio::test]
async fn generation_context_keeps_legacy_and_interleaved_job_order() {
    let pool = crate::db::test_pool().await;
    crate::db::set_test_pool(pool.clone());
    let conversation: i64 = query_scalar("INSERT INTO assistant_conversations(user_id,system_prompt) VALUES ('owner','') RETURNING id")
        .fetch_one(&pool).await.unwrap();
    let a = uuid::Uuid::from_u128(1);
    let b = uuid::Uuid::from_u128(2);
    let current = uuid::Uuid::from_u128(3);
    let queued = uuid::Uuid::from_u128(4);
    for (job, status, at) in [(a,"completed",10_i64),(b,"completed",10),(current,"running",20),(queued,"queued",30)] {
        query("INSERT INTO assistant_generation_jobs(id,conversation_id,user_id,status,created_at) VALUES (?1,?2,'owner',?3,?4)")
            .bind(job).bind(conversation).bind(status).bind(at).execute(&pool).await.unwrap();
    }
    let mut ids = Vec::new();
    for (job, at) in [(Some(b),1_i64),(None,200),(Some(current),2),(Some(a),3),
        (None,100),(Some(b),0),(Some(queued),4)] {
        let id: i64 = query_scalar("INSERT INTO assistant_messages(conversation_id,role,content,generation_job_id,created_at) VALUES (?1,'assistant','payload',?2,?3) RETURNING id")
            .bind(conversation).bind(job).bind(at).fetch_one(&pool).await.unwrap();
        ids.push(id);
    }
    let rows = fetch_generation_context_messages(conversation, "owner", current).await.unwrap();
    assert_eq!(rows.iter().map(|m| m.id).collect::<Vec<_>>(), [ids[4],ids[1],ids[3],ids[0],ids[5],ids[2]]);
    assert!(fetch_generation_context_messages(conversation, "other", current).await.unwrap().is_empty());
}

#[tokio::test]
async fn persisted_context_summary_replaces_history_after_its_boundary() {
    let pool = crate::db::test_pool().await;
    crate::db::set_test_pool(pool.clone());
    let conversation: i64 = query_scalar(
        "INSERT INTO assistant_conversations(user_id,system_prompt) VALUES ('owner','') RETURNING id",
    ).fetch_one(&pool).await.unwrap();
    let job = uuid::Uuid::new_v4();
    query("INSERT INTO assistant_generation_jobs(id,conversation_id,user_id,status) VALUES (?1,?2,'owner','running')")
        .bind(job).bind(conversation).execute(&pool).await.unwrap();
    let user_message: i64 = query_scalar(
        "INSERT INTO assistant_messages(conversation_id,generation_job_id,role,content) VALUES (?1,?2,'user','current') RETURNING id",
    ).bind(conversation).bind(job).fetch_one(&pool).await.unwrap();
    let after: i64 = query_scalar(
        "INSERT INTO assistant_messages(conversation_id,generation_job_id,role,content) VALUES (?1,?2,'assistant','new output') RETURNING id",
    ).bind(conversation).bind(job).fetch_one(&pool).await.unwrap();
    save_generation_context_summary(conversation, "owner", user_message, "previous work is complete").await.unwrap();
    assert_eq!(fetch_generation_context_summary(conversation, "owner").await.unwrap(),
        Some((user_message, "previous work is complete".into())));
    let rows = fetch_generation_context_messages(conversation, "owner", job).await.unwrap();
    assert_eq!(rows.iter().map(|row| (row.id, row.content.as_str())).collect::<Vec<_>>(), vec![(after, "new output")]);
    assert!(fetch_generation_context_summary(conversation, "other").await.unwrap().is_none());
    save_generation_context_summary(conversation, "owner", user_message, "replacement").await.unwrap();
    assert_eq!(fetch_generation_context_summary(conversation, "owner").await.unwrap().unwrap().1, "replacement");
}

#[test]
fn message_page_question_lookup_skips_job_history() {
    let connection = crate::db::test_connection();
    connection.execute_batch("INSERT INTO assistant_conversations(id,user_id,system_prompt) VALUES (1,'owner',''),(2,'other','');
        INSERT INTO assistant_generation_jobs(id,conversation_id,user_id,status) VALUES (zeroblob(16),1,'owner','completed');
        INSERT INTO assistant_messages(conversation_id,role,content,generation_job_id) VALUES (1,'user','legacy',NULL),(1,'user','modern',zeroblob(16));
        WITH RECURSIVE n(i) AS (VALUES(1) UNION ALL SELECT i+1 FROM n WHERE i<20000)
        INSERT INTO assistant_messages(conversation_id,role,content,generation_job_id)
        SELECT 1,'assistant','modern output',zeroblob(16) FROM n;
        INSERT INTO assistant_messages(conversation_id,role,content) VALUES (2,'user','other question'),(1,'assistant','legacy reply');").unwrap();
    for analyze in [false, true] {
        if analyze { connection.execute_batch("ANALYZE").unwrap(); }
        let mut statement = connection.prepare(PAGE_QUESTION_SQL).unwrap();
        let question: i64 = statement.query_row([1, 20002, 20004], |row| row.get(0)).unwrap();
        assert_eq!(question, 1);
        let steps = statement.get_status(rusqlite::StatementStatus::VmStep);
        assert!(steps < 1000, "question lookup walked modern history: {steps} steps");
        let mut plan = connection.prepare(&format!("EXPLAIN QUERY PLAN {PAGE_QUESTION_SQL}")).unwrap();
        let details: Vec<String> = plan.query_map([1, 20002, 20004], |row| row.get(3))
            .unwrap().collect::<Result<_, _>>().unwrap();
        assert_eq!(details.iter().filter(|detail| detail.contains("COVERING INDEX idx_assistant_messages_conversation_id")).count(), 2, "{details:?}");
    }
    // No preceding legacy question must also be a cheap miss; the job's
    // question still completes the page and other conversations stay out.
    connection.execute("DELETE FROM assistant_messages WHERE id=1", []).unwrap();
    let mut statement = connection.prepare(PAGE_QUESTION_SQL).unwrap();
    let question: i64 = statement.query_row([1, 20002, 20004], |row| row.get(0)).unwrap();
    assert_eq!(question, 2);
    assert!(statement.get_status(rusqlite::StatementStatus::VmStep) < 1000);
}

#[tokio::test]
async fn attachment_file_lookup_supports_storage_paths_and_checks_owner() {
    let pool = crate::db::test_pool().await;
    let conversation: i64 =
        query_scalar("INSERT INTO assistant_conversations(user_id,system_prompt) VALUES ('owner','') RETURNING id")
            .fetch_one(&pool).await.unwrap();
    let message: i64 = query_scalar(
        "INSERT INTO assistant_messages(conversation_id,role,content) VALUES (?1,'user','image') RETURNING id",
    ).bind(conversation).fetch_one(&pool).await.unwrap();
    for prefix in ["/srv/firefly/storage/assistant_uploads/", "storage/assistant_uploads/", "custom/uploads/", ""] {
        let file_id = uuid::Uuid::new_v4();
        let storage_path = format!("{prefix}{file_id}");
        query("INSERT INTO assistant_message_attachments(message_id,file_name,content_type,file_size,storage_path) VALUES (?1,'图片.png','image/png',4,?2)")
            .bind(message).bind(&storage_path).execute(&pool).await.unwrap();
        let record = get_attachment_by_file_with_pool(&pool, file_id, "owner").await.unwrap().unwrap();
        assert_eq!(record.storage_path, storage_path);
        assert_eq!(record.content_type, "image/png");
        assert!(get_attachment_by_file_with_pool(&pool, file_id, "other").await.unwrap().is_none());
    }
    assert!(get_attachment_by_file_with_pool(&pool, uuid::Uuid::new_v4(), "owner").await.unwrap().is_none());
}

#[tokio::test]
async fn db_tests_conversation_deletion_blocks_running_jobs_and_serializes_with_claims() {
    let pool = crate::db::test_pool().await;
    crate::db::set_test_pool(pool.clone());
    let conversation: i64 = query_scalar("INSERT INTO assistant_conversations(user_id,agent,system_prompt) VALUES ('owner','claude','') RETURNING id")
        .fetch_one(&pool).await.unwrap();
    let job = uuid::Uuid::new_v4();
    create_queued_user_turn(job, conversation, "owner", true, "hello".into(), &[]).await.unwrap();
    claim_next_queued_generation(conversation, "owner").await.unwrap().unwrap();
    assert!(delete_conversation(conversation, "owner").await.unwrap_err().is::<ConversationDeletionBlocked>());
    assert!(delete_conversation(conversation, "other").await.unwrap().is_none());
    update_generation_status(job, "failed", Some("stopped")).await.unwrap();
    assert!(delete_conversation(conversation, "owner").await.unwrap().is_some());

    // Pure queued turns can still be deleted, and deletion/claim compete
    // under the same writer lock so they can never both succeed.
    let queued: i64 =
        query_scalar("INSERT INTO assistant_conversations(user_id,system_prompt) VALUES ('owner','') RETURNING id")
            .fetch_one(&pool)
            .await
            .unwrap();
    create_queued_user_turn(uuid::Uuid::new_v4(), queued, "owner", true, "queued".into(), &[]).await.unwrap();
    assert!(delete_conversation(queued, "owner").await.unwrap().is_some());
    let competing: i64 =
        query_scalar("INSERT INTO assistant_conversations(user_id,system_prompt) VALUES ('owner','') RETURNING id")
            .fetch_one(&pool)
            .await
            .unwrap();
    create_queued_user_turn(uuid::Uuid::new_v4(), competing, "owner", true, "race".into(), &[]).await.unwrap();
    let (claimed, deleted) =
        tokio::join!(claim_next_queued_generation(competing, "owner"), delete_conversation(competing, "owner"),);
    match (claimed.unwrap(), deleted) {
        (Some(_), Err(error)) => assert!(error.is::<ConversationDeletionBlocked>()),
        (None, Ok(Some(_))) => {}
        result => panic!("claim/delete must serialize: {result:?}"),
    }
}

#[tokio::test]
async fn db_tests_conversation_reasoning_survives_reload() {
    let pool = crate::db::test_pool().await;
    crate::db::set_test_pool(pool.clone());
    let workspace_id = uuid::Uuid::new_v4();
    let conversation = create_conversation("owner", crate::assistant_store::NewConversation {
        system_prompt: "",
        title: None,
        agent: "codex",
        codex_cwd: Some("project"),
        provider: Some("relay"),
        model: Some("model-a"),
        reasoning_level: Some("xhigh"),
        parallel: Some(("source", workspace_id)),
    })
    .await
    .unwrap();
    let saved = get_conversation(conversation.id, "owner").await.unwrap().unwrap();
    assert_eq!(saved.codex_reasoning_level.as_deref(), Some("xhigh"));
    assert_eq!(saved.codex_model.as_deref(), Some("model-a"));
    assert_eq!(saved.workspace_mode, "parallel");
    assert_eq!(saved.source_cwd.as_deref(), Some("source"));
    assert_eq!(saved.workspace_id, Some(workspace_id));
    let legacy: i64 =
        query_scalar("INSERT INTO assistant_conversations(user_id,system_prompt) VALUES ('owner','') RETURNING id")
            .fetch_one(&pool)
            .await
            .unwrap();
    let saved = get_conversation(legacy, "owner").await.unwrap().unwrap();
    assert_eq!(saved.codex_reasoning_level, None);
}

#[tokio::test]
async fn db_tests_empty_assistant_records_do_not_complete_interrupted_turns() {
    let pool = crate::db::test_pool().await;
    crate::db::set_test_pool(pool.clone());
    let conversation: i64 = query_scalar(
        "INSERT INTO assistant_conversations(user_id,agent,system_prompt) VALUES ('owner','firefly','') RETURNING id",
    ).fetch_one(&pool).await.unwrap();
    let job = uuid::Uuid::new_v4();
    create_queued_user_turn(job, conversation, "owner", false, "question".into(), &[]).await.unwrap();
    claim_next_queued_generation(conversation, "owner").await.unwrap().unwrap();
    query("INSERT INTO assistant_messages(conversation_id,generation_job_id,role,content,metadata) VALUES (?1,?2,'assistant','','{}')")
        .bind(conversation).bind(job).execute(&pool).await.unwrap();
    let recovery = requeue_interrupted_generations().await.unwrap();
    assert_eq!((recovery.requeued, recovery.completed, recovery.failed), (1, 0, 0));
    assert_eq!(get_generation_job(job, "owner").await.unwrap().unwrap().status, "queued");
}

#[tokio::test]
async fn db_tests_generation_input_and_restart_recovery() {
    let pool = crate::db::test_pool().await;
    crate::db::set_test_pool(pool.clone());
    let conversation: i64 = query_scalar(
        "INSERT INTO assistant_conversations(user_id,agent,system_prompt) VALUES ('owner','codex','') RETURNING id",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    let job = uuid::Uuid::new_v4();
    query(
        "INSERT INTO assistant_generation_jobs(id,conversation_id,user_id,status) VALUES (?1,?2,'owner','running')",
    )
    .bind(job)
    .bind(conversation)
    .execute(&pool)
    .await
    .unwrap();
    let original = "继续处理原任务\n保留 '引用' 与已有文件";
    let message_id: i64 = query_scalar("INSERT INTO assistant_messages(conversation_id,role,content,generation_job_id) VALUES (?1,'user',?2,?3) RETURNING id")
        .bind(conversation).bind(original).bind(job).fetch_one(&pool).await.unwrap();
    let fresh = fetch_generation_user_message(job, conversation, "owner").await.unwrap().unwrap();
    assert_eq!(fresh.content, original);
    assert_eq!(fresh.processing_status.as_deref(), Some("running"));
    assert!(fetch_generation_user_message(job, conversation, "other").await.unwrap().is_none());
    assert!(fetch_generation_user_message(job, conversation + 1, "owner").await.unwrap().is_none());
    assert!(fetch_generation_user_message(uuid::Uuid::new_v4(), conversation, "owner").await.unwrap().is_none());

    let recovery = requeue_interrupted_generations().await.unwrap();
    assert_eq!((recovery.requeued, recovery.completed, recovery.failed), (1, 0, 0));
    let hint = "\n\n[服务重启恢复] 请从原会话的最新进度继续完成任务，保留并检查已经完成的工作，不要从头重复执行。";
    for _ in 0..2 {
        let restored = fetch_generation_user_message(job, conversation, "owner").await.unwrap().unwrap();
        assert_eq!(restored.content, format!("{original}{hint}"));
        assert_eq!(restored.processing_status.as_deref(), Some("queued"));
        assert_eq!(restored.metadata.unwrap()["restart_recovery"], true);
    }
    assert_eq!(requeue_interrupted_generations().await.unwrap().requeued, 0);
    // Preserve PostgreSQL's previous handling of a string "true", while
    // false, null, and numeric flags must not gain a recovery instruction.
    for (flag, recovering) in [
        (serde_json::json!("true"), true),
        (serde_json::json!(false), false),
        (serde_json::json!("false"), false),
        (serde_json::json!(null), false),
        (serde_json::json!(1), false),
    ] {
        query("UPDATE assistant_messages SET metadata=?1 WHERE id=?2")
            .bind(serde_json::json!({"restart_recovery":flag}))
            .bind(message_id)
            .execute(&pool)
            .await
            .unwrap();
        let loaded = fetch_generation_user_message(job, conversation, "owner").await.unwrap().unwrap();
        assert_eq!(loaded.content, if recovering { format!("{original}{hint}") } else { original.into() });
    }
    let persisted: String = query_scalar("SELECT content FROM assistant_messages WHERE id=?1")
        .bind(message_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(persisted, original);
}

#[tokio::test]
async fn db_tests_failed_turn_context_preserves_tool_facts_and_closes_unknown_results() {
    let pool = crate::db::test_pool().await;
    crate::db::set_test_pool(pool.clone());
    let conversation: i64 =
        query_scalar("INSERT INTO assistant_conversations(user_id,system_prompt) VALUES ('owner','') RETURNING id")
            .fetch_one(&pool)
            .await
            .unwrap();
    let failed = uuid::Uuid::new_v4();
    let without_effects = uuid::Uuid::new_v4();
    let current = uuid::Uuid::new_v4();
    for (id, timestamp) in [(failed, 1_i64), (without_effects, 2), (current, 3)] {
        create_queued_user_turn(id, conversation, "owner", true, "original request".into(), &[]).await.unwrap();
        query("UPDATE assistant_generation_jobs SET status=?2, created_at=?3 WHERE id=?1")
            .bind(id)
            .bind(if id == current { "running" } else { "failed" })
            .bind(timestamp)
            .execute(&pool)
            .await
            .unwrap();
    }
    let calls = serde_json::json!([
        {"id":"known", "name":"write_file", "arguments":{}},
        {"id":"unknown", "name":"send_request", "arguments":{}},
    ]);
    query("INSERT INTO assistant_messages(conversation_id,role,content,tool_calls,generation_job_id) VALUES (?1,'assistant','partial prose',?2,?3)")
        .bind(conversation).bind(calls).bind(failed).execute(&pool).await.unwrap();
    query("INSERT INTO assistant_messages(conversation_id,role,content,tool_call_id,generation_job_id) VALUES (?1,'tool','file was written','known',?2)")
        .bind(conversation).bind(failed).execute(&pool).await.unwrap();
    query("INSERT INTO assistant_messages(conversation_id,role,content,generation_job_id) VALUES (?1,'assistant','unfinished final prose',?2)")
        .bind(conversation).bind(failed).execute(&pool).await.unwrap();
    let context = fetch_generation_context_messages(conversation, "owner", current).await.unwrap();
    assert_eq!(
        context.iter().map(|row| row.role.as_str()).collect::<Vec<_>>(),
        ["user", "assistant", "tool", "tool", "user"]
    );
    assert!(context[0].content.contains("不是重新执行原请求的指令"));
    assert!(context[1].content.is_empty());
    assert_eq!(context[2].content, "file was written");
    assert_eq!(context[3].tool_call_id.as_deref(), Some("unknown"));
    assert!(context[3].is_error);
    assert!(context[3].content.contains("interrupted_unknown_outcome"));
    assert_eq!(context[4].generation_job_id, Some(current));
    assert!(context.iter().all(|row| row.generation_job_id != Some(without_effects)));
    assert_eq!(
        query_scalar::<_, i64>("SELECT COUNT(*) FROM assistant_messages WHERE tool_call_id='unknown'")
            .fetch_one(&pool)
            .await
            .unwrap(),
        0
    );
}

#[tokio::test]
async fn db_tests_queued_user_turn_is_atomic() {
    let pool = crate::db::test_pool().await;
    crate::db::set_test_pool(pool.clone());
    let conversation: i64 =
        query_scalar("INSERT INTO assistant_conversations(user_id,system_prompt) VALUES ('owner','') RETURNING id")
            .fetch_one(&pool)
            .await
            .unwrap();
    let attachment = NewAttachment {
        file_name: "a.txt".into(),
        content_type: "text/plain".into(),
        file_size: 3,
        storage_path: "stored/a.txt".into(),
    };
    let job = uuid::Uuid::new_v4();
    let (message, attachments) =
        create_queued_user_turn(job, conversation, "owner", true, "hi".into(), std::slice::from_ref(&attachment))
            .await
            .unwrap();
    assert_eq!(message.generation_job_id, Some(job));
    assert_eq!(attachments.len(), 1);
    assert_eq!(attachments[0].message_id, message.id);
    let claimed = claim_next_queued_generation_with_pool(&pool, conversation, "owner").await.unwrap().unwrap();
    assert_eq!(claimed.id, job);
    let count: i64 = query_scalar("SELECT message_count FROM assistant_conversations WHERE id=?1")
        .bind(conversation)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 1);

    // A failed attachment insert must not leave a claimable job or an orphan message behind.
    query("CREATE TRIGGER reject_attachment BEFORE INSERT ON assistant_message_attachments BEGIN SELECT RAISE(ABORT, 'rejected'); END")
        .execute(&pool)
        .await
        .unwrap();
    query("UPDATE assistant_generation_jobs SET status='completed' WHERE id=?1")
        .bind(job)
        .execute(&pool)
        .await
        .unwrap();
    let failed = uuid::Uuid::new_v4();
    assert!(create_queued_user_turn(failed, conversation, "owner", true, "again".into(), &[attachment])
        .await
        .is_err());
    assert!(claim_next_queued_generation_with_pool(&pool, conversation, "owner").await.unwrap().is_none());
    let messages: i64 = query_scalar("SELECT COUNT(*) FROM assistant_messages WHERE conversation_id=?1")
        .bind(conversation)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(messages, 1);
}

#[tokio::test]
async fn db_tests_competing_claims_and_recovery() {
    let pool = crate::db::test_pool().await;
    crate::db::set_test_pool(pool.clone());
    let conversation: i64 =
        query_scalar("INSERT INTO assistant_conversations(user_id,system_prompt) VALUES ('owner','') RETURNING id")
            .fetch_one(&pool)
            .await
            .unwrap();
    let first = uuid::Uuid::new_v4();
    let second = uuid::Uuid::new_v4();
    for (job, at) in [(first, 1i64), (second, 2)] {
        query("INSERT INTO assistant_generation_jobs(id,conversation_id,user_id,status,created_at) VALUES (?1,?2,'owner','queued',?3)").bind(job).bind(conversation).bind(at).execute(&pool).await.unwrap();
    }
    let claims = futures_util::future::join_all(
        (0..12).map(|_| claim_next_queued_generation_with_pool(&pool, conversation, "owner")),
    )
    .await;
    let claimed: Vec<_> = claims.into_iter().filter_map(Result::unwrap).collect();
    assert_eq!(claimed.len(), 1);
    assert_eq!(claimed[0].id, first);
    assert!(claim_next_queued_generation_with_pool(&pool, conversation, "other").await.unwrap().is_none());
    query("UPDATE assistant_generation_jobs SET status='completed' WHERE id=?1")
        .bind(first)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        claim_next_queued_generation_with_pool(&pool, conversation, "owner").await.unwrap().unwrap().id,
        second
    );
}

#[tokio::test]
async fn db_tests_conversation_pages() {
    let pool = crate::db::test_pool().await;
    let at = Utc::now();
    let mut ids = Vec::new();
    for (owner, agent, count) in [
        ("owner", "codex", 1),
        ("owner", "claude", 1),
        ("owner", "codex", 1),
        ("other", "codex", 1),
        ("owner", "codex", 1),
        ("owner", "firefly", 1),
        ("owner", "codex", 0),
    ] {
        let id: i64 = query_scalar(
            "INSERT INTO assistant_conversations
             (user_id, agent, system_prompt, message_count, last_message_at, source_cwd)
             VALUES (?1, ?2, '', ?3, ?4, 'project_100%') RETURNING id",
        )
        .bind(owner)
        .bind(agent)
        .bind(count as i64)
        .bind((at).timestamp_micros())
        .fetch_one(&pool)
        .await
        .unwrap();
        ids.push(id);
    }
    let first = list_workspace_conversations(&pool, "owner", false, 2, None, "").await.unwrap();
    assert_eq!(first.iter().map(|r| r.id).collect::<Vec<_>>(), vec![ids[5], ids[4]]);
    // Removing the boundary record cannot shift the next page or invalidate its cursor.
    query("DELETE FROM assistant_conversations WHERE id = ?1").bind(ids[4]).execute(&pool).await.unwrap();
    let second = list_workspace_conversations(&pool, "owner", false, 2, Some((false, at, ids[4])), "").await.unwrap();
    assert_eq!(second.iter().map(|r| r.id).collect::<Vec<_>>(), vec![ids[2], ids[1]]);
    let third = list_workspace_conversations(&pool, "owner", false, 2, Some((false, at, ids[1])), "").await.unwrap();
    assert_eq!(third.iter().map(|r| r.id).collect::<Vec<_>>(), vec![ids[0]]);
    let end = list_workspace_conversations(&pool, "owner", false, 2, Some((false, at, ids[0])), "").await.unwrap();
    assert!(end.is_empty());
    let matched = list_workspace_conversations(&pool, "owner", false, 1, None, " PROJECT_100% ").await.unwrap();
    assert_eq!(matched[0].id, ids[5]);
    assert!(list_workspace_conversations(&pool, "owner", false, 2, None, "missing%").await.unwrap().is_empty());
}

#[tokio::test]
async fn db_tests_parallel_workspace_queue_retry_and_ownership() {
    let pool = crate::db::test_pool().await;
    let workspace_id = uuid::Uuid::new_v4();
    let id: i64 = query_scalar("INSERT INTO assistant_conversations (user_id, agent, system_prompt, codex_cwd, workspace_mode, source_cwd, workspace_id, workspace_state) VALUES ('owner','codex','','.assistant-parallel/test/work','parallel','project',?1,'pending') RETURNING id")
        .bind(workspace_id).fetch_one(&pool).await.unwrap();
    let first = uuid::Uuid::new_v4();
    let second = uuid::Uuid::new_v4();
    for (job, seconds) in [(first, 2), (second, 1)] {
        query("INSERT INTO assistant_generation_jobs (id,conversation_id,user_id,status,enable_tools,created_at) VALUES (?1,?2,'owner','queued',true,CAST(unixepoch('subsec') * 1000000 AS INTEGER)-CAST(?3 * 1000000 AS INTEGER))")
            .bind(job).bind(id).bind(seconds as f64).execute(&pool).await.unwrap();
        query("INSERT INTO assistant_messages (conversation_id,role,content,generation_job_id) VALUES (?1,'user','retained question',?2)")
            .bind(id).bind(job).execute(&pool).await.unwrap();
    }
    assert!(claim_next_queued_generation_with_pool(&pool, id, "other").await.unwrap().is_none());
    assert_eq!(claim_next_queued_generation_with_pool(&pool, id, "owner").await.unwrap().unwrap().id, first);
    assert!(claim_next_queued_generation_with_pool(&pool, id, "owner").await.unwrap().is_none());
    query("UPDATE assistant_conversations SET workspace_state='failed', workspace_metadata=?2 WHERE id=?1")
        .bind(id)
        .bind(serde_json::json!({"job_id":first}))
        .execute(&pool)
        .await
        .unwrap();
    assert!(!retry_workspace_with_pool(&pool, id, "owner").await.unwrap());
    query("UPDATE assistant_generation_jobs SET status='failed' WHERE id=?1")
        .bind(first)
        .execute(&pool)
        .await
        .unwrap();
    query("UPDATE assistant_messages SET metadata=json('{\"cancellation_requested\":true}') WHERE generation_job_id=?1")
        .bind(first).execute(&pool).await.unwrap();
    for state in ["failed", "cancelled", "missing"] {
        query("UPDATE assistant_conversations SET workspace_state=?2 WHERE id=?1")
            .bind(id)
            .bind(state)
            .execute(&pool)
            .await
            .unwrap();
        assert!(claim_next_queued_generation_with_pool(&pool, id, "owner").await.unwrap().is_none());
    }
    assert!(!retry_workspace_with_pool(&pool, id, "other").await.unwrap());
    // Missing workspaces can only be checked again: they never return to pending/reset.
    assert!(retry_workspace_with_pool(&pool, id, "owner").await.unwrap());
    assert!(!query_scalar::<_, bool>("SELECT EXISTS(SELECT 1 FROM assistant_messages WHERE generation_job_id=?1 AND json_extract(metadata, '$.cancellation_requested')=1)")
        .bind(first).fetch_one(&pool).await.unwrap());
    let state: String = query_scalar("SELECT workspace_state FROM assistant_conversations WHERE id=?1")
        .bind(id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(state, "ready");
    assert_eq!(claim_next_queued_generation_with_pool(&pool, id, "owner").await.unwrap().unwrap().id, first);
    query("UPDATE assistant_generation_jobs SET status='failed' WHERE id=?1")
        .bind(first)
        .execute(&pool)
        .await
        .unwrap();
    query("UPDATE assistant_conversations SET workspace_state='cancelled' WHERE id=?1")
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
    assert!(retry_workspace_with_pool(&pool, id, "owner").await.unwrap());
    let state: String = query_scalar("SELECT workspace_state FROM assistant_conversations WHERE id=?1")
        .bind(id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(state, "pending");
    let count: i64 = query_scalar("SELECT COUNT(*) FROM assistant_messages WHERE conversation_id=?1")
        .bind(id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 2);
    query("UPDATE assistant_conversations SET workspace_state='failed' WHERE id=?1")
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
    assert!(!retry_workspace_with_pool(&pool, id, "other").await.unwrap());
    let shared: (String, String) = sqlx::query_as("INSERT INTO assistant_conversations (user_id,system_prompt) VALUES ('legacy','') RETURNING workspace_mode,workspace_state")
        .fetch_one(&pool).await.unwrap();
    assert_eq!(shared, ("shared".into(), "ready".into()));
}

#[tokio::test]
async fn db_tests_weekly_batch_preserves_scope_and_message_order() {
    let pool = crate::db::test_pool().await;
    let since = DateTime::parse_from_rfc3339("2026-09-07T00:00:00Z").unwrap().with_timezone(&Utc);
    let mut ids = Vec::new();
    for (owner, agent) in [
        ("owner", "codex"),
        ("owner", "claude"),
        ("other", "codex"),
        ("owner", "firefly"),
        ("owner", "codex"),
    ] {
        let id: i64 = sqlx::query_scalar(
            "INSERT INTO assistant_conversations (user_id, agent, system_prompt, last_message_at)
             VALUES (?1, ?2, '', ?3) RETURNING id",
        )
        .bind(owner)
        .bind(agent)
        .bind((since).timestamp_micros())
        .fetch_one(&pool)
        .await
        .unwrap();
        ids.push(id);
    }
    for id in &ids {
        for (role, content, days) in [
            ("user", "outside week", -1),
            ("assistant", "older reply", -1),
            ("user", "first question", 0),
            ("user", "second question", 1),
            ("assistant", "latest reply", 1),
            ("assistant", "   ", 2),
            ("user", "  ", 2),
        ] {
            sqlx::query("INSERT INTO assistant_messages (conversation_id, role, content, created_at) VALUES (?1, ?2, ?3, ?4)")
                .bind(id).bind(role).bind(content).bind((since + chrono::Duration::days(days)).timestamp_micros())
                .execute(&pool).await.unwrap();
        }
    }
    // A conversation's latest reply may predate this week.
    sqlx::query("DELETE FROM assistant_messages WHERE conversation_id = ?1 AND created_at >= ?2")
        .bind(ids[1])
        .bind((since).timestamp_micros())
        .execute(&pool)
        .await
        .unwrap();
    // Empty/tool-only conversations remain omitted.
    sqlx::query("UPDATE assistant_messages SET role = 'tool' WHERE conversation_id = ?1")
        .bind(ids[4])
        .execute(&pool)
        .await
        .unwrap();
    let records = fetch_weekly_agent_work_with_pool(&pool, "owner", since).await.unwrap();
    assert_eq!(records.iter().map(|r| r.conversation_id).collect::<Vec<_>>(), vec![ids[0], ids[1]]);
    assert_eq!(records[0].user_messages, ["first question", "second question"]);
    assert_eq!(records[0].last_assistant_reply.as_deref(), Some("latest reply"));
    assert!(records[1].user_messages.is_empty());
    assert_eq!(records[1].last_assistant_reply.as_deref(), Some("older reply"));
    assert!(fetch_weekly_agent_work_with_pool(&pool, "missing", since).await.unwrap().is_empty());
}

#[tokio::test]
async fn db_tests_message_page_completes_bubble_boundaries() {
    let pool = crate::db::test_pool().await;
    let conversation: i64 = sqlx::query_scalar(
        "INSERT INTO assistant_conversations (user_id, system_prompt) VALUES ('owner', '') RETURNING id",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    sqlx::query(
        "WITH RECURSIVE series(i) AS (VALUES(1) UNION ALL SELECT i+1 FROM series WHERE i<4000) INSERT INTO assistant_messages (conversation_id, role, content)
        SELECT ?1, CASE WHEN i % 4 = 1 THEN 'user' WHEN i % 4 = 3 THEN 'tool' ELSE 'assistant' END,
            CASE WHEN i % 4 = 2 THEN '' ELSE 'message message message' END
        FROM series ORDER BY i",
    )
    .bind(conversation)
    .execute(&pool)
    .await
    .unwrap();
    for before in [None, Some(3900), Some(4), Some(i64::MIN)] {
        for limit in [0, 1, 2, 20] {
            // The bubble budget selects a suffix, then the fixture's
            // four-record turns must be completed back to their question.
            let budget_ids: Vec<i64> = sqlx::query_scalar("WITH weighted AS (
                SELECT id, CASE WHEN role = 'user' OR (role = 'assistant' AND trim(content) <> '') THEN 1 ELSE 0 END AS weight
                FROM assistant_messages WHERE conversation_id = ?1 AND (?2 IS NULL OR id < ?2)
            ), counted AS (SELECT *, SUM(weight) OVER (ORDER BY id DESC) AS total FROM weighted)
            SELECT id FROM counted WHERE total - weight <= ?3 ORDER BY id")
                .bind(conversation).bind(before).bind(limit).fetch_all(&pool).await.unwrap();
            let expected: Vec<i64> = match (budget_ids.first(), budget_ids.last()) {
                (Some(&first), Some(&last)) => (((first - 1) / 4 * 4 + 1)..=last).collect(),
                _ => Vec::new(),
            };
            let page =
                fetch_messages_page_with_pool(&pool, conversation, "owner", limit, before).await.unwrap().unwrap();
            assert_eq!(page.messages.iter().map(|m| m.id).collect::<Vec<_>>(), expected);
            assert_eq!(page.has_more, expected.first().is_some_and(|&id| id > 1));
        }
    }
    // 无归属的用户拿到 None（handler 映射为 404），而不是空页
    assert!(fetch_messages_page_with_pool(&pool, conversation, "other", 20, None).await.unwrap().is_none());
    sqlx::query("UPDATE assistant_messages SET role = 'tool'").execute(&pool).await.unwrap();
    let tools = fetch_messages_page_with_pool(&pool, conversation, "owner", 1, Some(5)).await.unwrap().unwrap();
    assert_eq!(tools.messages.len(), 4);
    assert!(!tools.has_more);
}

#[tokio::test]
async fn db_tests_message_page_completes_interleaved_jobs_without_gaps() {
    let pool = crate::db::test_pool().await;
    let conversation: i64 = query_scalar(
        "INSERT INTO assistant_conversations (user_id, system_prompt) VALUES ('owner', '') RETURNING id",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    let a = uuid::Uuid::new_v4();
    let b = uuid::Uuid::new_v4();
    let c = uuid::Uuid::new_v4();
    for job in [a, b, c] {
        query("INSERT INTO assistant_generation_jobs (id, conversation_id, user_id, status) VALUES (?1, ?2, 'owner', 'completed')")
            .bind(job).bind(conversation).execute(&pool).await.unwrap();
    }
    let mut ids = Vec::new();
    // Completing C includes A's output, and completing A includes B's
    // output. B's question must also be included, despite the queue order.
    for (role, content, job) in [
        ("user", "older question", None),
        ("assistant", "older answer", None),
        ("user", "question B", Some(b)),
        ("user", "question A", Some(a)),
        ("tool", "output B", Some(b)),
        ("user", "question C", Some(c)),
        ("tool", "output A", Some(a)),
        ("tool", "output C", Some(c)),
        ("assistant", "answer C", Some(c)),
    ] {
        let id: i64 = query_scalar("INSERT INTO assistant_messages (conversation_id, role, content, generation_job_id) VALUES (?1, ?2, ?3, ?4) RETURNING id")
            .bind(conversation).bind(role).bind(content).bind(job).fetch_one(&pool).await.unwrap();
        ids.push(id);
    }
    let newest = fetch_messages_page_with_pool(&pool, conversation, "owner", 1, None).await.unwrap().unwrap();
    assert_eq!(newest.messages.iter().map(|m| m.id).collect::<Vec<_>>(), ids[2..]);
    assert!(newest.has_more);
    // Appending new messages cannot move the stored cursor or duplicate
    // any of the already loaded turns on the next page.
    query("INSERT INTO assistant_messages (conversation_id, role, content) VALUES (?1, 'user', 'new question')")
        .bind(conversation)
        .execute(&pool)
        .await
        .unwrap();
    let older =
        fetch_messages_page_with_pool(&pool, conversation, "owner", 1, Some(ids[2])).await.unwrap().unwrap();
    assert_eq!(older.messages.iter().map(|m| m.id).collect::<Vec<_>>(), ids[..2]);
    assert!(!older.has_more);
    assert!(fetch_messages_page_with_pool(&pool, conversation, "other", 1, None).await.unwrap().is_none());
}
