use super::*;

#[derive(Debug, Serialize)]
pub struct MemoryResponse {
    pub id: i64,
    pub title: String,
    pub content: String,
    pub enabled: bool,
    pub source: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

impl From<assistant_store::MemoryRecord> for MemoryResponse {
    fn from(record: assistant_store::MemoryRecord) -> Self {
        Self {
            id: record.id,
            title: record.title,
            content: record.content,
            enabled: record.enabled,
            source: record.source,
            created_at: record.created_at,
            updated_at: record.updated_at,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct MemoryListResponse {
    pub memories: Vec<MemoryResponse>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateMemoryRequest {
    pub title: String,
    pub content: String,
    pub enabled: Option<bool>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateMemoryRequest {
    pub title: Option<String>,
    pub content: Option<String>,
    pub enabled: Option<bool>,
}

use assistant_store::{MAX_MEMORY_TITLE_LEN, MAX_MEMORY_CONTENT_LEN, MAX_MEMORY_COUNT};

fn validate_memory_fields(title: &str, content: &str) -> Result<(String, String), AppError> {
    let title = title.trim().to_string();
    let content = content.trim().to_string();
    if title.is_empty() {
        return Err(AppError::bad_request("记忆标题不能为空"));
    }
    if content.is_empty() {
        return Err(AppError::bad_request("记忆内容不能为空"));
    }
    if title.chars().count() > MAX_MEMORY_TITLE_LEN {
        return Err(AppError::bad_request(format!("记忆标题不能超过 {} 字", MAX_MEMORY_TITLE_LEN)));
    }
    if content.chars().count() > MAX_MEMORY_CONTENT_LEN {
        return Err(AppError::bad_request(format!("记忆内容不能超过 {} 字", MAX_MEMORY_CONTENT_LEN)));
    }
    Ok((title, content))
}

pub async fn list_memories(auth_user: Session) -> Result<Json<MemoryListResponse>, AppError> {
    let records = assistant_store::list_memories(auth_user.storage_scope()).await.map_err(AppError::internal_server_error)?;
    Ok(Json(MemoryListResponse { memories: records.into_iter().map(MemoryResponse::from).collect() }))
}

pub async fn create_memory(
    auth_user: Session, Json(payload): Json<CreateMemoryRequest>,
) -> Result<Json<MemoryResponse>, AppError> {
    let (title, content) = validate_memory_fields(&payload.title, &payload.content)?;
    let existing = assistant_store::list_memories(auth_user.storage_scope()).await.map_err(AppError::internal_server_error)?;
    if existing.len() >= MAX_MEMORY_COUNT {
        return Err(AppError::bad_request(format!("记忆数量已达上限（{} 条）", MAX_MEMORY_COUNT)));
    }
    let record = assistant_store::create_memory(auth_user.storage_scope(), &title, &content, payload.enabled.unwrap_or(true))
        .await
        .map_err(AppError::internal_server_error)?;
    crate::assistant_memory::refresh(&record).await;
    Ok(Json(MemoryResponse::from(record)))
}

pub async fn update_memory(
    auth_user: Session, Path(memory_id): Path<i64>, Json(payload): Json<UpdateMemoryRequest>,
) -> Result<Json<MemoryResponse>, AppError> {
    let title = payload.title.map(|t| validate_memory_fields(&t, "x").map(|(t, _)| t)).transpose()?;
    let content = payload.content.map(|c| validate_memory_fields("x", &c).map(|(_, c)| c)).transpose()?;
    let record = assistant_store::update_memory(
        memory_id,
        auth_user.storage_scope(),
        title.as_deref(),
        content.as_deref(),
        payload.enabled,
    )
    .await
    .map_err(AppError::internal_server_error)?
    .ok_or(AppError::not_found("记忆不存在"))?;
    crate::assistant_memory::refresh(&record).await;
    Ok(Json(MemoryResponse::from(record)))
}

pub async fn delete_memory(auth_user: Session, Path(memory_id): Path<i64>) -> Result<Json<Value>, AppError> {
    let deleted =
        assistant_store::delete_memory(memory_id, auth_user.storage_scope()).await.map_err(AppError::internal_server_error)?;
    if !deleted {
        return Err(AppError::not_found("记忆不存在"));
    }
    Ok(Json(json!({ "ok": true })))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{body::{to_bytes, Body}, http::{Request, StatusCode}, routing::{get, patch}, Router};
    use tower::ServiceExt;

    #[tokio::test]
    async fn memory_api_requires_user_session_and_keeps_sources_server_controlled() {
        crate::config::init_test_config();
        crate::db::set_test_pool(crate::db::test_pool().await);
        let app = Router::new()
            .route("/memories", get(list_memories).post(create_memory))
            .route("/memories/{id}", patch(update_memory).delete(delete_memory));
        let token = crate::api::jwt::create(&crate::api::jwt::Claims::new(1)).unwrap();
        let request = |method: &str, path: &str, body: Value, authenticated: bool| {
            let mut request = Request::builder().method(method).uri(path).header("content-type", "application/json");
            if authenticated {
                request = request.header("cookie", format!("firefly_session={token}"));
            }
            request.body(Body::from(body.to_string())).unwrap()
        };
        for (method, path) in [("POST", "/memories"), ("PATCH", "/memories/1"), ("DELETE", "/memories/1")] {
            let response = app.clone().oneshot(request(method, path, json!({"title":"标题","content":"内容"}), false)).await.unwrap();
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        }
        let forged = app.clone().oneshot(request("POST", "/memories", json!({"title":"标题","content":"内容","source":"agent"}), true)).await.unwrap();
        assert_eq!(forged.status(), StatusCode::UNPROCESSABLE_ENTITY);
        let created = app.clone().oneshot(request("POST", "/memories", json!({"title":"标题","content":"内容"}), true)).await.unwrap();
        assert_eq!(created.status(), StatusCode::OK);
        let body: Value = serde_json::from_slice(&to_bytes(created.into_body(), usize::MAX).await.unwrap()).unwrap();
        assert_eq!(body["source"], "manual");
        let id = body["id"].as_i64().unwrap();
        let path = format!("/memories/{id}");
        let forged = app.clone().oneshot(request("PATCH", &path, json!({"source":"agent","content":"覆盖"}), true)).await.unwrap();
        assert_eq!(forged.status(), StatusCode::UNPROCESSABLE_ENTITY);
        let agent = assistant_store::create_reviewed_memory(crate::db::SINGLE_SCOPE, "自动标题", "自动内容",
            &assistant_store::list_memories(crate::db::SINGLE_SCOPE).await.unwrap()).await.unwrap().unwrap();
        let listed = app.clone().oneshot(request("GET", "/memories", json!(null), true)).await.unwrap();
        assert_eq!(listed.status(), StatusCode::OK);
        let body: Value = serde_json::from_slice(&to_bytes(listed.into_body(), usize::MAX).await.unwrap()).unwrap();
        let memories = body["memories"].as_array().unwrap();
        assert_eq!(memories.len(), 2);
        assert!(memories.iter().any(|m| m["id"] == id && m["source"] == "manual"));
        assert!(memories.iter().any(|m| m["id"] == agent.id && m["source"] == "agent"));
        for (id, source) in [(id, "manual"), (agent.id, "agent")] {
            let path = format!("/memories/{id}");
            let updated = app.clone().oneshot(request("PATCH", &path, json!({"content":"手动编辑","enabled":false}), true)).await.unwrap();
            assert_eq!(updated.status(), StatusCode::OK);
            let body: Value = serde_json::from_slice(&to_bytes(updated.into_body(), usize::MAX).await.unwrap()).unwrap();
            assert_eq!(body["content"], "手动编辑");
            assert_eq!(body["enabled"], false);
            assert_eq!(body["source"], source);
            let deleted = app.clone().oneshot(request("DELETE", &path, json!(null), true)).await.unwrap();
            assert_eq!(deleted.status(), StatusCode::OK);
        }
    }
}
