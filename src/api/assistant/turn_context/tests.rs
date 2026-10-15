use super::*;

struct Summary;

#[async_trait]
impl Model for Summary {
    async fn generate(&self, _: g::ModelRequest) -> Result<g::ModelResponse, g::ModelError> {
        panic!("reading a saved legacy summary must not call the model");
    }
}

#[tokio::test]
async fn legacy_completed_reply_summary_is_reused_and_scoped_to_branch_point() {
    let pool = crate::db::test_pool().await;
    crate::db::set_test_pool(pool.clone());
    let conversation: i64 = sqlx::query_scalar(
        "INSERT INTO assistant_conversations(user_id, agent, system_prompt) VALUES ('owner', 'firefly', '') RETURNING id",
    ).fetch_one(&pool).await.unwrap();
    let first = uuid::Uuid::new_v4();
    assistant_store::create_queued_user_turn(
        first,
        conversation,
        "owner",
        false,
        "检查项目 A".into(),
        &[],
    )
    .await
    .unwrap();
    assistant_store::claim_next_queued_generation(conversation, "owner")
        .await
        .unwrap()
        .unwrap();
    let answer = assistant_store::append_message(
        conversation,
        assistant_store::StoredMessage {
            role: "assistant".into(),
            content: "已检查，可以部署".into(),
            tool_calls: None,
            tool_call_id: None,
            metadata: Some(json!({"usage":{"input_tokens":10},
                "context_summary":{"version":1,"content":"项目 A 已检查，待继续部署。"}})),
            is_error: false,
            generation_job_id: Some(first),
        },
    )
    .await
    .unwrap();
    let config = AssistantConfig {
        model: Arc::new(Summary),
        budget: llm::ModelBudget::fixed(32768),
        system_prompt: String::new(),
        agent_settings: None,
    };
    let metadata: Value = sqlx::query_scalar("SELECT metadata FROM assistant_messages WHERE id=?1")
        .bind(answer.id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(metadata["usage"]["input_tokens"], 10);
    assert_eq!(metadata["context_summary"]["version"], 1);

    // A running turn's summary must never replace its user message on recovery.
    let mut history =
        assistant_store::fetch_generation_context_messages(conversation, "owner", first)
            .await
            .unwrap();
    assert!(restore(&mut history).is_none());
    assert_eq!(history.len(), 2);
    sqlx::query("UPDATE assistant_generation_jobs SET status='completed' WHERE id=?1")
        .bind(first)
        .execute(&pool)
        .await
        .unwrap();
    let second = uuid::Uuid::new_v4();
    assistant_store::create_queued_user_turn(
        second,
        conversation,
        "owner",
        false,
        "继续部署".into(),
        &[],
    )
    .await
    .unwrap();
    assistant_store::claim_next_queued_generation(conversation, "owner")
        .await
        .unwrap()
        .unwrap();
    assistant_store::create_queued_user_turn(
        uuid::Uuid::new_v4(),
        conversation,
        "owner",
        false,
        "future".into(),
        &[],
    )
    .await
    .unwrap();
    let mut history =
        assistant_store::fetch_generation_context_messages(conversation, "owner", second)
            .await
            .unwrap();
    let summary = restore(&mut history).unwrap();
    assert!(summary.content.contains("项目 A 已检查"));
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].content, "继续部署");
    assert!(
        assistant_store::fetch_generation_context_messages(conversation, "other", second)
            .await
            .unwrap()
            .is_empty()
    );

    // Subsequent turns do not leak into a snapshot taken at the first reply.
    let (context, _) = super::super::firefly::snapshot_with_config("owner", answer.id, &config)
        .await
        .unwrap();
    assert_eq!(context.len(), 1);
    assert_eq!(context[0].content, summary.content);
    let original: String = sqlx::query_scalar("SELECT content FROM assistant_messages WHERE id=?1")
        .bind(answer.id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(original, "已检查，可以部署");
}
