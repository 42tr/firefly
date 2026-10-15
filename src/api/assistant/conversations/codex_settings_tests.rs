use super::*;
use axum::http::StatusCode;

async fn update(id: i64, user: &str, settings: Value) -> Result<ConversationSummary, AppError> {
    update_codex_settings(Session::for_storage(user), Path(id), Json(serde_json::from_value(settings).unwrap()))
        .await.map(|response| response.0)
}

#[tokio::test]
async fn db_tests_codex_settings_preserve_thread_history_workspace_and_survive_reload() {
    let pool = crate::db::test_pool().await;
    crate::db::set_test_pool(pool.clone());
    let conversation = assistant_store::create_conversation("owner", crate::assistant_store::NewConversation {
        system_prompt: "prompt",
        title: Some("existing conversation"),
        agent: "codex",
        codex_cwd: Some("project"),
        provider: Some("previous"),
        model: Some("old-model"),
        reasoning_level: Some("low"),
        parallel: None,
    }).await.unwrap();
    assistant_store::set_codex_thread_id(conversation.id, "owner", "same-thread").await.unwrap();
    sqlx::query("INSERT INTO assistant_messages(conversation_id, role, content) VALUES (?1, 'assistant', 'keep history')")
        .bind(conversation.id).execute(&pool).await.unwrap();
    let updated = update(conversation.id, "owner", json!({
        "provider": " openai ", "model": " new-model ", "reasoning_level": " high ",
    })).await.unwrap();
    assert_eq!(updated.id, conversation.id);
    assert_eq!(updated.title, conversation.title);
    assert_eq!(updated.cwd.as_deref(), Some("project"));
    assert_eq!(updated.codex_provider.as_deref(), Some("openai"));
    assert_eq!(updated.codex_model.as_deref(), Some("new-model"));
    assert_eq!(updated.codex_reasoning_level.as_deref(), Some("high"));
    let stored = assistant_store::get_conversation(conversation.id, "owner").await.unwrap().unwrap();
    assert_eq!(stored.codex_thread_id.as_deref(), Some("same-thread"));
    assert_eq!(stored.system_prompt, "prompt");
    assert_eq!(stored.codex_model, updated.codex_model);
    let history: String = sqlx::query_scalar("SELECT content FROM assistant_messages WHERE conversation_id=?1")
        .bind(conversation.id).fetch_one(&pool).await.unwrap();
    assert_eq!(history, "keep history");
    let defaults = update(conversation.id, "owner", json!({"provider": "", "model": null, "reasoning_level": " "})).await.unwrap();
    assert!(defaults.codex_provider.is_none());
    assert!(defaults.codex_model.is_none());
    assert!(defaults.codex_reasoning_level.is_none());
    let stored = assistant_store::get_conversation(conversation.id, "owner").await.unwrap().unwrap();
    assert!(stored.codex_model.is_none());
    assert_eq!(stored.codex_thread_id.as_deref(), Some("same-thread"));
}

#[tokio::test]
async fn db_tests_codex_settings_reject_archived_foreign_and_invalid_updates() {
    let pool = crate::db::test_pool().await;
    crate::db::set_test_pool(pool.clone());
    let conversation = assistant_store::create_conversation("owner", crate::assistant_store::NewConversation {
        system_prompt: "",
        title: None,
        agent: "codex",
        codex_cwd: Some("project"),
        provider: Some("openai"),
        model: Some("original"),
        reasoning_level: Some("low"),
        parallel: None,
    }).await.unwrap();
    let id = conversation.id;
    let settings = || json!({"provider":"openai", "model":"changed", "reasoning_level":"high"});
    assert_eq!(update(id, "other", settings()).await.unwrap_err().status(), StatusCode::NOT_FOUND);
    assert_eq!(update(i64::MAX, "owner", settings()).await.unwrap_err().status(), StatusCode::NOT_FOUND);
    assert_eq!(update(id, "owner", json!({"provider":"__unknown_firefly_test_provider__"})).await.unwrap_err().status(), StatusCode::BAD_REQUEST);
    assert_eq!(update(id, "owner", json!({"reasoning_level":"invalid effort"})).await.unwrap_err().status(), StatusCode::BAD_REQUEST);
    assistant_store::set_conversation_archived(id, "owner", true).await.unwrap();
    assert_eq!(update(id, "owner", settings()).await.unwrap_err().status(), StatusCode::CONFLICT);
    assistant_store::set_conversation_archived(id, "owner", false).await.unwrap();
    let stored = assistant_store::get_conversation(id, "owner").await.unwrap().unwrap();
    assert_eq!(stored.codex_model.as_deref(), Some("original"));
    sqlx::query("UPDATE assistant_conversations SET agent='claude' WHERE id=?1").bind(id).execute(&pool).await.unwrap();
    assert_eq!(update(id, "owner", settings()).await.unwrap_err().status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn db_tests_codex_settings_can_change_while_busy_without_changing_submitted_messages() {
    let pool = crate::db::test_pool().await;
    crate::db::set_test_pool(pool.clone());
    let conversation = assistant_store::create_conversation("owner", crate::assistant_store::NewConversation {
        system_prompt: "",
        title: None,
        agent: "codex",
        codex_cwd: Some("project"),
        provider: Some("openai"),
        model: Some("original"),
        reasoning_level: Some("low"),
        parallel: None,
    }).await.unwrap();
    let id = conversation.id;
    assistant_store::set_codex_thread_id(id, "owner", "original-thread").await.unwrap();
    let request_id = uuid::Uuid::new_v4();
    let fingerprint = "a".repeat(64);
    let first = assistant_store::submit_queued_user_turn(id, "owner", Some(request_id), &fingerprint, true, "first question".into(), &[]).await.unwrap();
    let first_id = first.message.generation_job_id.unwrap();
    let queued_update = update(id, "owner", json!({"provider":"openai", "model":"queued-change", "reasoning_level":"medium"})).await.unwrap();
    assert_eq!(queued_update.status, "running");
    let second = assistant_store::submit_queued_user_turn(id, "owner", None, "second", true, "second question".into(), &[]).await.unwrap();
    let second_id = second.message.generation_job_id.unwrap();
    let running = assistant_store::claim_next_queued_generation(id, "owner").await.unwrap().unwrap();
    assert_eq!(running.id, first_id);
    let expected = assistant_store::CodexGenerationSettings {
        provider: Some("openai".into()), model: Some("original".into()), reasoning_level: Some("low".into()),
    };
    assert_eq!(running.codex_settings.as_ref().unwrap().0, expected);
    let running_update = update(id, "owner", json!({"provider":"openai", "model":"running-change", "reasoning_level":"high"})).await.unwrap();
    assert_eq!(running_update.status, "running");
    let third = assistant_store::submit_queued_user_turn(id, "owner", None, "third", true, "third question".into(), &[]).await.unwrap();
    let third_id = third.message.generation_job_id.unwrap();
    let retry = assistant_store::submit_queued_user_turn(id, "owner", Some(request_id), &fingerprint, true, "first question".into(), &[]).await.unwrap();
    assert!(retry.replayed);
    assert_eq!(retry.message.generation_job_id, Some(first_id));
    // Apply the durable job snapshot to the live conversation exactly as the worker does.
    let mut execution = assistant_store::get_conversation(id, "owner").await.unwrap().unwrap();
    assert_eq!(execution.codex_model.as_deref(), Some("running-change"));
    running.codex_settings.as_ref().unwrap().apply_to(&mut execution);
    assert_eq!(execution.codex_model.as_deref(), Some("original"));
    assert_eq!(execution.codex_reasoning_level.as_deref(), Some("low"));
    assert_eq!(execution.codex_cwd.as_deref(), Some("project"));
    assert_eq!(execution.codex_thread_id.as_deref(), Some("original-thread"));
    let recovered = assistant_store::requeue_interrupted_generations().await.unwrap();
    assert_eq!(recovered.requeued, 1);
    let restarted = assistant_store::claim_next_queued_generation(id, "owner").await.unwrap().unwrap();
    assert_eq!(restarted.id, first_id);
    assert_eq!(restarted.codex_settings.as_ref().unwrap().0, expected);
    assistant_store::update_generation_status(first_id, "completed", None).await.unwrap();
    let next = assistant_store::claim_next_queued_generation(id, "owner").await.unwrap().unwrap();
    assert_eq!(next.id, second_id);
    assert_eq!(next.codex_settings.as_ref().unwrap().model.as_deref(), Some("queued-change"));
    assert_eq!(next.codex_settings.as_ref().unwrap().reasoning_level.as_deref(), Some("medium"));
    let newest = assistant_store::get_generation_job(third_id, "owner").await.unwrap().unwrap();
    assert_eq!(newest.codex_settings.as_ref().unwrap().model.as_deref(), Some("running-change"));
    assert_eq!(newest.codex_settings.as_ref().unwrap().reasoning_level.as_deref(), Some("high"));
}

#[tokio::test]
async fn db_tests_codex_settings_and_submission_snapshot_are_serialized() {
    let pool = crate::db::test_pool().await;
    crate::db::set_test_pool(pool.clone());
    for send_first in [true, false] {
        let conversation = assistant_store::create_conversation("owner", crate::assistant_store::NewConversation {
            system_prompt: "",
            title: None,
            agent: "codex",
            codex_cwd: Some("project"),
            provider: Some("openai"),
            model: Some("original"),
            reasoning_level: Some("low"),
            parallel: None,
        }).await.unwrap();
        let save = update(conversation.id, "owner", json!({"provider":"openai", "model":"changed", "reasoning_level":"high"}));
        let send = assistant_store::submit_queued_user_turn(conversation.id, "owner", None, "", true, "next turn".into(), &[]);
        let submitted = if send_first {
            let (submitted, updated) = tokio::join!(send, save);
            updated.unwrap(); submitted.unwrap()
        } else {
            let (updated, submitted) = tokio::join!(save, send);
            updated.unwrap(); submitted.unwrap()
        };
        let job = assistant_store::get_generation_job(submitted.message.generation_job_id.unwrap(), "owner").await.unwrap().unwrap();
        let snapshot = &job.codex_settings.as_ref().unwrap().0;
        match snapshot.model.as_deref() {
            Some("original") => assert_eq!(snapshot.reasoning_level.as_deref(), Some("low")),
            Some("changed") => assert_eq!(snapshot.reasoning_level.as_deref(), Some("high")),
            _ => panic!("Submission received a mixed settings snapshot"),
        }
        let stored = assistant_store::get_conversation(conversation.id, "owner").await.unwrap().unwrap();
        assert_eq!(stored.codex_model.as_deref(), Some("changed"));
        let next = assistant_store::submit_queued_user_turn(conversation.id, "owner", None, "", true, "after save".into(), &[]).await.unwrap();
        let next_job = assistant_store::get_generation_job(next.message.generation_job_id.unwrap(), "owner").await.unwrap().unwrap();
        assert_eq!(next_job.codex_settings.as_ref().unwrap().model.as_deref(), Some("changed"));
    }
}
