use std::sync::Arc;

use g::{Content as GContent, Message as GMessage, Role as GRole, RunEvent};
use serde_json::{json, Value};
use tokio::fs;

use super::{
    codex::{codex_tool_payload, consume_codex_turn, persist_codex_tool_result},
    events::{handle_run_event, to_g_message, HandoffState},
    firefly::append_task_attention,
};
use crate::{api::assistant::{Conversation, Message}, assistant_job};

#[test]
fn task_history_is_bounded_for_model_context_without_changing_the_transcript() {
    let tasks: Vec<_> = (0..438).map(|index| json!({
        "id":format!("t-{index}"),"task_id":format!("t-{index}"),"status":"succeeded",
        "title":"旧任务","result":{"exit_code":0},
        "recent_output":[{"data":{"text":"历史输出".repeat(1000)}}]
    })).collect();
    let original = json!({"completed":tasks,"pending":[]}).to_string();
    let message = Message::tool(original.clone(), "old-query".into(), false);
    let modeled = to_g_message(&message).unwrap();
    let GContent::ToolResult { result, call_id, is_error } = &modeled.content[0] else { panic!("missing tool result"); };
    assert_eq!(call_id, "old-query");
    assert!(!is_error);
    assert_eq!(result["historical_snapshot_truncated"], true);
    assert_eq!(result["completed"].as_array().unwrap().len(), 32);
    assert_eq!(result["omitted_completed"], 406);
    assert!(result.to_string().len() < 64 * 1024);
    assert_eq!(message.content, original);
    let unrelated = json!({"completed":[{"document":"x".repeat(100000)}],"pending":[]});
    assert_eq!(crate::assistant_tasks::bounded_snapshot_history(unrelated.clone()), unrelated);
    let empty_arrays = json!({"completed":[],"pending":[],"document":"x".repeat(100000)});
    assert_eq!(crate::assistant_tasks::bounded_snapshot_history(empty_arrays.clone()), empty_arrays);
}

#[test]
fn task_startup_prompt_only_mentions_tasks_needing_attention() {
    let mut prompt = "base".to_owned();
    append_task_attention(&mut prompt, &json!({"total":0,"tasks":[]}));
    assert_eq!(prompt, "base");
    append_task_attention(&mut prompt, &json!({"total":1,"tasks":[{"id":"needed","status":"unknown"}]}));
    assert!(prompt.contains("needed"));
    assert!(prompt.contains("指定 task_ids"));
    assert!(!prompt.contains("先用 get_tasks"));
}

#[tokio::test]
async fn codex_image_events_persist_visible_attachments_without_duplicate_base64() {
    use base64::Engine;
    crate::config::init_test_config();
    for only_terminal_item in [false, true] {
        let pool = crate::db::test_pool().await;
        crate::db::set_test_pool(pool.clone());
        let id: i64 = sqlx::query_scalar("INSERT INTO assistant_conversations (user_id, system_prompt, agent) VALUES ('owner','','codex') RETURNING id")
            .fetch_one(&pool).await.unwrap();
        let job = Arc::new(assistant_job::GenerationJob::new(uuid::Uuid::new_v4(), id, "owner".into()));
        sqlx::query("INSERT INTO assistant_generation_jobs (id,conversation_id,user_id,status) VALUES (?1,?2,'owner','running')")
            .bind(job.id).bind(id).execute(&pool).await.unwrap();
        let mut png = std::io::Cursor::new(Vec::new());
        image::DynamicImage::new_rgba8(1, 1).write_to(&mut png, image::ImageFormat::Png).unwrap();
        let encoded = base64::engine::general_purpose::STANDARD.encode(png.get_ref());
        let item = json!({"type":"imageGeneration", "id":"drawing", "status":"completed",
            "savedPath":"/root/.codex/generated_images/thread/drawing.png", "result":encoded});
        let (tx, rx) = tokio::sync::broadcast::channel(16);
        if !only_terminal_item {
            tx.send(json!({"method":"item/started", "params":{"threadId":"thread", "turnId":"turn",
                "item":{"type":"imageGeneration","id":"drawing","status":"inProgress"}}})).unwrap();
            for _ in 0..2 {
                tx.send(json!({"method":"item/completed", "params":{"threadId":"thread", "turnId":"turn", "item":item}})).unwrap();
            }
        }
        tx.send(json!({"method":"turn/completed", "params":{"threadId":"thread",
            "turn":{"id":"turn","status":"completed","items":[item]}}})).unwrap();
        drop(tx);
        consume_codex_turn("thread", "turn", rx, id, &job).await.unwrap();
        let rows: Vec<(String, Value)> = sqlx::query_as("SELECT content,metadata FROM assistant_messages WHERE conversation_id=?1 AND role='tool'")
            .bind(id).fetch_all(&pool).await.unwrap();
        assert_eq!(rows.len(), 1);
        let output: Value = serde_json::from_str(&rows[0].0).unwrap();
        assert!(output["image_url"].as_str().unwrap().starts_with("/api/assistant/attachments/by-file/"));
        assert!(output.get("result").is_none());
        assert_eq!(rows[0].1["tool_name"], "image_generation");
        let attachments: Vec<(String, String)> = sqlx::query_as("SELECT storage_path,content_type FROM assistant_message_attachments")
            .fetch_all(&pool).await.unwrap();
        assert_eq!(attachments.len(), 1);
        assert_eq!(attachments[0].1, "image/png");
        assert_eq!(fs::read(&attachments[0].0).await.unwrap(), *png.get_ref());
        fs::remove_file(&attachments[0].0).await.unwrap();
        let log = job.event_log.lock().unwrap();
        let results: Vec<_> = log.values().filter(|event| event.event == "tool_result").collect();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].data["attachments"][0]["content_type"], "image/png");
        assert!(results[0].data["message_id"].as_i64().is_some());
        assert!(!results[0].data.to_string().contains(&encoded));
    }
}

#[tokio::test]
async fn codex_image_failures_and_missing_files_are_visible_errors() {
    crate::config::init_test_config();
    let pool = crate::db::test_pool().await;
    crate::db::set_test_pool(pool.clone());
    let id: i64 = sqlx::query_scalar("INSERT INTO assistant_conversations (user_id,system_prompt) VALUES ('owner','') RETURNING id")
        .fetch_one(&pool).await.unwrap();
    let job = Arc::new(assistant_job::GenerationJob::new(uuid::Uuid::new_v4(), id, "owner".into()));
    sqlx::query("INSERT INTO assistant_generation_jobs (id,conversation_id,user_id,status) VALUES (?1,?2,'owner','running')")
        .bind(job.id).bind(id).execute(&pool).await.unwrap();
    for failure in [json!({"code":"quota_exceeded"}), Value::Null] {
        let item = json!({"type":"imageGeneration","id":uuid::Uuid::new_v4(),"status":"completed",
            "savedPath":null,"result":"","failure":failure});
        persist_codex_tool_result("thread", id, &job, codex_tool_payload(&item, true).unwrap()).await.unwrap();
    }
    let results: Vec<bool> = sqlx::query_scalar("SELECT is_error FROM assistant_messages WHERE role='tool'")
        .fetch_all(&pool).await.unwrap();
    assert_eq!(results, [true, true]);
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM assistant_message_attachments").fetch_one(&pool).await.unwrap();
    assert_eq!(count, 0);
}

#[tokio::test]
async fn persisted_tool_error_and_usage_match_live_events() {
    let pool = crate::db::test_pool().await;
    crate::db::set_test_pool(pool.clone());
    let id: i64 = sqlx::query_scalar("INSERT INTO assistant_conversations (user_id, system_prompt, agent) VALUES ('owner','','firefly') RETURNING id")
        .fetch_one(&pool).await.unwrap();
    let job = assistant_job::GenerationJob::new(uuid::Uuid::new_v4(), id, "owner".into());
    sqlx::query("INSERT INTO assistant_generation_jobs (id,conversation_id,user_id,status) VALUES (?1,?2,'owner','running')")
        .bind(job.id).bind(id).execute(&pool).await.unwrap();
    let mut conversation = Conversation::new();
    let mut handoff = HandoffState::default();
    handle_run_event(RunEvent::ModelCompleted { run_id: job.id, turn: 1,
        message: GMessage::new(GRole::Assistant, vec![GContent::ToolCall { id: "call".into(), name: "todos".into(), arguments: json!({"action":"list"}) }]),
        usage: g::Usage { input_tokens: 12, output_tokens: 3, ..Default::default() } }, &mut handoff, &mut conversation, id, &job).await.unwrap();
    handle_run_event(RunEvent::ToolCompleted { run_id: job.id, call_id: "call".into(), name: "todos".into(), result: json!({"error":"unavailable"}), is_error: true },
        &mut handoff, &mut conversation, id, &job).await.unwrap();
    let (is_error,): (bool,) = sqlx::query_as("SELECT is_error FROM assistant_messages WHERE conversation_id=?1 AND role='tool'")
        .bind(id).fetch_one(&pool).await.unwrap();
    assert!(is_error);
    let metadata: Value = sqlx::query_scalar("SELECT metadata FROM assistant_messages WHERE conversation_id=?1 AND role='assistant'")
        .bind(id).fetch_one(&pool).await.unwrap();
    assert_eq!(metadata["usage"]["input_tokens"], 12);
    let log = job.event_log.lock().unwrap();
    let result = log.values().find(|event| event.event == "tool_result").unwrap();
    assert_eq!(result.data["is_error"], true);
    assert!(result.data.get("result").is_none());
    assert!(result.data["message_id"].as_i64().is_some());
}
