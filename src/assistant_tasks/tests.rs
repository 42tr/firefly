use async_trait::async_trait;
use g::{TaskBackend, TaskCall, TaskSubmission, Tool, ToolBehavior, ToolError, ToolSpec};
use serde_json::{json, Value};
use sqlx::{query, query_as, query_scalar};
use std::{sync::Arc, time::Duration};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use super::{command, now, preview::result_preview, result, Backend, Manager, TaskRecord};

struct Gate {
    name: &'static str,
    release: Arc<Notify>,
}
#[async_trait]
impl Tool for Gate {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: self.name.into(),
            description: String::new(),
            input_schema: json!({"type":"object"}),
            behavior: ToolBehavior {
                read_only: true,
                idempotent: true,
                parallel_safe: true,
            },
        }
    }
    async fn call(&self, _: g::ToolContext, _: Value) -> Result<Value, ToolError> {
        self.release.notified().await;
        Ok(json!({"answer":self.name}))
    }
}

async fn setup() -> Arc<Backend> {
    let pool = crate::db::test_pool().await;
    let conversation: i64 = query_scalar(
        "INSERT INTO assistant_conversations(user_id,system_prompt,agent) VALUES ('owner','','firefly') RETURNING id",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    let job = Uuid::new_v4();
    query("INSERT INTO assistant_generation_jobs(id,conversation_id,user_id,status) VALUES (?1,?2,'owner','running')")
        .bind(job)
        .bind(conversation)
        .execute(&pool)
        .await
        .unwrap();
    Arc::new(Backend {
        manager: Manager::new(pool),
        user: "owner".into(),
        conversation,
        job: job.to_string(),
        scope_id: None,
    })
}

#[tokio::test]
async fn invalid_titles_reject_the_entire_batch_without_creating_tasks() {
    let backend = setup().await;
    for title in [None, Some("".into()), Some(" \t\n\u{3000}".into()),
        Some(" command ".into()), Some("COMMAND".into()), Some("检".repeat(201))] {
        let run = Uuid::new_v4();
        let valid = submission("lookup", Arc::new(Notify::new()), 0, run);
        let mut invalid = submission("command", Arc::new(Notify::new()), 1, run);
        invalid.request.title = title;
        let error = backend.submit(vec![valid, invalid]).await.unwrap_err();
        assert!(error.to_string().contains("title"), "{error}");
        assert!(backend.manager.list(&backend.user, backend.conversation).await.unwrap().is_empty());
        assert!(backend.manager.tools.lock().await.is_empty());
    }
}

#[tokio::test]
async fn workflow_titles_are_trimmed_and_unicode_length_is_supported() {
    let backend = setup().await;
    let run = Uuid::new_v4();
    let titles = ["检查 RDP 会话状态".to_owned(), "检".repeat(200)];
    let calls = titles.iter().enumerate().map(|(index, title)| {
        let mut task = submission("lookup", Arc::new(Notify::new()), index, run);
        task.request.title = Some(format!(" \t{title}\u{3000}\n"));
        task
    }).collect();
    let ids = backend.submit(calls).await.unwrap();
    let tasks = backend.manager.list(&backend.user, backend.conversation).await.unwrap();
    for (index, id) in ids.iter().enumerate() {
        assert_eq!(tasks.iter().find(|task| &task.id == id).unwrap().title, titles[index]);
    }
    let snapshot = backend.control("get_tasks", json!({"task_ids":[ids[0]]})).await.unwrap();
    assert_eq!(snapshot["pending"][0]["title"], titles[0]);
}

#[tokio::test]
async fn handoff_scopes_isolate_ownership_completion_and_recovery() {
    let root = setup().await;
    let parent = root.submit(vec![submission("parent", Arc::new(Notify::new()), 0, Uuid::new_v4())])
        .await.unwrap();
    let child = root.for_handoff("programmer", "h1").unwrap();
    let ids = child.submit(vec![submission("child", Arc::new(Notify::new()), 0, Uuid::new_v4())])
        .await.unwrap();
    let sibling = root.for_handoff("programmer", "h2").unwrap();
    assert!(sibling.ready_to_finish().await.unwrap());
    for operation in ["get_tasks", "wait_tools", "cancel_tools", "detach_tools"] {
        assert!(child.control(operation, json!({"task_ids":parent,"wait_timeout_secs":0})).await.is_err());
        assert!(sibling.control(operation, json!({"task_ids":ids,"wait_timeout_secs":0})).await.is_err());
    }
    let snapshot = child.control("get_tasks", json!({})).await.unwrap();
    assert_eq!(snapshot["pending"].as_array().unwrap().len(), 1);
    assert_eq!(snapshot["pending"][0]["scope_id"], "programmer:h1");
    assert!(!child.ready_to_finish().await.unwrap());
    root.manager.finish(&ids[0], "succeeded", Some(json!({"answer":42})), None).await.unwrap();
    assert!(!child.ready_to_finish().await.unwrap());
    let result = child.control("get_tasks", json!({})).await.unwrap();
    child.acknowledge(&result).await.unwrap();
    assert!(child.ready_to_finish().await.unwrap());
    // Child acknowledgement cannot make a parent task appear observed.
    child.acknowledge(&json!({"completed":[{"id":parent[0]}]})).await.unwrap();
    assert!(!root.ready_to_finish().await.unwrap());
    assert!(!root.manager.list(&root.user, root.conversation).await.unwrap()
        .iter().find(|task| task.id == parent[0]).unwrap().observed);
    // Recovery preserves delivered results, while a result not yet delivered
    // remains available to the recreated scoped backend without replaying it.
    drop(child);
    root.manager.recover().await.unwrap();
    let recovered = root.for_handoff("programmer", "h1").unwrap();
    let result = recovered.control("get_tasks", json!({})).await.unwrap();
    assert!(result["completed"].as_array().unwrap().is_empty());
    assert!(recovered.ready_to_finish().await.unwrap());
    let unreceived = recovered.submit(vec![submission("unreceived", Arc::new(Notify::new()), 0, Uuid::new_v4())])
        .await.unwrap();
    root.manager.finish(&unreceived[0], "succeeded", Some(json!({"answer":42})), None).await.unwrap();
    root.manager.recover().await.unwrap();
    let result = recovered.control("get_tasks", json!({})).await.unwrap();
    assert_eq!(result["completed"][0]["result"]["answer"], 42);
    assert!(!recovered.ready_to_finish().await.unwrap());
    recovered.acknowledge(&result).await.unwrap();
    assert!(recovered.ready_to_finish().await.unwrap());
}

async fn insert_task(backend: &Backend, id: &str, job: &str, status: &str, observed: bool) {
    query("INSERT INTO assistant_tasks(id,user_id,conversation_id,generation_job_id,run_id,source_call_id,source_index,title,tool,arguments,status,observed,execution_timeout_secs,created_at) VALUES (?1,?2,?3,?4,?1,?1,0,'检查任务状态','command','{}',?5,?6,60,1)")
        .bind(id).bind(&backend.user).bind(backend.conversation).bind(job).bind(status).bind(observed)
        .execute(&backend.manager.pool).await.unwrap();
}

#[tokio::test]
async fn default_task_query_excludes_completed_history_and_acknowledges_old_unknown_warnings() {
    let backend = setup().await;
    let past = Uuid::new_v4().to_string();
    for i in 0..438 {
        insert_task(&backend, &format!("past-{i:03}"), &past, "succeeded", true).await;
    }
    // A prior run's unreceived success is not an obligation of this reply.
    insert_task(&backend, "past-unreceived", &past, "succeeded", false).await;
    let summary = backend.manager.attention_summary(&backend.user, backend.conversation, &backend.job).await.unwrap();
    assert_eq!(summary["total"], 0);
    let empty = backend.control("get_tasks", json!({})).await.unwrap();
    assert_eq!(empty["total"], 0);
    assert!(empty.to_string().len() < 1024);

    insert_task(&backend, "past-active", &past, "running", false).await;
    insert_task(&backend, "past-unknown", &past, "unknown", false).await;
    insert_task(&backend, "current-unreceived", &backend.job, "succeeded", false).await;
    let page = backend.control("get_tasks", json!({})).await.unwrap();
    assert_eq!(page["total"], 3);
    assert_eq!(page["completed"][0]["id"], "current-unreceived");
    assert_eq!(page["pending"][0]["id"], "past-active");
    assert!(!backend.ready_to_finish().await.unwrap());
    backend.acknowledge(&page).await.unwrap();
    assert!(backend.ready_to_finish().await.unwrap());
    let page = backend.control("get_tasks", json!({})).await.unwrap();
    assert_eq!(page["total"], 1);
    let unknown: (String, bool) = sqlx::query_as("SELECT status,observed FROM assistant_tasks WHERE id='past-unknown'")
        .fetch_one(&backend.manager.pool).await.unwrap();
    assert_eq!(unknown, ("unknown".into(), true));

    let page = backend.control("get_tasks", json!({"include_history":true,"limit":32})).await.unwrap();
    assert_eq!(page["completed"].as_array().unwrap().len() + page["pending"].as_array().unwrap().len(), 32);
    assert_eq!(page["next_offset"], 32);
    assert_eq!(page["has_more"], true);
    let next = backend.control("get_tasks", json!({"include_history":true,"limit":32,"offset":32})).await.unwrap();
    let ids: std::collections::HashSet<_> = page["completed"].as_array().unwrap().iter()
        .chain(page["pending"].as_array().unwrap()).map(|task| task["id"].clone()).collect();
    assert!(next["completed"].as_array().unwrap().iter().chain(next["pending"].as_array().unwrap())
        .all(|task| !ids.contains(&task["id"])));
}

#[tokio::test]
async fn task_result_and_output_previews_are_bounded_and_receipts_drain_in_batches() {
    let backend = setup().await;
    let long = "界\n\"\\".repeat(16000);
    let mut ids = Vec::new();
    for i in 0..64 {
        let id = format!("required-{i:03}");
        insert_task(&backend, &id, &backend.job, "succeeded", false).await;
        query("UPDATE assistant_tasks SET result=?1,error=?2 WHERE id=?3")
            .bind(json!({"large":long})).bind(&long).bind(&id).execute(&backend.manager.pool).await.unwrap();
        backend.manager.event(&id, "output", json!({"stream":"stdout","text":format!("{long}LAST-LINE-{i}")})).await.unwrap();
        ids.push(id);
    }
    let first = backend.control("get_tasks", json!({})).await.unwrap();
    assert_eq!(first["completed"].as_array().unwrap().len(), 32);
    assert_eq!(first["has_more"], true);
    assert!(first.to_string().len() < 64 * 1024);
    assert_eq!(first["completed"][0]["output_truncated"], true);
    assert_eq!(first["completed"][0]["result"]["truncated"], true);
    assert!(first["completed"][0]["output_preview"].as_str().unwrap().ends_with("LAST-LINE-0"));
    backend.acknowledge(&first).await.unwrap();
    let second = backend.control("get_tasks", json!({})).await.unwrap();
    assert_eq!(second["completed"][0]["id"], "required-032");
    assert!(!backend.ready_to_finish().await.unwrap());
    backend.acknowledge(&second).await.unwrap();
    assert!(backend.ready_to_finish().await.unwrap());

    let explicit = backend.control("get_tasks", json!({"task_ids":ids})).await.unwrap();
    assert_eq!(explicit["completed"].as_array().unwrap().len(), 64);
    assert!(explicit.to_string().len() < 128 * 1024);
    // Summaries should not reload output from already received historical tasks.
    let history = backend.control("get_tasks", json!({"include_history":true,"limit":1})).await.unwrap();
    assert!(history["completed"][0].get("output_preview").is_none());
    assert!(backend.control("get_tasks", json!({"limit":65})).await.is_err());
}

#[tokio::test]
async fn completion_feedback_identifies_only_current_attached_blockers() {
    struct CompletionModel { step: std::sync::atomic::AtomicUsize }
    #[async_trait]
    impl g::Model for CompletionModel {
        async fn generate(&self, request: g::ModelRequest) -> Result<g::ModelResponse, g::ModelError> {
            let message = match self.step.fetch_add(1, std::sync::atomic::Ordering::SeqCst) {
                0 => g::Message::assistant("二维码已准备好"),
                1 => {
                    let feedback = request.messages.last().unwrap().text_content();
                    assert!(feedback.contains("required-unknown"));
                    assert!(!feedback.contains("previous-unknown"));
                    assert!(!feedback.contains("detached-session"));
                    g::Message::new(g::Role::Assistant, vec![g::Content::ToolCall {
                        id: "receive".into(), name: "get_tasks".into(),
                        arguments: json!({"task_ids":["required-unknown"]}),
                    }])
                },
                2 => g::Message::assistant("二维码已准备好，会话保留"),
                _ => panic!("completion loop did not converge"),
            };
            Ok(g::ModelResponse::new(message))
        }
    }
    let backend = setup().await;
    insert_task(&backend, "required-unknown", &backend.job, "unknown", false).await;
    insert_task(&backend, "previous-unknown", &Uuid::new_v4().to_string(), "unknown", false).await;
    insert_task(&backend, "detached-session", &backend.job, "running", false).await;
    query("UPDATE assistant_tasks SET detached=1 WHERE id='detached-session'")
        .execute(&backend.manager.pool).await.unwrap();
    let agent = g::Agent::new(Arc::new(CompletionModel { step: std::sync::atomic::AtomicUsize::new(0) }))
        .with_task_backend(backend.clone());
    let output = agent.run("给个新的二维码").await.unwrap();
    assert_eq!(output.turns, 3);
    assert!(output.final_text.contains("会话保留"));
    assert!(backend.ready_to_finish().await.unwrap());
}

#[tokio::test]
async fn dropping_handoff_cancels_its_tasks_and_preserves_parent_sibling_and_detached_work() {
    let root = setup().await;
    let parent = root.submit(vec![submission("parent", Arc::new(Notify::new()), 0, Uuid::new_v4())])
        .await.unwrap();
    let child = root.for_handoff("programmer", "h1").unwrap();
    let child_release = Arc::new(Notify::new());
    let detached_release = Arc::new(Notify::new());
    let run = Uuid::new_v4();
    let ids = child.submit(vec![submission("child", child_release, 0, run),
        submission("detached", detached_release.clone(), 1, run)]).await.unwrap();
    let sibling = root.for_handoff("programmer", "h2").unwrap();
    let other = sibling.submit(vec![submission("sibling", Arc::new(Notify::new()), 0, Uuid::new_v4())])
        .await.unwrap();
    child.control("detach_tools", json!({"task_ids":[ids[1]]})).await.unwrap();
    root.manager.dispatch().await.unwrap();
    wait_status(&root, &ids[0], "running").await;
    drop(child);
    wait_status(&root, &ids[0], "cancelled").await;
    let tasks = root.manager.list(&root.user, root.conversation).await.unwrap();
    for id in [&parent[0], &other[0], &ids[1]] {
        assert!(tasks.iter().find(|task| &task.id == id).unwrap().active());
    }
    root.close().await.unwrap();
    wait_status(&root, &parent[0], "cancelled").await;
    wait_status(&root, &other[0], "cancelled").await;
    assert!(!root.manager.running.lock().await[&ids[1]].token.is_cancelled());
    detached_release.notify_one();
    wait_status(&root, &ids[1], "succeeded").await;
}

#[tokio::test]
async fn submitted_commands_use_only_explicit_mutexes() {
    crate::config::init_test_config();
    let backend = setup().await;
    let workspace = std::path::PathBuf::from(&crate::config::get().workspace_root);
    tokio::fs::create_dir_all(&workspace).await.unwrap();
    let dir = tempfile::tempdir_in(workspace).unwrap();
    let cwd = tokio::fs::canonicalize(dir.path()).await.unwrap();
    tokio::fs::create_dir(cwd.join(".git")).await.unwrap();
    let run = Uuid::new_v4();
    let calls = (0..4).map(|index| {
        let mut arguments = json!({"command":"sleep 300","cwd":cwd});
        if index >= 2 { arguments["resource_key"] = json!("repo:test"); }
        TaskSubmission {
            run_id: run, call_id: "commands".into(), index,
            request: TaskCall { tool: "command".into(), arguments, title: Some("检查命令互斥".into()),
                parent_task_id: None, execution_timeout_secs: Some(60) },
            tool: Arc::new(command::CommandTool),
        }
    }).collect();
    let ids = backend.submit(calls).await.unwrap();
    let tasks = backend.manager.list(&backend.user, backend.conversation).await.unwrap();
    for (index, id) in ids.iter().enumerate() {
        let task = tasks.iter().find(|task| &task.id == id).unwrap();
        assert_eq!(task.arguments["cwd"], json!(cwd));
        assert_eq!(task.resource_key.as_deref(), if index >= 2 { Some("repo:test") } else { None });
    }
    backend.manager.dispatch().await.unwrap();
    assert_eq!(backend.manager.running.lock().await.len(), 3);
    backend.manager.cancel(&backend.user, backend.conversation, &ids).await.unwrap();
    for id in ids { wait_status(&backend, &id, "cancelled").await; }
}

#[tokio::test]
async fn commands_in_same_repository_and_parent_directory_run_concurrently() {
    let backend = setup().await;
    let dir = tempfile::tempdir().unwrap();
    let repo = tokio::fs::canonicalize(dir.path()).await.unwrap();
    tokio::fs::create_dir(repo.join(".git")).await.unwrap();
    let child = repo.join("frontend");
    tokio::fs::create_dir(&child).await.unwrap();
    let mut ids = Vec::new();
    for (index, cwd) in [&repo, &repo, &child].into_iter().enumerate() {
        ids.push(command_task(&backend, cwd, "sleep 300", index as i64).await);
    }
    backend.manager.dispatch().await.unwrap();
    assert_eq!(backend.manager.running.lock().await.len(), 3);
    let tasks = backend.manager.list(&backend.user, backend.conversation).await.unwrap();
    assert!(tasks.iter().all(|task| task.status == "running" && task.resource_key.is_none()));
    backend.manager.cancel(&backend.user, backend.conversation, &ids).await.unwrap();
    for id in ids { wait_status(&backend, &id, "cancelled").await; }
}

#[tokio::test]
async fn explicit_mutexes_serialize_across_conversations_and_release_after_execution() {
    let backend = setup().await;
    let conversation: i64 = query_scalar(
        "INSERT INTO assistant_conversations(user_id,system_prompt,agent) VALUES ('owner','','firefly') RETURNING id",
    ).fetch_one(&backend.manager.pool).await.unwrap();
    let job = Uuid::new_v4();
    query("INSERT INTO assistant_generation_jobs(id,conversation_id,user_id,status) VALUES (?1,?2,'owner','running')")
        .bind(job).bind(conversation).execute(&backend.manager.pool).await.unwrap();
    let other = Backend {
        manager: backend.manager.clone(), user: backend.user.clone(), conversation,
        job: job.to_string(), scope_id: None,
    };
    let dir = tempfile::tempdir().unwrap();
    let child = dir.path().join("frontend");
    tokio::fs::create_dir(&child).await.unwrap();
    let first = command_task_with_resource(&backend, dir.path(), "sleep 300", 0, Some("repo:project")).await;
    let second = command_task_with_resource(&other, &child, "pwd", 1, Some("repo:project")).await;
    // Keys are opaque names: sharing a prefix or cwd does not cause exclusion.
    let independent = command_task_with_resource(&backend, dir.path(), "sleep 300", 2, Some("repo:project/frontend")).await;
    let unlocked = command_task(&backend, dir.path(), "sleep 300", 3).await;
    backend.manager.dispatch().await.unwrap();
    assert_eq!(backend.manager.running.lock().await.len(), 3);
    wait_status(&other, &second, "queued").await;
    backend.manager.cancel(&backend.user, backend.conversation, std::slice::from_ref(&first)).await.unwrap();
    wait_status(&backend, &first, "cancelled").await;
    tokio::time::timeout(Duration::from_secs(5), async {
        while backend.manager.running.lock().await.contains_key(&first) {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }).await.unwrap();
    backend.manager.dispatch().await.unwrap();
    wait_status(&other, &second, "succeeded").await;
    let events = backend.manager.events(&other.user, other.conversation, 0).await.unwrap();
    assert!(events.iter().any(|event| event.task_id == second && event.data["text"]
        .as_str().is_some_and(|text| text.trim() == child.to_string_lossy())));
    backend.manager.cancel(&backend.user, backend.conversation, &[independent.clone(), unlocked.clone()]).await.unwrap();
    for id in [independent, unlocked] { wait_status(&backend, &id, "cancelled").await; }
}

#[tokio::test]
async fn restart_clears_legacy_directory_locks_without_replaying_unknown_commands() {
    let backend = setup().await;
    let dir = tempfile::tempdir().unwrap();
    let child = dir.path().join("project");
    tokio::fs::create_dir(&child).await.unwrap();
    let unknown = command_task(&backend, dir.path(), "touch must-not-replay", 0).await;
    let queued = command_task(&backend, dir.path(), "echo root-ok", 1).await;
    let subdir = command_task(&backend, &child, "echo project-ok", 2).await;
    query("UPDATE assistant_tasks SET resource_key=?1 WHERE id IN (?2,?3,?4)")
        .bind(dir.path().to_string_lossy().as_ref()).bind(&unknown).bind(&queued).bind(&subdir)
        .execute(&backend.manager.pool).await.unwrap();
    query("UPDATE assistant_tasks SET status='running' WHERE id=?1")
        .bind(&unknown).execute(&backend.manager.pool).await.unwrap();
    let explicit = command_task_with_resource(&backend, &child, "echo explicit-ok", 3, Some("repo:project")).await;
    backend.manager.recover().await.unwrap();
    backend.manager.recover().await.unwrap(); // Upgrade is idempotent.
    let tasks = backend.manager.list(&backend.user, backend.conversation).await.unwrap();
    let old = tasks.iter().find(|task| task.id == unknown).unwrap();
    assert_eq!(old.status, "unknown");
    assert!(old.resource_key.is_none());
    assert!(old.error.as_deref().unwrap().contains("服务重启"));
    assert!(tasks.iter().filter(|task| task.id == queued || task.id == subdir).all(|task| task.resource_key.is_none()));
    assert_eq!(tasks.iter().find(|task| task.id == explicit).unwrap().resource_key.as_deref(), Some("repo:project"));
    backend.manager.dispatch().await.unwrap();
    for id in [queued, subdir, explicit] { wait_status(&backend, &id, "succeeded").await; }
    assert!(!dir.path().join("must-not-replay").exists());
    assert_eq!(backend.manager.list(&backend.user, backend.conversation).await.unwrap()
        .iter().find(|task| task.id == unknown).unwrap().status, "unknown");
}

#[tokio::test]
async fn unknown_results_do_not_hold_explicit_mutexes() {
    let backend = setup().await;
    let dir = tempfile::tempdir().unwrap();
    let unknown = command_task_with_resource(&backend, dir.path(), "touch must-not-replay", 0, Some("repo:project")).await;
    query("UPDATE assistant_tasks SET status='unknown',error='result uncertain' WHERE id=?1")
        .bind(&unknown).execute(&backend.manager.pool).await.unwrap();
    let next = command_task_with_resource(&backend, dir.path(), "echo recovered", 1, Some("repo:project")).await;
    backend.manager.dispatch().await.unwrap();
    wait_status(&backend, &next, "succeeded").await;
    assert!(!dir.path().join("must-not-replay").exists());
}

#[tokio::test]
async fn programmer_handoff_receives_its_result_while_parent_work_is_pending() {
    struct Model {
        child: bool,
        parent_task: String,
        step: std::sync::atomic::AtomicUsize,
    }
    fn call(id: &str, name: &str, arguments: Value) -> g::Message {
        g::Message::new(g::Role::Assistant, vec![g::Content::ToolCall {
            id: id.into(), name: name.into(), arguments,
        }])
    }
    #[async_trait]
    impl g::Model for Model {
        async fn generate(&self, request: g::ModelRequest) -> Result<g::ModelResponse, g::ModelError> {
            let step = self.step.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let message = match (self.child, step) {
                (false, 0) => call("h1", "handoff_to_programmer", json!({"task":
                    "{\"goal\":\"check project\",\"workspace\":\"project\",\"authorized_actions\":[\"read\",\"test\"]}"})),
                (true, 0) => call("checks", "run_tools", json!({
                    "calls":[{"tool":"check_project","arguments":{},"title":"检查项目"}],"wait_timeout_secs":5
                })),
                (true, 1) => {
                    let result = request.messages.iter().flat_map(|message| &message.content)
                        .find_map(|content| match content {
                            g::Content::ToolResult { call_id, result, .. } if call_id == "checks" => Some(result),
                            _ => None,
                        }).unwrap();
                    assert_eq!(result["completed"][0]["result"]["answer"], "check_project");
                    assert_eq!(result["completed"][0]["scope_id"], "programmer:h1");
                    assert!(result["pending"].as_array().unwrap().is_empty());
                    g::Message::assistant("{\"status\":\"completed\",\"summary\":\"checks passed\",\"files\":[],\"checks\":[{\"command\":\"check_project\",\"status\":\"passed\"}],\"remaining\":[],\"commits\":[],\"background_task_ids\":[]}")
                },
                (false, 1) => {
                    let report = request.messages.iter().flat_map(|message| &message.content)
                        .find_map(|content| match content {
                            g::Content::ToolResult { call_id, result, .. } if call_id == "h1" => Some(result),
                            _ => None,
                        }).unwrap();
                    assert_eq!(report["agent"], "programmer");
                    assert_eq!(serde_json::from_str::<Value>(report["response"].as_str().unwrap()).unwrap()["status"], "completed");
                    call("detach", "detach_tools", json!({"task_ids":[self.parent_task]}))
                },
                (false, 2) => g::Message::assistant("programming completed; parent task continues in background"),
                _ => panic!("unexpected model turn: child={} step={step}", self.child),
            };
            Ok(g::ModelResponse::new(message))
        }
    }
    let root = setup().await;
    let release_parent = Arc::new(Notify::new());
    let parent_task = root.submit(vec![submission("parent", release_parent.clone(), 0, Uuid::new_v4())])
        .await.unwrap()[0].clone();
    let release_child = Arc::new(Notify::new());
    release_child.notify_one();
    let child_model = Arc::new(Model { child: true, parent_task: String::new(), step: 0.into() });
    let programmer = g::Agent::new(child_model).name("programmer").with_task_backend(root.clone()).with_limits(g::RunLimits {
        max_tool_calls: 8, timeout: Duration::from_secs(10),
    }).tool(Gate { name: "check_project", release: release_child });
    let parent = g::Agent::new(Arc::new(Model { child: false, parent_task: parent_task.clone(), step: 0.into() }))
        .with_task_backend(root.clone()).handoff([programmer]);
    let manager = root.manager.clone();
    let stop = CancellationToken::new();
    let stopped = stop.clone();
    let worker = tokio::spawn(async move {
        loop {
            manager.dispatch().await.unwrap();
            tokio::select! { _=stopped.cancelled()=>break, _=tokio::time::sleep(Duration::from_millis(5))=>{} }
        }
    });
    let output = tokio::time::timeout(Duration::from_secs(10), parent.run("work")).await.unwrap().unwrap();
    assert!(output.final_text.contains("programming completed"));
    let tasks = root.manager.list(&root.user, root.conversation).await.unwrap();
    assert_eq!(tasks.len(), 2);
    assert!(tasks.iter().any(|task| task.scope_id.as_deref() == Some("programmer:h1")
        && task.status == "succeeded" && task.observed));
    assert!(tasks.iter().any(|task| task.id == parent_task && task.active() && task.detached));
    root.close().await.unwrap();
    release_parent.notify_one();
    wait_status(&root, &parent_task, "succeeded").await;
    stop.cancel();
    worker.await.unwrap();
}
fn submission(name: &'static str, release: Arc<Notify>, index: usize, run: Uuid) -> TaskSubmission {
    TaskSubmission {
        run_id: run,
        call_id: "batch".into(),
        index,
        request: TaskCall {
            tool: name.into(),
            arguments: json!({}),
            title: Some(format!("检查 {name}")),
            parent_task_id: None,
            execution_timeout_secs: None,
        },
        tool: Arc::new(Gate { name, release }),
    }
}
async fn wait_status(backend: &Backend, id: &str, status: &str) {
    tokio::time::timeout(Duration::from_secs(12), async {
        loop {
            if backend
                .manager
                .list(&backend.user, backend.conversation)
                .await
                .unwrap()
                .iter()
                .any(|t| t.id == id && t.status == status)
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn parallel_tasks_start_without_an_execution_slot_limit() {
    let backend = setup().await;
    let run = Uuid::new_v4();
    let releases: Vec<_> = (0..8).map(|_| Arc::new(Notify::new())).collect();
    let mut ids = Vec::new();
    // New tasks can start while the first batch still occupies the former four slots.
    for batch in [0..4, 4..8] {
        let submissions = batch
            .map(|index| submission("parallel", releases[index].clone(), index, run))
            .collect();
        ids.extend(backend.submit(submissions).await.unwrap());
        backend.manager.dispatch().await.unwrap();
    }
    let tasks = backend
        .manager
        .list(&backend.user, backend.conversation)
        .await
        .unwrap();
    assert_eq!(tasks.len(), 8);
    assert!(tasks.iter().all(|task| task.status == "running"));
    assert_eq!(backend.manager.running.lock().await.len(), 8);
    for release in releases {
        release.notify_one();
    }
    for id in ids {
        wait_status(&backend, &id, "succeeded").await;
    }
}

#[tokio::test]
async fn partial_results_and_detached_tasks_survive_reply_end() {
    let backend = setup().await;
    let a = Arc::new(Notify::new());
    let b = Arc::new(Notify::new());
    let run = Uuid::new_v4();
    let ids = backend
        .submit(vec![
            submission("a", a.clone(), 0, run),
            submission("b", b.clone(), 1, run),
        ])
        .await
        .unwrap();
    backend.manager.dispatch().await.unwrap();
    assert!(!backend.ready_to_finish().await.unwrap());
    a.notify_one();
    let partial = backend
        .control(
            "wait_tools",
            json!({"task_ids":ids,"yield_when":"any","wait_timeout_secs":5}),
        )
        .await
        .unwrap();
    assert_eq!(partial["completed"].as_array().unwrap().len(), 1);
    assert_eq!(partial["pending"].as_array().unwrap().len(), 1);
    assert!(!backend.ready_to_finish().await.unwrap());
    backend.acknowledge(&partial).await.unwrap();
    backend
        .control("detach_tools", json!({"task_ids":[ids[1]]}))
        .await
        .unwrap();
    assert!(backend.ready_to_finish().await.unwrap());
    backend.close().await.unwrap();
    assert!(!backend.manager.running.lock().await[&ids[1]].token.is_cancelled());
    b.notify_one();
    wait_status(&backend, &ids[1], "succeeded").await;
}

#[tokio::test]
async fn waiting_timeout_does_not_cancel_and_queue_never_expires() {
    let backend = setup().await;
    let release = Arc::new(Notify::new());
    let ids = backend
        .submit(vec![submission("slow", release.clone(), 0, Uuid::new_v4())])
        .await
        .unwrap();
    query("UPDATE assistant_tasks SET created_at=1 WHERE id=?1")
        .bind(&ids[0])
        .execute(&backend.manager.pool)
        .await
        .unwrap();
    let snapshot = backend
        .control("wait_tools", json!({"task_ids":ids,"wait_timeout_secs":0}))
        .await
        .unwrap();
    assert_eq!(snapshot["pending"][0]["status"], "queued");
    assert!(snapshot["pending"][0]["deadline_at"].is_null());
    backend.manager.dispatch().await.unwrap();
    let snapshot = backend.control("get_tasks", json!({"task_ids":ids})).await.unwrap();
    assert_eq!(snapshot["pending"][0]["status"], "running");
    assert!(snapshot["pending"][0]["deadline_at"].as_i64().unwrap() > now());
    backend
        .manager
        .cancel("owner", backend.conversation, &ids)
        .await
        .unwrap();
    wait_status(&backend, &ids[0], "cancelled").await;
}

#[tokio::test]
async fn submissions_are_idempotent_and_other_owners_cannot_cancel() {
    let backend = setup().await;
    let release = Arc::new(Notify::new());
    let run = Uuid::new_v4();
    let ids = backend
        .submit(vec![submission("slow", release.clone(), 0, run)])
        .await
        .unwrap();
    assert_eq!(
        ids,
        backend.submit(vec![submission("slow", release, 0, run)]).await.unwrap()
    );
    assert_eq!(
        backend.manager.list("owner", backend.conversation).await.unwrap().len(),
        1
    );
    assert!(backend
        .manager
        .cancel("other", backend.conversation, &ids)
        .await
        .is_err());
    backend
        .manager
        .cancel("owner", backend.conversation, &ids)
        .await
        .unwrap();
    backend.manager.dispatch().await.unwrap();
    wait_status(&backend, &ids[0], "cancelled").await;
    assert!(backend.manager.running.lock().await.is_empty());
}

#[tokio::test]
async fn parent_checks_and_recursive_cancellation_preserve_scope() {
    let root = setup().await;
    let parent = root.submit(vec![submission("parent", Arc::new(Notify::new()), 0, Uuid::new_v4())])
        .await.unwrap().remove(0);
    let child_backend = root.for_handoff("programmer", "child").unwrap();
    let mut invalid = submission("child", Arc::new(Notify::new()), 0, Uuid::new_v4());
    invalid.request.parent_task_id = Some(parent.clone());
    assert!(child_backend.submit(vec![invalid]).await.is_err());
    let mut child = submission("child", Arc::new(Notify::new()), 0, Uuid::new_v4());
    child.request.parent_task_id = Some(parent.clone());
    let child = root.submit(vec![child]).await.unwrap().remove(0);
    let mut grandchild = submission("grandchild", Arc::new(Notify::new()), 0, Uuid::new_v4());
    grandchild.request.parent_task_id = Some(child.clone());
    let grandchild = root.submit(vec![grandchild]).await.unwrap().remove(0);
    root.manager.cancel(&root.user, root.conversation, std::slice::from_ref(&parent)).await.unwrap();
    for id in [parent, child, grandchild] {
        let status: String = query_scalar("SELECT status FROM assistant_tasks WHERE id=?1")
            .bind(id).fetch_one(&root.manager.pool).await.unwrap();
        assert_eq!(status, "cancelled");
    }
}

#[tokio::test]
async fn submission_identity_includes_generation_and_malformed_jobs_are_cancelled() {
    let first = setup().await;
    let run = Uuid::new_v4();
    let first_id = first.submit(vec![submission("check", Arc::new(Notify::new()), 0, run)]).await.unwrap();
    query("UPDATE assistant_generation_jobs SET status='completed' WHERE id=?1")
        .bind(Uuid::parse_str(&first.job).unwrap()).execute(&first.manager.pool).await.unwrap();
    let job = Uuid::new_v4();
    query("INSERT INTO assistant_generation_jobs(id,conversation_id,user_id,status) VALUES (?1,?2,'owner','running')")
        .bind(job).bind(first.conversation).execute(&first.manager.pool).await.unwrap();
    let next = Backend { manager:first.manager.clone(), user:first.user.clone(), conversation:first.conversation,
        job:job.to_string(), scope_id:None };
    let next_id = next.submit(vec![submission("check", Arc::new(Notify::new()), 0, run)]).await.unwrap();
    assert_ne!(first_id, next_id);
    insert_task(&next, "malformed", "not-a-uuid", "queued", false).await;
    next.manager.recover().await.unwrap();
    let statuses: Vec<(String,String)> = query_as("SELECT id,status FROM assistant_tasks ORDER BY id")
        .fetch_all(&next.manager.pool).await.unwrap();
    assert!(statuses.contains(&(first_id[0].clone(), "cancelled".into())));
    assert!(statuses.contains(&(next_id[0].clone(), "queued".into())));
    assert!(statuses.contains(&("malformed".into(), "cancelled".into())));
    insert_task(&next, "malformed-dispatch", "invalid", "queued", false).await;
    next.manager.cancel(&next.user, next.conversation, &next_id).await.unwrap();
    next.manager.dispatch().await.unwrap();
    let status: String = query_scalar("SELECT status FROM assistant_tasks WHERE id='malformed-dispatch'")
        .fetch_one(&next.manager.pool).await.unwrap();
    assert_eq!(status, "cancelled");
}

#[tokio::test]
async fn execution_deadline_stops_a_running_task() {
    let backend = setup().await;
    let mut task = submission("slow", Arc::new(Notify::new()), 0, Uuid::new_v4());
    task.request.execution_timeout_secs = Some(1);
    let ids = backend.submit(vec![task]).await.unwrap();
    backend.manager.dispatch().await.unwrap();
    wait_status(&backend, &ids[0], "timed_out").await;
}

#[tokio::test]
async fn restart_preserves_results_and_never_replays_uncertain_work() {
    let backend = setup().await;
    let run = Uuid::new_v4();
    let ids = backend
        .submit(vec![
            submission("a", Arc::new(Notify::new()), 0, run),
            submission("b", Arc::new(Notify::new()), 1, run),
        ])
        .await
        .unwrap();
    query("UPDATE assistant_tasks SET status='running' WHERE id=?1")
        .bind(&ids[0])
        .execute(&backend.manager.pool)
        .await
        .unwrap();
    query("UPDATE assistant_tasks SET detached=1 WHERE id=?1")
        .bind(&ids[1])
        .execute(&backend.manager.pool)
        .await
        .unwrap();
    backend.manager.recover().await.unwrap();
    let tasks = backend.manager.list("owner", backend.conversation).await.unwrap();
    assert_eq!(tasks.iter().find(|t| t.id == ids[0]).unwrap().status, "unknown");
    assert_eq!(tasks.iter().find(|t| t.id == ids[1]).unwrap().status, "queued");
    assert!(!backend.ready_to_finish().await.unwrap());
}

async fn command_task(backend: &Backend, dir: &std::path::Path, command: &str, index: i64) -> String {
    command_task_with_resource(backend, dir, command, index, None).await
}

async fn command_task_with_resource(backend: &Backend, dir: &std::path::Path, command: &str, index: i64, resource: Option<&str>) -> String {
    let id = Uuid::new_v4().to_string();
    let mut arguments = json!({"command":command,"cwd":dir});
    if let Some(key) = resource { arguments["resource_key"] = json!(key); }
    query("INSERT INTO assistant_tasks(id,user_id,conversation_id,generation_job_id,run_id,source_call_id,source_index,title,tool,arguments,background_allowed,parallel_safe,resource_key,execution_timeout_secs,created_at) VALUES (?1,'owner',?2,?3,?4,'cmd',?5,'test command','command',?6,1,1,?7,60,?8)")
        .bind(&id).bind(backend.conversation).bind(&backend.job).bind(Uuid::new_v4().to_string()).bind(index)
        .bind(arguments).bind(resource).bind(now()+index).execute(&backend.manager.pool).await.unwrap();
    id
}

#[tokio::test]
async fn command_logs_and_explicit_mutex_and_group_cancellation() {
    let backend = setup().await;
    let dir = tempfile::tempdir().unwrap();
    let first = command_task_with_resource(&backend, dir.path(), "sleep 300 & echo child:$!; wait", 0, Some("test:command")).await;
    let second = command_task_with_resource(&backend, dir.path(), "echo finished", 1, Some("test:command")).await;
    backend.manager.dispatch().await.unwrap();
    // Creation timestamps can tie; explicitly assert there is only one owner.
    assert_eq!(backend.manager.running.lock().await.len(), 1);
    let tasks = backend.manager.list("owner", backend.conversation).await.unwrap();
    assert_eq!(tasks.iter().filter(|t| t.status == "queued").count(), 1);
    // Cancel both, including the queued task, and confirm no child survives.
    let running_id = tasks.iter().find(|t| t.status == "running").unwrap().id.clone();
    if running_id == first {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if backend
                    .manager
                    .events("owner", backend.conversation, 0)
                    .await
                    .unwrap()
                    .iter()
                    .any(|e| e.kind == "output")
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        let events = backend.manager.events("owner", backend.conversation, 0).await.unwrap();
        let text = events
            .iter()
            .filter_map(|e| e.data["text"].as_str())
            .collect::<String>();
        let pid: u32 = text.trim().strip_prefix("child:").unwrap().parse().unwrap();
        backend
            .manager
            .cancel("owner", backend.conversation, &[first.clone(), second.clone()])
            .await
            .unwrap();
        wait_status(&backend, &first, "cancelled").await;
        let proc = std::fs::read_to_string(format!("/proc/{pid}/stat"));
        assert!(proc.is_err() || proc.unwrap().split(") ").nth(1).unwrap().starts_with('Z'));
    } else {
        backend
            .manager
            .cancel("owner", backend.conversation, &[first.clone(), second.clone()])
            .await
            .unwrap();
    }
    wait_status(&backend, &first, "cancelled").await;
}

#[tokio::test]
async fn model_continues_on_fast_result_while_slow_tool_keeps_running() {
    struct Model {
        step: std::sync::atomic::AtomicUsize,
    }
    #[async_trait]
    impl g::Model for Model {
        async fn generate(&self, request: g::ModelRequest) -> Result<g::ModelResponse, g::ModelError> {
            let message = match self.step.fetch_add(1, std::sync::atomic::Ordering::SeqCst) {
                0 => g::Message::new(
                    g::Role::Assistant,
                    vec![g::Content::ToolCall {
                        id: "batch".into(),
                        name: "run_tools".into(),
                        arguments: json!({
                            "calls":[{"tool":"a","arguments":{},"title":"检查 A"},{"tool":"b","arguments":{},"title":"检查 B"}],"wait_timeout_secs":5
                        }),
                    }],
                ),
                1 => {
                    let result = request
                        .messages
                        .iter()
                        .flat_map(|m| &m.content)
                        .find_map(|c| match c {
                            g::Content::ToolResult { call_id, result, .. } if call_id == "batch" => Some(result),
                            _ => None,
                        })
                        .unwrap();
                    assert_eq!(result["completed"][0]["result"]["answer"], "a");
                    assert_eq!(result["pending"][0]["status"], "running");
                    g::Message::new(
                        g::Role::Assistant,
                        vec![
                            g::Content::text("A is sufficient; B can continue in the background"),
                            g::Content::ToolCall {
                                id: "detach".into(),
                                name: "detach_tools".into(),
                                arguments: json!({"task_ids":[result["pending"][0]["id"]]}),
                            },
                        ],
                    )
                }
                2 => g::Message::assistant("done using A"),
                _ => panic!("unexpected model request"),
            };
            Ok(g::ModelResponse::new(message))
        }
    }
    let backend = setup().await;
    let fast = Arc::new(Notify::new());
    let release_fast = fast.clone();
    let slow = Arc::new(Notify::new());
    let agent = g::Agent::new(Arc::new(Model {
        step: std::sync::atomic::AtomicUsize::new(0),
    }))
    .tool(Gate {
        name: "a",
        release: fast,
    })
    .tool(Gate {
        name: "b",
        release: slow.clone(),
    })
    .with_task_backend(backend.clone());
    let manager = backend.manager.clone();
    let stop = CancellationToken::new();
    let stopped = stop.clone();
    let worker = tokio::spawn(async move {
        loop {
            manager.dispatch().await.unwrap();
            // Release A only after both tasks have been admitted. Otherwise a
            // fast A can legitimately finish before B leaves the queue.
            if manager.running.lock().await.len() == 2 {
                release_fast.notify_one();
            }
            tokio::select! { _=stopped.cancelled()=>break, _=tokio::time::sleep(Duration::from_millis(5))=>{} }
        }
    });
    let output = g::Runtime::new()
        .run(&agent, g::RunRequest::new(vec![g::Message::user("work")]))
        .await
        .unwrap();
    assert_eq!(output.final_text, "done using A");
    let b = backend
        .manager
        .list("owner", backend.conversation)
        .await
        .unwrap()
        .into_iter()
        .find(|t| t.tool == "b")
        .unwrap();
    assert_eq!(b.status, "running");
    assert!(b.detached);
    backend.close().await.unwrap();
    slow.notify_one();
    wait_status(&backend, &b.id, "succeeded").await;
    stop.cancel();
    worker.await.unwrap();
}

#[tokio::test]
async fn a_durable_stop_request_prevents_detach_and_queued_execution() {
    let backend = setup().await;
    let ids = backend
        .submit(vec![submission("slow", Arc::new(Notify::new()), 0, Uuid::new_v4())])
        .await
        .unwrap();
    query("INSERT INTO assistant_messages(conversation_id,role,content,generation_job_id,metadata) VALUES (?1,'user','stop',?2,?3)")
        .bind(backend.conversation).bind(Uuid::parse_str(&backend.job).unwrap()).bind(json!({"cancellation_requested":true}))
        .execute(&backend.manager.pool).await.unwrap();
    assert!(backend.control("detach_tools", json!({"task_ids":ids})).await.is_err());
    backend.manager.dispatch().await.unwrap();
    wait_status(&backend, &ids[0], "cancelled").await;
    assert!(backend.manager.running.lock().await.is_empty());
}

#[tokio::test]
async fn reads_original_313_record_result_tail_and_enforces_execution_scope() {
    let backend = setup().await;
    insert_task(&backend, "quotes", &backend.job, "succeeded", false).await;
    let prices: Vec<Value> = (0..313)
        .map(|i| {
            let date = chrono::NaiveDate::from_ymd_opt(2026, 10, 13).unwrap()
                - chrono::Days::new(312 - i);
            json!({"date": date.to_string(), "price": if i == 312 { 902.1 } else { 900.0 }})
        })
        .collect();
    let structured = json!({"body":{"prices":prices,"latest_date":"2026-10-13"},"status":200});
    let original = json!({"content":[{"type":"text","text":structured.to_string()}],"structuredContent":structured,"isError":false});
    query("UPDATE assistant_tasks SET result=?1 WHERE id='quotes'").bind(&original).execute(&backend.manager.pool).await.unwrap();
    let execution: TaskRecord = query_as("SELECT * FROM assistant_tasks WHERE id='quotes'").fetch_one(&backend.manager.pool).await.unwrap();
    let preview = backend.snapshot(vec![execution.clone()], true).await.unwrap();
    assert_eq!(preview["completed"][0]["result"]["truncated"], true);
    assert_eq!(preview["completed"][0]["read_result"]["arguments"]["json_pointer"], "/structuredContent/body/prices");
    let args = json!({"task_id":"quotes","json_pointer":"/structuredContent/body/prices","offset":283,"limit":30});
    let page = result::read(&backend.manager.pool, &execution, args.clone()).await.unwrap();
    assert_eq!(page["total"], 313);
    assert_eq!(page["data"].as_array().unwrap().len(), 30);
    assert_eq!(page["data"][29]["date"], "2026-10-13");
    assert_eq!(page["data"][29]["price"], 902.1);
    assert!(page["next_offset"].is_null());
    assert!(page.to_string().len() <= 4096);
    for (user, conversation, scope) in [("other",backend.conversation,None),("owner",backend.conversation+1,None),("owner",backend.conversation,Some("child"))] {
        let mut foreign = execution.clone(); foreign.user_id=user.into(); foreign.conversation_id=conversation; foreign.scope_id=scope.map(str::to_owned);
        assert!(result::read(&backend.manager.pool, &foreign, args.clone()).await.is_err());
    }
    let stored: Value = query_scalar("SELECT result FROM assistant_tasks WHERE id='quotes'").fetch_one(&backend.manager.pool).await.unwrap();
    assert_eq!(stored, original);
}

#[tokio::test]
async fn result_pages_preserve_whole_unicode_items_and_pointer_escaping() {
    let backend = setup().await;
    insert_task(&backend, "unicode", &backend.job, "succeeded", false).await;
    let original = json!({"a/b~c":(0..20).map(|i| json!({"index":i,"text":"中文\n\"".repeat(100)})).collect::<Vec<_>>()});
    query("UPDATE assistant_tasks SET result=?1 WHERE id='unicode'").bind(&original).execute(&backend.manager.pool).await.unwrap();
    let execution: TaskRecord = query_as("SELECT * FROM assistant_tasks WHERE id='unicode'").fetch_one(&backend.manager.pool).await.unwrap();
    let mut offset=0;
    loop {
        let page=result::read(&backend.manager.pool,&execution,json!({"task_id":"unicode","json_pointer":"/a~1b~0c","offset":offset,"limit":20})).await.unwrap();
        assert!(page.to_string().len() <= 4096);
        let data=page["data"].as_array().unwrap(); assert!(!data.is_empty());
        for (i,item) in data.iter().enumerate() { assert_eq!(item,&original["a/b~c"][offset+i]); }
        offset+=data.len();
        if page["next_offset"].is_null() { break; }
        assert_eq!(page["next_offset"].as_u64().unwrap() as usize,offset);
    }
    assert_eq!(offset,20);
    assert!(result::read(&backend.manager.pool,&execution,json!({"task_id":"unicode","json_pointer":"/a~2b"})).await.is_err());
    let media=json!({"structuredContent":{"ok":false},"isError":true,"content":[
        {"type":"text","text":"{\"ok\":false}"},{"type":"text","text":"failure explanation"},
        {"type":"image","mimeType":"image/png","data":"image-data"},
        {"type":"resource_link","uri":"artifact:test","name":"chart.png"}]});
    let view=result::model_view(&media);
    assert_eq!(view["content"].as_array().unwrap().len(),3);
    assert_eq!(view["content"][1]["data"],"image-data");
    assert_eq!(view["content"][2]["name"],"chart.png");
    assert_eq!(view["isError"],true);
}

#[test]
fn large_structured_preview_keeps_error_warning_and_artifact_descriptions() {
    let raw=json!({"isError":true,"structuredContent":{"status":503,"body":{"warning":"cached quotes","prices":["x".repeat(10000)]}},
        "content":[{"type":"image","mimeType":"image/png","data":"x".repeat(10000)},
            {"type":"resource_link","name":"chart.png","uri":"artifact:chart"}]});
    let preview=result_preview(&raw,4096);
    assert_eq!(preview["preserved"]["isError"],true);
    assert_eq!(preview["preserved"]["/structuredContent/body/warning"],"cached quotes");
    assert_eq!(preview["preserved"]["content"][1]["name"],"chart.png");
    assert!(preview.to_string().len()<=4096);
}
