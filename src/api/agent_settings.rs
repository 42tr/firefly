use axum::{
    extract::DefaultBodyLimit,
    http::{header, HeaderMap},
    routing::get,
    Json, Router,
};
use serde_json::{json, Value};

use super::{auth::Session, error::AppError};
use crate::{
    agent_settings::{self, Settings},
    db, mcp,
};

pub fn routes() -> Router {
    Router::new()
        .route("/api/assistant/agent-settings", get(read).put(update))
        .route("/api/assistant/agent-settings/tools", get(tools))
        .layer(DefaultBodyLimit::max(1024 * 1024))
}

fn no_cache(value: Value) -> (HeaderMap, Json<Value>) {
    let mut headers = HeaderMap::new();
    headers.insert(header::CACHE_CONTROL, "no-store".parse().unwrap());
    (headers, Json(value))
}

async fn read(_: Session) -> Result<(HeaderMap, Json<Value>), AppError> {
    Ok(no_cache(Settings::load(db::pool()).await?.public()))
}

async fn update(
    _: Session,
    headers: HeaderMap,
    Json(settings): Json<Settings>,
) -> Result<(HeaderMap, Json<Value>), AppError> {
    super::auth::check_origin(&headers)?;
    settings.validate().map_err(AppError::bad_request)?;
    let saved = settings
        .save(db::pool())
        .await?
        .ok_or_else(|| AppError::conflict("配置已被其他页面修改，请重新读取后再保存"))?;
    Ok(no_cache(saved.public()))
}

async fn tools(session: Session) -> Result<(HeaderMap, Json<Value>), AppError> {
    let available = agent_settings::available_tools(mcp::ToolContext {
        user_id: session.storage_scope().into(),
    })
    .await;
    Ok(no_cache(
        json!({"tools": agent_settings::catalog(&available)}),
    ))
}
