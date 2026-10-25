use super::*;
use sqlx::{query, query_scalar};

#[tokio::test]
async fn user_memory_mutations_preserve_source_and_ownership() {
    db::set_test_pool(db::test_pool().await);
    let manual = create_memory("owner", "手动标题", "手动内容", true).await.unwrap();
    let agent = create_reviewed_memory("owner", "自动标题", "自动内容", &[manual.clone()])
        .await.unwrap().unwrap();
    assert_eq!(manual.source, "manual");
    assert_eq!(agent.source, "agent");

    // Users can manage both sources, while writes always check ownership.
    for memory in [&manual, &agent] {
        assert!(update_memory(memory.id, "other", Some("越权"), None, None).await.unwrap().is_none());
        assert!(!delete_memory(memory.id, "other").await.unwrap());
        let updated = update_memory(memory.id, "owner", Some("手动更新"), Some("新内容"), Some(false))
            .await.unwrap().unwrap();
        assert_eq!(updated.source, memory.source);
        assert_eq!(updated.title, "手动更新");
        assert_eq!(updated.content, "新内容");
        assert!(!updated.enabled);
    }
    assert!(delete_memory(agent.id, "owner").await.unwrap());
    assert!(delete_memory(manual.id, "owner").await.unwrap());
    assert!(list_memories("owner").await.unwrap().is_empty());
}

#[tokio::test]
async fn memory_creates_and_agent_reviews_are_not_capped() {
    let pool = db::test_pool().await;
    db::set_test_pool(pool.clone());
    for index in 0..100 {
        query("INSERT INTO assistant_memories(user_id,title,content,source) VALUES ('owner',?1,?2,'manual')")
            .bind(format!("memory-{index}"))
            .bind(format!("content-{index}"))
            .execute(&pool)
            .await
            .unwrap();
    }
    let checked = list_memories("owner").await.unwrap();
    create_reviewed_memory("owner", "自动标题", "自动内容", &checked)
        .await
        .unwrap()
        .unwrap();
    let (left, right) = tokio::join!(
        create_memory("owner", "last-a", "content-a", true),
        create_memory("owner", "last-b", "content-b", true),
    );
    assert!(left.is_ok());
    assert!(right.is_ok());
    assert_eq!(query_scalar::<_, i64>("SELECT count(*) FROM assistant_memories WHERE user_id='owner'")
        .fetch_one(&pool).await.unwrap(), 103);
}

#[tokio::test]
async fn completed_firefly_turn_is_queued_for_independent_memory_review() {
    let pool = db::test_pool().await;
    db::set_test_pool(pool.clone());
    let conversation: i64 = query_scalar(
        "INSERT INTO assistant_conversations(user_id,system_prompt,agent) VALUES ('owner','', 'firefly') RETURNING id",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    let job_id = uuid::Uuid::new_v4();
    query_scalar::<_, i64>(
        "INSERT INTO assistant_generation_jobs(id,conversation_id,user_id,status) VALUES (?1,?2,'owner','running') RETURNING rowid",
    )
    .bind(job_id)
    .bind(conversation)
    .fetch_one(&pool)
    .await
    .unwrap();
    query(
        "INSERT INTO assistant_messages(conversation_id,generation_job_id,role,content) VALUES (?1,?2,'user','请记住我的后台地址')",
    )
    .bind(conversation)
    .bind(job_id)
    .execute(&pool)
    .await
    .unwrap();
    let final_message: i64 = query_scalar(
        "INSERT INTO assistant_messages(conversation_id,generation_job_id,role,content) VALUES (?1,?2,'assistant','已记录') RETURNING id",
    )
    .bind(conversation)
    .bind(job_id)
    .fetch_one(&pool)
    .await
    .unwrap();

    crate::assistant_store::update_generation_status(job_id, "completed", None)
        .await
        .unwrap();
    let queued = claim_next_memory_review().await.unwrap().unwrap();
    assert_eq!(queued.generation_job_id, job_id);
    assert_eq!(queued.final_message_id, final_message);
    let messages = fetch_memory_review_messages(job_id, conversation, "owner")
        .await
        .unwrap();
    assert_eq!(messages.iter().filter(|message| message.role == "user").count(), 1);
    assert_eq!(messages.iter().filter(|message| message.role == "assistant").count(), 1);
    fail_memory_review(job_id, "test retry").await.unwrap();
    assert_eq!(claim_next_memory_review().await.unwrap().unwrap().attempts, 2);
    apply_reviewed_memories_for_turn("owner", &[], &[], Some((job_id, final_message)))
        .await
        .unwrap()
        .unwrap();
    let status: String = query_scalar("SELECT status FROM assistant_memory_jobs WHERE generation_job_id=?1")
        .bind(job_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(status, "completed");
}

#[tokio::test]
async fn memory_source_migration_protects_legacy_entries_and_rejects_invalid_sources() {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    let previous = sqlx::migrate::Migrator::with_migrations(
        db::MIGRATOR
            .iter()
            .filter(|migration| migration.version < 20261025000000)
            .cloned()
            .collect(),
    );
    previous.run(&pool).await.unwrap();
    query("INSERT INTO assistant_memories (user_id, title, content, enabled) VALUES ('owner', '旧记忆', '原内容', FALSE)")
        .execute(&pool).await.unwrap();
    db::MIGRATOR.run(&pool).await.unwrap();
    db::set_test_pool(pool);
    let memories = list_memories("owner").await.unwrap();
    assert_eq!(memories.len(), 1);
    let memory = &memories[0];
    assert_eq!(memory.source, "manual");
    assert_eq!(memory.content, "原内容");
    assert!(!memory.enabled);
    assert!(query("INSERT INTO assistant_memories (user_id, title, content, source) VALUES ('owner', '非法来源', '内容', 'unknown')")
        .execute(db::pool()).await.is_err());
    db::MIGRATOR.run(db::pool()).await.unwrap();
}

fn change(id: Option<i64>, content: &str) -> ReviewedMemoryChange {
    ReviewedMemoryChange {
        id,
        title: "标题".into(),
        content: content.into(),
        evidence: "本轮已验证".into(),
    }
}

#[tokio::test]
async fn reviewed_batch_is_atomic_protected_and_rejects_stale_snapshots() {
    db::set_test_pool(db::test_pool().await);
    let manual = create_memory("owner", "手动", "手动事实", true)
        .await
        .unwrap();
    let agent = create_reviewed_memory("owner", "自动", "旧事实", &[manual.clone()])
        .await
        .unwrap()
        .unwrap();
    let snapshot = list_memories("owner").await.unwrap();
    // A bad final item must not leave earlier additions or audit records behind.
    for changes in [
        vec![change(None, "新事实"), change(Some(manual.id), "覆盖手动")],
        vec![change(Some(agent.id), "新事实"), change(None, "新事实")],
        vec![change(None, "新事实"), change(Some(i64::MAX), "不存在")],
        vec![change(None, "新事实"), change(None, &"中".repeat(200))],
    ] {
        assert!(apply_reviewed_memories("owner", &changes, &snapshot)
            .await
            .is_err());
        assert_eq!(list_memories("owner").await.unwrap().len(), 2);
        assert_eq!(
            query_scalar::<_, i64>("SELECT count(*) FROM assistant_memory_revisions")
                .fetch_one(db::pool())
                .await
                .unwrap(),
            0
        );
    }
    update_memory(
        agent.id,
        "owner",
        None,
        None,
        Some(false),
    )
    .await
    .unwrap();
    assert!(
        apply_reviewed_memories("owner", &[change(Some(agent.id), "新事实")], &snapshot)
            .await
            .unwrap()
            .is_none()
    );
    let snapshot = list_memories("owner").await.unwrap();
    assert!(
        apply_reviewed_memories("owner", &[change(Some(agent.id), "新事实")], &snapshot)
            .await
            .is_err()
    );
    assert!(
        apply_reviewed_memories("other", &[change(Some(agent.id), "越权")], &[])
            .await
            .is_err()
    );
}

#[tokio::test]
async fn reviewed_batch_saves_history_and_deletion_removes_it() {
    db::set_test_pool(db::test_pool().await);
    let old = create_reviewed_memory("owner", "旧标题", "旧事实", &[])
        .await
        .unwrap()
        .unwrap();
    let saved = apply_reviewed_memories(
        "owner",
        &[change(Some(old.id), "新事实"), change(None, "另一事实")],
        &[old.clone()],
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(saved.len(), 2);
    assert_eq!(saved[0].id, old.id);
    assert!(saved[0].updated_at > old.updated_at);
    let row:(String,String,i64,String)=sqlx::query_as("SELECT title,content,previous_updated_at,evidence FROM assistant_memory_revisions WHERE memory_id=?1")
        .bind(old.id).fetch_one(db::pool()).await.unwrap();
    assert_eq!(
        row,
        (
            old.title,
            old.content,
            old.updated_at.timestamp_micros(),
            "本轮已验证".into()
        )
    );
    delete_memory(old.id, "owner")
        .await
        .unwrap();
    assert_eq!(
        query_scalar::<_, i64>("SELECT count(*) FROM assistant_memory_revisions")
            .fetch_one(db::pool())
            .await
            .unwrap(),
        0
    );
}

#[tokio::test]
async fn revision_migration_preserves_long_legacy_memories_and_is_repeatable() {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    let previous = sqlx::migrate::Migrator::with_migrations(
        db::MIGRATOR
            .iter()
            .filter(|m| m.version < 20261027000000)
            .cloned()
            .collect(),
    );
    previous.run(&pool).await.unwrap();
    let content = "旧事实".repeat(500);
    query("INSERT INTO assistant_memories (user_id,title,content,enabled,source) VALUES ('owner','旧记忆',?1,FALSE,'agent')")
        .bind(&content).execute(&pool).await.unwrap();
    db::MIGRATOR.run(&pool).await.unwrap();
    db::MIGRATOR.run(&pool).await.unwrap();
    let row: (String, bool, String) =
        sqlx::query_as("SELECT content,enabled,source FROM assistant_memories")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(row, (content, false, "agent".into()));
}

#[tokio::test]
async fn database_failure_rolls_back_both_edits_and_revision_history() {
    db::set_test_pool(db::test_pool().await);
    let old = create_reviewed_memory("owner", "标题", "旧事实", &[])
        .await
        .unwrap()
        .unwrap();
    query("CREATE TRIGGER fail_memory_insert BEFORE INSERT ON assistant_memories BEGIN SELECT RAISE(ABORT, 'test write failure'); END")
        .execute(db::pool()).await.unwrap();
    assert!(apply_reviewed_memories(
        "owner",
        &[change(Some(old.id), "新事实"), change(None, "另一事实")],
        &[old.clone()]
    )
    .await
    .is_err());
    let memories = list_memories("owner").await.unwrap();
    assert_eq!(memories.len(), 1);
    assert_eq!(memories[0].content, old.content);
    assert_eq!(
        query_scalar::<_, i64>("SELECT count(*) FROM assistant_memory_revisions")
            .fetch_one(db::pool())
            .await
            .unwrap(),
        0
    );
}
