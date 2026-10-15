use std::{process::Stdio, time::Duration};

use axum::{
    body::Body,
    extract::{Path, Query},
    http::{header, Method},
    response::Response,
    Json,
};
use serde::Deserialize;
use serde_json::Value;
use tokio::{
    io::AsyncReadExt,
    process::{Child, Command},
    time::timeout,
};

use super::{auth::Session, error::AppError};
use crate::{assistant_store, codex, office};

#[derive(Deserialize)]
pub struct FileQuery {
    #[serde(default)]
    path: String,
    #[serde(default)]
    directory: bool,
}


async fn spawn(auth: &Session, id: i64, path: &str, operation: &str) -> Result<Child, AppError> {
    let conversation = assistant_store::get_conversation_for_read(id, auth.storage_scope())
        .await?
        .ok_or_else(|| AppError::not_found("会话不存在或无权访问"))?;
    if !matches!(conversation.agent.as_str(), "codex" | "claude") {
        return Err(AppError::bad_request("该会话没有工作目录"));
    }
    if conversation.workspace_mode == "parallel" && conversation.workspace_state != "ready" {
        return Err(AppError::conflict("独立工作目录尚未就绪"));
    }
    let (abs, _display) =
        codex::resolve_workspace_path(conversation.codex_cwd.as_deref()).await.map_err(AppError::bad_request)?;
    let mut command = Command::new(std::env::current_exe().map_err(AppError::service_unavailable)?);
    command.arg("--file-worker").arg(abs).arg(path).arg(operation);
    let child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| AppError::service_unavailable(format!("启动文件服务失败：{e}")))?;
    Ok(child)
}

async fn metadata(auth: &Session, id: i64, path: &str, operation: &str) -> Result<Value, AppError> {
    let child = spawn(auth, id, path, operation).await?;
    let output = timeout(Duration::from_secs(30), child.wait_with_output())
        .await
        .map_err(|_| AppError::service_unavailable("读取文件超时，请重试"))?
        .map_err(AppError::service_unavailable)?;
    if !output.status.success() {
        return Err(AppError::bad_request(String::from_utf8_lossy(&output.stderr)));
    }
    serde_json::from_slice(&output.stdout).map_err(AppError::internal_server_error)
}

pub async fn list(
    auth: Session, Path(id): Path<i64>, Query(query): Query<FileQuery>,
) -> Result<Json<Value>, AppError> {
    Ok(Json(metadata(&auth, id, &query.path, "list").await?))
}

pub async fn download(
    auth: Session, Path(id): Path<i64>, Query(query): Query<FileQuery>, method: Method,
) -> Result<Response, AppError> {
    if method == Method::HEAD {
        metadata(&auth, id, &query.path, "check").await?;
        return Ok(Response::new(Body::empty()));
    }
    let mut child = spawn(&auth, id, &query.path, if query.directory { "archive" } else { "download" }).await?;
    let mut stdout = child.stdout.take().unwrap();
    let mut stderr = child.stderr.take().unwrap();
    let errors = tokio::spawn(async move {
        let mut buffer = Vec::new();
        let _ = stderr.read_to_end(&mut buffer).await;
        buffer
    });
    let mut ready = [0; 3];
    let result = timeout(Duration::from_secs(300), stdout.read_exact(&mut ready)).await;
    if !matches!(result, Ok(Ok(_))) || ready != *b"OK\n" {
        let _ = child.kill().await;
        let message = errors.await.unwrap_or_default();
        return Err(AppError::bad_request(if message.is_empty() {
            "文件准备失败或打包超时".to_owned()
        } else {
            String::from_utf8_lossy(&message).into_owned()
        }));
    }
    let stream =
        futures_util::stream::try_unfold((child, stdout, errors), |(mut child, mut stdout, errors)| async move {
            let mut buffer = vec![0; 64 * 1024];
            let count = stdout.read(&mut buffer).await?;
            if count == 0 {
                let status = child.wait().await?;
                let message = errors.await.unwrap_or_default();
                if !status.success() {
                    return Err(std::io::Error::other(String::from_utf8_lossy(&message).into_owned()));
                }
                return Ok(None);
            }
            buffer.truncate(count);
            Ok(Some((bytes::Bytes::from(buffer), (child, stdout, errors))))
        });
    let name = query.path.rsplit('/').next().filter(|v| !v.is_empty()).unwrap_or("workspace");
    let name = if query.directory { format!("{name}.zip") } else { name.to_owned() };
    let mut response = Response::new(Body::from_stream(stream));
    response.headers_mut().insert(header::CONTENT_TYPE, "application/octet-stream".parse().unwrap());
    response.headers_mut().insert(header::CONTENT_DISPOSITION, office::build_disposition_header("attachment", &name));
    response.headers_mut().insert(header::CACHE_CONTROL, "no-store".parse().unwrap());
    Ok(response)
}
