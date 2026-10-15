use axum::{http::StatusCode, response::Json};
use serde_json::json;

use crate::db;

pub async fn health_check() -> (StatusCode, Json<serde_json::Value>) {
    match db_check().await {
        Ok(()) => (
            StatusCode::OK,
            Json(json!({
                "status": "ok",
                "db": "connected",
            })),
        ),
        Err(err) => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({
                "status": "error",
                "db": "disconnected",
                "error": err.to_string(),
            })),
        ),
    }
}

async fn db_check() -> anyhow::Result<()> {
    let pool = db::pool();
    sqlx::query("SELECT 1").fetch_one(pool).await?;
    Ok(())
}
