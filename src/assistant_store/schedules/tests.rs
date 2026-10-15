use super::*;
use chrono::Duration;

fn at(value: &str) -> DateTime<Utc> { value.parse().unwrap() }
fn input(agent: &str) -> ScheduleInput {
    ScheduleInput {
        title: "每日巡检".into(), prompt: "检查项目并汇总结果".into(), cron: "* * * * *".into(), enabled: true,
        execution: ExecutionSettings { agent: agent.into(), workspace_mode: "shared".into(),
            cwd: (agent != "firefly").then(|| "project".into()), provider: None, model: None, reasoning_level: None },
    }
}
async fn setup() { db::set_test_pool(db::test_pool().await); }
async fn count(table: &'static str) -> i64 {
    query_scalar(sqlx::AssertSqlSafe(format!("SELECT count(*) FROM {table}"))).fetch_one(db::pool()).await.unwrap()
}

#[test]
fn schedule_occurrences_use_the_configured_timezone_and_dst() {
    let zone = chrono_tz::America::New_York;
    assert_eq!(next_occurrence_in("0 9 * * *", at("2026-03-07T12:00:00Z"), zone).unwrap(), at("2026-03-07T14:00:00Z"));
    assert_eq!(next_occurrence_in("0 9 * * *", at("2026-03-08T12:00:00Z"), zone).unwrap(), at("2026-03-08T13:00:00Z"));
    assert_eq!(next_occurrence_in("0 9 * * *", at("2026-11-01T12:00:00Z"), zone).unwrap(), at("2026-11-01T14:00:00Z"));
    assert_eq!(next_occurrence_in("0 9 * * *", at("2026-10-08T00:30:00Z"), chrono_tz::UTC).unwrap(), at("2026-10-08T09:00:00Z"));
}

#[tokio::test]
async fn timezone_changes_rebind_schedules_but_restarts_preserve_due_work() {
    setup().await;
    let now = at("2026-10-08T00:30:00Z");
    let mut task = input("firefly");
    task.cron = "0 9 * * *".into();
    let enabled = save(None, "owner", &task, now).await.unwrap().unwrap();
    task.enabled = false;
    let disabled = save(None, "owner", &task, now).await.unwrap().unwrap();
    assert_eq!(get(enabled, "owner").await.unwrap().unwrap().next_run_at, Some(at("2026-10-08T01:00:00Z")));
    let zone = chrono_tz::America::New_York;
    synchronize_timezone(db::pool(), now, zone).await.unwrap();
    assert_eq!(get(enabled, "owner").await.unwrap().unwrap().next_run_at, Some(at("2026-10-08T13:00:00Z")));
    assert!(get(disabled, "owner").await.unwrap().unwrap().next_run_at.is_none());
    let zones: Vec<String> = query_scalar("SELECT timezone FROM assistant_schedules").fetch_all(db::pool()).await.unwrap();
    assert_eq!(zones, [zone.name(), zone.name()]);
    synchronize_timezone(db::pool(), at("2026-10-09T00:30:00Z"), zone).await.unwrap();
    assert_eq!(get(enabled, "owner").await.unwrap().unwrap().next_run_at, Some(at("2026-10-08T13:00:00Z")));
    synchronize_timezone(db::pool(), at("2026-10-09T00:30:00Z"), chrono_tz::UTC).await.unwrap();
    assert_eq!(get(enabled, "owner").await.unwrap().unwrap().next_run_at, Some(at("2026-10-09T09:00:00Z")));
}

#[test]
fn schedules_cron_uses_five_fields_and_beijing_time() {
    let now = at("2026-10-08T00:30:00Z");
    assert_eq!(next_occurrence("0 9 * * *", now).unwrap(), at("2026-10-08T01:00:00Z"));
    assert_eq!(next_occurrence("0 9 * * *", at("2026-10-08T01:00:00Z")).unwrap(), at("2026-10-09T01:00:00Z"));
    assert_eq!(next_occurrence("0 9 * * 1-5", at("2026-10-09T02:00:00Z")).unwrap(), at("2026-10-12T01:00:00Z"));
    assert_eq!(next_occurrence("*/15 * * * *", now).unwrap(), at("2026-10-08T00:45:00Z"));
    assert_eq!(next_occurrence("0 9 * * 0", now).unwrap(), next_occurrence("0 9 * * 7", now).unwrap());
    for invalid in ["", "* * * *", "* * * * * *", "61 * * * *", "0 25 * * *", "@daily"] {
        assert!(next_occurrence(invalid, now).is_err(), "{invalid}");
    }
    let mut task = input("firefly");
    task.prompt.clear();
    assert!(task.validate(now).is_err());
    task = input("unknown");
    assert!(task.validate(now).is_err());
}

#[tokio::test]
async fn schedules_dispatch_is_atomic_deduplicated_and_skips_overlapping_runs() {
    setup().await;
    let now = at("2026-10-08T00:00:00Z");
    let id = save(None, "owner", &input("firefly"), now).await.unwrap().unwrap();
    assert!(due(now).await.unwrap().is_empty());
    let due_at = now + Duration::minutes(1);
    let (a, b) = tokio::join!(dispatch(id, "owner", due_at, "system", true, 32), dispatch(id, "owner", due_at, "system", true, 32));
    let results = [a.unwrap(), b.unwrap()];
    assert_eq!(results.iter().filter(|id| id.is_some()).count(), 1);
    let conversation = results.into_iter().flatten().next().unwrap();
    assert_eq!(count("assistant_conversations").await, 1);
    assert_eq!(count("assistant_messages").await, 1);
    assert_eq!(count("assistant_generation_jobs").await, 1);
    let saved = get(id, "owner").await.unwrap().unwrap();
    assert_eq!(saved.last_status.as_deref(), Some("queued"));
    assert_eq!(saved.last_conversation_id, Some(conversation));
    let content: String = query_scalar("SELECT content FROM assistant_messages WHERE conversation_id=?1")
        .bind(conversation).fetch_one(db::pool()).await.unwrap();
    assert_eq!(content, "检查项目并汇总结果");
    let job = crate::assistant_store::claim_next_queued_generation(conversation, "owner").await.unwrap().unwrap();
    assert!(job.enable_tools);
    let next = due_at + Duration::minutes(1);
    assert!(dispatch(id, "owner", next, "system", true, 32).await.unwrap().is_none());
    let saved = get(id, "owner").await.unwrap().unwrap();
    assert_eq!(saved.last_status.as_deref(), Some("running"));
    assert!(saved.last_notice.unwrap().contains("跳过"));
    assert_eq!(count("assistant_conversations").await, 1);
    crate::assistant_store::update_generation_status(job.id, "failed", Some("模拟失败")).await.unwrap();
    assert_eq!(get(id, "owner").await.unwrap().unwrap().last_error.as_deref(), Some("模拟失败"));
    let second = dispatch(id, "owner", next + Duration::minutes(1), "system", true, 32).await.unwrap().unwrap();
    assert_ne!(conversation, second);
    assert_eq!(count("assistant_conversations").await, 2);
}

#[tokio::test]
async fn schedules_preserve_agent_settings_and_use_existing_queue_recovery() {
    setup().await;
    let now = at("2026-10-08T00:00:00Z");
    for agent in ["firefly", "codex", "claude"] {
        let mut task = input(agent);
        if agent == "codex" {
            task.execution.provider = Some("relay".into());
            task.execution.model = Some("model-a".into());
            task.execution.reasoning_level = Some("high".into());
            task.execution.workspace_mode = "parallel".into();
        }
        if agent == "claude" { task.execution.model = Some("opus".into()); }
        let id = save(None, "owner", &task, now).await.unwrap().unwrap();
        // Restart after several missed minutes submits once and advances into the future.
        let restarted_at = now + Duration::hours(2);
        let conversation = dispatch(id, "owner", restarted_at, "system", false, 32).await.unwrap().unwrap();
        let record = crate::assistant_store::get_conversation(conversation, "owner").await.unwrap().unwrap();
        assert_eq!(record.agent, agent);
        assert_eq!(record.codex_provider, task.execution.provider);
        if agent == "claude" {
            assert_eq!(record.claude_model,task.execution.model);
            assert!(record.codex_model.is_none());
        } else { assert_eq!(record.codex_model, task.execution.model); }
        assert_eq!(record.codex_reasoning_level, task.execution.reasoning_level);
        assert_eq!(record.message_count, 1);
        if agent == "codex" {
            assert_eq!(record.workspace_state, "pending");
            assert_eq!(record.source_cwd.as_deref(), Some("project"));
            assert!(record.codex_cwd.unwrap().contains(".assistant-parallel"));
        }
        let job = get(id, "owner").await.unwrap().unwrap();
        assert!(job.next_run_at.unwrap() > restarted_at);
        assert!(crate::assistant_store::list_queued_generation_conversations().await.unwrap().iter().any(|(id, _)| *id == conversation));
        assert!(!crate::assistant_store::get_generation_job(job.last_job_id.unwrap(), "owner").await.unwrap().unwrap().enable_tools);
        assert!(dispatch(id, "owner", restarted_at, "system", false, 32).await.unwrap().is_none());
    }
}

#[tokio::test]
async fn schedules_respect_edit_disable_delete_ownership_and_queue_capacity() {
    setup().await;
    let now = at("2026-10-08T00:00:00Z");
    let mut task = input("firefly");
    let id = save(None, "owner", &task, now).await.unwrap().unwrap();
    let due_at = now + Duration::minutes(1);
    assert!(list("other").await.unwrap().is_empty());
    assert!(!set_enabled(id, "other", false, now).await.unwrap());
    assert!(!delete(id, "other").await.unwrap());
    assert!(save(Some(id), "other", &task, now).await.unwrap().is_none());
    assert!(dispatch(id, "other", due_at, "", true, 32).await.unwrap().is_none());
    // A user edit after the due list was read cancels that old occurrence.
    assert_eq!(due(due_at).await.unwrap().len(), 1);
    task.cron = "0 9 * * *".into();
    save(Some(id), "owner", &task, due_at).await.unwrap();
    assert!(dispatch(id, "owner", due_at, "", true, 32).await.unwrap().is_none());
    set_enabled(id, "owner", false, due_at).await.unwrap();
    assert!(get(id, "owner").await.unwrap().unwrap().next_run_at.is_none());
    assert!(dispatch(id, "owner", due_at + Duration::days(1), "", true, 32).await.unwrap().is_none());
    task.cron = "* * * * *".into();
    save(Some(id), "owner", &task, now).await.unwrap();
    set_enabled(id, "owner", true, due_at).await.unwrap();
    assert_eq!(get(id, "owner").await.unwrap().unwrap().next_run_at, Some(due_at));
    let blocker = save(None, "owner", &task, now).await.unwrap().unwrap();
    let blocked_conversation = dispatch(blocker, "owner", due_at, "", true, 1).await.unwrap().unwrap();
    assert!(dispatch(id, "owner", due_at, "", true, 1).await.unwrap().is_none());
    assert_eq!(count("assistant_conversations").await, 1);
    let pending = get(id, "owner").await.unwrap().unwrap();
    assert_eq!(pending.next_run_at, Some(due_at));
    assert!(pending.last_notice.unwrap().contains("队列已满"));
    let job = crate::assistant_store::claim_next_queued_generation(blocked_conversation, "owner").await.unwrap().unwrap();
    let conversation = dispatch(id, "owner", due_at, "", true, 1).await.unwrap().unwrap();
    assert!(delete(id, "owner").await.unwrap());
    assert!(dispatch(id, "owner", due_at + Duration::minutes(1), "", true, 1).await.unwrap().is_none());
    assert!(crate::assistant_store::get_conversation(conversation, "owner").await.unwrap().is_some());
    // Removing a past conversation safely clears the links, not the schedule.
    query("DELETE FROM assistant_conversations WHERE id=?1").bind(blocked_conversation).execute(db::pool()).await.unwrap();
    let blocker = get(blocker, "owner").await.unwrap().unwrap();
    assert!(blocker.last_conversation_id.is_none());
    assert!(blocker.last_job_id.is_none());
    assert!(crate::assistant_store::get_generation_job(job.id, "owner").await.unwrap().is_none());
}
