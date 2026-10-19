use std::collections::HashSet;

use axum::{extract::{Path, Query}, http::StatusCode, Json};
use serde_json::{json, Value};

use super::{
    get_conversation_messages, list_conversations, mark_conversation_read, update_conversation,
    MarkConversationReadRequest,
};
use crate::{
    api::{
        assistant::{ConversationListResponse, MessageListQuery, UpdateConversationRequest},
        auth::Session,
    },
    assistant_store,
};

#[tokio::test]
async fn db_tests_conversation_reads_sync_and_never_acknowledge_newer_messages() {
    let pool = crate::db::test_pool().await;
    crate::db::set_test_pool(pool.clone());
    let conversation = assistant_store::create_conversation("owner", crate::assistant_store::NewConversation {
        system_prompt: "",
        title: None,
        agent: "firefly",
        codex_cwd: None,
        provider: None,
        model: None,
        reasoning_level: None,
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
