//! Durable task scheduling, execution, cancellation, and recovery.
use g::Tool;
use serde_json::{json, Value};
use sqlx::{query, query_as, query_scalar, SqlitePool};
use std::{collections::{HashMap, HashSet}, sync::Arc, time::Duration};
use tokio::sync::{Mutex, Notify};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use super::{
    command, now, result, setting, ReadTaskResultTool, TaskRecord, TaskSummary,
    ACTIVE, ATTENTION_FILTER, TASK_PAGE_SIZE,
};
#[cfg(test)]
use super::TaskEvent;

pub(super) struct Running {
    pub(super) token: CancellationToken,
    task: TaskRecord,
}
pub struct Manager {
    pub pool: SqlitePool,
    pub(super) running: Mutex<HashMap<String, Running>>,
    pub(super) tools: Mutex<HashMap<String, Arc<dyn Tool>>>,
    pub changed: Notify,
    pub(super) mutation: Mutex<()>,
    pub(super) max_queued: usize,
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
        // Output chunks never enter polling snapshots and do not affect
        // scheduling; waking the dispatcher and every long poll per 4 KiB
        // chunk of command output would be pure overhead.
        if kind != "output" {
            self.changed.notify_waiters();
        }
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

    pub(super) async fn summary(
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
        let running_ids: Vec<String> = self.running.lock().await.keys().cloned().collect();
        if !running_ids.is_empty() {
            // One batched existence check instead of a query per running task.
            let mut builder = sqlx::QueryBuilder::new("SELECT id FROM assistant_tasks WHERE id IN (");
            let mut separated = builder.separated(", ");
            for id in &running_ids {
                separated.push_bind(id);
            }
            builder.push(")");
            let existing: Vec<String> =
                builder.build_query_scalar().fetch_all(&self.pool).await?;
            let existing: HashSet<&String> = existing.iter().collect();
            let stale: Vec<String> = running_ids
                .iter()
                .filter(|id| !existing.contains(*id))
                .cloned()
                .collect();
            if !stale.is_empty() {
                let running = self.running.lock().await;
                for id in &stale {
                    if let Some(running) = running.get(id) {
                        running.token.cancel();
                    }
                }
            }
        }
        let queued: Vec<TaskRecord> =
            query_as("SELECT * FROM assistant_tasks WHERE status='queued' ORDER BY created_at,id LIMIT ?1")
                .bind(self.max_queued.min(10000) as i64)
                .fetch_all(&self.pool)
                .await?;
        // Resolve live jobs once for the whole queue instead of one EXISTS per
        // non-detached task. Jobs use BLOB UUIDs; malformed historical ids are
        // not live.
        let live_jobs: HashSet<[u8; 16]> = if queued.iter().any(|task| !task.detached) {
            query_scalar("SELECT j.id FROM assistant_generation_jobs j WHERE j.status IN ('queued','running') AND NOT EXISTS(SELECT 1 FROM assistant_messages m WHERE m.generation_job_id=j.id AND m.role='user' AND json_extract(m.metadata,'$.cancellation_requested')=1)")
                .fetch_all(&self.pool)
                .await?
                .into_iter()
                .filter_map(|bytes: Vec<u8>| <[u8; 16]>::try_from(bytes.as_slice()).ok())
                .collect()
        } else {
            HashSet::new()
        };
        for task in queued {
            if !task.detached {
                let live = Uuid::parse_str(&task.generation_job_id)
                    .ok()
                    .is_some_and(|id| live_jobs.contains(&id.into_bytes()));
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

    pub(super) async fn finish(&self, id: &str, status: &str, result: Option<Value>, error: Option<String>) -> anyhow::Result<()> {
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

    pub(super) async fn cancel_attached_scope(
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
