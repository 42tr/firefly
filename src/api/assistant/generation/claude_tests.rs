use super::*;

const ASYNC_CLI: &str = r#"
import json,sys,time
def emit(e): print(json.dumps(e),flush=True)
user=json.loads(sys.stdin.readline())
emit({'type':'system','subtype':'session_state_changed','state':'running'})
emit({'type':'system','subtype':'init','session_id':'fixture-session'})
emit(dict(user,isReplay=True))
emit({'type':'system','subtype':'background_tasks_changed','tasks':[{'task_id':'child','task_type':'local_agent','description':'检查实现'}]})
if user['message']['content']=='streamed':
    emit({'type':'stream_event','event':{'type':'message_start','message':{'id':'first'}}})
    emit({'type':'stream_event','event':{'type':'content_block_delta','delta':{'type':'text_delta','text':'正在等待子任务'}}})
emit({'type':'assistant','message':{'id':'first','content':[{'type':'text','text':'正在等待子任务'}]}})
emit({'type':'result','subtype':'success','is_error':False})
time.sleep(0.3)
if user['message']['content']=='cancel': time.sleep(60)
if user['message']['content']=='exit': sys.exit(0)
emit({'type':'system','subtype':'background_tasks_changed','tasks':[]})
emit({'type':'system','subtype':'task_notification','task_id':'child','status':'completed','summary':'检查通过'})
time.sleep(0.1)
emit({'type':'stream_event','event':{'type':'message_start','message':{'id':'last'}}})
if user['message']['content']=='streamed':
    emit({'type':'stream_event','event':{'type':'content_block_delta','delta':{'type':'text_delta','text':'子任务返回，汇总完成'}}})
emit({'type':'assistant','message':{'id':'last','content':[{'type':'text','text':'子任务返回，汇总完成'}]}})
emit({'type':'result','subtype':'success','is_error':False})
emit({'type':'system','subtype':'session_state_changed','state':'idle'})
for line in sys.stdin: pass
"#;

async fn fixture(
    prompt: &str,
) -> (
    assistant_store::ConversationRecord,
    Arc<assistant_job::GenerationJob>,
    claude::Lease,
) {
    let pool = crate::db::test_pool().await;
    crate::db::set_test_pool(pool.clone());
    let conversation: i64 = sqlx::query_scalar("INSERT INTO assistant_conversations(user_id,system_prompt,agent) VALUES ('claude-fixture','','claude') RETURNING id")
        .fetch_one(&pool).await.unwrap();
    let job = Arc::new(assistant_job::GenerationJob::new(
        uuid::Uuid::new_v4(),
        conversation,
        "claude-fixture".into(),
    ));
    sqlx::query("INSERT INTO assistant_generation_jobs(id,conversation_id,user_id,status) VALUES (?1,?2,'claude-fixture','running')")
        .bind(job.id).bind(conversation).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO assistant_messages(conversation_id,role,content,generation_job_id) VALUES (?1,'user','fixture',?2)")
        .bind(conversation).bind(job.id).execute(&pool).await.unwrap();
    let record = assistant_store::get_conversation(conversation, "claude-fixture")
        .await
        .unwrap()
        .unwrap();
    let mut command = tokio::process::Command::new("python3");
    command.env_clear().args(["-u", "-c", ASYNC_CLI]);
    let lease = claude::acquire(
        &job.id.to_string(),
        conversation,
        claude::PreparedTurn::for_test(command),
        prompt,
    )
    .await
    .unwrap();
    (record, job, lease)
}

#[tokio::test]
async fn result_keeps_job_alive_until_background_notification_and_parent_summary() {
    let (record, job, mut lease) = fixture("complete").await;
    let prompt = lease.prompt_id.clone();
    {
        let turn = lease.turn();
        let run = run_claude_events(turn, &record, "claude-fixture", record.id, &job, &prompt);
        tokio::pin!(run);
        tokio::time::timeout(Duration::from_secs(5), async {
            while job
                .runtime_activity()
                .is_none_or(|v| v["phase"] != "waiting_background")
            {
                tokio::select! {
                    done = &mut run => panic!("completed before background report: {done:?}"),
                    _ = tokio::time::sleep(Duration::from_millis(10)) => {}
                }
            }
        })
        .await
        .unwrap();
        assert!(!job.is_done());
        let metadata: Value = sqlx::query_scalar(
            "SELECT metadata FROM assistant_messages WHERE role='user' AND generation_job_id=?1",
        )
        .bind(job.id)
        .fetch_one(crate::db::pool())
        .await
        .unwrap();
        assert_eq!(metadata["runtime_activity"]["background_count"], 1);
        let (_, result) = tokio::time::timeout(Duration::from_secs(5), &mut run)
            .await
            .unwrap();
        result.unwrap();
        let texts: Vec<String> = sqlx::query_scalar(
            "SELECT content FROM assistant_messages WHERE role='assistant' ORDER BY id",
        )
        .fetch_all(crate::db::pool())
        .await
        .unwrap();
        assert_eq!(texts, ["正在等待子任务", "子任务返回，汇总完成"]);
        let deltas: Vec<String> = job
            .event_log
            .lock()
            .unwrap()
            .values()
            .filter(|entry| entry.event == "assistant_delta")
            .filter_map(|entry| entry.data["delta"].as_str().map(str::to_owned))
            .collect();
        assert_eq!(deltas, ["正在等待子任务", "子任务返回，汇总完成"]);
        let status: String = sqlx::query_scalar(
            "SELECT status FROM assistant_tool_details WHERE call_id='claude-task:child:'",
        )
        .fetch_one(crate::db::pool())
        .await
        .unwrap();
        assert_eq!(status, "completed");
    }
    lease.release(true).await.unwrap();
}

#[tokio::test]
async fn cancelling_a_background_wait_stops_the_cli() {
    let (record, job, mut lease) = fixture("cancel").await;
    let prompt = lease.prompt_id.clone();
    let cancellation = job.cancellation_token();
    let cancel = async {
        tokio::time::sleep(Duration::from_millis(100)).await;
        cancellation.cancel();
    };
    let (_, (_, result)) = tokio::join!(
        cancel,
        run_claude_events(
            lease.turn(),
            &record,
            "claude-fixture",
            record.id,
            &job,
            &prompt
        )
    );
    result.unwrap();
    assert!(!lease.turn().is_running().unwrap());
    lease.release(false).await.unwrap();
}

#[tokio::test]
async fn clean_process_exit_with_pending_tasks_is_an_interruption() {
    let (record, job, mut lease) = fixture("exit").await;
    let prompt = lease.prompt_id.clone();
    let (_, result) = run_claude_events(
        lease.turn(),
        &record,
        "claude-fixture",
        record.id,
        &job,
        &prompt,
    )
    .await;
    assert!(result.unwrap_err().to_string().contains("工作完成前已断开"));
    lease.release(false).await.unwrap();
}

#[tokio::test]
async fn complete_claude_messages_do_not_duplicate_text_already_streamed() {
    let (record, job, mut lease) = fixture("streamed").await;
    let prompt = lease.prompt_id.clone();
    let (_, result) = run_claude_events(
        lease.turn(),
        &record,
        "claude-fixture",
        record.id,
        &job,
        &prompt,
    )
    .await;
    result.unwrap();
    let text: String = job
        .event_log
        .lock()
        .unwrap()
        .values()
        .filter(|entry| entry.event == "assistant_delta")
        .filter_map(|entry| entry.data["delta"].as_str())
        .collect();
    assert_eq!(text, "正在等待子任务子任务返回，汇总完成");
    lease.release(false).await.unwrap();
}
