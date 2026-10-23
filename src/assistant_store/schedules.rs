use anyhow::{bail, ensure};
use chrono::{DateTime, Timelike, Utc};
use chrono_tz::Tz;
use serde::{Deserialize, Serialize};
use sqlx::{query, query_as, query_scalar, types::Json, FromRow};

use crate::db;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutionSettings {
    pub agent: String,
    pub workspace_mode: String,
    pub cwd: Option<String>,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub reasoning_level: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScheduleInput {
    pub title: String,
    pub prompt: String,
    pub cron: String,
    pub enabled: bool,
    pub execution: ExecutionSettings,
}

impl ScheduleInput {
    pub fn validate(&mut self, now: DateTime<Utc>) -> anyhow::Result<()> {
        self.title = self.title.trim().into();
        self.prompt = self.prompt.trim().into();
        self.cron = self.cron.split_whitespace().collect::<Vec<_>>().join(" ");
        ensure!(!self.title.is_empty() && self.title.chars().count() <= 100, "任务名称须为 1–100 字");
        ensure!(!self.prompt.is_empty() && self.prompt.chars().count() <= 32000, "任务内容须为 1–32000 字");
        ensure!(matches!(self.execution.agent.as_str(), "firefly" | "codex" | "claude"), "Agent 无效");
        ensure!(matches!(self.execution.workspace_mode.as_str(), "shared" | "parallel"), "工作目录模式无效");
        ensure!(self.execution.agent != "firefly" || self.execution.workspace_mode == "shared", "流萤不支持并行目录");
        next_occurrence(&self.cron, now)?;
        Ok(())
    }
}

/// Standard five-field cron, in the same explicit timezone as other user-facing schedules.
pub fn next_occurrence(expression: &str, after: DateTime<Utc>) -> anyhow::Result<DateTime<Utc>> {
    next_occurrence_in(expression, after, crate::config::timezone())
}

fn next_occurrence_in(expression: &str, after: DateTime<Utc>, timezone: Tz) -> anyhow::Result<DateTime<Utc>> {
    ensure!(expression.len() <= 120 && expression.split_whitespace().count() == 5,
        "cron 须为五段：分 时 日 月 星期，例如 0 9 * * *");
    let cron = croner::Cron::new(expression).parse().map_err(|e| anyhow::anyhow!("cron 表达式无效：{e}"))?;
    // Cron ticks occur on exact seconds; Croner otherwise carries subsecond input forward.
    let next = cron.find_next_occurrence(&after.with_timezone(&timezone).with_nanosecond(0).unwrap(), false)
        .map_err(|_| anyhow::anyhow!("cron 没有可执行的未来时间，请检查日期"))?;
    Ok(next.with_timezone(&Utc))
}

/// Rebind schedules once when APP_TIMEZONE changes; ordinary restarts keep due work.
pub async fn synchronize_timezone(pool: &sqlx::SqlitePool, now: DateTime<Utc>, timezone: Tz) -> anyhow::Result<()> {
    let mut tx = pool.begin_with("BEGIN IMMEDIATE").await?;
    let changed: Vec<(i64, String, bool)> = query_as(
        "SELECT id,cron,enabled FROM assistant_schedules WHERE timezone != ?1")
        .bind(timezone.name()).fetch_all(&mut *tx).await?;
    for (id, cron, enabled) in changed {
        let next = enabled.then(|| next_occurrence_in(&cron, now, timezone)).transpose()?
            .map(|at| at.timestamp_micros());
        query("UPDATE assistant_schedules SET timezone=?2,next_run_at=?3 WHERE id=?1")
            .bind(id).bind(timezone.name()).bind(next).execute(&mut *tx).await?;
    }
    tx.commit().await?;
    Ok(())
}

#[derive(Debug, Serialize, FromRow)]
pub struct ScheduleRecord {
    pub id: i64,
    #[serde(skip_serializing)]
    pub user_id: String,
    pub title: String,
    pub prompt: String,
    pub cron: String,
    pub execution: Json<ExecutionSettings>,
    pub enabled: bool,
    #[sqlx(try_from = "crate::db::NullableTimestamp")]
    pub next_run_at: Option<DateTime<Utc>>,
    #[sqlx(try_from = "crate::db::NullableTimestamp")]
    pub last_run_at: Option<DateTime<Utc>>,
    pub last_conversation_id: Option<i64>,
    pub last_job_id: Option<uuid::Uuid>,
    pub last_notice: Option<String>,
    pub last_status: Option<String>,
    pub last_error: Option<String>,
}

const SELECT: &str = "SELECT s.*, j.status AS last_status, j.error_message AS last_error
    FROM assistant_schedules s LEFT JOIN assistant_generation_jobs j ON j.id = s.last_job_id";

#[derive(Debug, thiserror::Error)]
#[error("定时任务数量已达上限（100 条）")]
pub struct ScheduleLimit;

pub async fn list(user: &str) -> anyhow::Result<Vec<ScheduleRecord>> {
    let sql = format!("{SELECT} WHERE s.user_id = ?1 ORDER BY s.id DESC");
    Ok(query_as(sqlx::AssertSqlSafe(sql)).bind(user).fetch_all(db::pool()).await?)
}

pub async fn get(id: i64, user: &str) -> anyhow::Result<Option<ScheduleRecord>> {
    let sql = format!("{SELECT} WHERE s.id = ?1 AND s.user_id = ?2");
    Ok(query_as(sqlx::AssertSqlSafe(sql)).bind(id).bind(user).fetch_optional(db::pool()).await?)
}

pub async fn save(id: Option<i64>, user: &str, input: &ScheduleInput, now: DateTime<Utc>) -> anyhow::Result<Option<i64>> {
    let next = input.enabled.then(|| next_occurrence(&input.cron, now)).transpose()?.map(|at| at.timestamp_micros());
    let mut tx = db::pool().begin_with("BEGIN IMMEDIATE").await?;
    let saved = if let Some(id) = id {
        query_scalar("UPDATE assistant_schedules SET title=?3, prompt=?4, cron=?5, execution=?6,
            enabled=?7, next_run_at=?8, last_notice=NULL, updated_at=?9, timezone=?10
            WHERE id=?1 AND user_id=?2 RETURNING id")
            .bind(id).bind(user).bind(&input.title).bind(&input.prompt).bind(&input.cron)
            .bind(Json(&input.execution)).bind(input.enabled).bind(next).bind(now.timestamp_micros())
            .bind(crate::config::timezone().name())
            .fetch_optional(&mut *tx).await?
    } else {
        let count: i64 = query_scalar("SELECT count(*) FROM assistant_schedules WHERE user_id=?1")
            .bind(user).fetch_one(&mut *tx).await?;
        if count >= 100 { return Err(ScheduleLimit.into()) }
        Some(query_scalar("INSERT INTO assistant_schedules (user_id,title,prompt,cron,execution,enabled,next_run_at,timezone)
            VALUES (?1,?2,?3,?4,?5,?6,?7,?8) RETURNING id")
            .bind(user).bind(&input.title).bind(&input.prompt).bind(&input.cron)
            .bind(Json(&input.execution)).bind(input.enabled).bind(next)
            .bind(crate::config::timezone().name()).fetch_one(&mut *tx).await?)
    };
    tx.commit().await?;
    Ok(saved)
}

pub async fn set_enabled(id: i64, user: &str, enabled: bool, now: DateTime<Utc>) -> anyhow::Result<bool> {
    let mut tx = db::pool().begin_with("BEGIN IMMEDIATE").await?;
    let row: Option<(String, bool)> = query_as("SELECT cron,enabled FROM assistant_schedules WHERE id=?1 AND user_id=?2")
        .bind(id).bind(user).fetch_optional(&mut *tx).await?;
    let Some((cron, previous)) = row else { return Ok(false) };
    // Retried enable requests must not postpone an already due occurrence.
    if previous != enabled {
        let next = enabled.then(|| next_occurrence(&cron, now)).transpose()?.map(|at| at.timestamp_micros());
        query("UPDATE assistant_schedules SET enabled=?3,next_run_at=?4,last_notice=NULL,updated_at=?5 WHERE id=?1 AND user_id=?2")
            .bind(id).bind(user).bind(enabled).bind(next).bind(now.timestamp_micros()).execute(&mut *tx).await?;
    }
    tx.commit().await?;
    Ok(true)
}

pub async fn delete(id: i64, user: &str) -> anyhow::Result<bool> {
    Ok(query("DELETE FROM assistant_schedules WHERE id=?1 AND user_id=?2")
        .bind(id).bind(user).execute(db::pool()).await?.rows_affected() > 0)
}

pub async fn due(now: DateTime<Utc>) -> anyhow::Result<Vec<ScheduleRecord>> {
    let sql = format!("{SELECT} WHERE s.enabled=TRUE AND s.next_run_at<=?1 ORDER BY s.next_run_at,s.id LIMIT 100");
    Ok(query_as(sqlx::AssertSqlSafe(sql)).bind(now.timestamp_micros()).fetch_all(db::pool()).await?)
}

/// Claim, conversation, user message, queued job and next tick commit together.
/// A crash or a second scheduler can neither lose nor duplicate an occurrence.
pub async fn dispatch(
    id: i64, user: &str, now: DateTime<Utc>, system_prompt: &str, enable_tools: bool, queue_limit: usize,
) -> anyhow::Result<Option<i64>> {
    let mut tx = db::pool().begin_with("BEGIN IMMEDIATE").await?;
    let sql = format!("{SELECT} WHERE s.id=?1 AND s.user_id=?2 AND s.enabled=TRUE AND s.next_run_at<=?3");
    let record: Option<ScheduleRecord> = query_as(sqlx::AssertSqlSafe(sql)).bind(id).bind(user)
        .bind(now.timestamp_micros()).fetch_optional(&mut *tx).await?;
    let Some(record) = record else { return Ok(None) };
    // Missed occurrences while offline coalesce into one run, never a backlog.
    let next = next_occurrence(&record.cron, now)?.timestamp_micros();
    if matches!(record.last_status.as_deref(), Some("queued" | "running")) {
        query("UPDATE assistant_schedules SET next_run_at=?2,last_notice='上一轮尚未结束，本次触发已跳过' WHERE id=?1")
            .bind(id).bind(next).execute(&mut *tx).await?;
        tx.commit().await?;
        return Ok(None);
    }
    if let Err(error) = super::enforce_submission_capacity(&mut tx, user, queue_limit).await {
        if !matches!(error.downcast_ref::<super::SubmissionConflict>(), Some(super::SubmissionConflict::QueueFull)) {
            return Err(error);
        }
        query("UPDATE assistant_schedules SET last_notice='执行队列已满，稍后自动重试' WHERE id=?1")
            .bind(id).execute(&mut *tx).await?;
        tx.commit().await?;
        return Ok(None);
    }
    let settings = record.execution.0;
    if !matches!(settings.agent.as_str(), "firefly" | "codex" | "claude") { bail!("未知 Agent") }
    let workspace_id = (settings.workspace_mode == "parallel").then(uuid::Uuid::new_v4);
    let actual_cwd = workspace_id.map(crate::assistant_workspace::destination);
    let conversation_id: i64 = query_scalar("INSERT INTO assistant_conversations
        (user_id,title,system_prompt,agent,codex_cwd,codex_provider,codex_model,codex_reasoning_level,workspace_mode,source_cwd,workspace_id,workspace_state,claude_model,claude_provider,claude_reasoning_level)
        VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15) RETURNING id")
        .bind(user).bind(format!("定时 · {}", record.title)).bind(system_prompt).bind(&settings.agent)
        .bind(actual_cwd.as_deref().or(settings.cwd.as_deref()))
        .bind(if settings.agent == "codex" { settings.provider.as_deref() } else { None })
        .bind(if settings.agent == "codex" { settings.model.as_deref() } else { None })
        .bind(if settings.agent == "codex" { settings.reasoning_level.as_deref() } else { None }).bind(&settings.workspace_mode)
        .bind(workspace_id.map(|_| settings.cwd.as_deref().unwrap_or("")))
        .bind(workspace_id).bind(if workspace_id.is_some() { "pending" } else { "ready" })
        .bind(if settings.agent == "claude" { settings.model.as_deref() } else { None })
        .bind(if settings.agent == "claude" { settings.provider.as_deref() } else { None })
        .bind(if settings.agent == "claude" { settings.reasoning_level.as_deref() } else { None })
        .fetch_one(&mut *tx).await?;
    let job = uuid::Uuid::new_v4();
    super::generation::insert_queued_user_turn(&mut tx, job, conversation_id, user, enable_tools, record.prompt, &[], None).await?;
    query("UPDATE assistant_schedules SET next_run_at=?2,last_run_at=?3,last_conversation_id=?4,last_job_id=?5,last_notice=NULL WHERE id=?1")
        .bind(id).bind(next).bind(now.timestamp_micros()).bind(conversation_id).bind(job).execute(&mut *tx).await?;
    tx.commit().await?;
    crate::assistant_events::notify(Some(user), "conversations");
    Ok(Some(conversation_id))
}

#[cfg(test)]
mod tests;
