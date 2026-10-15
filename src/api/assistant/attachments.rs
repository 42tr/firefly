use super::*;
use sha2::{Digest, Sha256};

pub async fn download_attachment(
    auth_user: Session, Path(attachment_id): Path<i64>, request: axum::extract::Request,
) -> Result<Response, AppError> {
    let record = assistant_store::get_attachment_for_user(attachment_id, auth_user.storage_scope())
        .await
        .map_err(AppError::internal_server_error)?
        .ok_or(AppError::not_found("附件不存在或无权访问"))?;

    serve_attachment(record, request, false).await
}

pub async fn preview_uploaded_attachment(
    auth_user: Session, Path(file_id): Path<uuid::Uuid>, request: axum::extract::Request,
) -> Result<Response, AppError> {
    let record = assistant_store::get_attachment_by_file_for_user(file_id, auth_user.storage_scope())
        .await
        .map_err(AppError::internal_server_error)?
        .ok_or(AppError::not_found("附件不存在或无权访问"))?;
    serve_attachment(record, request, true).await
}

async fn serve_attachment(
    record: assistant_store::AttachmentRecord, request: axum::extract::Request, inline: bool,
) -> Result<Response, AppError> {
    let path = PathBuf::from(&record.storage_path);
    let content_type = attachment_content_type(&record.file_name, Some(&record.content_type));
    if content_type.starts_with("video/") {
        return serve_video_attachment(&path, &record.file_name, &content_type, request).await;
    }
    let file = fs::File::open(&path).await.map_err(|_| AppError::not_found("附件暂不可用，可能已被清理"))?;
    let stream = tokio_util::io::ReaderStream::new(file);
    let body = Body::from_stream(stream);

    let mut response = Response::new(body);
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        office::header_value_or_default(&content_type, "application/octet-stream"),
    );

    let disposition_value =
        office::build_disposition_header(if inline { "inline" } else { "attachment" }, &record.file_name);
    response.headers_mut().insert(header::CONTENT_DISPOSITION, disposition_value);
    response.headers_mut().insert(header::CACHE_CONTROL, "private, no-store".parse().unwrap());
    response.headers_mut().insert(header::X_CONTENT_TYPE_OPTIONS, "nosniff".parse().unwrap());
    if inline {
        response.headers_mut().insert(header::CONTENT_SECURITY_POLICY, "sandbox".parse().unwrap());
    }
    Ok(response)
}

// ServeFile implements byte ranges and HEAD so browsers can seek without fetching the whole video.
async fn serve_video_attachment(
    path: &std::path::Path, file_name: &str, content_type: &str, request: axum::extract::Request,
) -> Result<Response, AppError> {
    let mut response = tower_http::services::ServeFile::new(path)
        .try_call(request)
        .await
        .map_err(AppError::internal_server_error)?
        .map(Body::new);
    response
        .headers_mut()
        .insert(header::CONTENT_TYPE, office::header_value_or_default(content_type, "application/octet-stream"));
    response.headers_mut().insert(header::CONTENT_DISPOSITION, office::build_disposition_header("inline", file_name));
    response.headers_mut().insert(header::CACHE_CONTROL, "private, no-store".parse().unwrap());
    Ok(response)
}

fn attachment_content_type(file_name: &str, content_type: Option<&str>) -> String {
    let content_type = content_type.unwrap_or("application/octet-stream").to_ascii_lowercase();
    if content_type.is_empty() || content_type == "application/octet-stream" {
        if let Some(mime) = mime_guess::from_path(file_name).first() {
            if mime.type_() == "video" || mime.type_() == "image" {
                return mime.to_string();
            }
        }
    }
    content_type
}

pub(super) async fn parse_chat_multipart(multipart: &mut Multipart) -> Result<ParsedChatPayload, AppError> {
    let mut message = String::new();
    let mut enable_tools = None;
    let mut conversation_id = None;
    // Removes already-saved files if a later field fails or the request is dropped mid-upload.
    let mut attachments = AttachmentCleanup::new(Vec::new());
    let storage_dir = attachment_storage_dir().await.map_err(AppError::internal_server_error)?;
    let size_limit = config::get().assistant_max_attachment_bytes();

    while let Some(field) = multipart.next_field().await.map_err(|err| AppError::bad_request(err.to_string()))? {
        let name = field.name().unwrap_or_default().to_string();
        match name.as_str() {
            "message" => {
                message = field.text().await.map_err(|err| AppError::bad_request(err.to_string()))?;
            }
            "conversation_id" => {
                let raw = field.text().await.map_err(|err| AppError::bad_request(err.to_string()))?;
                let parsed =
                    raw.trim().parse::<i64>().map_err(|_| AppError::bad_request("conversation_id 必须是数字"))?;
                conversation_id = Some(parsed);
            }
            "enable_tools" => {
                let raw = field.text().await.map_err(|err| AppError::bad_request(err.to_string()))?;
                enable_tools = parse_bool_flag(&raw);
            }
            "attachments" | "attachments[]" => {
                let pending = save_multipart_attachment(field, &storage_dir, size_limit)
                    .await
                    .map_err(|err| AppError::bad_request(err.to_string()))?;
                attachments.entries.push(pending);
            }
            _ => {}
        }
    }

    attachments.disarm();
    Ok(ParsedChatPayload { message, enable_tools, conversation_id, attachments: std::mem::take(&mut attachments.entries) })
}

pub(super) fn is_multipart_content_type(value: &str) -> bool {
    value.to_ascii_lowercase().starts_with("multipart/form-data")
}

pub(super) fn ensure_payload_size_limit(headers: &HeaderMap) -> Result<(), AppError> {
    let size_limit = config::get().assistant_max_chat_upload_bytes();
    if let Some(len_header) = headers.get(header::CONTENT_LENGTH) {
        if let Ok(text) = len_header.to_str() {
            if let Ok(len) = text.parse::<u64>() {
                if len > size_limit {
                    let mb = size_limit / (1024 * 1024);
                    return Err(AppError::new(
                        StatusCode::PAYLOAD_TOO_LARGE,
                        format!("上传内容过大，最大支持 {} MB", mb),
                    ));
                }
            }
        }
    }
    Ok(())
}

fn parse_bool_flag(raw: &str) -> Option<bool> {
    match raw.trim().to_lowercase().as_str() {
        "true" | "1" | "yes" | "on" => Some(true),
        "false" | "0" | "no" | "off" => Some(false),
        _ => None,
    }
}

async fn save_multipart_attachment(
    field: Field<'_>, storage_dir: &std::path::Path, size_limit: i64,
) -> anyhow::Result<PendingAttachment> {
    let mut field = field;
    let file_name = attachment_utils::sanitize_file_name(field.file_name());
    let content_type = attachment_content_type(&file_name, field.content_type());
    let file_id = uuid::Uuid::new_v4().to_string();
    let path = storage_dir.join(&file_id);
    // Deletes the partial file on any error below or if the upload is aborted.
    let mut partial = PartialFile(Some(path.clone()));
    let mut file = fs::File::create(&path).await?;
    let mut written: i64 = 0;
    let mut digest = Sha256::new();

    while let Some(chunk) = field.chunk().await? {
        written += chunk.len() as i64;
        if written > size_limit {
            return Err(anyhow!(
                "单个{}大小不能超过 {}MB",
                if content_type.starts_with("video/") { "视频" } else { "附件" },
                size_limit / (1024 * 1024)
            ));
        }
        file.write_all(&chunk).await?;
        digest.update(&chunk);
    }
    // Tokio file writes may still be pending; finish them before exposing the attachment to a reader.
    file.flush().await?;

    partial.0 = None;
    Ok(PendingAttachment {
        file_name,
        content_type,
        file_size: written,
        storage_path: path.to_string_lossy().to_string(),
        sha256: format!("{:x}", digest.finalize()),
    })
}

struct PartialFile(Option<PathBuf>);

impl Drop for PartialFile {
    fn drop(&mut self) {
        if let Some(path) = self.0.take() {
            let _ = std::fs::remove_file(path);
        }
    }
}

async fn attachment_storage_dir() -> anyhow::Result<PathBuf> {
    let base = config::get().assistant_upload_dir.clone();
    let path = PathBuf::from(base);
    fs::create_dir_all(&path).await?;
    Ok(path)
}

/// Import only native Codex image results, never paths mentioned in assistant text.
pub(super) async fn save_codex_image(
    result: &Value, thread_id: &str, storage_dir: &std::path::Path,
) -> anyhow::Result<PendingAttachment> {
    use base64::Engine;
    let encoded = result["result"].as_str().unwrap_or_default();
    let bytes = if !encoded.is_empty() {
        anyhow::ensure!(encoded.len() <= (MAX_GENERATED_IMAGE_BYTES as usize).div_ceil(3) * 4, "生成图片超过附件大小限制");
        base64::engine::general_purpose::STANDARD.decode(encoded)?
    } else {
        let path = result["saved_path"].as_str().filter(|value| !value.is_empty())
            .ok_or_else(|| anyhow!("生图结果没有图片数据或保存路径"))?;
        let home = std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".codex"))
            .ok_or_else(|| anyhow!("无法定位 $HOME/.codex 生图目录"))?;
        read_codex_image(std::path::Path::new(path), thread_id, &home).await?
    };
    anyhow::ensure!(bytes.len() <= MAX_GENERATED_IMAGE_BYTES as usize, "生成图片超过附件大小限制");
    let (extension, content_type) = match image::guess_format(&bytes)? {
        image::ImageFormat::Png => ("png", "image/png"),
        image::ImageFormat::Jpeg => ("jpg", "image/jpeg"),
        image::ImageFormat::WebP => ("webp", "image/webp"),
        _ => return Err(anyhow!("不支持的生成图片格式")),
    };
    fs::create_dir_all(storage_dir).await?;
    let path = storage_dir.join(uuid::Uuid::new_v4().to_string());
    let mut partial = PartialFile(Some(path.clone()));
    fs::write(&path, &bytes).await?;
    let attachment = PendingAttachment {
        file_name: format!("generated-image.{extension}"), content_type: content_type.into(),
        file_size: bytes.len() as i64, storage_path: path.to_string_lossy().into_owned(),
        sha256: format!("{:x}", Sha256::digest(&bytes)),
    };
    partial.0 = None;
    Ok(attachment)
}

async fn read_codex_image(path: &std::path::Path, thread_id: &str, home: &std::path::Path) -> anyhow::Result<Vec<u8>> {
    use tokio::io::AsyncReadExt;
    uuid::Uuid::parse_str(thread_id)?;
    let home = fs::canonicalize(home).await?;
    let root = fs::canonicalize(home.join("generated_images").join(thread_id)).await?;
    anyhow::ensure!(root.starts_with(&home), "生图目录越出 $HOME/.codex");
    let path = fs::canonicalize(path).await?;
    anyhow::ensure!(path.starts_with(&root), "生图文件越出当前 Codex 会话目录");
    // Descriptor traversal also refuses link replacement after canonicalization.
    let relative = path.strip_prefix(&home)?.to_str().ok_or_else(|| anyhow!("生图文件路径不是 UTF-8"))?.to_owned();
    let file = tokio::task::spawn_blocking(move || crate::workspace_files::open_path(&home, &relative)).await??;
    let file = fs::File::from_std(file);
    let mut bytes = Vec::new();
    file.take(MAX_GENERATED_IMAGE_BYTES as u64 + 1).read_to_end(&mut bytes).await?;
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn saved_codex_images_are_limited_to_the_current_thread_in_the_home_directory() {
        let directory = tempfile::tempdir().unwrap();
        let home = directory.path().join(".codex");
        let thread = uuid::Uuid::new_v4().to_string();
        let other = uuid::Uuid::new_v4().to_string();
        let root = home.join("generated_images").join(&thread);
        let other_root = home.join("generated_images").join(&other);
        fs::create_dir_all(&root).await.unwrap();
        fs::create_dir_all(&other_root).await.unwrap();
        let image = root.join("image.png");
        let bytes = b"\x89PNG\r\n\x1a\nimage data";
        fs::write(&image, bytes).await.unwrap();
        assert_eq!(read_codex_image(&image, &thread, &home).await.unwrap(), bytes);
        let outside = directory.path().join("image.png");
        fs::write(&outside, bytes).await.unwrap();
        let other_image = other_root.join("image.png");
        fs::write(&other_image, bytes).await.unwrap();
        for path in [&outside, &other_image] {
            assert!(read_codex_image(path, &thread, &home).await.is_err());
        }
        std::os::unix::fs::symlink(&outside, root.join("escape.png")).unwrap();
        assert!(read_codex_image(&root.join("escape.png"), &thread, &home).await.is_err());
        assert!(read_codex_image(&image, "../other", &home).await.is_err());
        let escaped_thread = uuid::Uuid::new_v4().to_string();
        std::os::unix::fs::symlink(directory.path(), home.join("generated_images").join(&escaped_thread)).unwrap();
        assert!(read_codex_image(&outside, &escaped_thread, &home).await.is_err());
    }

    #[tokio::test]
    async fn extensionless_uploaded_image_preview_uses_metadata_and_inline_response() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join(uuid::Uuid::new_v4().to_string());
        let bytes = b"\x89PNG\r\n\x1a\n";
        fs::write(&path, bytes).await.unwrap();
        for (mime, expected) in [("image/png", "image/png"), ("application/octet-stream", "image/png")] {
            let record = assistant_store::AttachmentRecord {
                id: 1, message_id: 1, file_name: "图片.png".into(), content_type: mime.into(),
                file_size: bytes.len() as i64, storage_path: path.to_string_lossy().into_owned(), created_at: chrono::Utc::now(),
            };
            let response = serve_attachment(record, axum::extract::Request::new(Body::empty()), true).await.unwrap();
            assert_eq!(response.headers()[header::CONTENT_TYPE], expected);
            assert!(response.headers()[header::CONTENT_DISPOSITION].as_bytes().starts_with(b"inline;"));
            assert_eq!(response.headers()[header::CACHE_CONTROL], "private, no-store");
            assert_eq!(response.headers()[header::X_CONTENT_TYPE_OPTIONS], "nosniff");
            assert_eq!(axum::body::to_bytes(response.into_body(), 1024).await.unwrap().as_ref(), bytes);
        }
    }

    #[test]
    fn video_mime_fallback() {
        for name in ["clip.MP4", "clip.mov", "clip.webm", "clip.mkv", "clip.avi", "clip.m4v", "clip.ogv", "clip.3gp"] {
            let mime = attachment_content_type(name, Some("application/octet-stream"));
            assert!(mime.starts_with("video/"), "{name}: {mime}");
        }
        assert_eq!(attachment_content_type("clip.mp4", Some("text/plain")), "text/plain");
    }

    #[tokio::test]
    async fn multipart_enforces_configured_limit_for_regular_files_and_videos() {
        use tower::{Layer, ServiceExt};
        let directory = tempfile::tempdir().unwrap();
        for (limit_mb, size, accepted) in [
            (100, 100 * 1024 * 1024, true),
            (100, 100 * 1024 * 1024 + 1, false),
            (1, 1024 * 1024, true),
            (1, 1024 * 1024 + 1, false),
            (2, 1024 * 1024 + 1, true),
        ] {
            let config: config::Config = serde_json::from_value(json!({
                "jwt_secret": "test", "auth_password": "test", "assistant_max_attachment_mb": limit_mb,
            })).unwrap();
            for (name, expected_type) in [("demo.html", "application/octet-stream"), ("recording.mp4", "video/mp4")] {
                let prefix = format!(
                    "--boundary\r\nContent-Disposition: form-data; name=\"attachments[]\"; filename=\"{name}\"\r\nContent-Type: application/octet-stream\r\n\r\n"
                ).into_bytes();
                // Stream repeated chunks instead of allocating a 100MB request body for each boundary case.
                let chunk = bytes::Bytes::from(vec![b'x'; 1024 * 1024]);
                let mut chunks = vec![Ok::<_, Infallible>(bytes::Bytes::from(prefix))];
                for _ in 0..size / chunk.len() {
                    chunks.push(Ok(chunk.clone()));
                }
                if size % chunk.len() > 0 {
                    chunks.push(Ok(chunk.slice(..size % chunk.len())));
                }
                chunks.push(Ok(bytes::Bytes::from_static(b"\r\n--boundary--\r\n")));
                let request = axum::http::Request::builder()
                    .header(header::CONTENT_TYPE, "multipart/form-data; boundary=boundary")
                    .body(Body::from_stream(futures_util::stream::iter(chunks)))
                    .unwrap();
                let request = axum::extract::DefaultBodyLimit::max(config.assistant_max_chat_upload_bytes() as usize)
                    .layer(tower::service_fn(|request: axum::extract::Request| async move { Ok::<_, Infallible>(request) }))
                    .oneshot(request)
                    .await
                    .unwrap();
                let mut multipart = Multipart::from_request(request, &()).await.unwrap();
                let field = multipart.next_field().await.unwrap().unwrap();
                let result = save_multipart_attachment(field, directory.path(), config.assistant_max_attachment_bytes()).await;
                if accepted {
                    let attachment = result.unwrap();
                    assert_eq!(attachment.content_type, expected_type);
                    assert_eq!(attachment.file_size, size as i64);
                    assert_eq!(fs::metadata(&attachment.storage_path).await.unwrap().len(), size as u64);
                    fs::remove_file(attachment.storage_path).await.unwrap();
                } else {
                    assert!(result.err().unwrap().to_string().contains(&format!("{limit_mb}MB")));
                    assert!(fs::read_dir(directory.path()).await.unwrap().next_entry().await.unwrap().is_none());
                }
            }
        }
    }

    #[tokio::test]
    async fn interrupted_upload_leaves_no_partial_file() {
        let directory = std::env::temp_dir().join(uuid::Uuid::new_v4().to_string());
        fs::create_dir(&directory).await.unwrap();
        // The body ends mid-file without a closing boundary, as when the client disconnects.
        let mut body = b"--boundary\r\nContent-Disposition: form-data; name=\"attachments[]\"; filename=\"a.txt\"\r\n\r\n".to_vec();
        body.extend(std::iter::repeat_n(b'x', 64 * 1024));
        let request = axum::http::Request::builder()
            .header(header::CONTENT_TYPE, "multipart/form-data; boundary=boundary")
            .body(Body::from(body))
            .unwrap();
        let mut multipart = Multipart::from_request(request, &()).await.unwrap();
        let field = multipart.next_field().await.unwrap().unwrap();
        assert!(save_multipart_attachment(field, &directory, 100 * 1024 * 1024).await.is_err());
        assert!(fs::read_dir(&directory).await.unwrap().next_entry().await.unwrap().is_none());
        fs::remove_dir(directory).await.unwrap();
    }

    #[tokio::test]
    async fn video_response_supports_ranges_head_and_missing_files() {
        let path = std::env::temp_dir().join(uuid::Uuid::new_v4().to_string());
        fs::write(&path, b"0123456789").await.unwrap();
        for (method, range, status, expected) in [
            ("GET", None, StatusCode::OK, "0123456789"),
            ("GET", Some("bytes=2-5"), StatusCode::PARTIAL_CONTENT, "2345"),
            ("GET", Some("bytes=-3"), StatusCode::PARTIAL_CONTENT, "789"),
            ("GET", Some("bytes=20-"), StatusCode::RANGE_NOT_SATISFIABLE, ""),
            ("HEAD", None, StatusCode::OK, ""),
        ] {
            let mut request = axum::http::Request::builder().method(method);
            if let Some(range) = range {
                request = request.header(header::RANGE, range);
            }
            let response = serve_video_attachment(&path, "视频.mp4", "video/mp4", request.body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(response.status(), status);
            assert_eq!(response.headers()[header::CONTENT_TYPE], "video/mp4");
            assert!(response.headers()[header::CONTENT_DISPOSITION].as_bytes().starts_with(b"inline;"));
            if range == Some("bytes=2-5") {
                assert_eq!(response.headers()[header::CONTENT_RANGE], "bytes 2-5/10");
                assert_eq!(response.headers()[header::CONTENT_LENGTH], "4");
            }
            let body = axum::body::to_bytes(response.into_body(), 100).await.unwrap();
            assert_eq!(body.as_ref(), expected.as_bytes());
        }
        fs::remove_file(&path).await.unwrap();
        let response = serve_video_attachment(&path, "clip.mp4", "video/mp4", axum::http::Request::new(Body::empty()))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }
}
