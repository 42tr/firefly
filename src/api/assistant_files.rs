use std::{process::Stdio, time::Duration};

use axum::{
    body::Body,
    extract::{Path, Query},
    http::{header, HeaderValue, Method},
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

use super::{auth::Session, error::AppError, modules::SANDBOXED_PAGE_CSP};
use crate::{assistant_store, codex, office, workspace_files::PREVIEW_MAX_BYTES};

/// 读取预览协议头部的最长等待时间；worker 只做本地打开与 stat。
const PREVIEW_HEADER_TIMEOUT: Duration = Duration::from_secs(10);
/// 协议头部的字节上限：`OK ` 加 20 位十进制数字。
const PREVIEW_HEADER_LIMIT: usize = 32;

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

/// 校验预览参数，返回用于 `Content-Disposition` 的文件名。
///
/// 纯参数校验先于会话查询：非法请求不会查数据库，也不会启动 worker。
fn validate_preview_query(query: &FileQuery) -> Result<String, AppError> {
    if query.directory {
        return Err(AppError::bad_request("只能预览 .html/.htm 文件"));
    }
    let path = query.path.as_str();
    if path.is_empty() || path.contains('\0') {
        return Err(AppError::bad_request("非法文件路径"));
    }
    if path.starts_with('/')
        || path.ends_with('/')
        || path.split('/').any(|part| part.is_empty() || part == "." || part == "..")
    {
        return Err(AppError::bad_request("非法文件路径"));
    }
    let extension = std::path::Path::new(path)
        .extension()
        .and_then(|value| value.to_str())
        .map(|value| value.to_ascii_lowercase());
    if !matches!(extension.as_deref(), Some("html" | "htm")) {
        return Err(AppError::bad_request("只能预览 .html/.htm 文件"));
    }
    Ok(path.rsplit('/').next().filter(|name| !name.is_empty()).unwrap_or("preview.html").to_owned())
}

/// 解析 worker `preview` 协议的一行头部（不含换行）：`OK <十进制字节数>`。
fn parse_preview_header(header: &[u8]) -> Option<u64> {
    let digits = header.strip_prefix(b"OK ")?;
    if digits.is_empty() || !digits.iter().all(u8::is_ascii_digit) {
        return None;
    }
    std::str::from_utf8(digits).ok()?.parse().ok()
}

/// 逐字节读取协议头部，读到换行即返回；超长或提前 EOF 交给调用方映射错误码。
async fn read_preview_header(stdout: &mut tokio::process::ChildStdout) -> std::io::Result<Vec<u8>> {
    let mut header = Vec::with_capacity(PREVIEW_HEADER_LIMIT);
    loop {
        let mut byte = [0u8; 1];
        stdout.read_exact(&mut byte).await?;
        if byte[0] == b'\n' {
            return Ok(header);
        }
        if header.len() >= PREVIEW_HEADER_LIMIT {
            return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "预览协议头部过长"));
        }
        header.push(byte[0]);
    }
}

/// 预览响应头：沙箱 CSP 与模块页共用，`X-Frame-Options` 显式覆盖全局 `DENY`。
fn preview_response(body: Body, size: u64, name: &str) -> Response {
    let mut response = Response::new(body);
    let headers = response.headers_mut();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static("text/html; charset=utf-8"));
    headers.insert(header::CONTENT_DISPOSITION, office::build_disposition_header("inline", name));
    headers.insert(header::CONTENT_LENGTH, HeaderValue::from_str(&size.to_string()).expect("长度一定可编码"));
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("private, no-store"));
    headers.insert(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    headers.insert(header::CONTENT_SECURITY_POLICY, HeaderValue::from_static(SANDBOXED_PAGE_CSP));
    headers.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("SAMEORIGIN"));
    response
}

/// 只转发头部声明的那 `size` 个字节；每块最多 64 KiB，内存占用与文件大小无关。
fn preview_stream(
    child: Child,
    stdout: tokio::process::ChildStdout,
    errors: tokio::task::JoinHandle<Vec<u8>>,
    remaining: u64,
) -> impl futures_util::Stream<Item = Result<bytes::Bytes, std::io::Error>> {
    futures_util::stream::try_unfold(
        (child, stdout, errors, remaining),
        |(mut child, mut stdout, errors, remaining)| async move {
            if remaining == 0 {
                let status = child.wait().await?;
                let message = errors.await.unwrap_or_default();
                if !status.success() {
                    return Err(std::io::Error::other(String::from_utf8_lossy(&message).into_owned()));
                }
                return Ok(None);
            }
            let mut buffer = vec![0; 64 * 1024];
            let limit = buffer.len().min(remaining as usize);
            let count = stdout.read(&mut buffer[..limit]).await?;
            if count == 0 {
                let _ = child.kill().await;
                return Err(std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "文件读取中断"));
            }
            buffer.truncate(count);
            Ok(Some((bytes::Bytes::from(buffer), (child, stdout, errors, remaining - count as u64))))
        },
    )
}

/// `GET/HEAD /api/assistant/conversations/{id}/files/preview?path=`：沙箱内渲染 HTML 原型。
pub async fn preview(
    auth: Session, Path(id): Path<i64>, Query(query): Query<FileQuery>, method: Method,
) -> Result<Response, AppError> {
    let name = validate_preview_query(&query)?;
    let mut child = spawn(&auth, id, &query.path, "preview").await?;
    let mut stdout = child.stdout.take().unwrap();
    let mut stderr = child.stderr.take().unwrap();
    let errors = tokio::spawn(async move {
        let mut buffer = Vec::new();
        let _ = stderr.read_to_end(&mut buffer).await;
        buffer
    });
    let header = match timeout(PREVIEW_HEADER_TIMEOUT, read_preview_header(&mut stdout)).await {
        Ok(Ok(header)) => header,
        Ok(Err(error)) => {
            let _ = child.kill().await;
            let message = errors.await.unwrap_or_default();
            // worker 的 stderr 只写日志，不回显给客户端，避免泄露文件系统细节。
            tracing::warn!(conversation = id, path = %query.path, error = %error, "HTML 预览 worker 失败：{}", String::from_utf8_lossy(&message));
            return Err(if message.is_empty() {
                AppError::service_unavailable("文件预览服务返回异常")
            } else {
                AppError::not_found("文件不存在或无法预览")
            });
        }
        Err(_) => {
            let _ = child.kill().await;
            return Err(AppError::service_unavailable("读取预览超时，请重试"));
        }
    };
    let Some(size) = parse_preview_header(&header) else {
        let _ = child.kill().await;
        tracing::warn!(conversation = id, path = %query.path, "HTML 预览协议异常：{}", String::from_utf8_lossy(&header));
        return Err(AppError::service_unavailable("文件预览服务返回异常"));
    };
    // 超限时 worker 只发头部就退出，这里不读正文，413 可以在提交响应体之前返回。
    if size > PREVIEW_MAX_BYTES {
        let _ = child.kill().await;
        return Err(AppError::payload_too_large("文件超过预览上限（8MB），请下载查看"));
    }
    if method == Method::HEAD {
        let _ = child.kill().await;
        return Ok(preview_response(Body::empty(), size, &name));
    }
    Ok(preview_response(Body::from_stream(preview_stream(child, stdout, errors, size)), size, &name))
}

#[cfg(test)]
mod tests {
    use axum::http::StatusCode;

    use super::*;

    fn query(path: &str) -> FileQuery {
        FileQuery { path: path.to_owned(), directory: false }
    }

    #[test]
    fn preview_query_accepts_only_html_files() {
        assert_eq!(validate_preview_query(&query("index.html")).unwrap(), "index.html");
        assert_eq!(validate_preview_query(&query("PROTOTYPE.HTM")).unwrap(), "PROTOTYPE.HTM");
        assert_eq!(validate_preview_query(&query("design/x/Index.Html")).unwrap(), "Index.Html");
        for bad in ["", "/index.html", "../index.html", "a/../b.html", "a//b.html", "./a.html", "index.html/", "index.txt", "index.html.txt", "index.html\0"] {
            assert_eq!(validate_preview_query(&query(bad)).unwrap_err().status(), StatusCode::BAD_REQUEST, "accepted {bad:?}");
        }
        let directory = FileQuery { path: "index.html".to_owned(), directory: true };
        assert_eq!(validate_preview_query(&directory).unwrap_err().status(), StatusCode::BAD_REQUEST);
    }

    #[test]
    fn preview_header_parses_worker_sizes() {
        assert_eq!(parse_preview_header(b"OK 0"), Some(0));
        assert_eq!(parse_preview_header(b"OK 8388608"), Some(PREVIEW_MAX_BYTES));
        assert_eq!(parse_preview_header(b"OK 8388609"), Some(PREVIEW_MAX_BYTES + 1));
        for bad in [&b"OK"[..], b"OK ", b"OK\n", b"OK abc", b"OK -1", b"NO 12", b"OK 1 2", b"OK 123456789012345678901"] {
            assert_eq!(parse_preview_header(bad), None, "accepted {:?}", String::from_utf8_lossy(bad));
        }
    }

    #[test]
    fn preview_response_is_inline_sandboxed_html() {
        let response = preview_response(Body::empty(), 123, "index.html");
        let headers = response.headers();
        assert_eq!(headers[header::CONTENT_TYPE], "text/html; charset=utf-8");
        assert!(headers[header::CONTENT_DISPOSITION].to_str().unwrap().starts_with("inline;"));
        assert_eq!(headers[header::CONTENT_LENGTH], "123");
        assert_eq!(headers[header::CACHE_CONTROL], "private, no-store");
        assert_eq!(headers[header::X_CONTENT_TYPE_OPTIONS], "nosniff");
        assert_eq!(headers[header::X_FRAME_OPTIONS], "SAMEORIGIN");
        assert_eq!(headers[header::CONTENT_SECURITY_POLICY], SANDBOXED_PAGE_CSP);
        let csp = headers[header::CONTENT_SECURITY_POLICY].to_str().unwrap();
        assert!(csp.contains("sandbox allow-scripts allow-forms allow-downloads"));
        assert!(!csp.contains("allow-same-origin"));
        assert!(!csp.contains("frame-ancestors 'none'"));
    }
}
