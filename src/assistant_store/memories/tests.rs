use super::*;
use sqlx::query_scalar;

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
