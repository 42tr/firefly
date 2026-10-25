//! Durable tasks owned by the service, independent of a reply or SSE connection.
use anyhow::anyhow;
use g::ToolError;
use sqlx::SqlitePool;
use std::{
    sync::{Arc, OnceLock},
    time::Duration,
};

mod backend;
pub mod command;
mod manager;
mod preview;
mod result;
mod types;

pub use backend::{Backend, RunGuard};
pub use manager::Manager;
pub(crate) use preview::bounded_snapshot_history;
pub use result::ReadTaskResultTool;
pub use types::{TaskEvent, TaskRecord, TaskSummary};

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

static MANAGER: OnceLock<Arc<Manager>> = OnceLock::new();
pub fn global() -> Arc<Manager> {
    MANAGER.get().expect("task manager initialized").clone()
}

pub(crate) fn notify_changed() {
    if let Some(manager) = MANAGER.get() {
        manager.changed.notify_waiters();
    }
}

pub async fn start(pool: SqlitePool) -> anyhow::Result<()> {
    let manager = Manager::new(pool);
    manager.recover().await?;
    MANAGER
        .set(manager.clone())
        .map_err(|_| anyhow!("task manager already initialized"))?;
    tokio::spawn(async move {
        // Coalesce wakeup bursts (tool writes, task completions) so dispatch
        // runs at most once per interval; the periodic fallback still reruns.
        const DISPATCH_MIN_INTERVAL: Duration = Duration::from_millis(250);
        loop {
            let started = tokio::time::Instant::now();
            if let Err(e) = manager.dispatch().await {
                tracing::error!("任务调度失败: {e:#}");
            }
            tokio::select! {
                _ = manager.changed.notified() => {
                    if let Some(remaining) = DISPATCH_MIN_INTERVAL.checked_sub(started.elapsed()) {
                        tokio::time::sleep(remaining).await;
                    }
                },
                _ = tokio::time::sleep(Duration::from_millis(500)) => {},
            }
        }
    });
    Ok(())
}
