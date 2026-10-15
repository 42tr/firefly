use super::{auth::Session, error::AppError};
use axum::{
    extract::{Path, Query},
    Json,
};
use serde::Deserialize;
use serde_json::{json, Value};

#[derive(Deserialize)]
pub struct TaskQuery {
    #[serde(default)]
    after: i64,
    #[serde(default)]
    tools_after: i64,
    #[serde(default)]
    wait_secs: u64,
}
#[derive(Deserialize)]
pub struct CancelRequest {
    task_ids: Vec<String>,
}

async fn authorize(auth: &Session, conversation: i64) -> Result<(), AppError> {
    let exists: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM assistant_conversations WHERE id=?1 AND user_id=?2 AND agent='firefly')",
    )
    .bind(conversation)
    .bind(auth.storage_scope())
    .fetch_one(crate::db::pool())
    .await
    .map_err(AppError::internal_server_error)?;
    if exists {
        Ok(())
    } else {
        Err(AppError::not_found("会话不存在"))
    }
}

/// A durable cursor plus a full state snapshot. Long polling remains active
/// after the reply's SSE ends; reconnecting never depends on an in-memory log.
pub async fn list(
    auth: Session,
    Path(conversation): Path<i64>,
    Query(args): Query<TaskQuery>,
) -> Result<Json<Value>, AppError> {
    authorize(&auth, conversation).await?;
    let manager = crate::assistant_tasks::global();
    Ok(Json(
        task_page(
            auth.storage_scope(),
            conversation,
            args.after.max(0),
            args.tools_after.max(0),
            std::time::Duration::from_secs(args.wait_secs.min(20)),
            &manager.changed,
        )
        .await?,
    ))
}

async fn task_page(
    user: &str,
    conversation: i64,
    after: i64,
    tools_after: i64,
    wait: std::time::Duration,
    changed: &tokio::sync::Notify,
) -> Result<Value, AppError> {
    task_page_observed(user, conversation, after, tools_after, wait, changed, || {}).await
}

async fn task_page_observed(
    user: &str,
    conversation: i64,
    after: i64,
    tools_after: i64,
    wait: std::time::Duration,
    changed: &tokio::sync::Notify,
    mut snapshot_read: impl FnMut(),
) -> Result<Value, AppError> {
    let deadline = tokio::time::Instant::now() + wait;
    let mut cursor = after;
    let mut previous_tasks = None;
    loop {
        // Register before reading so a mutation between the reads and the wait
        // cannot be lost. Output and other conversations share this notifier.
        let wake = changed.notified();
        tokio::pin!(wake);
        wake.as_mut().enable();
        let (events, next_cursor) = summary_events(user, conversation, cursor).await?;
        cursor = next_cursor;
        // Tools have their own revision cursor: even a start/finish that
        // happened before this request must return immediately.
        let tool_cursor: i64 = sqlx::query_scalar("SELECT COALESCE(MAX(d.revision),0) FROM assistant_tool_details d JOIN assistant_conversations c ON c.id=d.conversation_id WHERE c.user_id=?1 AND c.id=?2")
            .bind(user).bind(conversation).fetch_one(crate::db::pool()).await.map_err(AppError::internal_server_error)?;
        let tasks = task_summaries(user, conversation).await?;
        // Some mutations (submission, detach, cancelling) update summaries
        // without adding state events, so compare the snapshot as well.
        if !events.is_empty()
            || tool_cursor > tools_after
            || previous_tasks
                .as_ref()
                .is_some_and(|previous| previous != &tasks)
            || tokio::time::Instant::now() >= deadline
        {
            return Ok(
                json!({"tasks":tasks,"has_more":events.len()==256,"events":events,"cursor":cursor,"tool_cursor":tool_cursor}),
            );
        }
        previous_tasks = Some(tasks);
        snapshot_read();
        // Keep the original deadline across unrelated wakeups.
        let _ = tokio::time::timeout_at(deadline, wake).await;
    }
}

async fn task_summaries(user: &str, conversation: i64) -> Result<Vec<Value>, AppError> {
    let mut tasks: Vec<Value> = sqlx::query_scalar("SELECT json_object('id',id,'generation_job_id',generation_job_id,'scope_id',scope_id,'parent_task_id',parent_task_id,'title',title,'tool',tool,'status',status,'detached',json(CASE detached WHEN 1 THEN 'true' ELSE 'false' END),'created_at',created_at,'started_at',started_at,'deadline_at',deadline_at,'ended_at',ended_at) FROM assistant_tasks WHERE user_id=?1 AND conversation_id=?2 ORDER BY created_at,id")
        .bind(user).bind(conversation).fetch_all(crate::db::pool()).await.map_err(AppError::internal_server_error)?;
    // Only a bounded description enters polling snapshots; full tool I/O
    // continues to be loaded on demand. Programmer reports are JSON strings,
    // while other agents can return plain text.
    let calls: Vec<Value> = sqlx::query_scalar("WITH descriptions AS (
        SELECT d.*,d.rowid AS detail_order,
            CASE WHEN d.name GLOB 'handoff_to_*' AND json_valid(d.output)
                THEN CASE json_type(d.output) WHEN 'text' THEN json_extract(d.output,'$') ELSE d.output END END AS response,
            CASE WHEN d.name GLOB 'handoff_to_*' AND json_valid(d.input)
                THEN json_extract(d.input,'$.task') END AS assignment
        FROM assistant_tool_details d JOIN assistant_conversations c ON c.id=d.conversation_id
        WHERE c.user_id=?1 AND c.id=?2 AND d.job_id!=''),
        summaries AS (
            SELECT *,
                CASE WHEN json_valid(response) THEN COALESCE(NULLIF(json_extract(response,'$.summary'),''),
                    NULLIF(json_extract(response,'$.response'),''),NULLIF(json_extract(response,'$.message'),''))
                    ELSE NULLIF(response,'') END AS result_summary,
                CASE WHEN json_valid(assignment) THEN COALESCE(NULLIF(json_extract(assignment,'$.goal'),''),
                    NULLIF(json_extract(assignment,'$.summary'),'')) ELSE NULLIF(assignment,'') END AS task_summary
            FROM descriptions)
        SELECT json_object(
        'call_id',d.call_id,'parent_call_id',d.parent_id,'generation_job_id',d.job_id,'tool',d.name,
        'status',CASE
            WHEN d.status='completed' THEN 'succeeded'
            WHEN d.status='error' THEN 'failed'
            WHEN d.status IN ('queued','running') AND j.status='failed' AND j.error_message='用户已停止生成' THEN 'cancelled'
            WHEN d.status IN ('queued','running') AND (j.status IN ('completed','failed') OR p.status IN ('completed','error','unknown','cancelled')) THEN 'unknown'
            ELSE d.status END,
        'created_at',d.created_at,'started_at',d.started_at,
        'ended_at',COALESCE(d.ended_at,p.ended_at,CASE WHEN j.status IN ('completed','failed') THEN j.updated_at/1000 END),
        'summary',substr(COALESCE(d.result_summary,d.task_summary),1,241))
        FROM summaries d
        LEFT JOIN assistant_generation_jobs j ON j.id=unhex(replace(d.job_id,'-',''))
        LEFT JOIN assistant_tool_details p ON p.conversation_id=d.conversation_id AND p.job_id=d.job_id AND p.call_id=d.parent_id AND p.parent_id=''
        WHERE d.name NOT IN ('run_tools','wait_tools','get_tasks','detach_tools','cancel_tools')
        ORDER BY d.created_at,d.detail_order")
        .bind(user).bind(conversation).fetch_all(crate::db::pool()).await.map_err(AppError::internal_server_error)?;
    let call_id = |job: &str, parent: &str, call: &str| format!("tool:{}", json!([job,parent,call]));
    let handoffs: std::collections::HashMap<_, _> = calls.iter().filter_map(|call| {
        let agent = call["tool"].as_str()?.strip_prefix("handoff_to_")?;
        let job = call["generation_job_id"].as_str()?;
        let id = call["call_id"].as_str()?;
        Some(((job.to_owned(), format!("{agent}:{id}")), call_id(job, call["parent_call_id"].as_str().unwrap_or_default(), id)))
    }).collect();
    for task in &mut tasks {
        // Explicit task parents still take precedence over the owning agent.
        if !task["parent_task_id"].is_null() { continue; }
        if let (Some(job), Some(scope)) = (task["generation_job_id"].as_str(), task["scope_id"].as_str()) {
            if let Some((_, parent)) = handoffs.iter().find(|((owner, branch), _)| owner == job && (scope == branch || scope.ends_with(&format!("/{branch}")))) {
                task["parent_task_id"] = json!(parent);
            }
        }
    }
    for mut call in calls {
        let job = call["generation_job_id"].as_str().unwrap_or_default().to_owned();
        let parent = call["parent_call_id"].as_str().unwrap_or_default().to_owned();
        call["id"] = json!(call_id(&job, &parent, call["call_id"].as_str().unwrap_or_default()));
        if !parent.is_empty() { call["parent_task_id"] = json!(call_id(&job, "", &parent)); }
        let name = call["tool"].as_str().unwrap_or_default();
        let agent = name.strip_prefix("handoff_to_");
        let title = agent.map(|agent| format!("{agent} agent")).unwrap_or_else(|| name.to_owned());
        let is_handoff = agent.is_some();
        call["kind"] = json!(if is_handoff { "handoff" } else { "tool" });
        call["title"] = json!(title);
        if is_handoff {
            call["summary"] = json!(workflow_agent_summary(call["summary"].as_str().unwrap_or_default()));
        } else {
            call.as_object_mut().unwrap().remove("summary");
        }
        tasks.push(call);
    }
    tasks.sort_by_key(|task| task["created_at"].as_i64().unwrap_or_default());
    Ok(tasks)
}

fn workflow_agent_summary(text: &str) -> String {
    let line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut chars = line.chars();
    let mut summary: String = chars.by_ref().take(160).collect();
    if chars.next().is_some() { summary.push('…'); }
    summary
}

const TASK_EVENT_CEILING_SQL: &str = "SELECT COALESCE(MAX((SELECT MAX(e.seq) FROM assistant_task_events e WHERE e.task_id=t.id)),?3) FROM assistant_tasks t WHERE t.user_id=?1 AND t.conversation_id=?2";
// Keep scoped tasks outermost and seek the partial covering index. After
// ANALYZE, SQLite can otherwise prefer a rowid scan including output chunks
// and unrelated tasks. The polling-index migration installs this index.
const TASK_SUMMARY_EVENTS_SQL: &str = "SELECT json_object('seq',e.seq,'task_id',e.task_id,'kind',e.kind,'created_at',e.created_at) FROM assistant_tasks t CROSS JOIN assistant_task_events e INDEXED BY assistant_task_events_summary ON e.task_id=t.id WHERE t.user_id=?1 AND t.conversation_id=?2 AND e.seq>?3 AND e.seq<=?4 AND e.kind!='output' ORDER BY e.seq LIMIT 256";

async fn summary_events(user: &str, conversation: i64, after: i64) -> Result<(Vec<Value>, i64), AppError> {
    // Output chunks do not enter snapshots or consume the state-page budget.
    // Seek to the last event of each task via (task_id, seq), rather than
    // aggregating every output chunk in the conversation on every poll.
    let ceiling: i64 = sqlx::query_scalar(TASK_EVENT_CEILING_SQL)
        .bind(user).bind(conversation).bind(after).fetch_one(crate::db::pool()).await.map_err(AppError::internal_server_error)?;
    let events: Vec<Value> = sqlx::query_scalar(TASK_SUMMARY_EVENTS_SQL)
        .bind(user).bind(conversation).bind(after).bind(ceiling).fetch_all(crate::db::pool()).await.map_err(AppError::internal_server_error)?;
    let cursor = if events.len() == 256 { events.last().and_then(|e| e["seq"].as_i64()).unwrap_or(after) } else { ceiling.max(after) };
    Ok((events, cursor))
}

pub async fn detail(auth: Session, Path((conversation, task_id)): Path<(i64, String)>) -> Result<Json<Value>, AppError> {
    authorize(&auth, conversation).await?;
    let task: crate::assistant_tasks::TaskRecord = sqlx::query_as("SELECT * FROM assistant_tasks WHERE id=?1 AND conversation_id=?2 AND user_id=?3")
        .bind(&task_id).bind(conversation).bind(auth.storage_scope()).fetch_optional(crate::db::pool()).await
        .map_err(AppError::internal_server_error)?.ok_or(AppError::not_found("任务不存在"))?;
    let events: Vec<Value> = sqlx::query_scalar("SELECT data FROM (SELECT seq,data FROM assistant_task_events WHERE task_id=?1 AND kind='output' ORDER BY seq DESC LIMIT 64) ORDER BY seq")
        .bind(task_id).fetch_all(crate::db::pool()).await.map_err(AppError::internal_server_error)?;
    let mut output = String::new();
    for event in events {
        if let Some(text) = event["text"].as_str() {
            if event["stream"] == "stderr" { output.push_str("[stderr] "); }
            output.push_str(text);
        }
    }
    let start = output.char_indices().rev().nth(99_999).map_or(0, |(index,_)| index);
    Ok(Json(json!({"id":task.id,"arguments":task.arguments,"result":task.result,"error":task.error,"status":task.status,"output":&output[start..]})))
}

pub async fn cancel(
    auth: Session,
    Path(conversation): Path<i64>,
    Json(args): Json<CancelRequest>,
) -> Result<Json<Value>, AppError> {
    authorize(&auth, conversation).await?;
    if args.task_ids.is_empty() || args.task_ids.len() > 64 {
        return Err(AppError::bad_request("请选择 1–64 个任务"));
    }
    crate::assistant_tasks::global()
        .cancel(auth.storage_scope(), conversation, &args.task_ids)
        .await
        .map_err(AppError::bad_request)?;
    Ok(Json(json!({"ok":true})))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use tokio::{sync::Notify, time::timeout};

    #[test]
    fn summary_queries_skip_output_history_and_unrelated_tasks() {
        let connection = crate::db::test_connection();
        connection.execute_batch("INSERT INTO assistant_conversations(id,user_id,system_prompt) VALUES (1,'owner',''),(2,'owner','');
            INSERT INTO assistant_tasks(id,user_id,conversation_id,generation_job_id,run_id,source_call_id,source_index,title,tool,arguments,execution_timeout_secs,created_at)
            VALUES ('a','owner',1,'job','run','a',0,'a','command','{}',60,1),
                   ('b','owner',1,'job','run','b',0,'b','command','{}',60,1),
                   ('other-owner','other',1,'job','run','c',0,'c','command','{}',60,1),
                   ('other-conversation','owner',2,'job','run','d',0,'d','command','{}',60,1);
            WITH RECURSIVE n(i) AS (VALUES(1) UNION ALL SELECT i+1 FROM n WHERE i<20000)
            INSERT INTO assistant_task_events(task_id,kind,data,created_at)
            SELECT CASE WHEN i%2=0 THEN 'other-owner' ELSE 'other-conversation' END,'state','{}',i FROM n;
            WITH RECURSIVE n(i) AS (VALUES(1) UNION ALL SELECT i+1 FROM n WHERE i<20000)
            INSERT INTO assistant_task_events(task_id,kind,data,created_at)
            SELECT CASE WHEN i%2=0 THEN 'a' ELSE 'b' END,CASE WHEN i%157=0 THEN 'state' ELSE 'output' END,'{}',i FROM n;").unwrap();
        for analyze in [false, true] {
            if analyze { connection.execute_batch("ANALYZE").unwrap(); }
            let mut ceiling_query = connection.prepare(TASK_EVENT_CEILING_SQL).unwrap();
            let ceiling: i64 = ceiling_query.query_row(rusqlite::params!["owner", 1, 0], |row| row.get(0)).unwrap();
            assert_eq!(ceiling, 40000);
            let steps = ceiling_query.get_status(rusqlite::StatementStatus::VmStep);
            assert!(steps < 500, "ceiling scanned output history: {steps} steps");

            let mut events_query = connection.prepare(TASK_SUMMARY_EVENTS_SQL).unwrap();
            let events: Vec<String> = events_query.query_map(rusqlite::params!["owner", 1, 0, ceiling], |row| row.get(0))
                .unwrap().collect::<Result<_, _>>().unwrap();
            let seqs: Vec<i64> = events.iter().map(|event| serde_json::from_str::<Value>(event).unwrap()["seq"].as_i64().unwrap()).collect();
            assert_eq!(seqs, (1..=127).map(|n| 20000+n*157).collect::<Vec<_>>());
            let steps = events_query.get_status(rusqlite::StatementStatus::VmStep);
            assert!(steps < 10000, "summary scanned output/unrelated history: {steps} steps");
        }
    }

    async fn polling_fixture() -> (sqlx::SqlitePool, i64) {
        let pool = crate::db::test_pool().await;
        crate::db::set_test_pool(pool.clone());
        let conversation = sqlx::query_scalar("INSERT INTO assistant_conversations(user_id,system_prompt,agent) VALUES ('owner','','firefly') RETURNING id")
            .fetch_one(&pool).await.unwrap();
        sqlx::query("INSERT INTO assistant_tasks(id,user_id,conversation_id,generation_job_id,run_id,source_call_id,source_index,title,tool,arguments,status,execution_timeout_secs,created_at) VALUES ('task','owner',?1,'job','run','call',0,'build','command','{}','running',60,1),('other','other-owner',?1,'other-job','run','call',0,'other build','command','{}','running',60,1)")
            .bind(conversation).execute(&pool).await.unwrap();
        (pool, conversation)
    }

    #[tokio::test]
    async fn workflow_groups_child_calls_and_scoped_tasks_without_exposing_io() {
        let (pool, conversation) = polling_fixture().await;
        sqlx::query("DELETE FROM assistant_tasks").execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO assistant_tool_details(conversation_id,job_id,call_id,parent_id,name,input,output,status,created_at,started_at,ended_at,revision)
            VALUES (?1,'job','delegate','','handoff_to_programmer','{\"task\":\"运行项目测试\",\"context\":\"private input\"}',NULL,'running',1,2,NULL,1),
                (?1,'job','reviewer','','handoff_to_reviewer','{}',NULL,'running',3,3,NULL,2),
                (?1,'job','same','delegate','shell','{}','\"private output\"','completed',4,4,9,3),
                (?1,'job','same','reviewer','inspect_resource','{}',NULL,'error',5,6,10,4),
                (?1,'job','wait','delegate','wait_tools','{}',NULL,'running',6,6,NULL,5)")
            .bind(conversation).execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO assistant_tasks(id,user_id,conversation_id,generation_job_id,run_id,source_call_id,source_index,parent_task_id,scope_id,title,tool,arguments,status,execution_timeout_secs,created_at)
            VALUES ('parent','owner',?1,'job','run','batch',0,NULL,'programmer:delegate','运行项目测试','command','{}','running',60,7),
                ('child','owner',?1,'job','run','batch',1,'parent','programmer:delegate','检查测试结果','command','{}','queued',60,8)")
            .bind(conversation).execute(&pool).await.unwrap();
        let tasks = task_summaries("owner", conversation).await.unwrap();
        assert_eq!(tasks.len(), 6);
        let handoff = tasks.iter().find(|task| task["call_id"] == "delegate").unwrap();
        assert_eq!(handoff["kind"], "handoff");
        assert_eq!(handoff["title"], "programmer agent");
        assert_eq!(handoff["status"], "running");
        assert_eq!(handoff["summary"], "运行项目测试");
        let same: Vec<_> = tasks.iter().filter(|task| task["call_id"] == "same").collect();
        assert_ne!(same[0]["id"], same[1]["id"]);
        assert_eq!(same[0]["parent_task_id"], handoff["id"]);
        assert_eq!(same[0]["status"], "succeeded");
        assert_eq!(same[0]["title"], "shell");
        assert_eq!(same[1]["title"], "inspect_resource");
        assert_eq!(same[0]["ended_at"], 9);
        assert_eq!(same[1]["status"], "failed");
        assert_eq!(tasks.iter().find(|task| task["id"] == "parent").unwrap()["parent_task_id"], handoff["id"]);
        assert_eq!(tasks.iter().find(|task| task["id"] == "child").unwrap()["parent_task_id"], "parent");
        for task in &tasks {
            for key in ["input", "output", "arguments", "result", "error"] { assert!(task.get(key).is_none()); }
        }
        assert!(!json!(tasks).to_string().contains("private"));
        assert!(task_summaries("other-owner", conversation).await.unwrap().is_empty());

        sqlx::query("UPDATE assistant_tool_details SET status='error',ended_at=20 WHERE call_id='reviewer'")
            .execute(&pool).await.unwrap();
        sqlx::query("UPDATE assistant_tool_details SET status='running',ended_at=NULL WHERE parent_id='reviewer'")
            .execute(&pool).await.unwrap();
        let tasks = task_summaries("owner", conversation).await.unwrap();
        let interrupted = tasks.iter().find(|task| task["parent_call_id"] == "reviewer").unwrap();
        assert_eq!(interrupted["status"], "unknown");
        assert_eq!(interrupted["ended_at"], 20);
    }

    #[tokio::test]
    async fn workflow_agent_summaries_use_reports_and_keep_full_io_out_of_snapshots() {
        let (pool, conversation) = polling_fixture().await;
        let cases = [
            ("running", json!({"task":json!({"goal":"复核前端样式","context":"private input"}).to_string()}), None, "running", "复核前端样式".to_owned()),
            ("report", json!({"task":"复核前端样式"}), Some(json!(json!({"summary":"已核实样式引用\n 并生成审查报告","files":["private output"]}).to_string())), "completed", "已核实样式引用 并生成审查报告".to_owned()),
            ("weekly", json!({"task":"生成周报"}), Some(json!("已汇总本周工作并生成周报")), "completed", "已汇总本周工作并生成周报".to_owned()),
            ("object", json!({"task":"检查构建"}), Some(json!({"summary":"构建已通过","details":"private output"})), "completed", "构建已通过".to_owned()),
            ("long", json!({"task":"检查代码"}), Some(json!("检查完成".repeat(100))), "completed", format!("{}…", "检查完成".repeat(40))),
        ];
        for (call, input, output, status, _) in &cases {
            sqlx::query("INSERT INTO assistant_tool_details(conversation_id,job_id,call_id,name,input,output,status) VALUES (?1,'job',?2,'handoff_to_programmer',?3,?4,?5)")
                .bind(conversation).bind(call).bind(input).bind(output).bind(status).execute(&pool).await.unwrap();
        }
        let tasks = task_summaries("owner", conversation).await.unwrap();
        for (call, _, _, _, summary) in cases {
            let task = tasks.iter().find(|task| task["call_id"] == call).unwrap();
            assert_eq!(task["summary"], summary);
            for key in ["input", "output", "arguments", "result"] { assert!(task.get(key).is_none()); }
        }
        assert!(!json!(tasks).to_string().contains("private"));
    }

    #[tokio::test]
    async fn restarted_handoffs_stop_timing_while_the_parent_resumes() {
        let (pool, conversation) = polling_fixture().await;
        let job = uuid::Uuid::new_v4();
        sqlx::query("INSERT INTO assistant_generation_jobs(id,conversation_id,user_id,status) VALUES (?1,?2,'owner','running')")
            .bind(job).bind(conversation).execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO assistant_messages(conversation_id,generation_job_id,role,content,tool_calls) VALUES (?1,?2,'user','检查样式',NULL),(?1,?2,'assistant','',?3)")
            .bind(conversation).bind(job).bind(json!([{"id":"delegate","name":"handoff_to_programmer","arguments":{"task":"检查样式"}}]))
            .execute(&pool).await.unwrap();
        // A previous restart already added a result but left its archive live.
        sqlx::query("INSERT INTO assistant_messages(conversation_id,generation_job_id,role,content,tool_call_id,metadata,is_error,created_at)
            VALUES (?1,?2,'tool','{\"error\":\"interrupted_by_restart\"}','older','{\"interrupted\":true}',1,7000000),
                (?1,?2,'tool','检查已完成','returned','{}',0,6000000),
                (?1,?2,'tool','{\"error\":\"build failed\"}','failed','{}',1,8000000)")
            .bind(conversation).bind(job).execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO assistant_tool_details(conversation_id,job_id,call_id,parent_id,name,status,created_at,started_at,ended_at,revision)
            VALUES (?1,?2,'delegate','','handoff_to_programmer','running',1000,2000,NULL,1),
                (?1,?2,'older','','handoff_to_programmer','running',1000,2000,NULL,2),
                (?1,?2,'pending','delegate','shell','running',3000,3000,NULL,3),
                (?1,?2,'finished','delegate','shell','completed',3000,3000,4000,4),
                (?1,?2,'returned','','handoff_to_programmer','queued',1000,2000,NULL,4),
                (?1,?2,'failed','','handoff_to_programmer','running',1000,2000,NULL,4),
                (?1,'other-job','delegate','','handoff_to_programmer','running',1000,2000,NULL,5)")
            .bind(conversation).bind(job.to_string()).execute(&pool).await.unwrap();
        let recovery = crate::assistant_store::requeue_interrupted_generations().await.unwrap();
        assert_eq!(recovery.requeued, 1);
        // The parent can continue, but its old child must remain terminal.
        sqlx::query("UPDATE assistant_generation_jobs SET status='running' WHERE id=?1")
            .bind(job).execute(&pool).await.unwrap();
        let tasks = task_summaries("owner", conversation).await.unwrap();
        let own: Vec<_> = tasks.iter().filter(|task| task["generation_job_id"] == job.to_string()).collect();
        let delegate = own.iter().find(|task| task["call_id"] == "delegate").unwrap();
        assert_eq!(delegate["status"], "unknown");
        assert!(delegate["ended_at"].as_i64().unwrap() >= 2000);
        let pending = own.iter().find(|task| task["call_id"] == "pending").unwrap();
        assert_eq!(pending["status"], "unknown");
        assert_eq!(pending["ended_at"], delegate["ended_at"]);
        let older = own.iter().find(|task| task["call_id"] == "older").unwrap();
        assert_eq!(older["status"], "unknown");
        assert_eq!(older["ended_at"], 7000);
        let finished = own.iter().find(|task| task["call_id"] == "finished").unwrap();
        assert_eq!(finished["status"], "succeeded");
        assert_eq!(finished["ended_at"], 4000);
        let returned = own.iter().find(|task| task["call_id"] == "returned").unwrap();
        assert_eq!(returned["status"], "succeeded");
        assert_eq!(returned["ended_at"], 6000);
        assert_eq!(returned["summary"], "检查已完成");
        let failed = own.iter().find(|task| task["call_id"] == "failed").unwrap();
        assert_eq!(failed["status"], "failed");
        assert_eq!(failed["ended_at"], 8000);
        let other = tasks.iter().find(|task| task["generation_job_id"] == "other-job").unwrap();
        assert_eq!(other["status"], "running");
        assert!(other["ended_at"].is_null());
        let revision: i64 = sqlx::query_scalar("SELECT MAX(revision) FROM assistant_tool_details WHERE conversation_id=?1")
            .bind(conversation).fetch_one(&pool).await.unwrap();
        assert!(revision > 5);
        crate::assistant_store::requeue_interrupted_generations().await.unwrap();
        let tasks_after = task_summaries("owner", conversation).await.unwrap();
        assert_eq!(tasks_after, tasks);
    }

    #[tokio::test]
    async fn tool_revisions_return_immediately_and_wake_an_existing_long_poll() {
        let (pool, conversation) = polling_fixture().await;
        let changed = Notify::new();
        sqlx::query("INSERT INTO assistant_tool_details(conversation_id,job_id,call_id,name,status,revision) VALUES (?1,'job','handoff','handoff_to_weekly','running',1)")
            .bind(conversation).execute(&pool).await.unwrap();
        let page = timeout(Duration::from_millis(100), task_page("owner", conversation, 0, 0, Duration::from_secs(2), &changed))
            .await.unwrap().unwrap();
        assert_eq!(page["tool_cursor"], 1);
        assert!(page["events"].as_array().unwrap().is_empty());
        let request = task_page("owner", conversation, 0, 1, Duration::from_secs(2), &changed);
        tokio::pin!(request);
        assert!(timeout(Duration::from_millis(30), &mut request).await.is_err());
        sqlx::query("UPDATE assistant_tool_details SET status='completed',revision=2 WHERE conversation_id=?1")
            .bind(conversation).execute(&pool).await.unwrap();
        changed.notify_waiters();
        let page = timeout(Duration::from_millis(100), &mut request).await.unwrap().unwrap();
        assert_eq!(page["tool_cursor"], 2);
        assert_eq!(page["tasks"][0]["status"], "succeeded");
    }

    #[tokio::test]
    async fn stopped_or_failed_replies_do_not_leave_inline_steps_running() {
        let (pool, conversation) = polling_fixture().await;
        let job = uuid::Uuid::new_v4();
        sqlx::query("INSERT INTO assistant_generation_jobs(id,conversation_id,user_id,status,error_message,updated_at) VALUES (?1,?2,'owner','failed','用户已停止生成',5000000)")
            .bind(job).bind(conversation).execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO assistant_tool_details(conversation_id,job_id,call_id,name,status) VALUES (?1,?2,'handoff','handoff_to_weekly','running'),(?1,?2,'waiting','mcp__example__read','queued')")
            .bind(conversation).bind(job.to_string()).execute(&pool).await.unwrap();
        let tasks = task_summaries("owner", conversation).await.unwrap();
        for task in tasks.iter().filter(|task| task["kind"].is_string()) {
            assert_eq!(task["status"], "cancelled");
            assert_eq!(task["ended_at"], 5000);
            assert!(task["started_at"].is_null());
        }
        sqlx::query("UPDATE assistant_generation_jobs SET error_message='connection lost' WHERE id=?1")
            .bind(job).execute(&pool).await.unwrap();
        let tasks = task_summaries("owner", conversation).await.unwrap();
        assert!(tasks.iter().filter(|task| task["kind"].is_string()).all(|task| task["status"] == "unknown"));
    }

    #[tokio::test]
    async fn long_poll_ignores_output_and_other_owners_but_returns_summary_mutations() {
        let (pool, conversation) = polling_fixture().await;
        let changed = Notify::new();
        let snapshot_read = Notify::new();
        let request = task_page_observed("owner", conversation, 0, 0, Duration::from_secs(2), &changed, || snapshot_read.notify_one());
        tokio::pin!(request);
        tokio::select! {
            result = &mut request => panic!("poll returned before a change: {result:?}"),
            _ = snapshot_read.notified() => {},
        }

        for (id, kind) in [("task", "output"), ("other", "state")] {
            sqlx::query("INSERT INTO assistant_task_events(task_id,kind,data,created_at) VALUES (?1,?2,'{}',1)")
                .bind(id).bind(kind).execute(&pool).await.unwrap();
            changed.notify_waiters();
            assert!(timeout(Duration::from_millis(30), &mut request)
                .await
                .is_err());
        }
        // Detach and cancelling have no durable state event of their own.
        sqlx::query("UPDATE assistant_tasks SET detached=1,status='cancelling' WHERE id='task'")
            .execute(&pool)
            .await
            .unwrap();
        changed.notify_waiters();
        let page = timeout(Duration::from_secs(1), &mut request)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(page["events"], json!([]));
        assert_eq!(page["tasks"].as_array().unwrap().len(), 1);
        assert_eq!(page["tasks"][0]["detached"], true);
        assert_eq!(page["tasks"][0]["status"], "cancelling");
        assert_eq!(page["cursor"], 1); // Output skipped; another owner's cursor excluded.
    }

    #[tokio::test]
    async fn long_poll_returns_new_tasks_and_state_events_without_waiting_for_timeout() {
        let (pool, conversation) = polling_fixture().await;
        let changed = Notify::new();
        let snapshot_read = Notify::new();
        let request = task_page_observed("owner", conversation, 0, 0, Duration::from_secs(2), &changed, || snapshot_read.notify_one());
        tokio::pin!(request);
        tokio::select! {
            result = &mut request => panic!("poll returned before a change: {result:?}"),
            _ = snapshot_read.notified() => {},
        }
        sqlx::query("INSERT INTO assistant_tasks(id,user_id,conversation_id,generation_job_id,run_id,source_call_id,source_index,title,tool,arguments,status,execution_timeout_secs,created_at) VALUES ('new','owner',?1,'job','run','new-call',0,'test','command','{}','queued',60,2)")
            .bind(conversation).execute(&pool).await.unwrap();
        changed.notify_waiters();
        let page = timeout(Duration::from_secs(1), &mut request)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(page["tasks"].as_array().unwrap().len(), 2);
        assert_eq!(page["events"], json!([]));

        sqlx::query("INSERT INTO assistant_task_events(task_id,kind,data,created_at) VALUES ('task','state','{}',1)")
            .execute(&pool).await.unwrap();
        let page = timeout(
            Duration::from_secs(1),
            task_page("owner", conversation, 0, 0, Duration::from_secs(2), &changed),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(page["events"][0]["kind"], "state");
        assert_eq!(page["cursor"], 1);
    }

    #[tokio::test]
    async fn unrelated_wakeups_do_not_extend_the_long_poll_deadline() {
        let (_pool, conversation) = polling_fixture().await;
        let changed = Notify::new();
        let request = async {
            timeout(
                Duration::from_millis(300),
                task_page(
                    "owner",
                    conversation,
                    0,
                    0,
                    Duration::from_millis(100),
                    &changed,
                ),
            )
            .await
            .unwrap()
            .unwrap()
        };
        let notifications = async {
            for _ in 0..20 {
                tokio::time::sleep(Duration::from_millis(20)).await;
                changed.notify_waiters();
            }
        };
        let (page, ()) = tokio::join!(request, notifications);
        assert_eq!(page["events"], json!([]));
        assert_eq!(page["cursor"], 0);
        assert_eq!(page["tasks"].as_array().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn summaries_omit_io_and_skip_output_chunks_without_losing_state_events() {
        let pool = crate::db::test_pool().await;
        crate::db::set_test_pool(pool.clone());
        let conversation: i64 = sqlx::query_scalar("INSERT INTO assistant_conversations(user_id,system_prompt,agent) VALUES ('owner','','firefly') RETURNING id")
            .fetch_one(&pool).await.unwrap();
        sqlx::query("INSERT INTO assistant_tasks(id,user_id,conversation_id,generation_job_id,run_id,source_call_id,source_index,title,tool,arguments,status,execution_timeout_secs,created_at,result,error) VALUES ('task','owner',?1,'job','run','call',0,'build','command',?2,'failed',60,1,?3,'private error')")
            .bind(conversation).bind(json!({"command":"private input"})).bind(json!({"stdout":"private result"})).execute(&pool).await.unwrap();
        let output = "好".repeat(100_010);
        sqlx::query("INSERT INTO assistant_task_events(task_id,kind,data,created_at) VALUES ('task','output',?1,1)")
            .bind(json!({"stream":"stdout","text":output})).execute(&pool).await.unwrap();
        sqlx::query("WITH RECURSIVE n(x) AS (SELECT 1 UNION ALL SELECT x+1 FROM n WHERE x<270) INSERT INTO assistant_task_events(task_id,kind,data,created_at) SELECT 'task','state','{\"private\":\"diagnostics\"}',x FROM n")
            .execute(&pool).await.unwrap();
        let tasks = task_summaries("owner", conversation).await.unwrap();
        assert_eq!(tasks.len(), 1);
        assert!(!json!(tasks).to_string().contains("private"));
        for key in ["arguments", "result", "error"] { assert!(tasks[0].get(key).is_none()); }
        let (first, cursor) = summary_events("owner", conversation, 0).await.unwrap();
        assert_eq!(first.len(), 256);
        assert!(!json!(first).to_string().contains("private"));
        assert!(first.iter().all(|event| event["kind"] == "state" && event.get("data").is_none()));
        let (second, end) = summary_events("owner", conversation, cursor).await.unwrap();
        assert_eq!(second.len(), 14);
        assert!(end > cursor);
        assert!(summary_events("owner", conversation, end).await.unwrap().0.is_empty());
        let path = || Path((conversation, "task".into()));
        let Json(details) = detail(Session::for_storage("owner"), path()).await.unwrap();
        assert_eq!(details["arguments"]["command"], "private input");
        assert_eq!(details["result"]["stdout"], "private result");
        assert_eq!(details["output"].as_str().unwrap().chars().count(), 100_000);
        assert!(detail(Session::for_storage("other"), path()).await.is_err());
        assert!(detail(Session::for_storage("owner"), Path((conversation+1, "task".into()))).await.is_err());
    }
    #[tokio::test]
    async fn task_endpoints_reject_other_owners_and_non_firefly_conversations() {
        let pool = crate::db::test_pool().await;
        crate::db::set_test_pool(pool.clone());
        let id:i64=sqlx::query_scalar("INSERT INTO assistant_conversations(user_id,system_prompt,agent) VALUES ('owner','','firefly') RETURNING id")
            .fetch_one(&pool).await.unwrap();
        assert!(authorize(&Session::for_storage("owner"), id).await.is_ok());
        assert!(authorize(&Session::for_storage("other"), id).await.is_err());
        assert!(authorize(&Session::for_storage("owner"), id + 1).await.is_err());
        sqlx::query("UPDATE assistant_conversations SET agent='codex' WHERE id=?1")
            .bind(id)
            .execute(&pool)
            .await
            .unwrap();
        assert!(authorize(&Session::for_storage("owner"), id).await.is_err());
    }
}
