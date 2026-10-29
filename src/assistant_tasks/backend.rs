//! Model task submission, scoped control, and result acknowledgement.
use anyhow::anyhow;
use async_trait::async_trait;
use g::{TaskBackend, TaskSubmission, ToolError};
use serde_json::{json, Value};
use sqlx::{query, query_as, query_scalar};
use std::{collections::HashMap, sync::Arc, time::Duration};
use uuid::Uuid;

use super::{
    command, global, now,
    preview::{result_preview, text_preview},
    result, setting, tool_error, Manager, TaskEvent, TaskRecord,
    ATTENTION_FILTER, MAX_TASK_PAGE_SIZE, TASK_PAGE_SIZE, TASK_PAYLOAD_BYTES,
};

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
    pub(super) async fn snapshot(&self, tasks: Vec<TaskRecord>, logs: bool) -> anyhow::Result<Value> {
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
                    value["output_hint"] = json!("结果已完整保存。直接调用 read_task_result，按 read_result 示例选择字段并分页；不要重复调用原工具或扫描目录找结果。next_offset 为 null 时已到末尾。");
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
                let generated_title = || {
                    let detail = match submission.request.tool.as_str() {
                        "command" => submission.request.arguments["command"].as_str().map(|value| format!("执行命令：{value}")),
                        "read_task_result" => submission.request.arguments["task_id"].as_str().map(|value| format!("读取任务结果 {value}")),
                        _ => Some(format!("执行 {}", submission.request.tool)),
                    };
                    detail.unwrap_or_else(|| format!("执行 {}", submission.request.tool))
                        .chars().take(200).collect::<String>()
                };
                let title = submission.request.title.as_deref().map(str::trim).filter(|title| !title.is_empty()).map(ToOwned::to_owned)
                    .unwrap_or_else(generated_title);
                anyhow::ensure!(!title.is_empty() && title.chars().count() <= 200, "任务 title 必须为 1–200 个字符，不能全为空白");
                anyhow::ensure!(!title.eq_ignore_ascii_case(&submission.request.tool)
                    && !title.eq_ignore_ascii_case(&submission.tool.spec().name),
                    "任务 title 不能只填写工具名，请描述具体动作和对象，例如：运行项目测试");
                submission.request.title = Some(title);
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
