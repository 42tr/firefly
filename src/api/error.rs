use anyhow::Error;
use axum::{
    http::{header, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde_json::json;

/// 统一的 API 错误类型。
///
/// 通过 `?` 自动转换时，任何 `anyhow::Error` 都会变成 500 Internal Server Error。
/// 需要返回特定状态码时，使用 `AppError::bad_request("...")` 等构造方法。
#[derive(Debug)]
pub struct AppError {
    status: StatusCode,
    message: String,
    source: Option<Error>,
    retry_after_secs: Option<u64>,
}

impl AppError {
    pub fn new(status: StatusCode, message: impl Into<String>) -> Self {
        Self { status, message: message.into(), source: None, retry_after_secs: None }
    }

    pub fn status(&self) -> StatusCode {
        self.status
    }

    pub fn bad_request(msg: impl std::fmt::Display) -> Self {
        Self { status: StatusCode::BAD_REQUEST, message: msg.to_string(), source: None, retry_after_secs: None }
    }

    pub fn unauthorized(msg: impl std::fmt::Display) -> Self {
        Self { status: StatusCode::UNAUTHORIZED, message: msg.to_string(), source: None, retry_after_secs: None }
    }

    pub fn forbidden(msg: impl std::fmt::Display) -> Self {
        Self { status: StatusCode::FORBIDDEN, message: msg.to_string(), source: None, retry_after_secs: None }
    }

    pub fn not_found(msg: impl std::fmt::Display) -> Self {
        Self { status: StatusCode::NOT_FOUND, message: msg.to_string(), source: None, retry_after_secs: None }
    }

    pub fn conflict(msg: impl std::fmt::Display) -> Self {
        Self { status: StatusCode::CONFLICT, message: msg.to_string(), source: None, retry_after_secs: None }
    }

    pub fn internal_server_error(msg: impl std::fmt::Display) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: msg.to_string(),
            source: None,
            retry_after_secs: None,
        }
    }

    pub fn service_unavailable(msg: impl std::fmt::Display) -> Self {
        Self { status: StatusCode::SERVICE_UNAVAILABLE, message: msg.to_string(), source: None, retry_after_secs: None }
    }

    pub fn too_many_requests(msg: impl std::fmt::Display, retry_after_secs: u64) -> Self {
        Self {
            status: StatusCode::TOO_MANY_REQUESTS,
            message: msg.to_string(),
            source: None,
            retry_after_secs: Some(retry_after_secs.max(1)),
        }
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        // 只有 5xx 或未知错误才打印内部错误栈
        if self.status.is_server_error() {
            if let Some(source) = &self.source {
                tracing::error!("Internal error ({}): {:#}", self.status, source);
            } else {
                tracing::error!("Internal error ({}): {}", self.status, self.message);
            }
        }

        let body = Json(json!({
            "error": self.message,
            "status": self.status.as_u16(),
        }));

        let mut response = (self.status, body).into_response();
        if let Some(seconds) = self.retry_after_secs {
            if let Ok(value) = HeaderValue::from_str(&seconds.to_string()) {
                response.headers_mut().insert(header::RETRY_AFTER, value);
            }
        }
        response
    }
}

impl<E> From<E> for AppError
where
    E: Into<Error>,
{
    fn from(err: E) -> Self {
        let source = err.into();
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: "Internal Server Error".to_string(),
            source: Some(source),
            retry_after_secs: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use axum::http::{header, StatusCode};

    use super::*;

    #[test]
    fn too_many_requests_sets_retry_after_header() {
        let response = AppError::too_many_requests("try later", 42).into_response();

        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(response.headers().get(header::RETRY_AFTER).unwrap(), "42");
    }
}
