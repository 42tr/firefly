//! Goal mode endpoints. Goal state is conversation-scoped, while the input
//! toggle is applied to the next submitted message by the frontend.

use axum::{extract::Path, Json};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::{api::{auth::Session, error::AppError}, assistant_store};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateGoalRequest {
    pub prompt: String,
    pub max_turns: Option<i64>,
    pub max_runtime_secs: Option<i64>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateGoalRequest {
    pub action: String,
}

fn response(goal: assistant_store::GoalRecord, stages: Vec<assistant_store::GoalStageRecord>) -> Value {
    json!({
        "goal": {
            "id": goal.id,
            "conversation_id": goal.conversation_id,
            "prompt": goal.prompt,
            "status": goal.status,
            "auto_continue": goal.auto_continue,
            "current_stage_id": goal.current_stage_id,
            "turn_count": goal.turn_count,
            "max_turns": goal.max_turns,
            "max_runtime_secs": goal.max_runtime_secs,
            "last_check_result": goal.last_check_result,
            "created_at": goal.created_at,
            "updated_at": goal.updated_at,
            "paused_at": goal.paused_at,
            "completed_at": goal.completed_at,
        },
        "stages": stages,
    })
}

pub async fn get(
    auth: Session, Path(conversation_id): Path<i64>,
) -> Result<Json<Value>, AppError> {
    let user_id = auth.storage_scope();
    crate::assistant_store::get_conversation_for_read(conversation_id, user_id)
        .await
        .map_err(AppError::internal_server_error)?
        .ok_or(AppError::not_found("Conversation not found"))?;
    let Some(goal) = assistant_store::get_goal(conversation_id, user_id)
        .await
        .map_err(AppError::internal_server_error)? else {
        return Ok(Json(json!({ "goal": null, "stages": [] })));
    };
    let stages = assistant_store::list_stages(goal.id, user_id)
        .await
        .map_err(AppError::internal_server_error)?;
    Ok(Json(response(goal, stages)))
}

pub async fn create(
    auth: Session, Path(conversation_id): Path<i64>, Json(payload): Json<CreateGoalRequest>,
) -> Result<Json<Value>, AppError> {
    let user_id = auth.storage_scope();
    let goal = assistant_store::create_goal(
        conversation_id,
        user_id,
        &payload.prompt,
        payload.max_turns.unwrap_or(50),
        payload.max_runtime_secs.unwrap_or(24 * 60 * 60),
    )
    .await
    .map_err(|err| {
        if err.to_string().contains("已有正在运行") || err.to_string().contains("不能为空") || err.to_string().contains("过长") {
            AppError::conflict(err)
        } else {
            AppError::internal_server_error(err)
        }
    })?;
    let stages = assistant_store::list_stages(goal.id, user_id)
        .await
        .map_err(AppError::internal_server_error)?;
    Ok(Json(response(goal, stages)))
}

pub async fn update(
    auth: Session, Path(conversation_id): Path<i64>, Json(payload): Json<UpdateGoalRequest>,
) -> Result<Json<Value>, AppError> {
    let user_id = auth.storage_scope();
    let Some(current) = assistant_store::get_active_goal(conversation_id, user_id)
        .await
        .map_err(AppError::internal_server_error)?
        .or(assistant_store::get_goal(conversation_id, user_id).await.map_err(AppError::internal_server_error)?) else {
        return Err(AppError::not_found("Goal not found"));
    };
    let status = match payload.action.as_str() {
        "pause" => "paused",
        "resume" => "running",
        "stop" | "cancel" => "cancelled",
        "complete" => "completed",
        _ => return Err(AppError::bad_request("不支持的 Goal 操作")),
    };
    let Some(goal) = assistant_store::set_goal_status(conversation_id, current.id, user_id, status)
        .await
        .map_err(AppError::internal_server_error)? else {
        return Err(AppError::conflict("Goal 已经结束或状态已变化"));
    };
    let stages = assistant_store::list_stages(goal.id, user_id)
        .await
        .map_err(AppError::internal_server_error)?;
    Ok(Json(response(goal, stages)))
}
