use super::{auth::Session, error::AppError};
use crate::modules;
use axum::{
    extract::{DefaultBodyLimit, Path},
    http::{header, HeaderMap},
    response::{IntoResponse, Response},
    routing::{get, post, put},
    Json, Router,
};
use serde::Deserialize;
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

pub fn routes() -> Router {
    Router::new()
        .route("/api/modules", get(list))
        .route("/api/modules/reload", post(reload))
        .route("/api/modules/{id}", put(update))
        .route("/api/modules/{id}/tools", get(tools))
        .route("/api/modules/{id}/indicator", get(indicator))
        .route("/api/modules/{id}/call", post(call))
        .route("/api/modules/{id}/runs", get(runs))
        .route("/api/modules/{id}/pages/{page}/open", post(open_page))
        .route("/api/modules/{id}/pages/{page}/html", get(page_html))
        .layer(DefaultBodyLimit::max(8 * 1024 * 1024))
}
fn failure(error: anyhow::Error) -> AppError {
    AppError::bad_request(error.to_string())
}
async fn list(_: Session) -> Result<Json<Value>, AppError> {
    Ok(Json(modules::get()?.list().await))
}
async fn reload(_: Session, headers: HeaderMap) -> Result<Json<Value>, AppError> {
    super::auth::check_origin(&headers)?;
    modules::get()?.reload().await.map_err(failure)?;
    Ok(Json(modules::get()?.list().await))
}
async fn update(
    _: Session,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(input): Json<modules::Update>,
) -> Result<Json<Value>, AppError> {
    super::auth::check_origin(&headers)?;
    modules::get()?.update(&id, input).await.map_err(failure)?;
    Ok(Json(modules::get()?.list().await))
}
async fn tools(_: Session, Path(id): Path<String>) -> Result<Json<Value>, AppError> {
    let module = modules::get()?.module(&id).await.map_err(failure)?;
    Ok(Json(
        json!({"tools":module.tools().await.map_err(failure)?}),
    ))
}
async fn indicator(_: Session, Path(id): Path<String>) -> Result<Json<Value>, AppError> {
    let module = modules::get()?.module(&id).await.map_err(failure)?;
    Ok(Json(module.indicator().await.map_err(failure)?))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Call {
    name: String,
    arguments: Value,
}
async fn call(
    _: Session,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(input): Json<Call>,
) -> Result<Json<Value>, AppError> {
    super::auth::check_origin(&headers)?;
    let module = modules::get()?.module(&id).await.map_err(failure)?;
    Ok(Json(
        module
            .call(
                &input.name,
                input.arguments,
                "app",
                CancellationToken::new(),
            )
            .await
            .map_err(failure)?,
    ))
}
async fn runs(_: Session, Path(id): Path<String>) -> Result<Json<Value>, AppError> {
    Ok(Json(modules::get()?.runs(&id).await?))
}
async fn open_page(
    _: Session,
    headers: HeaderMap,
    Path((id, page)): Path<(String, String)>,
) -> Result<Json<Value>, AppError> {
    super::auth::check_origin(&headers)?;
    let module = modules::get()?.module(&id).await.map_err(failure)?;
    let definition = module
        .manifest
        .pages
        .iter()
        .find(|p| p.id == page)
        .ok_or_else(|| AppError::not_found("模块页面不存在"))?;
    let result = module
        .call(
            &definition.open_tool,
            definition.arguments.clone(),
            "app",
            CancellationToken::new(),
        )
        .await
        .map_err(failure)?;
    if result["isError"] == true {
        return Err(AppError::bad_request(
            "模块页面入口执行失败，请在工具测试中查看结果",
        ));
    }
    Ok(Json(
        json!({"result":result,"arguments":definition.arguments}),
    ))
}
async fn page_html(
    _: Session,
    Path((id, page)): Path<(String, String)>,
) -> Result<Response, AppError> {
    let module = modules::get()?.module(&id).await.map_err(failure)?;
    let (_, html) = module.page_resource(&page).await.map_err(failure)?;
    let mut response = html.into_response();
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        "text/html; charset=utf-8".parse().unwrap(),
    );
    headers.insert(header::CACHE_CONTROL, "no-store".parse().unwrap());
    headers.insert(header::CONTENT_SECURITY_POLICY, "default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; img-src data: blob:; font-src data:; connect-src 'none'; frame-src 'none'; object-src 'none'; base-uri 'none'; form-action 'none'; frame-ancestors 'self'; sandbox allow-scripts allow-forms allow-downloads".parse().unwrap());
    headers.insert(header::X_FRAME_OPTIONS, "SAMEORIGIN".parse().unwrap());
    Ok(response)
}
