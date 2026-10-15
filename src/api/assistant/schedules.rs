use super::*;
use assistant_store::schedules::{self as store, ScheduleInput};
use chrono::Utc;

pub async fn list(auth: Session) -> Result<Json<Value>, AppError> {
    let tasks = store::list(auth.storage_scope()).await.map_err(AppError::internal_server_error)?;
    Ok(Json(json!({"schedules": tasks, "timezone": config::timezone().name()})))
}

async fn validate(input: &mut ScheduleInput) -> Result<(), AppError> {
    input.validate(Utc::now()).map_err(AppError::bad_request)?;
    let settings = &input.execution;
    input.execution = prepare_conversation(&CreateConversationRequest {
        title: None, agent: Some(settings.agent.clone()), cwd: settings.cwd.clone(),
        workspace_mode: Some(settings.workspace_mode.clone()), provider: settings.provider.clone(),
        model: settings.model.clone(), reasoning_level: settings.reasoning_level.clone(),
    }).await?;
    Ok(())
}

async fn read(id: i64, user: &str) -> Result<Json<store::ScheduleRecord>, AppError> {
    Ok(Json(store::get(id, user).await.map_err(AppError::internal_server_error)?
        .ok_or_else(|| AppError::not_found("定时任务不存在"))?))
}

pub async fn create(auth: Session, Json(mut input): Json<ScheduleInput>) -> Result<Json<store::ScheduleRecord>, AppError> {
    validate(&mut input).await?;
    let id = store::save(None, auth.storage_scope(), &input, Utc::now()).await.map_err(|error| {
        if error.is::<store::ScheduleLimit>() { AppError::bad_request(error) } else { AppError::internal_server_error(error) }
    })?.ok_or_else(|| AppError::internal_server_error("定时任务未保存"))?;
    read(id, auth.storage_scope()).await
}

pub async fn update(auth: Session, Path(id): Path<i64>, Json(mut input): Json<ScheduleInput>) -> Result<Json<store::ScheduleRecord>, AppError> {
    let _ = read(id, auth.storage_scope()).await?;
    validate(&mut input).await?;
    store::save(Some(id), auth.storage_scope(), &input, Utc::now()).await.map_err(AppError::internal_server_error)?
        .ok_or_else(|| AppError::not_found("定时任务不存在"))?;
    read(id, auth.storage_scope()).await
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToggleRequest { enabled: bool }

pub async fn toggle(auth: Session, Path(id): Path<i64>, Json(input): Json<ToggleRequest>) -> Result<Json<store::ScheduleRecord>, AppError> {
    if !store::set_enabled(id, auth.storage_scope(), input.enabled, Utc::now()).await.map_err(AppError::internal_server_error)? {
        return Err(AppError::not_found("定时任务不存在"));
    }
    read(id, auth.storage_scope()).await
}

pub async fn delete(auth: Session, Path(id): Path<i64>) -> Result<Json<Value>, AppError> {
    if !store::delete(id, auth.storage_scope()).await.map_err(AppError::internal_server_error)? {
        return Err(AppError::not_found("定时任务不存在"));
    }
    Ok(Json(json!({"ok": true})))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PreviewRequest { cron: String }

pub async fn preview(_auth: Session, Json(input): Json<PreviewRequest>) -> Result<Json<Value>, AppError> {
    let mut at = Utc::now();
    let mut times = Vec::new();
    for _ in 0..3 {
        at = store::next_occurrence(&input.cron, at).map_err(AppError::bad_request)?;
        times.push(at);
    }
    Ok(Json(json!({"times": times, "timezone": config::timezone().name()})))
}

pub async fn tick() -> anyhow::Result<()> {
    let cfg = config::get();
    for task in store::due(Utc::now()).await? {
        match store::dispatch(task.id, &task.user_id, Utc::now(), &cfg.assistant_system_prompt,
            cfg.assistant_enable_tools, cfg.assistant_max_queued_jobs).await {
            Ok(Some(conversation)) => {
                kick_conversation_queue(conversation, &task.user_id).await?;
            }
            Ok(None) => {},
            Err(error) => warn!(schedule_id = task.id, "scheduled task dispatch failed; will retry: {error:#}"),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
