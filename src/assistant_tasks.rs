//! Durable tasks owned by the service, independent of a reply or SSE connection.
use anyhow::{anyhow, Context};
use async_trait::async_trait;
use g::{TaskBackend, TaskSubmission, Tool, ToolError};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::{query, query_as, query_scalar, SqlitePool};
use std::{
    collections::HashMap,
    sync::{Arc, OnceLock},
    time::Duration,
};
use tokio::sync::{Mutex, Notify};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

pub mod command;
mod result;
pub use result::ReadTaskResultTool;
#[cfg(test)]
mod tests;

const ACTIVE: &[&str] = &["queued", "running", "cancelling"];
const TASK_PAGE_SIZE: usize = 32;
const MAX_TASK_PAGE_SIZE: usize = 64;
const TASK_PAYLOAD_BYTES: usize = 32 * 1024;
const ATTENTION_FILTER: &str = "user_id=?1 AND conversation_id=?2 AND (?4 IS NULL OR (generation_job_id=?3 AND scope_id=?4)) AND (status IN ('queued','running','cancelling') OR (observed=0 AND ((generation_job_id=?3 AND detached=0) OR status='unknown')))";
fn now() -> i64 {
    chrono::Utc::now().timestamp_millis()
}
fn tool_error(error: impl std::fmt::Display) -> ToolError {
    ToolError::new(error.to_string())
}
fn setting(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .filter(|v| *v > 0)
        .unwrap_or(default)
}

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

// Budgets are measured after JSON escaping; a truncated string must remain
// valid UTF-8 and cannot expand an output budget through control characters.
fn text_preview(text: &str, budget: usize, tail: bool) -> (String, bool) {
    if serde_json::to_string(text).expect("serialize text").len() <= budget {
        return (text.to_owned(), false);
    }
    let bytes = budget.saturating_sub(2) / 6;
    let preview = if tail {
        let mut start = text.len().saturating_sub(bytes);
        while !text.is_char_boundary(start) { start += 1; }
        &text[start..]
    } else {
        let mut end = bytes.min(text.len());
        while !text.is_char_boundary(end) { end -= 1; }
        &text[..end]
    };
    (preview.to_owned(), true)
}

fn result_preview(value: &Value, budget: usize) -> Value {
    let view = result::model_view(value);
    let serialized = serde_json::to_string(&view).expect("serialize task result");
    if serialized.len() <= budget { return view; }
    let data = serde_json::to_string(view.get("structuredContent").unwrap_or(&view)).expect("serialize preview");
    let mut preserved = json!({});
    for key in ["isError", "error", "warning"] {
        if let Some(field) = view.get(key) {
            if field.to_string().len() <= 128 { preserved[key] = field.clone(); }
        }
    }
    for pointer in ["/structuredContent/status", "/structuredContent/error", "/structuredContent/warning", "/structuredContent/body/error", "/structuredContent/body/warning"] {
        if let Some(field)=view.pointer(pointer).filter(|value|!value.is_null() && value.to_string().len()<=128) { preserved[pointer]=field.clone(); }
    }
    if let Some(content) = view["content"].as_array() {
        let media: Vec<_> = content.iter().filter(|part| part["type"] != "text").take(4)
            .map(|part| {
                let mut descriptor=json!({"type":part["type"],"mimeType":part["mimeType"],"data_omitted":true});
                for key in ["name","uri","filename"] {
                    if let Some(field)=part.get(key).filter(|value|value.to_string().len()<=128) { descriptor[key]=field.clone(); }
                }
                if let Some(uri)=part.pointer("/resource/uri").filter(|value|value.to_string().len()<=128) { descriptor["uri"]=uri.clone(); }
                descriptor
            }).collect();
        if !media.is_empty() { preserved["content"] = json!(media); }
    }
    let kept = preserved.to_string().len();
    let (preview, _) = text_preview(&data, budget.saturating_sub(128 + kept), false);
    json!({"truncated":true,"original_bytes":value.to_string().len(),"preview":preview,"preserved":preserved})
}

/// Compact legacy task snapshots only for model context, preserving the stored
/// transcript and task details. Older get_tasks replies can contain megabytes
/// of already delivered history, before the current paging rules existed.
pub(crate) fn bounded_snapshot_history(value: Value) -> Value {
    let Some(completed) = value["completed"].as_array() else { return value; };
    let Some(pending) = value["pending"].as_array() else { return value; };
    if (completed.is_empty() && pending.is_empty())
        || !completed.iter().chain(pending).all(|task| task["task_id"].is_string() && task["status"].is_string())
        || value.to_string().len() <= 64 * 1024 {
        return value;
    }
    let done_start = completed.len().saturating_sub(TASK_PAGE_SIZE);
    let pending_len = pending.len().min(TASK_PAGE_SIZE);
    let payload = TASK_PAYLOAD_BYTES / (completed.len() - done_start + pending_len).max(1);
    let compact = |task: &Value| {
        let mut summary = json!({});
        for key in ["id", "task_id", "tool", "title", "status", "detached", "parent_task_id", "scope_id", "started_at", "deadline_at", "error"] {
            if let Some(field) = task.get(key) {
                summary[key] = if matches!(key, "title" | "error" | "scope_id") && field.is_string() {
                    json!(text_preview(field.as_str().unwrap(), 512, false).0)
                } else { field.clone() };
            }
        }
        if let Some(result) = task.get("result") { summary["result"] = result_preview(result, payload / 2); }
        let output = if let Some(output) = task["output_preview"].as_str() { output.to_owned() } else {
            task["recent_output"].as_array().into_iter().flatten()
                .filter_map(|event| event["data"]["text"].as_str()).collect()
        };
        let (preview, truncated) = text_preview(&output, payload / 2, true);
        summary["output_preview"] = json!(preview);
        summary["output_truncated"] = json!(truncated || task["output_truncated"] == true);
        summary
    };
    json!({
        "completed":completed[done_start..].iter().map(compact).collect::<Vec<_>>(),
        "pending":pending[..pending_len].iter().map(compact).collect::<Vec<_>>(),
        "historical_snapshot_truncated":true,
        "omitted_completed":done_start,"omitted_pending":pending.len()-pending_len,
        "hint":"历史任务结果仅保留有界概况，完整记录仍在任务详情中；不应根据此旧快照判断当前任务状态。"
    })
}

#[derive(Clone, Debug, Serialize, sqlx::FromRow)]
pub struct TaskEvent {
    pub seq: i64,
    pub task_id: String,
    pub kind: String,
    pub data: Value,
    pub created_at: i64,
}

struct Running {
    token: CancellationToken,
    task: TaskRecord,
}
pub struct Manager {
    pub pool: SqlitePool,
    running: Mutex<HashMap<String, Running>>,
    tools: Mutex<HashMap<String, Arc<dyn Tool>>>,
    pub changed: Notify,
    mutation: Mutex<()>,
    max_queued: usize,
}
static MANAGER: OnceLock<Arc<Manager>> = OnceLock::new();
pub fn global() -> Arc<Manager> {
    MANAGER.get().expect("task manager initialized").clone()
}

pub(crate) fn notify_changed() {
    if let Some(manager) = MANAGER.get() {
        manager.changed.notify_waiters();
    }
}

impl Manager {
    pub fn new(pool: SqlitePool) -> Arc<Self> {
        Arc::new(Self {
            pool,
            running: Mutex::new(HashMap::new()),
            tools: Mutex::new(HashMap::new()),
            changed: Notify::new(),
            mutation: Mutex::new(()),
            max_queued: setting("ASSISTANT_TASK_QUEUE_LIMIT", 256),
        })
    }

    pub async fn event(&self, id: &str, kind: &str, data: Value) -> anyhow::Result<()> {
        query("INSERT INTO assistant_task_events(task_id,kind,data,created_at) VALUES (?1,?2,?3,?4)")
            .bind(id)
            .bind(kind)
            .bind(data)
            .bind(now())
            .execute(&self.pool)
            .await?;
        self.changed.notify_waiters();
        Ok(())
    }

    pub async fn list(&self, user: &str, conversation: i64) -> anyhow::Result<Vec<TaskRecord>> {
        Ok(
            query_as("SELECT * FROM assistant_tasks WHERE user_id=?1 AND conversation_id=?2 ORDER BY created_at,id")
                .bind(user)
                .bind(conversation)
                .fetch_all(&self.pool)
                .await?,
        )
    }

    pub async fn attention_summary(
        &self, user: &str, conversation: i64, job: &str,
    ) -> anyhow::Result<Value> {
        self.summary(user, conversation, job, None, false).await
    }

    async fn summary(
        &self, user: &str, conversation: i64, job: &str, scope: Option<&str>, required_only: bool,
    ) -> anyhow::Result<Value> {
        let filter = if required_only {
            "user_id=?1 AND conversation_id=?2 AND generation_job_id=?3 AND (?4 IS NULL OR scope_id=?4) AND detached=0 AND (status IN ('queued','running','cancelling') OR observed=0)"
        } else { ATTENTION_FILTER };
        let total: i64 = sqlx::QueryBuilder::new("SELECT count(*) FROM assistant_tasks WHERE ").push(filter)
            .build_query_scalar()
            .bind(user).bind(conversation).bind(job).bind(scope).fetch_one(&self.pool).await?;
        let tasks: Vec<TaskSummary> = sqlx::QueryBuilder::new("SELECT id,tool,status,detached FROM assistant_tasks WHERE ").push(filter)
            .push(" ORDER BY CASE WHEN generation_job_id=?3 AND detached=0 AND observed=0 AND status NOT IN ('queued','running','cancelling') THEN 0 ELSE 1 END,created_at,id LIMIT ?5")
            .build_query_as().bind(user).bind(conversation).bind(job).bind(scope).bind(TASK_PAGE_SIZE as i64)
            .fetch_all(&self.pool).await?;
        Ok(json!({"tasks":tasks,"total":total,"has_more":total as usize > tasks.len()}))
    }

    #[cfg(test)]
    pub async fn events(&self, user: &str, conversation: i64, after: i64) -> anyhow::Result<Vec<TaskEvent>> {
        Ok(query_as("SELECT e.* FROM assistant_task_events e JOIN assistant_tasks t ON t.id=e.task_id WHERE t.user_id=?1 AND t.conversation_id=?2 AND e.seq>?3 ORDER BY e.seq LIMIT 256")
            .bind(user).bind(conversation).bind(after).fetch_all(&self.pool).await?)
    }

    /// On restart, process identity cannot safely be recovered from an OS PID.
    /// Never replay an uncertain side effect. Detached queued tasks can restart.
    pub async fn recover(&self) -> anyhow::Result<()> {
        // Upgrade old cwd/repository locks to explicit mutexes. Preserve the
        // execution arguments and unknown results without replaying commands.
        query("UPDATE assistant_tasks SET resource_key=json_extract(arguments,'$.resource_key') WHERE tool='command' AND status IN ('queued','running','cancelling','unknown') AND resource_key IS NOT json_extract(arguments,'$.resource_key')")
            .execute(&self.pool).await?;
        query("UPDATE assistant_tasks SET status='unknown', error='服务重启，命令执行结果未知；请检查实际状态，不要直接重复执行。', ended_at=?1 WHERE status IN ('running','cancelling')")
            .bind(now()).execute(&self.pool).await?;
        query("UPDATE assistant_tasks SET status='cancelled',error='所属回答已结束',ended_at=?1 WHERE status='queued' AND detached=0 AND NOT EXISTS (SELECT 1 FROM assistant_generation_jobs j WHERE j.id=unhex(replace(assistant_tasks.generation_job_id,'-','')) AND j.status IN ('queued','running'))")
            .bind(now()).execute(&self.pool).await?;
        // Acknowledgement happens only after the persisted tool response reaches
        // the event sink. Preserve it across restarts; unacknowledged results
        // already remain eligible for redelivery without resetting all history.
        Ok(())
    }

    fn recovered_tool(task: &TaskRecord) -> Option<Arc<dyn Tool>> {
        if task.tool == "command" {
            return Some(Arc::new(command::CommandTool));
        }
        if task.tool == "read_task_result" { return Some(Arc::new(ReadTaskResultTool)); }
        let ctx = crate::mcp::ToolContext {
            user_id: task.user_id.clone(),
        };
        crate::mcp::build_tool_set(ctx)
            .into_iter()
            .find(|t| t.spec().name == task.tool)
    }

    pub async fn dispatch(self: &Arc<Self>) -> anyhow::Result<()> {
        let _lock = self.mutation.lock().await;
        // A deleted conversation must not leave an invisible command running.
        for (id, running) in self.running.lock().await.iter() {
            let exists: bool = query_scalar("SELECT EXISTS(SELECT 1 FROM assistant_tasks WHERE id=?1)")
                .bind(id)
                .fetch_one(&self.pool)
                .await?;
            if !exists {
                running.token.cancel();
            }
        }
        let queued: Vec<TaskRecord> =
            query_as("SELECT * FROM assistant_tasks WHERE status='queued' ORDER BY created_at,id LIMIT ?1")
                .bind(self.max_queued.min(10000) as i64)
                .fetch_all(&self.pool)
                .await?;
        for task in queued {
            if !task.detached {
                // Jobs use BLOB UUIDs; normalize the parameter so the primary
                // key stays searchable. Malformed historical ids are not live.
                let live:bool = query_scalar("SELECT EXISTS(SELECT 1 FROM assistant_generation_jobs j WHERE j.id=?1 AND j.status IN ('queued','running') AND NOT EXISTS(SELECT 1 FROM assistant_messages m WHERE m.generation_job_id=j.id AND m.role='user' AND json_extract(m.metadata,'$.cancellation_requested')=1))")
                    .bind(Uuid::parse_str(&task.generation_job_id).ok()).fetch_one(&self.pool).await?;
                if !live {
                    self.finish(&task.id, "cancelled", None, Some("所属回答已停止".into()))
                        .await?;
                    self.tools.lock().await.remove(&task.id);
                    continue;
                }
            }
            let mut running = self.running.lock().await;
            // Mutexes belong to execution owners, including cancellation and
            // result persistence. An unknown historical result is not an owner.
            if !task.parallel_safe && !running.is_empty() {
                break;
            }
            if running.values().any(|other| {
                !task.parallel_safe
                    || !other.task.parallel_safe
                    || matches!((&task.resource_key, &other.task.resource_key), (Some(a), Some(b))
                        if a == b)
            }) {
                continue;
            }
            let tool = self
                .tools
                .lock()
                .await
                .get(&task.id)
                .cloned()
                .or_else(|| Self::recovered_tool(&task));
            let tool = match tool {
                Some(tool) => Some(tool),
                None => crate::modules::recovered_tool(&task.tool).await,
            };
            let Some(tool) = tool else {
                drop(running);
                self.finish(&task.id, "failed", None, Some("工具已不可用".into()))
                    .await?;
                continue;
            };
            let started = now();
            let deadline = started.saturating_add(task.execution_timeout_secs.saturating_mul(1000));
            if query("UPDATE assistant_tasks SET status='running',started_at=?2,deadline_at=?3 WHERE id=?1 AND status='queued'")
                .bind(&task.id).bind(started).bind(deadline).execute(&self.pool).await?.rows_affected() == 0 { continue; }
            let mut task = task;
            task.status = "running".into();
            task.started_at = Some(started);
            task.deadline_at = Some(deadline);
            let token = CancellationToken::new();
            running.insert(
                task.id.clone(),
                Running {
                    token: token.clone(),
                    task: task.clone(),
                },
            );
            drop(running);
            if let Err(e) = self.event(&task.id, "state", json!({"status":"running"})).await {
                tracing::warn!("任务开始事件写入失败，状态已持久化: {e:#}");
            }
            let manager = self.clone();
            tokio::spawn(async move {
                use futures_util::FutureExt;
                let output = std::panic::AssertUnwindSafe(manager.execute(&task, tool, token))
                    .catch_unwind()
                    .await;
                let (status, value, error) = match output {
                    Ok(result) => result,
                    Err(_) => ("unknown", None, Some("执行器异常退出，结果未知".into())),
                };
                // Keep the resource claim until the terminal state is durable.
                loop {
                    match manager.finish(&task.id, status, value.clone(), error.clone()).await {
                        Ok(()) => break,
                        Err(e) => {
                            tracing::error!("保存后台任务结果失败: {e:#}");
                            tokio::time::sleep(Duration::from_secs(1)).await;
                        }
                    }
                }
                manager.running.lock().await.remove(&task.id);
                manager.tools.lock().await.remove(&task.id);
                manager.changed.notify_waiters();
            });
        }
        Ok(())
    }

    async fn execute(
        &self,
        task: &TaskRecord,
        tool: Arc<dyn Tool>,
        token: CancellationToken,
    ) -> (&'static str, Option<Value>, Option<String>) {
        if task.tool == "command" {
            return command::execute(self, task, token).await;
        }
        if task.tool == "read_task_result" {
            return tokio::select! {
                biased;
                _ = token.cancelled() => ("cancelled", None, Some("结果读取已取消".into())),
                output = tokio::time::timeout(Duration::from_secs(task.execution_timeout_secs as u64),
                    result::read(&self.pool, task, task.arguments.clone())) => match output {
                    Ok(Ok(value)) => ("succeeded", Some(value), None),
                    Ok(Err(error)) => ("failed", None, Some(error.to_string())),
                    Err(_) => ("timed_out", None, Some("结果读取超时".into())),
                }
            };
        }
        let read_only = tool.spec().behavior.read_only;
        let context = g::ToolContext {
            run_id: Uuid::parse_str(&task.run_id).unwrap_or_default(),
            cancellation_token: token.clone(),
        };
        tokio::select! {
            biased;
            _ = token.cancelled() => (if read_only {"cancelled"} else {"unknown"}, None, Some("本地执行已取消；有副作用的远端操作需核实实际状态".into())),
            result = tokio::time::timeout(Duration::from_secs(task.execution_timeout_secs as u64), tool.call_output(context, task.arguments.clone())) => match result {
                Ok(Ok(output)) => (if output.is_error { "failed" } else { "succeeded" }, Some(output.value), None),
                Ok(Err(error)) => ("failed", None, Some(error.message)),
                Err(_) => { token.cancel(); (if read_only {"timed_out"} else {"unknown"}, None, Some("执行超时，远端操作可能已生效".into())) },
            }
        }
    }

    async fn finish(&self, id: &str, status: &str, result: Option<Value>, error: Option<String>) -> anyhow::Result<()> {
        let mut tx = self.pool.begin().await?;
        let ended = now();
        let finished: Option<(String,String,Option<i64>)> = query_as("UPDATE assistant_tasks SET status=?2,result=?3,error=?4,ended_at=?5 WHERE id=?1 AND status IN ('queued','running','cancelling') RETURNING generation_job_id,tool,started_at")
            .bind(id).bind(status).bind(result).bind(error).bind(ended).fetch_optional(&mut *tx).await?;
        if finished.is_some() {
            query("INSERT INTO assistant_task_events(task_id,kind,data,created_at) VALUES (?1,'state',?2,?3)")
                .bind(id)
                .bind(json!({"status":status}))
                .bind(now())
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await?;
        if let Some((job_id,tool,started)) = finished {
            tracing::info!(job_id, task_id=id, tool, status, elapsed_ms=started.map(|at|ended.saturating_sub(at)), "assistant tool execution finished");
        }
        self.changed.notify_waiters();
        Ok(())
    }

    pub async fn cancel(&self, user: &str, conversation: i64, ids: &[String]) -> anyhow::Result<()> {
        let _lock = self.mutation.lock().await;
        // Cancellation only needs the hierarchy and state, not saved arguments
        // or results from every historical task in the conversation.
        let all: Vec<(String, Option<String>, String)> = query_as(
            "SELECT id,parent_task_id,status FROM assistant_tasks WHERE user_id=?1 AND conversation_id=?2",
        ).bind(user).bind(conversation).fetch_all(&self.pool).await?;
        anyhow::ensure!(
            ids.iter().all(|id| all.iter().any(|(task_id, _, _)| task_id == id)),
            "任务不存在或无权访问"
        );
        let mut selected = ids.to_vec();
        loop {
            let children: Vec<_> = all
                .iter()
                .filter(|(id, parent, _)| {
                    !selected.contains(id) && parent.as_ref().is_some_and(|p| selected.contains(p))
                })
                .map(|(id, _, _)| id.clone())
                .collect();
            if children.is_empty() {
                break;
            }
            selected.extend(children);
        }
        for (id, _, status) in all.iter().filter(|(id, _, _)| selected.contains(id)) {
            if status == "queued" {
                self.finish(id, "cancelled", None, Some("启动前取消".into()))
                        .await?;
                self.tools.lock().await.remove(id);
            } else if ACTIVE.contains(&status.as_str()) {
                query("UPDATE assistant_tasks SET status='cancelling' WHERE id=?1 AND status='running'")
                    .bind(id)
                    .execute(&self.pool)
                    .await?;
                if let Some(running) = self.running.lock().await.get(id) {
                    running.token.cancel();
                }
                self.event(id, "state", json!({"status":"cancelling"})).await?;
            }
        }
        Ok(())
    }

    pub async fn cancel_attached(&self, user: &str, conversation: i64, job: &str) -> anyhow::Result<()> {
        self.cancel_attached_scope(user, conversation, job, None).await
    }

    async fn cancel_attached_scope(
        &self, user: &str, conversation: i64, job: &str, scope: Option<&str>,
    ) -> anyhow::Result<()> {
        // Do not recursively cancel detached descendants when stopping a reply.
        let _lock = self.mutation.lock().await;
        let tasks: Vec<(String, String)> = query_as(
            "SELECT id,status FROM assistant_tasks WHERE user_id=?1 AND conversation_id=?2
             AND generation_job_id=?3 AND detached=0 AND status IN ('queued','running','cancelling')
             AND (?4 IS NULL OR scope_id=?4)",
        ).bind(user).bind(conversation).bind(job).bind(scope).fetch_all(&self.pool).await?;
        for (id, status) in tasks {
            if status == "queued" {
                self.finish(&id, "cancelled", None, Some("回答已停止".into()))
                    .await?;
                self.tools.lock().await.remove(&id);
            } else {
                query("UPDATE assistant_tasks SET status='cancelling' WHERE id=?1 AND status='running'")
                    .bind(&id)
                    .execute(&self.pool)
                    .await?;
                if let Some(running) = self.running.lock().await.get(&id) {
                    running.token.cancel();
                }
            }
        }
        self.changed.notify_waiters();
        Ok(())
    }
}

pub async fn start(pool: SqlitePool) -> anyhow::Result<()> {
    let manager = Manager::new(pool);
    manager.recover().await?;
    MANAGER
        .set(manager.clone())
        .map_err(|_| anyhow!("task manager already initialized"))?;
    tokio::spawn(async move {
        loop {
            if let Err(e) = manager.dispatch().await {
                tracing::error!("任务调度失败: {e:#}");
            }
            tokio::select! { _ = manager.changed.notified() => {}, _ = tokio::time::sleep(Duration::from_millis(500)) => {} }
        }
    });
    Ok(())
}

pub struct Backend {
    pub manager: Arc<Manager>,
    pub user: String,
    pub conversation: i64,
    pub job: String,
    /// Main agent: all tasks in the reply. Child: only this handoff's tasks.
    pub scope_id: Option<String>,
}
impl Backend {
    pub fn new(user: String, conversation: i64, job: Uuid) -> Arc<Self> {
        Arc::new(Self {
            manager: global(),
            user,
            conversation,
            job: job.to_string(),
            scope_id: None,
        })
    }
    pub async fn close(&self) -> anyhow::Result<()> {
        match self.scope_id.as_deref() {
            Some(scope) => self.manager.cancel_attached_scope(&self.user, self.conversation, &self.job, Some(scope)).await,
            None => self.manager.cancel_attached(&self.user, self.conversation, &self.job).await,
        }
    }
    fn owns(&self, task: &TaskRecord) -> bool {
        task.user_id == self.user && task.conversation_id == self.conversation
            && task.generation_job_id == self.job
            && self.scope_id.as_ref().is_none_or(|scope| task.scope_id.as_ref() == Some(scope))
    }
    async fn selected(&self, arguments: &Value) -> anyhow::Result<Vec<TaskRecord>> {
        if let Some(ids) = arguments.get("task_ids") {
            let ids: Vec<String> = serde_json::from_value(ids.clone())?;
            anyhow::ensure!(ids.len() <= 64, "最多查询 64 个任务");
            let mut tasks = Vec::new();
            for id in ids {
                if tasks.iter().any(|task: &TaskRecord| task.id == id) {
                    continue;
                }
                let task: TaskRecord = query_as("SELECT * FROM assistant_tasks WHERE id=?1 AND user_id=?2")
                    .bind(&id)
                    .bind(&self.user)
                    .fetch_optional(&self.manager.pool)
                    .await?
                    .ok_or_else(|| anyhow!("任务不存在或无权访问"))?;
                anyhow::ensure!(self.scope_id.is_none() || self.owns(&task), "任务不属于当前子 agent");
                tasks.push(task);
            }
            Ok(tasks)
        } else {
            if self.scope_id.is_none() {
                return self.manager.list(&self.user, self.conversation).await;
            }
            Ok(query_as("SELECT * FROM assistant_tasks WHERE user_id=?1 AND conversation_id=?2 AND generation_job_id=?3 AND scope_id=?4 ORDER BY created_at,id")
                .bind(&self.user).bind(self.conversation).bind(&self.job).bind(&self.scope_id)
                .fetch_all(&self.manager.pool).await?)
        }
    }
    async fn task_page(&self, arguments: &Value) -> anyhow::Result<Value> {
        let logs = arguments["include_output"].as_bool().unwrap_or(arguments.get("task_ids").is_some());
        if arguments.get("task_ids").is_some() {
            return self.snapshot(self.selected(arguments).await?, logs).await;
        }
        let limit = arguments["limit"].as_u64().unwrap_or(TASK_PAGE_SIZE as u64);
        anyhow::ensure!(limit > 0 && limit <= MAX_TASK_PAGE_SIZE as u64, "每页任务数量需为 1–64");
        let offset = arguments["offset"].as_u64().unwrap_or(0);
        anyhow::ensure!(offset <= i64::MAX as u64, "任务分页偏移超出范围");
        let history = arguments["include_history"].as_bool().unwrap_or(false);
        let filter = if history {
            "user_id=?1 AND conversation_id=?2 AND (?4 IS NULL OR (generation_job_id=?3 AND scope_id=?4))"
        } else { ATTENTION_FILTER };
        let total: i64 = sqlx::QueryBuilder::new("SELECT count(*) FROM assistant_tasks WHERE ").push(filter)
            .build_query_scalar()
            .bind(&self.user).bind(self.conversation).bind(&self.job).bind(&self.scope_id)
            .fetch_one(&self.manager.pool).await?;
        let order = if history { "created_at DESC,id DESC" } else {
            "CASE WHEN generation_job_id=?3 AND detached=0 AND observed=0 AND status NOT IN ('queued','running','cancelling') THEN 0 ELSE 1 END,created_at,id"
        };
        let tasks: Vec<TaskRecord> = sqlx::QueryBuilder::new("SELECT * FROM assistant_tasks WHERE ").push(filter)
            .push(" ORDER BY ").push(order).push(" LIMIT ?5 OFFSET ?6").build_query_as()
            .bind(&self.user).bind(self.conversation).bind(&self.job).bind(&self.scope_id)
            .bind(limit as i64).bind(offset as i64).fetch_all(&self.manager.pool).await?;
        let end = offset.saturating_add(tasks.len() as u64);
        let mut snapshot = self.snapshot(tasks, logs).await?;
        snapshot["total"] = json!(total);
        snapshot["has_more"] = json!(end < total as u64);
        snapshot["next_offset"] = if end < total as u64 && history { json!(end) } else { Value::Null };
        if end < total as u64 && !history {
            snapshot["hint"] = json!("领取本页结果后再次调用 get_tasks({})；未领取结果优先返回。查询历史请显式使用 include_history 和 offset，查看具体输出请指定 task_ids。" );
        }
        Ok(snapshot)
    }
    async fn snapshot(&self, tasks: Vec<TaskRecord>, logs: bool) -> anyhow::Result<Value> {
        let mut completed = Vec::new();
        let mut pending = Vec::new();
        // Split a fixed response payload budget across every selected task.
        // wait_tools/run_tools retain all selected IDs, even for large batches.
        let payload_budget = (TASK_PAYLOAD_BYTES / tasks.len().max(1)).min(8192);
        for task in tasks {
            let active = task.active();
            let (error, error_truncated) = text_preview(task.error.as_deref().unwrap_or_default(), 512, false);
            // Do not repeat large commands/arguments in every model response.
            let mut value = json!({"id":task.id,"task_id":task.id,"tool":task.tool,"title":task.title,"scope_id":task.scope_id,
                "status":task.status,"detached":task.detached,"parent_task_id":task.parent_task_id,
                "started_at":task.started_at,"deadline_at":task.deadline_at,
                "result":task.result.as_ref().map(|result| result_preview(result,payload_budget / 2)),
                "error":task.error.as_ref().map(|_|error),"error_truncated":error_truncated});
            // Required unreceived results include a bounded output preview even
            // in the default status query, so acknowledgement cannot hide them.
            if logs || (!active && self.owns(&task) && !task.observed && !task.detached) {
                let events:Vec<TaskEvent> = query_as("SELECT * FROM (SELECT * FROM assistant_task_events WHERE task_id=?1 AND kind='output' ORDER BY seq DESC LIMIT 8) ORDER BY seq")
                    .bind(&task.id).fetch_all(&self.manager.pool).await?;
                let output: String = events.iter().filter_map(|event| event.data["text"].as_str()).collect();
                let (preview, truncated) = text_preview(&output, payload_budget / 2, true);
                value["output_preview"] = json!(preview);
                value["output_truncated"] = json!(truncated);
                if truncated || value["result"]["truncated"] == true {
                    value["output_hint"] = json!("仅展示有界预览；更长输出请通过任务详情或工作目录中的结果文件读取，不能推断未展示的内容。");
                }
            }
            if value["result"]["truncated"] == true {
                if let Some(original) = &task.result {
                    value["result_bytes"] = json!(original.to_string().len());
                    value["readable_fields"] = json!(result::fields(original));
                    value["read_result"] = result::reading_hint(&task.id, original);
                    value["output_hint"] = json!("结果已完整保存。通过 run_tools 提交 read_task_result，按 read_result 示例选择字段并分页；不要重复调用原工具或扫描目录找结果。next_offset 为 null 时已到末尾。");
                    tracing::info!(job_id = %task.generation_job_id, task_id = %task.id, result_bytes = original.to_string().len(), preview_budget = payload_budget / 2, "task result preview truncated");
                }
            }
            if active {
                pending.push(value);
            } else {
                completed.push(value);
            }
        }
        Ok(json!({"completed":completed,"pending":pending}))
    }
}

#[async_trait]
impl TaskBackend for Backend {
    fn for_handoff(&self, agent: &str, call_id: &str) -> Option<Arc<dyn TaskBackend>> {
        let prefix = self.scope_id.as_ref().map_or(String::new(), |scope| format!("{scope}/"));
        Some(Arc::new(Self {
            manager: self.manager.clone(), user: self.user.clone(), conversation: self.conversation,
            job: self.job.clone(), scope_id: Some(format!("{prefix}{agent}:{call_id}")),
        }))
    }
    async fn submit(&self, tasks: Vec<TaskSubmission>) -> Result<Vec<String>, ToolError> {
        async {
            let _lock = self.manager.mutation.lock().await;
            anyhow::ensure!(!tasks.is_empty() && tasks.len() <= 32, "每批任务数量需为 1–32");
            let queued:i64 = query_scalar("SELECT count(*) FROM assistant_tasks WHERE status='queued'").fetch_one(&self.manager.pool).await?;
            anyhow::ensure!(queued as usize + tasks.len() <= self.manager.max_queued, "任务队列已满，请稍后再试");
            let mut prepared = Vec::new();
            for mut submission in tasks {
                let title = submission.request.title.as_deref().context("每个任务必须填写 title，简短描述具体动作和对象，例如：检查 RDP 会话状态")?.trim();
                anyhow::ensure!(!title.is_empty() && title.chars().count() <= 200, "任务 title 必须为 1–200 个字符，不能全为空白");
                anyhow::ensure!(!title.eq_ignore_ascii_case(&submission.request.tool)
                    && !title.eq_ignore_ascii_case(&submission.tool.spec().name),
                    "任务 title 不能只填写工具名，请描述具体动作和对象，例如：运行项目测试");
                submission.request.title = Some(title.to_owned());
                if let Some(parent) = &submission.request.parent_task_id {
                    let owned: bool = query_scalar("SELECT EXISTS(SELECT 1 FROM assistant_tasks WHERE id=?1 AND user_id=?2 AND conversation_id=?3 AND generation_job_id=?4 AND (?5 IS NULL OR scope_id=?5))")
                        .bind(parent).bind(&self.user).bind(self.conversation).bind(&self.job).bind(&self.scope_id)
                        .fetch_one(&self.manager.pool).await?;
                    anyhow::ensure!(owned, "父任务必须属于当前任务作用域");
                }
                let command = submission.request.tool == "command";
                let cwd = if command { Some(command::resolve_cwd(&submission.request.arguments).await?) } else { None };
                let resource = if command { command::resource_key(&submission.request.arguments)? } else { None };
                let default = if command { setting("ASSISTANT_COMMAND_TIMEOUT_SECS", 7200) } else { setting("ASSISTANT_TOOL_TIMEOUT_SECS", 300) };
                let limit = submission.request.execution_timeout_secs
                    .or_else(|| submission.request.arguments.get("execution_timeout_secs").and_then(Value::as_u64)).unwrap_or(default as u64);
                anyhow::ensure!(limit > 0 && limit <= setting("ASSISTANT_TASK_MAX_TIMEOUT_SECS", 7 * 86400) as u64, "执行超时超出允许范围");
                let existing: Option<String> = query_scalar("SELECT id FROM assistant_tasks WHERE generation_job_id=?1 AND run_id=?2 AND source_call_id=?3 AND source_index=?4 AND user_id=?5 AND conversation_id=?6")
                    .bind(&self.job).bind(submission.run_id.to_string()).bind(&submission.call_id)
                    .bind(submission.index as i64).bind(&self.user).bind(self.conversation)
                    .fetch_optional(&self.manager.pool).await?;
                let id = existing.unwrap_or_else(||Uuid::new_v4().to_string());
                prepared.push((id, submission, cwd, resource, limit));
            }
            let mut tx = self.manager.pool.begin().await?;
            let live:bool = query_scalar("SELECT EXISTS(SELECT 1 FROM assistant_generation_jobs j WHERE j.id=?1 AND j.status='running' AND NOT EXISTS(SELECT 1 FROM assistant_messages m WHERE m.generation_job_id=j.id AND m.role='user' AND json_extract(m.metadata,'$.cancellation_requested')=1))")
                .bind(Uuid::parse_str(&self.job)?).fetch_one(&mut *tx).await?;
            anyhow::ensure!(live,"回答已停止，不能提交新任务");
            for (id, submission, cwd, resource, limit) in &prepared {
                let spec = submission.tool.spec();
                let mut args = submission.request.arguments.clone();
                if let Some(cwd) = cwd { args["cwd"] = json!(cwd); }
                query("INSERT OR IGNORE INTO assistant_tasks(id,user_id,conversation_id,generation_job_id,run_id,source_call_id,source_index,parent_task_id,title,tool,arguments,background_allowed,parallel_safe,resource_key,execution_timeout_secs,created_at,scope_id) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17)")
                    .bind(id).bind(&self.user).bind(self.conversation).bind(&self.job).bind(submission.run_id.to_string())
                    .bind(&submission.call_id).bind(submission.index as i64).bind(&submission.request.parent_task_id)
                    .bind(submission.request.title.as_deref().expect("validated task title")).bind(&spec.name).bind(args)
                    .bind(submission.tool.background_allowed()).bind(spec.behavior.parallel_safe).bind(resource).bind(*limit as i64).bind(now()).bind(&self.scope_id)
                    .execute(&mut *tx).await?;
            }
            tx.commit().await?;
            let mut ids = Vec::new();
            for (id, submission, _, _, _) in prepared {
                self.manager.tools.lock().await.insert(id.clone(), submission.tool);
                ids.push(id);
            }
            self.manager.changed.notify_waiters();
            Ok::<_, anyhow::Error>(ids)
        }.await.map_err(tool_error)
    }

    async fn control(&self, operation: &str, arguments: Value) -> Result<Value, ToolError> {
        async {
            if operation == "get_tasks" { return self.task_page(&arguments).await; }
            let mut selected = self.selected(&arguments).await?;
            match operation {
                "wait_tools" => {
                    let mode = arguments["yield_when"].as_str().unwrap_or("any");
                    anyhow::ensure!(matches!(mode,"any"|"all"), "无效等待模式");
                    let wait = arguments["wait_timeout_secs"].as_u64().unwrap_or(30).min(604800);
                    let deadline = tokio::time::Instant::now() + Duration::from_secs(wait);
                    loop {
                        let wake = self.manager.changed.notified(); tokio::pin!(wake); wake.as_mut().enable();
                        selected = self.selected(&arguments).await?;
                        if selected.is_empty() || (mode == "all" && selected.iter().all(|t|!t.active()))
                            || (mode == "any" && selected.iter().any(|t|!t.active())) { break; }
                        if tokio::time::timeout_at(deadline, wake).await.is_err() { break; }
                    }
                },
                "detach_tools" => {
                    let _lock = self.manager.mutation.lock().await;
                    selected = self.selected(&arguments).await?;
                    anyhow::ensure!(selected.iter().all(|t| t.background_allowed && t.generation_job_id == self.job), "任务不允许转入后台，或不属于当前回答");
                    let mut tx = self.manager.pool.begin().await?;
                    let live:bool = query_scalar("SELECT EXISTS(SELECT 1 FROM assistant_generation_jobs j WHERE j.id=?1 AND j.status='running' AND NOT EXISTS (SELECT 1 FROM assistant_messages m WHERE m.generation_job_id=j.id AND m.role='user' AND json_extract(m.metadata,'$.cancellation_requested')=1))")
                        .bind(Uuid::parse_str(&self.job)?).fetch_one(&mut *tx).await?;
                    anyhow::ensure!(live, "回答已停止，不能再转入后台");
                    for task in &selected {
                        query("UPDATE assistant_tasks SET detached=1 WHERE id=?1").bind(&task.id).execute(&mut *tx).await?;
                    }
                    tx.commit().await?;
                    self.manager.changed.notify_waiters();
                    selected = self.selected(&arguments).await?;
                },
                "cancel_tools" => {
                    let mut groups=HashMap::<i64,Vec<String>>::new();
                    for task in &selected { groups.entry(task.conversation_id).or_default().push(task.id.clone()); }
                    for (conversation,ids) in groups { self.manager.cancel(&self.user,conversation,&ids).await?; }
                    selected = self.selected(&arguments).await?;
                },
                _ => anyhow::bail!("未知任务操作"),
            }
            self.snapshot(selected, true).await
        }.await.map_err(tool_error)
    }

    async fn acknowledge(&self, result: &Value) -> Result<(), ToolError> {
        for task in result["completed"].as_array().into_iter().flatten() {
            if let Some(id) = task["id"].as_str() {
                // Root may inspect a previous run's unknown task. Acknowledging
                // that warning removes it from default attention queries; it
                // does not change its uncertain execution status or replay it.
                query("UPDATE assistant_tasks SET observed=1 WHERE id=?1 AND user_id=?2 AND (generation_job_id=?3 OR (?4 IS NULL AND status='unknown')) AND (?4 IS NULL OR scope_id=?4) AND conversation_id=?5")
                    .bind(id)
                    .bind(&self.user)
                    .bind(&self.job)
                    .bind(&self.scope_id)
                    .bind(self.conversation)
                    .execute(&self.manager.pool)
                    .await
                    .map_err(tool_error)?;
            }
        }
        Ok(())
    }

    async fn ready_to_finish(&self) -> Result<bool, ToolError> {
        let blocked: bool = query_scalar("SELECT EXISTS(SELECT 1 FROM assistant_tasks WHERE user_id=?1 AND conversation_id=?2 AND generation_job_id=?3 AND (?4 IS NULL OR scope_id=?4) AND detached=0 AND (status IN ('queued','running','cancelling') OR observed=0))")
            .bind(&self.user).bind(self.conversation).bind(&self.job).bind(&self.scope_id)
            .fetch_one(&self.manager.pool).await.map_err(tool_error)?;
        Ok(!blocked)
    }

    async fn finish_blockers(&self) -> Result<Value, ToolError> {
        self.manager.summary(&self.user, self.conversation, &self.job, self.scope_id.as_deref(), true)
            .await.map_err(tool_error)
    }
}

impl Drop for Backend {
    fn drop(&mut self) {
        let Some(scope) = self.scope_id.clone() else { return; };
        let manager = self.manager.clone();
        let user = self.user.clone();
        let conversation = self.conversation;
        let job = self.job.clone();
        // Handoff completion, failure and dropped futures all release their
        // scoped backend. Detached work belongs to the host and survives.
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(async move {
                if let Err(error) = manager.cancel_attached_scope(&user, conversation, &job, Some(&scope)).await {
                    tracing::error!(scope, "取消子 agent 附属任务失败: {error:#}");
                }
            });
        }
    }
}

/// Covers worker aborts and errors, as well as explicit cancellation. Detached
/// tasks are intentionally not children of the run cancellation token.
pub struct RunGuard(pub Arc<Backend>);
impl Drop for RunGuard {
    fn drop(&mut self) {
        let backend = self.0.clone();
        tokio::spawn(async move {
            if let Err(e) = backend.close().await {
                tracing::error!("取消附属任务失败: {e:#}");
            }
        });
    }
}
