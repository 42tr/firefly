//! Durable Goal mode state and its visible stage timeline.

use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::Value;
use sqlx::{query, query_as, query_scalar, FromRow};

use crate::db;

#[derive(Debug, Clone, Serialize, FromRow)]
pub struct GoalRecord {
    pub id: i64,
    pub conversation_id: i64,
    #[serde(skip_serializing)]
    #[allow(dead_code)]
    pub user_id: String,
    pub prompt: String,
    pub status: String,
    pub auto_continue: bool,
    pub current_stage_id: Option<String>,
    pub turn_count: i64,
    pub max_turns: i64,
    pub max_runtime_secs: i64,
    pub last_check_result: Option<Value>,
    #[sqlx(try_from = "crate::db::Timestamp")]
    pub created_at: DateTime<Utc>,
    #[sqlx(try_from = "crate::db::Timestamp")]
    pub updated_at: DateTime<Utc>,
    #[sqlx(try_from = "crate::db::NullableTimestamp")]
    pub paused_at: Option<DateTime<Utc>>,
    #[sqlx(try_from = "crate::db::NullableTimestamp")]
    pub completed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, FromRow)]
pub struct GoalStageRecord {
    pub id: String,
    pub goal_id: i64,
    pub ordinal: i64,
    pub title: String,
    pub summary: String,
    pub status: String,
    #[sqlx(try_from = "crate::db::NullableTimestamp")]
    pub started_at: Option<DateTime<Utc>>,
    #[sqlx(try_from = "crate::db::NullableTimestamp")]
    pub completed_at: Option<DateTime<Utc>>,
}

fn goal_select() -> &'static str {
    "SELECT id, conversation_id, user_id, prompt, status, auto_continue, current_stage_id,
            turn_count, max_turns, max_runtime_secs, last_check_result,
            created_at, updated_at, paused_at, completed_at
     FROM assistant_goals"
}

const GOAL_COLUMNS: &str = "id, conversation_id, user_id, prompt, status, auto_continue, current_stage_id,
    turn_count, max_turns, max_runtime_secs, last_check_result, created_at, updated_at, paused_at, completed_at";

pub async fn get_goal(conversation_id: i64, user_id: &str) -> anyhow::Result<Option<GoalRecord>> {
    let sql = format!("{} WHERE conversation_id=?1 AND user_id=?2 ORDER BY created_at DESC LIMIT 1", goal_select());
    Ok(query_as::<_, GoalRecord>(sqlx::AssertSqlSafe(sql))
        .bind(conversation_id)
        .bind(user_id)
        .fetch_optional(db::pool())
        .await?)
}

pub async fn get_goal_by_id(goal_id: i64, conversation_id: i64, user_id: &str) -> anyhow::Result<Option<GoalRecord>> {
    let sql = format!("{} WHERE id=?1 AND conversation_id=?2 AND user_id=?3", goal_select());
    Ok(query_as::<_, GoalRecord>(sqlx::AssertSqlSafe(sql))
        .bind(goal_id)
        .bind(conversation_id)
        .bind(user_id)
        .fetch_optional(db::pool())
        .await?)
}

pub async fn get_active_goal(conversation_id: i64, user_id: &str) -> anyhow::Result<Option<GoalRecord>> {
    let sql = format!(
        "{} WHERE conversation_id=?1 AND user_id=?2 AND status IN ('planning','running','checking','continuing') ORDER BY created_at DESC LIMIT 1",
        goal_select()
    );
    Ok(query_as::<_, GoalRecord>(sqlx::AssertSqlSafe(sql))
        .bind(conversation_id)
        .bind(user_id)
        .fetch_optional(db::pool())
        .await?)
}

pub async fn list_stages(goal_id: i64, user_id: &str) -> anyhow::Result<Vec<GoalStageRecord>> {
    Ok(query_as::<_, GoalStageRecord>(
        "SELECT s.id, s.goal_id, s.ordinal, s.title, s.summary, s.status, s.started_at, s.completed_at
         FROM assistant_goal_stages s JOIN assistant_goals g ON g.id=s.goal_id
         WHERE s.goal_id=?1 AND g.user_id=?2 ORDER BY s.ordinal",
    )
    .bind(goal_id)
    .bind(user_id)
    .fetch_all(db::pool())
    .await?)
}

pub async fn fetch_goal_execution_results(
    conversation_id: i64, user_id: &str, goal_id: i64,
) -> anyhow::Result<Vec<String>> {
    Ok(query_scalar::<_, String>(
        "WITH goal_jobs AS (
             SELECT DISTINCT j.id, j.created_at
             FROM assistant_generation_jobs j
             JOIN assistant_messages u ON u.generation_job_id=j.id AND u.role='user'
             WHERE j.conversation_id=?1 AND j.user_id=?2
               AND json_extract(u.metadata,'$.goal.goal_id')=?3
         ), final_messages AS (
             SELECT g.created_at, m.id, m.content,
                    ROW_NUMBER() OVER (PARTITION BY m.generation_job_id ORDER BY m.id DESC) AS rn
             FROM goal_jobs g
             JOIN assistant_messages m ON m.generation_job_id=g.id AND m.role='assistant'
             WHERE trim(m.content)<>''
               AND COALESCE(json_extract(m.metadata,'$.goal.origin'),'')<>'goal_summary'
         )
         SELECT content
         FROM final_messages
         WHERE rn=1
         ORDER BY created_at, id",
    )
    .bind(conversation_id)
    .bind(user_id)
    .bind(goal_id)
    .fetch_all(db::pool())
    .await?)
}

pub async fn create_goal(
    conversation_id: i64,
    user_id: &str,
    prompt: &str,
    max_turns: i64,
    max_runtime_secs: i64,
) -> anyhow::Result<GoalRecord> {
    let prompt = prompt.trim();
    anyhow::ensure!(!prompt.is_empty(), "目标不能为空");
    anyhow::ensure!(prompt.chars().count() <= 100_000, "目标过长");
    let max_turns = max_turns.clamp(1, 100);
    let max_runtime_secs = max_runtime_secs.clamp(60, 604800);
    let mut tx = db::pool().begin_with("BEGIN IMMEDIATE").await?;
    let exists: bool = query_scalar(
        "SELECT EXISTS(SELECT 1 FROM assistant_conversations WHERE id=?1 AND user_id=?2 AND is_archived=FALSE)",
    )
    .bind(conversation_id)
    .bind(user_id)
    .fetch_one(&mut *tx)
    .await?;
    anyhow::ensure!(exists, "会话不存在或已归档");
    let busy: bool = query_scalar(
        "SELECT EXISTS(SELECT 1 FROM assistant_goals WHERE conversation_id=?1 AND user_id=?2 AND status IN ('planning','running','checking','continuing'))",
    )
    .bind(conversation_id)
    .bind(user_id)
    .fetch_one(&mut *tx)
    .await?;
    anyhow::ensure!(!busy, "当前会话已有正在运行的 Goal");

    let goal = query_as::<_, GoalRecord>(sqlx::AssertSqlSafe(format!(
        "INSERT INTO assistant_goals(conversation_id,user_id,prompt,status,auto_continue,max_turns,max_runtime_secs)
         VALUES(?1,?2,?3,'running',TRUE,?4,?5) RETURNING {}",
        GOAL_COLUMNS
    )))
    .bind(conversation_id)
    .bind(user_id)
    .bind(prompt)
    .bind(max_turns)
    .bind(max_runtime_secs)
    .fetch_one(&mut *tx)
    .await?;

    let stage_id = format!("goal-{}-stage-1", goal.id);
    query("INSERT INTO assistant_goal_stages(id,goal_id,ordinal,title,summary,status) VALUES(?1,?2,1,?3,?4,'active')")
        .bind(&stage_id)
        .bind(goal.id)
        .bind("执行用户目标")
        .bind(prompt)
        .execute(&mut *tx)
        .await?;
    query("UPDATE assistant_goals SET current_stage_id=?2 WHERE id=?1")
        .bind(goal.id)
        .bind(stage_id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    crate::assistant_events::notify(Some(user_id), "conversations");
    get_goal(conversation_id, user_id).await?.ok_or_else(|| anyhow::anyhow!("创建 Goal 后读取失败"))
}

pub async fn set_goal_status(
    conversation_id: i64,
    goal_id: i64,
    user_id: &str,
    status: &str,
) -> anyhow::Result<Option<GoalRecord>> {
    anyhow::ensure!(matches!(status, "running" | "paused" | "checking" | "continuing" | "needs_review" | "failed" | "limit_reached" | "cancelled" | "completed"), "Goal 状态无效");
    let result = query(
        "UPDATE assistant_goals SET status=?4,
          auto_continue=CASE WHEN ?4='running' THEN TRUE ELSE auto_continue END,
          paused_at=CASE WHEN ?4='paused' THEN CAST(unixepoch('subsec') * 1000000 AS INTEGER) ELSE paused_at END,
          completed_at=CASE WHEN ?4 IN ('completed','cancelled') THEN CAST(unixepoch('subsec') * 1000000 AS INTEGER) ELSE completed_at END,
          updated_at=CAST(unixepoch('subsec') * 1000000 AS INTEGER)
         WHERE id=?1 AND conversation_id=?2 AND user_id=?3 AND status NOT IN ('completed','cancelled')",
    )
    .bind(goal_id)
    .bind(conversation_id)
    .bind(user_id)
    .bind(status)
    .execute(db::pool())
    .await?;
    if result.rows_affected() == 0 {
        return Ok(None);
    }
    crate::assistant_events::notify(Some(user_id), "conversations");
    get_goal(conversation_id, user_id).await
}

pub async fn apply_goal_check(
    goal_id: i64,
    conversation_id: i64,
    user_id: &str,
    status: &str,
    result: &Value,
    current_stage_id: Option<&str>,
) -> anyhow::Result<Option<GoalRecord>> {
    anyhow::ensure!(matches!(status, "continuing" | "completed" | "needs_review" | "failed" | "limit_reached"), "Goal 检查状态无效");
    let record = query(
        "UPDATE assistant_goals SET status=?4, last_check_result=?5,
          current_stage_id=COALESCE(?6,current_stage_id), turn_count=turn_count+1,
          completed_at=CASE WHEN ?4='completed' THEN CAST(unixepoch('subsec') * 1000000 AS INTEGER) ELSE completed_at END,
          updated_at=CAST(unixepoch('subsec') * 1000000 AS INTEGER)
         WHERE id=?1 AND conversation_id=?2 AND user_id=?3 AND status IN ('running','checking','continuing')
         RETURNING id",
    )
    .bind(goal_id)
    .bind(conversation_id)
    .bind(user_id)
    .bind(status)
    .bind(result)
    .bind(current_stage_id)
    .fetch_optional(db::pool())
    .await?;
    if record.is_none() { return Ok(None); }
    crate::assistant_events::notify(Some(user_id), "conversations");
    get_goal_by_id(goal_id, conversation_id, user_id).await
}

pub async fn replace_goal_stages(
    goal_id: i64,
    conversation_id: i64,
    user_id: &str,
    stages: &[(String, String, String, String)],
    current_stage_id: Option<&str>,
) -> anyhow::Result<()> {
    if stages.is_empty() { return Ok(()); }
    let mut tx = db::pool().begin_with("BEGIN IMMEDIATE").await?;
    let owns: bool = query_scalar("SELECT EXISTS(SELECT 1 FROM assistant_goals WHERE id=?1 AND conversation_id=?2 AND user_id=?3)")
        .bind(goal_id).bind(conversation_id).bind(user_id).fetch_one(&mut *tx).await?;
    if !owns { return Ok(()); }
    query("DELETE FROM assistant_goal_stages WHERE goal_id=?1").bind(goal_id).execute(&mut *tx).await?;
    for (index, (id, title, summary, status)) in stages.iter().enumerate() {
        let status = match status.as_str() { "completed" | "active" | "blocked" => status.as_str(), _ => "pending" };
        query("INSERT INTO assistant_goal_stages(id,goal_id,ordinal,title,summary,status) VALUES(?1,?2,?3,?4,?5,?6)")
            .bind(if id.is_empty() { format!("goal-{goal_id}-stage-{}", index + 1) } else { id.clone() })
            .bind(goal_id).bind((index + 1) as i64).bind(title).bind(summary).bind(status)
            .execute(&mut *tx).await?;
    }
    query("UPDATE assistant_goals SET current_stage_id=COALESCE(?2,current_stage_id), updated_at=CAST(unixepoch('subsec') * 1000000 AS INTEGER) WHERE id=?1")
        .bind(goal_id).bind(current_stage_id).execute(&mut *tx).await?;
    tx.commit().await?;
    crate::assistant_events::notify(Some(user_id), "conversations");
    Ok(())
}
