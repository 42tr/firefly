use super::*;

#[derive(Deserialize)]
pub struct ToolDetailQuery {
    pub job_id: Option<uuid::Uuid>,
    pub call_id: String,
    #[serde(default)]
    pub parent_id: String,
    pub message_id: Option<i64>,
}

pub(super) struct ToolDetail<'a> {
    pub call_id: &'a str,
    pub parent_id: &'a str,
    pub name: &'a str,
    pub input: Option<&'a Value>,
    pub output: Option<&'a Value>,
    pub status: &'a str,
}

// Keep the durable detail archive separate from the browser event log and model history.
pub(super) async fn save_tool_detail(
    conversation_id: i64, job_id: Option<uuid::Uuid>, detail: ToolDetail<'_>,
) -> anyhow::Result<()> {
    let ToolDetail { call_id, parent_id, name, input, output, status } = detail;
    let now = chrono::Utc::now().timestamp_millis();
    sqlx::query("INSERT INTO assistant_tool_details(conversation_id,job_id,call_id,parent_id,name,input,output,status,created_at,started_at,ended_at,revision)
        VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,CASE WHEN ?8='running' THEN ?9 END,CASE WHEN ?8 NOT IN ('queued','running') THEN ?9 END,
            (SELECT COALESCE(MAX(revision),0)+1 FROM assistant_tool_details WHERE conversation_id=?1))
        ON CONFLICT(conversation_id,job_id,call_id,parent_id) DO UPDATE SET
            name=excluded.name,input=COALESCE(excluded.input,input),output=COALESCE(excluded.output,output),
            status=CASE WHEN status NOT IN ('queued','running') AND excluded.status IN ('queued','running') THEN status ELSE excluded.status END,
            created_at=COALESCE(created_at,excluded.created_at),started_at=COALESCE(started_at,excluded.started_at),
            ended_at=CASE WHEN excluded.status IN ('queued','running') THEN ended_at ELSE COALESCE(ended_at,excluded.ended_at) END,
            revision=excluded.revision")
        .bind(conversation_id).bind(job_id.map(|id| id.to_string()).unwrap_or_default())
        .bind(call_id).bind(parent_id).bind(name).bind(input).bind(output).bind(status).bind(now)
        .execute(crate::db::pool()).await?;
    crate::assistant_tasks::notify_changed();
    Ok(())
}

pub async fn get_tool_detail(
    auth: Session, Path(conversation_id): Path<i64>, Query(query): Query<ToolDetailQuery>,
) -> Result<Json<Value>, AppError> {
    assistant_store::get_conversation_for_read(conversation_id, auth.storage_scope()).await
        .map_err(AppError::internal_server_error)?.ok_or(AppError::not_found("Conversation not found"))?;
    if query.call_id.is_empty() || query.call_id.len() > 512 || query.parent_id.len() > 512 {
        return Err(AppError::bad_request("无效的工具调用 ID"));
    }
    let row: Option<(String, Option<Value>, Option<Value>, String)> = sqlx::query_as(
        "SELECT name,input,output,status FROM assistant_tool_details WHERE conversation_id=?1 AND job_id=?2 AND call_id=?3 AND parent_id=?4",
    ).bind(conversation_id).bind(query.job_id.map(|id| id.to_string()).unwrap_or_default())
        .bind(&query.call_id).bind(&query.parent_id).fetch_optional(crate::db::pool()).await
        .map_err(AppError::internal_server_error)?;
    if let Some((name, input, output, status)) = row {
        return Ok(Json(json!({"id":query.call_id,"name":name,"input":input,"output":output,"status":status})));
    }
    // Older conversations predate the detail archive; read their original records on demand.
    let records = assistant_store::fetch_job_process_messages(conversation_id, auth.storage_scope(), query.job_id, None, query.message_id)
        .await.map_err(AppError::internal_server_error)?;
    let mut detail = None;
    for record in records {
        if query.parent_id.is_empty() {
            if let Some(calls) = record.tool_calls.as_ref().and_then(Value::as_array) {
                for call in calls.iter().filter(|call| call["id"] == query.call_id) {
                    detail = Some(json!({"id":query.call_id,"name":call["name"],"input":call["arguments"],"output":null,"status":"running"}));
                }
            }
            if record.tool_call_id.as_deref() == Some(&query.call_id) {
                let value = detail.get_or_insert_with(|| json!({"id":query.call_id,"input":null}));
                let metadata = record.metadata.unwrap_or_default();
                value["name"] = metadata["tool_name"].clone();
                if !metadata["tool_input"].is_null() { value["input"] = metadata["tool_input"].clone(); }
                value["output"] = serde_json::from_str(&record.content).unwrap_or(Value::String(record.content));
                value["status"] = json!(if record.is_error {"error"} else {"completed"});
            }
        } else if record.tool_call_id.as_deref() == Some(&query.parent_id) {
            if let Some(steps) = record.metadata.as_ref().and_then(|m| m["steps"].as_array()) {
                detail = steps.iter().find(|step| step["id"] == query.call_id).cloned().or(detail);
            }
        }
    }
    detail.map(Json).ok_or(AppError::not_found("工具详情尚未记录"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn workflow_timing_and_revisions_survive_repeated_archive_writes() {
        let pool = crate::db::test_pool().await;
        crate::db::set_test_pool(pool.clone());
        let conversation: i64 = sqlx::query_scalar("INSERT INTO assistant_conversations(user_id,system_prompt) VALUES ('owner','') RETURNING id")
            .fetch_one(&pool).await.unwrap();
        let job = uuid::Uuid::new_v4();
        save_tool_detail(conversation, Some(job), ToolDetail {
            call_id: "call",
            parent_id: "handoff",
            name: "shell",
            input: Some(&json!({"cmd":"test"})),
            output: None,
            status: "queued",
        }).await.unwrap();
        let (created, started, ended, revision): (i64, Option<i64>, Option<i64>, i64) = sqlx::query_as("SELECT created_at,started_at,ended_at,revision FROM assistant_tool_details")
            .fetch_one(&pool).await.unwrap();
        assert!(started.is_none() && ended.is_none());
        save_tool_detail(conversation, Some(job), ToolDetail {
            call_id: "call",
            parent_id: "handoff",
            name: "shell",
            input: None,
            output: None,
            status: "running",
        }).await.unwrap();
        sqlx::query("UPDATE assistant_tool_details SET started_at=created_at-10000").execute(&pool).await.unwrap();
        save_tool_detail(conversation, Some(job), ToolDetail {
            call_id: "call",
            parent_id: "handoff",
            name: "shell",
            input: None,
            output: Some(&json!(false)),
            status: "completed",
        }).await.unwrap();
        save_tool_detail(conversation, Some(job), ToolDetail {
            call_id: "call",
            parent_id: "handoff",
            name: "shell",
            input: None,
            output: Some(&json!(false)),
            status: "completed",
        }).await.unwrap();
        let (created_after, started, ended, after, input): (i64, i64, i64, i64, Value) = sqlx::query_as("SELECT created_at,started_at,ended_at,revision,input FROM assistant_tool_details")
            .fetch_one(&pool).await.unwrap();
        assert_eq!(created_after, created);
        assert_eq!(started, created - 10000);
        assert!(ended >= created);
        assert_eq!(after, revision + 3);
        assert_eq!(input["cmd"], "test");
        // Replaying the original call/start must not reopen a finished step.
        for status in ["queued", "running"] {
            save_tool_detail(conversation, Some(job), ToolDetail {
                call_id: "call",
                parent_id: "handoff",
                name: "shell",
                input: None,
                output: None,
                status,
            }).await.unwrap();
        }
        let (status, end_after, output): (String, i64, Value) = sqlx::query_as("SELECT status,ended_at,output FROM assistant_tool_details")
            .fetch_one(&pool).await.unwrap();
        assert_eq!(status, "completed");
        assert_eq!(end_after, ended);
        assert_eq!(output, json!(false));
    }

    #[tokio::test]
    async fn live_child_details_are_durable_isolated_and_authorized() {
        let pool = crate::db::test_pool().await;
        crate::db::set_test_pool(pool.clone());
        let conversation: i64 = sqlx::query_scalar("INSERT INTO assistant_conversations(user_id,system_prompt) VALUES ('owner','') RETURNING id")
            .fetch_one(&pool).await.unwrap();
        let job = uuid::Uuid::new_v4();
        for parent in ["first", "second"] {
            save_tool_detail(conversation, Some(job), ToolDetail {
                call_id: "same-call",
                parent_id: parent,
                name: "command",
                input: Some(&json!({"command":parent})),
                output: None,
                status: "running",
            }).await.unwrap();
        }
        save_tool_detail(conversation, Some(job), ToolDetail {
            call_id: "same-call",
            parent_id: "first",
            name: "command",
            input: None,
            output: Some(&json!({"stdout":"private output"})),
            status: "completed",
        }).await.unwrap();
        let query = |parent: &str| Query(ToolDetailQuery { job_id: Some(job), call_id:"same-call".into(), parent_id:parent.into(), message_id:None });
        let Json(detail) = get_tool_detail(Session::for_storage("owner"), Path(conversation), query("first")).await.unwrap();
        assert_eq!(detail["input"]["command"], "first");
        assert_eq!(detail["output"]["stdout"], "private output");
        let Json(other) = get_tool_detail(Session::for_storage("owner"), Path(conversation), query("second")).await.unwrap();
        assert_eq!(other["status"], "running");
        assert!(other["output"].is_null());
        assert!(get_tool_detail(Session::for_storage("other"), Path(conversation), query("first")).await.is_err());
        assert!(get_tool_detail(Session::for_storage("owner"), Path(conversation+1), query("first")).await.is_err());
        assert!(get_tool_detail(Session::for_storage("owner"), Path(conversation), query("")).await.is_err());
        sqlx::query("DELETE FROM assistant_conversations WHERE id=?1").bind(conversation).execute(&pool).await.unwrap();
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM assistant_tool_details").fetch_one(&pool).await.unwrap();
        assert_eq!(count, 0);
    }

    #[tokio::test]
    async fn legacy_tool_inputs_and_child_results_remain_readable_on_demand() {
        let pool = crate::db::test_pool().await;
        crate::db::set_test_pool(pool.clone());
        let conversation: i64 = sqlx::query_scalar("INSERT INTO assistant_conversations(user_id,system_prompt) VALUES ('owner','') RETURNING id")
            .fetch_one(&pool).await.unwrap();
        sqlx::query("INSERT INTO assistant_messages(conversation_id,role,content,tool_calls) VALUES (?1,'assistant','',?2)")
            .bind(conversation).bind(json!([{"id":"handoff","name":"handoff_to_programmer","arguments":{"task":"fix"}}])).execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO assistant_messages(conversation_id,role,content,tool_call_id,metadata) VALUES (?1,'tool','\"done\"','handoff',?2)")
            .bind(conversation).bind(json!({"tool_name":"handoff_to_programmer","steps":[{"id":"child","name":"command","input":{"command":"test"},"output":"passed","status":"completed"}]})).execute(&pool).await.unwrap();
        for (call, parent) in [("handoff", ""), ("child", "handoff")] {
            let Json(detail) = get_tool_detail(Session::for_storage("owner"), Path(conversation), Query(ToolDetailQuery {
                job_id:None, call_id:call.into(), parent_id:parent.into(), message_id:None,
            })).await.unwrap();
            assert!(!detail["input"].is_null());
            assert_eq!(detail["output"], if parent.is_empty() {"done"} else {"passed"});
        }
    }
}
