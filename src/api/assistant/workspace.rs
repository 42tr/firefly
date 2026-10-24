use anyhow::anyhow;
use tokio_util::io::ReaderStream;

use super::*;

pub async fn retry_parallel_workspace(auth_user: Session, Path(id): Path<i64>) -> Result<Json<Value>, AppError> {
    if assistant_job::global_registry().find_active_by_conversation(id).is_some() {
        return Err(AppError::conflict("目录准备任务尚未退出，请稍后重试"));
    }
    if !assistant_store::retry_workspace(id, auth_user.storage_scope()).await.map_err(AppError::internal_server_error)? {
        return Err(AppError::conflict("当前会话不能重试目录准备，或任务仍在运行"));
    }
    kick_conversation_queue(id, auth_user.storage_scope()).await.map_err(AppError::internal_server_error)?;
    Ok(Json(json!({"ok": true})))
}

pub async fn list_workspace(
    _auth_user: Session, Query(query): Query<WorkspaceQuery>,
) -> Result<Json<codex::WorkspaceListing>, AppError> {
    let mut listing =
        codex::list_workspace(query.path.as_deref()).await.map_err(|err| AppError::bad_request(err.to_string()))?;
    listing.entries.retain(|entry| entry.name != ".assistant-parallel");
    Ok(Json(listing))
}

pub async fn codex_providers(_auth_user: Session) -> Json<Value> {
    let mut listing = codex::list_model_providers();
    let (_, official_models) =
        tokio::join!(codex::hydrate_provider_models(&mut listing), codex::list_official_models(),);
    let (official_models, official_models_error) = match official_models {
        Ok(models) => (models, None),
        Err(err) => {
            tracing::warn!(error = %err, "读取 Codex 官方模型列表失败");
            (Vec::new(), Some("读取模型列表失败，可手动输入模型名称或使用默认模型"))
        }
    };
    Json(json!({
        "default": listing.default,
        "default_model": listing.default_model,
        "default_reasoning_level": listing.default_reasoning_level,
        "providers": listing.providers,
        "official_models": official_models.iter().map(|model| &model.model).collect::<Vec<_>>(),
        "model_reasoning": official_models,
        "official_models_error": official_models_error,
    }))
}

pub async fn claude_models(_auth_user: Session) -> Result<Json<Value>, AppError> {
    let cfg = config::get();
    let listing = claude::providers().map_err(AppError::internal_server_error)?;
    let defaults = claude::cli_defaults(&cfg).map_err(AppError::internal_server_error)?;
    Ok(Json(json!({
        "default": "cli",
        "providers": listing.providers,
        "default_model": defaults.model,
        "cli_default_model": defaults.model,
        "default_reasoning_level": defaults.reasoning_level,
        "models": defaults.models,
        "models_error": listing.warnings.join("；"),
        "reasoning_levels": claude::REASONING_LEVELS,
    })))
}

pub async fn create_workspace_directory(
    _auth_user: Session, Json(payload): Json<CreateWorkspaceDirectoryRequest>,
) -> Result<Json<codex::WorkspaceEntry>, AppError> {
    let entry = codex::create_workspace_directory(payload.path.as_deref(), &payload.name)
        .await
        .map_err(|err| AppError::bad_request(err.to_string()))?;
    Ok(Json(entry))
}

pub async fn download_workspace_file(
    _auth_user: Session, Query(query): Query<WorkspaceFileQuery>,
) -> Result<Response, AppError> {
    if query.path.starts_with("/tmp/") {
        let file = open_temporary_image(&query.path)
            .await
            .map_err(|err| AppError::not_found(err.to_string()))?;
        let stream = ReaderStream::new(file);
        let mut response = workspace_file_response(Body::from_stream(stream), &query.path);
        response.headers_mut().insert(header::CACHE_CONTROL, "no-store".parse().unwrap());
        response.headers_mut().insert(header::X_CONTENT_TYPE_OPTIONS, "nosniff".parse().unwrap());
        return Ok(response);
    }
    let (path, _relative_path) =
        codex::resolve_workspace_file(&query.path).await.map_err(|err| AppError::not_found(err.to_string()))?;
    let file = fs::File::open(&path).await.map_err(|_| AppError::not_found("workspace 文件暂不可用"))?;
    let stream = tokio_util::io::ReaderStream::new(file);
    Ok(workspace_file_response(Body::from_stream(stream), &path))
}

/// Workspace text files are UTF-8. Declare the encoding for inline browser
/// previews instead of letting the browser infer a legacy encoding from locale.
fn workspace_file_response(body: Body, path: impl AsRef<std::path::Path>) -> Response {
    let path = path.as_ref();
    let mut response = Response::new(body);
    let mime = mime_guess::from_path(path).first_or_octet_stream();
    let content_type =
        if mime.type_().as_str() == "text" { format!("{mime}; charset=utf-8") } else { mime.to_string() };
    let file_name = path.file_name().and_then(|name| name.to_str()).unwrap_or("file");
    let active_content = matches!(
        (mime.type_().as_str(), mime.subtype().as_str()),
        ("text", "html" | "javascript" | "ecmascript") | ("image", "svg+xml")
    );
    response
        .headers_mut()
        .insert(header::CONTENT_TYPE, office::header_value_or_default(&content_type, "application/octet-stream"));
    response.headers_mut().insert(
        header::CONTENT_DISPOSITION,
        office::build_disposition_header(if active_content { "attachment" } else { "inline" }, file_name),
    );
    // Workspace files may come from an untrusted repository. Keep any document
    // opened directly from this endpoint in an opaque sandboxed origin.
    response.headers_mut().insert(header::CONTENT_SECURITY_POLICY, "sandbox".parse().unwrap());
    response
}

/// Open and validate the same descriptor that the response will stream.
async fn open_temporary_image(path: &str) -> anyhow::Result<tokio::fs::File> {
    let relative = path.strip_prefix("/tmp/").ok_or_else(|| anyhow!("临时图片路径非法"))?;
    codex::validate_relative_path(relative)?;
    let relative = relative.to_owned();
    let file = tokio::task::spawn_blocking(move || {
        let file = crate::workspace_files::open_path(std::path::Path::new("/tmp"), &relative)?;
        crate::workspace_files::check_image(&file, &relative)?;
        Ok::<_, anyhow::Error>(file)
    }).await??;
    Ok(tokio::fs::File::from_std(file))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn temporary_image_stream_preserves_bytes_and_rejects_non_images() {
        use futures_util::TryStreamExt;

        let path = format!("/tmp/assistant-image-test-{}.png", uuid::Uuid::new_v4());
        let bytes = b"\x89PNG\r\n\x1a\nimage data";
        tokio::fs::write(&path, bytes).await.unwrap();
        let file = super::open_temporary_image(&path).await.unwrap();
        let chunks: Vec<_> = ReaderStream::new(file).try_collect().await.unwrap();
        assert_eq!(chunks.concat(), bytes);
        tokio::fs::write(&path, b"not an image").await.unwrap();
        assert!(super::open_temporary_image(&path).await.is_err());
        tokio::fs::remove_file(&path).await.unwrap();
        for path in ["/etc/passwd", "/tmp/../etc/passwd", "/tmp-other/image.png"] {
            assert!(super::open_temporary_image(path).await.is_err());
        }
    }

    #[tokio::test]
    async fn workspace_text_preview_declares_utf8_and_preserves_chinese_bytes() {
        let text = "Primary request: 白发红瞳的女主角，全身立绘。";
        for path in ["game_spirits/output/imagegen/white-haired-heroine-v1.prompt.txt", "remote/notes.md"] {
            let response = workspace_file_response(Body::from(text), path);
            let content_type = response.headers()[header::CONTENT_TYPE].to_str().unwrap();
            assert!(content_type.starts_with("text/"));
            assert!(content_type.ends_with("; charset=utf-8"));
            assert!(response.headers()[header::CONTENT_DISPOSITION].to_str().unwrap().starts_with("inline;"));
            let bytes = axum::body::to_bytes(response.into_body(), 1024).await.unwrap();
            assert_eq!(bytes.as_ref(), text.as_bytes());
        }
    }

    #[tokio::test]
    async fn workspace_binary_preview_preserves_mime_and_bytes() {
        for (path, mime) in [("output/image.png", "image/png"), ("output/data.unknown", "application/octet-stream")] {
            let bytes = vec![0x89, 0x50, 0x00, 0xff];
            let response = workspace_file_response(Body::from(bytes.clone()), path);
            assert_eq!(response.headers()[header::CONTENT_TYPE], mime);
            assert_eq!(axum::body::to_bytes(response.into_body(), 1024).await.unwrap().as_ref(), bytes);
        }
    }

    #[test]
    fn active_workspace_documents_download_and_are_sandboxed() {
        let response = workspace_file_response(Body::from("<script>alert(1)</script>"), "index.html");
        assert!(response.headers()[header::CONTENT_DISPOSITION].to_str().unwrap().starts_with("attachment;"));
        assert_eq!(response.headers()[header::CONTENT_SECURITY_POLICY], "sandbox");
    }
}
