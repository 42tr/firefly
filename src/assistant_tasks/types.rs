//! Durable task records and event types.
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::ACTIVE;

#[derive(Clone, Debug, Serialize, Deserialize, sqlx::FromRow)]
pub struct TaskRecord {
    pub id: String,
    pub user_id: String,
    pub conversation_id: i64,
    pub generation_job_id: String,
    pub run_id: String,
    pub scope_id: Option<String>,
    pub source_call_id: String,
    pub source_index: i64,
    pub parent_task_id: Option<String>,
    pub title: String,
    pub tool: String,
    pub arguments: Value,
    pub status: String,
    pub detached: bool,
    pub background_allowed: bool,
    pub observed: bool,
    pub parallel_safe: bool,
    pub resource_key: Option<String>,
    pub execution_timeout_secs: i64,
    pub created_at: i64,
    pub started_at: Option<i64>,
    pub deadline_at: Option<i64>,
    pub ended_at: Option<i64>,
    pub result: Option<Value>,
    pub error: Option<String>,
}
impl TaskRecord {
    pub fn active(&self) -> bool {
        ACTIVE.contains(&self.status.as_str())
    }
}

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct TaskSummary {
    pub id: String,
    pub tool: String,
    pub status: String,
    pub detached: bool,
}

#[derive(Clone, Debug, Serialize, sqlx::FromRow)]
pub struct TaskEvent {
    pub seq: i64,
    pub task_id: String,
    pub kind: String,
    pub data: Value,
    pub created_at: i64,
}
