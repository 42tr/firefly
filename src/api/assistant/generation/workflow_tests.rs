use std::sync::Arc;

use async_trait::async_trait;
use g::{Agent, Content as GContent, Message as GMessage, Role as GRole, RunEvent};
use serde_json::{json, Value};
use tokio::sync::mpsc;

use super::events::{handle_run_event, AssistantRunEventSink, HandoffState};
use crate::{api::assistant::Conversation, assistant_job};
use std::sync::atomic::{AtomicUsize, Ordering};

struct WorkflowModel {
    parent: bool,
    turn: AtomicUsize,
}

#[async_trait]
impl g::Model for WorkflowModel {
    async fn generate(&self, _: g::ModelRequest) -> Result<g::ModelResponse, g::ModelError> {
        let message = if self.turn.fetch_add(1, Ordering::SeqCst) == 0 {
            GMessage::new(GRole::Assistant, vec![GContent::ToolCall {
                id: if self.parent { "delegate" } else { "child" }.into(),
                name: if self.parent { "handoff_to_weekly" } else { "fetch_weekly_work" }.into(),
                arguments: if self.parent { json!({"task":"生成周报"}) } else { json!({}) },
            }])
        } else {
            GMessage::assistant("周报已完成")
        };
        Ok(g::ModelResponse::new(message))
    }
}

struct WeeklyTool;

#[async_trait]
impl g::Tool for WeeklyTool {
    fn spec(&self) -> g::ToolSpec {
        g::ToolSpec { name: "fetch_weekly_work".into(), description: String::new(),
            input_schema: json!({"type":"object"}), behavior: Default::default() }
    }
    async fn call(&self, _: g::ToolContext, _: Value) -> Result<Value, g::ToolError> {
        Ok(json!({"conversation_count":2}))
    }
}

#[tokio::test]
async fn runtime_handoff_records_live_child_steps_and_terminal_timing() {
    let pool = crate::db::test_pool().await;
    crate::db::set_test_pool(pool.clone());
    let id: i64 = sqlx::query_scalar("INSERT INTO assistant_conversations(user_id,system_prompt,agent) VALUES ('owner','','firefly') RETURNING id")
        .fetch_one(&pool).await.unwrap();
    let job = assistant_job::GenerationJob::new(uuid::Uuid::new_v4(), id, "owner".into());
    sqlx::query("INSERT INTO assistant_generation_jobs(id,conversation_id,user_id,status) VALUES (?1,?2,'owner','running')")
        .bind(job.id).bind(id).execute(&pool).await.unwrap();
    let (sender, mut events) = mpsc::channel(64);
    let child = Agent::new(Arc::new(WorkflowModel { parent:false, turn:0.into() }))
        .name("weekly").tool(WeeklyTool);
    let agent = Agent::new(Arc::new(WorkflowModel { parent:true, turn:0.into() }))
        .handoff([child]).with_event_sink(Arc::new(AssistantRunEventSink { sender }));
    let run = agent.run("生成周报");
    tokio::pin!(run);
    let mut conversation = Conversation::new();
    let mut handoff = HandoffState::default();
    let mut child_started = false;
    loop {
        tokio::select! {
            result = &mut run => {
                assert_eq!(result.unwrap().final_text, "周报已完成");
                break;
            }
            next = events.recv() => {
                let (event, acknowledged) = next.unwrap();
                let starts_child = matches!(&event, RunEvent::ToolStarted { call_id, .. } if call_id == "child");
                handle_run_event(event, &mut handoff, &mut conversation, id, &job).await.unwrap();
                if starts_child {
                    let (parent, status, started, ended): (String, String, Option<i64>, Option<i64>) = sqlx::query_as(
                        "SELECT parent_id,status,started_at,ended_at FROM assistant_tool_details WHERE call_id='child'")
                        .fetch_one(&pool).await.unwrap();
                    assert_eq!(parent, "delegate");
                    assert_eq!(status, "running");
                    assert!(started.is_some() && ended.is_none());
                    child_started = true;
                }
                acknowledged.send(()).unwrap();
            }
        }
    }
    assert!(child_started);
    let rows: Vec<(String, String, String, i64, i64)> = sqlx::query_as(
        "SELECT call_id,parent_id,status,started_at,ended_at FROM assistant_tool_details ORDER BY call_id")
        .fetch_all(&pool).await.unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!((rows[0].0.as_str(), rows[0].1.as_str(), rows[0].2.as_str()), ("child", "delegate", "completed"));
    assert_eq!((rows[1].0.as_str(), rows[1].1.as_str(), rows[1].2.as_str()), ("delegate", "", "completed"));
    assert!(rows.iter().all(|row| row.4 >= row.3));
    assert!(!conversation.messages.iter().any(|message| message.tool_call_id.as_deref() == Some("child")));
}
