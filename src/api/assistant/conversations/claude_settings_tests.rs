use super::*;
use axum::http::StatusCode;

async fn update(id: i64, user: &str, model: Option<&str>) -> Result<ConversationSummary, AppError> {
    update_claude_settings(
        Session::for_storage(user),
        Path(id),
        Json(ClaudeSettingsRequest {
            model: model.map(str::to_owned),
            provider: None,
            reasoning_level: None,
        }),
    )
    .await
    .map(|response| response.0)
}

async fn fixture() -> (sqlx::SqlitePool, assistant_store::ConversationRecord) {
    crate::config::init_test_config();
    let pool = crate::db::test_pool().await;
    crate::db::set_test_pool(pool.clone());
    let conversation = assistant_store::create_conversation("owner", crate::assistant_store::NewConversation {
        system_prompt: "prompt",
        title: Some("existing"),
        agent: "claude",
        codex_cwd: Some("project"),
        provider: Some("cli"),
        model: Some("sonnet"),
        reasoning_level: Some("low"),
        parallel: None,
    })
    .await
    .unwrap();
    assistant_store::set_claude_session_id(conversation.id, "owner", "same-session")
        .await
        .unwrap();
    (pool, conversation)
}

#[tokio::test]
async fn claude_platform_and_effort_updates_are_validated_and_snapshotted_together() {
    let (_pool, conversation) = fixture().await;
    let save = |provider: Option<&str>, model: Option<&str>, level: Option<&str>| {
        update_claude_settings(Session::for_storage("owner"), Path(conversation.id), Json(ClaudeSettingsRequest {
            provider: provider.map(str::to_owned), model: model.map(str::to_owned), reasoning_level: level.map(str::to_owned),
        }))
    };
    for (provider, level) in [(Some("unknown"), Some("high")), (Some("cli"), Some("invalid"))] {
        assert_eq!(save(provider, Some("opus"), level).await.unwrap_err().status(), StatusCode::BAD_REQUEST);
    }
    let updated = save(Some(" cli "), Some(" opus "), Some(" max ")).await.unwrap().0;
    assert_eq!(updated.claude_provider.as_deref(), Some("cli"));
    assert_eq!(updated.claude_reasoning_level.as_deref(), Some("max"));
    assert!(updated.codex_provider.is_none() && updated.codex_reasoning_level.is_none());
    let submitted = assistant_store::submit_queued_user_turn(conversation.id, "owner", None, "", true, "question".into(), &[])
        .await.unwrap();
    let _ = save(None, None, None).await.unwrap();
    let stored = assistant_store::get_conversation(conversation.id, "owner").await.unwrap().unwrap();
    assert!(stored.claude_provider.is_none() && stored.claude_reasoning_level.is_none() && stored.claude_model.is_none());
    let job = assistant_store::get_generation_job(submitted.message.generation_job_id.unwrap(), "owner").await.unwrap().unwrap();
    let settings = job.claude_settings.unwrap();
    assert_eq!(settings.provider.as_deref(), Some("cli"));
    assert_eq!(settings.model.as_deref(), Some("opus"));
    assert_eq!(settings.reasoning_level.as_deref(), Some("max"));
    let mut execution = stored;
    settings.apply_to(&mut execution);
    assert_eq!(execution.claude_provider.as_deref(), Some("cli"));
    assert_eq!(execution.claude_reasoning_level.as_deref(), Some("max"));
    assert_eq!(execution.claude_session_id.as_deref(), Some("same-session"));
}

#[tokio::test]
async fn concurrent_claude_platform_effort_and_model_updates_do_not_mix_snapshots() {
    let (_pool, conversation) = fixture().await;
    let (updated, submitted) = tokio::join!(
        update_claude_settings(Session::for_storage("owner"), Path(conversation.id), Json(ClaudeSettingsRequest {
            provider: Some("cli".into()), model: Some("opus".into()), reasoning_level: Some("high".into()),
        })),
        assistant_store::submit_queued_user_turn(conversation.id, "owner", None, "", true, "question".into(), &[]),
    );
    let _ = updated.unwrap();
    let job = assistant_store::get_generation_job(submitted.unwrap().message.generation_job_id.unwrap(), "owner").await.unwrap().unwrap();
    let settings = job.claude_settings.unwrap();
    match settings.model.as_deref() {
        Some("opus") => {
            assert_eq!(settings.provider.as_deref(), Some("cli"));
            assert_eq!(settings.reasoning_level.as_deref(), Some("high"));
        }
        Some("sonnet") => {
            assert_eq!(settings.provider.as_deref(), Some("cli"));
            assert_eq!(settings.reasoning_level.as_deref(), Some("low"));
        }
        other => panic!("unexpected model: {other:?}"),
    }
}

#[tokio::test]
async fn claude_model_updates_preserve_session_history_workspace_and_can_restore_defaults() {
    let (pool, conversation) = fixture().await;
    sqlx::query("INSERT INTO assistant_messages(conversation_id,role,content) VALUES (?1,'assistant','keep history')")
        .bind(conversation.id).execute(&pool).await.unwrap();
    let updated = update(conversation.id, "owner", Some(" opus "))
        .await
        .unwrap();
    assert_eq!(updated.claude_model.as_deref(), Some("opus"));
    assert_eq!(updated.cwd, conversation.codex_cwd);
    assert!(updated.codex_model.is_none());
    let stored = assistant_store::get_conversation(conversation.id, "owner")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.claude_session_id.as_deref(), Some("same-session"));
    assert_eq!(stored.system_prompt, "prompt");
    let history: String =
        sqlx::query_scalar("SELECT content FROM assistant_messages WHERE conversation_id=?1")
            .bind(conversation.id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(history, "keep history");
    let defaults = update(conversation.id, "owner", Some(" ")).await.unwrap();
    assert!(defaults.claude_model.is_none());
}

#[tokio::test]
async fn claude_settings_reject_foreign_archived_wrong_agent_and_invalid_model_updates() {
    let (pool, conversation) = fixture().await;
    let id = conversation.id;
    assert_eq!(
        update(id, "other", Some("opus"))
            .await
            .unwrap_err()
            .status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        update(i64::MAX, "owner", Some("opus"))
            .await
            .unwrap_err()
            .status(),
        StatusCode::NOT_FOUND
    );
    for model in [
        "invalid model".to_owned(),
        "bad\nmodel".to_owned(),
        "--help".to_owned(),
        "x".repeat(257),
    ] {
        assert_eq!(
            update(id, "owner", Some(&model))
                .await
                .unwrap_err()
                .status(),
            StatusCode::BAD_REQUEST
        );
    }
    assistant_store::set_conversation_archived(id, "owner", true)
        .await
        .unwrap();
    assert_eq!(
        update(id, "owner", Some("opus"))
            .await
            .unwrap_err()
            .status(),
        StatusCode::CONFLICT
    );
    sqlx::query("UPDATE assistant_conversations SET agent='codex',is_archived=FALSE WHERE id=?1")
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        update(id, "owner", Some("opus"))
            .await
            .unwrap_err()
            .status(),
        StatusCode::BAD_REQUEST
    );
}

#[tokio::test]
async fn queued_running_retried_and_recovered_claude_messages_keep_their_model_snapshots() {
    let (_pool, conversation) = fixture().await;
    let id = conversation.id;
    let request = uuid::Uuid::new_v4();
    let fingerprint = "a".repeat(64);
    let first = assistant_store::submit_queued_user_turn(
        id,
        "owner",
        Some(request),
        &fingerprint,
        true,
        "first".into(),
        &[],
    )
    .await
    .unwrap();
    let first_id = first.message.generation_job_id.unwrap();
    assert_eq!(
        update(id, "owner", Some("opus")).await.unwrap().status,
        "running"
    );
    let second =
        assistant_store::submit_queued_user_turn(id, "owner", None, "", true, "second".into(), &[])
            .await
            .unwrap();
    let running = assistant_store::claim_next_queued_generation(id, "owner")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(running.id, first_id);
    assert_eq!(
        running.claude_settings.as_ref().unwrap().model.as_deref(),
        Some("sonnet")
    );
    assert!(running.codex_settings.is_none());
    assert_eq!(running.claude_settings.as_ref().unwrap().provider.as_deref(), Some("cli"));
    assert_eq!(running.claude_settings.as_ref().unwrap().reasoning_level.as_deref(), Some("low"));
    update(id, "owner", Some("haiku")).await.unwrap();
    let retry = assistant_store::submit_queued_user_turn(
        id,
        "owner",
        Some(request),
        &fingerprint,
        true,
        "first".into(),
        &[],
    )
    .await
    .unwrap();
    assert!(retry.replayed);
    assert_eq!(retry.message.generation_job_id, Some(first_id));
    let mut execution = assistant_store::get_conversation(id, "owner")
        .await
        .unwrap()
        .unwrap();
    running
        .claude_settings
        .as_ref()
        .unwrap()
        .apply_to(&mut execution);
    assert_eq!(execution.claude_model.as_deref(), Some("sonnet"));
    assert_eq!(execution.claude_provider.as_deref(), Some("cli"));
    assert_eq!(execution.claude_reasoning_level.as_deref(), Some("low"));
    assert_eq!(execution.claude_session_id.as_deref(), Some("same-session"));
    assert_eq!(
        assistant_store::requeue_interrupted_generations()
            .await
            .unwrap()
            .requeued,
        1
    );
    let recovered = assistant_store::claim_next_queued_generation(id, "owner")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(recovered.id, first_id);
    assert_eq!(recovered.claude_settings, running.claude_settings);
    assistant_store::update_generation_status(first_id, "completed", None)
        .await
        .unwrap();
    let queued = assistant_store::claim_next_queued_generation(id, "owner")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(queued.id, second.message.generation_job_id.unwrap());
    assert_eq!(
        queued.claude_settings.as_ref().unwrap().model.as_deref(),
        Some("opus")
    );
    update(id, "owner", None).await.unwrap();
    let defaults = assistant_store::submit_queued_user_turn(
        id,
        "owner",
        None,
        "",
        true,
        "default".into(),
        &[],
    )
    .await
    .unwrap();
    let job =
        assistant_store::get_generation_job(defaults.message.generation_job_id.unwrap(), "owner")
            .await
            .unwrap()
            .unwrap();
    assert!(job.claude_settings.as_ref().unwrap().model.is_none());
}

#[tokio::test]
async fn concurrent_claude_setting_updates_and_submission_have_atomic_snapshots() {
    let (_pool, conversation) = fixture().await;
    let (saved, submitted) = tokio::join!(
        update(conversation.id, "owner", Some("opus")),
        assistant_store::submit_queued_user_turn(
            conversation.id,
            "owner",
            None,
            "",
            true,
            "question".into(),
            &[]
        )
    );
    saved.unwrap();
    let first = assistant_store::get_generation_job(
        submitted.unwrap().message.generation_job_id.unwrap(),
        "owner",
    )
    .await
    .unwrap()
    .unwrap();
    assert!(matches!(
        first.claude_settings.as_ref().unwrap().model.as_deref(),
        Some("sonnet" | "opus")
    ));
    let after = assistant_store::submit_queued_user_turn(
        conversation.id,
        "owner",
        None,
        "",
        true,
        "after save".into(),
        &[],
    )
    .await
    .unwrap();
    let job =
        assistant_store::get_generation_job(after.message.generation_job_id.unwrap(), "owner")
            .await
            .unwrap()
            .unwrap();
    assert_eq!(
        job.claude_settings.as_ref().unwrap().model.as_deref(),
        Some("opus")
    );
}
