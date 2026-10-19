use axum::{extract::{Path, Query}, Json};
use serde_json::json;

use super::{
    get_conversation_messages, get_conversation_process,
    presentation::{summarize_process_messages, summarize_tool_record}, ProcessDetailQuery,
};
use crate::api::{
    assistant::{ConversationMessageResponse, MessageListQuery}, auth::Session,
};

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
