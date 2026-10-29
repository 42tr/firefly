use axum::{
    extract::DefaultBodyLimit,
    extract::Request,
    http::{header, HeaderMap, HeaderValue, Method, StatusCode, Uri},
    middleware::{self, Next},
    response::Response,
    routing::{delete, get, patch, post},
    Router,
};
use rust_embed::RustEmbed;
use tower_cookies::CookieManagerLayer;
use tower_http::compression::{
    predicate::{DefaultPredicate, NotForContentType, Predicate},
    CompressionLayer,
};
use tower_http::cors::CorsLayer;

use crate::config;

pub mod assistant;
mod agent_settings;
mod assistant_files;
mod assistant_realtime;
mod assistant_tasks;
mod attachment_utils;
mod auth;
pub mod error;
mod health;
mod jwt;
mod modules;

macro_rules! frontend_bundles {
    ($(($struct:ident, $handler:ident, $folder:literal, $prefix:literal)),* $(,)?) => {
        $(
            #[derive(RustEmbed)]
            #[folder = $folder]
            struct $struct;

            fn $handler(path: &str, if_none_match: Option<&str>) -> Option<Response> {
                serve_asset::<$struct>(path, $prefix, if_none_match)
            }
        )*

        const FRONTEND_ROUTES: &[(&str, AssetHandler)] = &[
            $(($prefix, $handler)),*
        ];
    };
}

pub fn app() -> Router {
    let allowed_origins = config::cors_allowed_origins().unwrap_or_else(|err| {
        tracing::error!("Failed to parse CORS_ALLOWED_ORIGINS: {}", err);
        vec!["http://localhost:5173".parse::<HeaderValue>().unwrap()]
    });

    routes()
        .layer(CookieManagerLayer::new())
        .layer(
            CorsLayer::new()
                .allow_origin(allowed_origins)
                .allow_methods([
                    Method::GET,
                    Method::POST,
                    Method::PATCH,
                    Method::OPTIONS,
                    Method::HEAD,
                    Method::PUT,
                    Method::DELETE,
                ])
                .allow_credentials(true)
                .allow_headers([header::AUTHORIZATION, header::CONTENT_TYPE, header::HeaderName::from_static("idempotency-key")]),
        )
        // 压缩内置静态资源与 API JSON。tower-http 默认断言已跳过 SSE
        // (text/event-stream)、gRPC、图片和小于 32 字节的响应，因此不影响
        // 流式对话与 WebSocket。文件下载（application/octet-stream）和
        // 字体类型响应（font/）也跳过压缩。
        .layer(
            CompressionLayer::new().compress_when(
                DefaultPredicate::new()
                    .and(NotForContentType::const_new("application/octet-stream"))
                    .and(NotForContentType::const_new("font/")),
            ),
        )
        // 放在最外层：压缩后的响应、304 和错误响应也都会带上安全头。
        .layer(middleware::from_fn(security_headers))
}

fn routes() -> Router {
    Router::new()
        .route("/health", get(health::health_check))



        .route("/auth", get(auth::page).post(auth::login).layer(DefaultBodyLimit::max(4096)))
        .route("/auth/", get(auth::page).post(auth::login).layer(DefaultBodyLimit::max(4096)))
        .route("/auth/logout", post(auth::logout))
        .merge(assistant_routes())
        .merge(modules::routes())
        .merge(agent_settings::routes())
        .fallback(get(frontend_router))
}



fn assistant_routes() -> Router {
    Router::new()
        .route("/api/assistant/ping", get(assistant::ping))
        .route("/api/assistant/ws", get(assistant_realtime::connect))









        .route("/api/assistant/codex/providers", get(assistant::codex_providers))
        .route("/api/assistant/claude/models", get(assistant::claude_models))



        .route("/api/assistant/workspace", get(assistant::list_workspace).post(assistant::create_workspace_directory))
        .route("/api/assistant/workspace/file", get(assistant::download_workspace_file))
        .route("/api/assistant/reset", post(assistant::reset))
        .route("/api/assistant/schedules", get(assistant::schedules::list).post(assistant::schedules::create))
        .route("/api/assistant/schedules/preview", post(assistant::schedules::preview))
        .route("/api/assistant/schedules/{id}", axum::routing::put(assistant::schedules::update).patch(assistant::schedules::toggle).delete(assistant::schedules::delete))
        .route("/api/assistant/memories", get(assistant::list_memories).post(assistant::create_memory))
        .route("/api/assistant/memories/{id}", patch(assistant::update_memory).delete(assistant::delete_memory))
        .route("/api/assistant/chat", post(assistant::chat_stream))
        .route("/api/assistant/attachments/{id}", get(assistant::download_attachment))
        .route("/api/assistant/attachments/by-file/{file_id}", get(assistant::preview_uploaded_attachment))
        .route("/api/assistant/firefly/messages", get(assistant::firefly::list).post(assistant::firefly::send))
        .route("/api/assistant/conversations/{id}/tasks", get(assistant_tasks::list))
        .route("/api/assistant/conversations/{id}/tasks/{task_id}", get(assistant_tasks::detail))
        .route("/api/assistant/conversations/{id}/tasks/cancel", post(assistant_tasks::cancel))
        .route("/api/assistant/conversations", get(assistant::list_conversations).post(assistant::create_conversation))
        .route("/api/assistant/conversations/{id}/files", get(assistant_files::list))
        .route("/api/assistant/conversations/{id}/workspace/retry", post(assistant::retry_parallel_workspace))
        .route("/api/assistant/conversations/{id}/files/download", get(assistant_files::download))
        .route("/api/assistant/conversations/{id}/files/preview", get(assistant_files::preview))
        .route(
            "/api/assistant/conversations/{id}/messages",
            get(assistant::get_conversation_messages).post(assistant::enqueue_conversation_message),
        )
        .route("/api/assistant/conversations/{id}/active-generation", get(assistant::get_active_generation))
        .route("/api/assistant/conversations/{id}/goal", get(assistant::get_goal).post(assistant::create_goal).patch(assistant::update_goal))
        .route("/api/assistant/conversations/{id}/read", post(assistant::mark_conversation_read))
        .route("/api/assistant/conversations/{id}/codex-settings", patch(assistant::update_codex_settings))
        .route("/api/assistant/conversations/{id}/claude-settings", patch(assistant::update_claude_settings))
        .route("/api/assistant/conversations/{id}/process", get(assistant::get_conversation_process))
        .route("/api/assistant/conversations/{id}/tool-detail", get(assistant::get_tool_detail))
        .route(
            "/api/assistant/conversations/{id}",
            patch(assistant::update_conversation).delete(assistant::delete_conversation),
        )
        .route("/api/assistant/jobs/{job_id}", delete(assistant::cancel_job))
        .route("/api/assistant/jobs/{job_id}/stream", get(assistant::resume_job_stream))
        .layer(DefaultBodyLimit::max(config::get().assistant_max_chat_upload_bytes() as usize))
}


frontend_bundles! {
    (AssistantAssets, serve_assistant_asset, "frontend/dist/", "/"),
}

type AssetHandler = fn(&str, Option<&str>) -> Option<Response>;

// 路由匹配
async fn frontend_router(uri: Uri, headers: HeaderMap) -> Response {
    let path = uri.path();
    if let Some(response) = crate::modules::public_resource(path).await { return response; }
    let if_none_match = headers.get(header::IF_NONE_MATCH).and_then(|value| value.to_str().ok());

    for (prefix, handler) in FRONTEND_ROUTES.iter() {
        if path.starts_with(prefix) {
            return handler(path, if_none_match).unwrap_or_else(not_found);
        }
    }

    not_found()
}

fn not_found() -> Response {
    Response::builder().status(StatusCode::NOT_FOUND).body("404 Not Found".into()).unwrap()
}

/// 统一安全响应头。
///
/// 消息正文经 DOMPurify 消毒后以 `v-html` 渲染，CSP 是第二层防线：脚本只允许
/// 本站资源（构建产物没有内联脚本，也没有 eval/new Function）。`style-src` 需要
/// `'unsafe-inline'`，因为 Vue 用行内 style 驱动进度条、萤火虫位置等；图片、媒体
/// 与连接保持宽松——消息会引用远端图片，工作区预览使用 `blob:`/`data:` URL。
const CONTENT_SECURITY_POLICY: &str = "default-src 'self'; \
     script-src 'self'; \
     style-src 'self' 'unsafe-inline'; \
     img-src 'self' data: blob: http: https:; \
     media-src 'self' data: blob: http: https:; \
     font-src 'self'; \
     connect-src 'self' ws: wss: http: https:; \
     object-src 'none'; \
     base-uri 'self'; \
     form-action 'self'; \
     frame-ancestors 'none'";

async fn security_headers(request: Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    headers
        .entry(header::CONTENT_SECURITY_POLICY)
        .or_insert_with(|| HeaderValue::from_static(CONTENT_SECURITY_POLICY));
    headers.entry(header::X_CONTENT_TYPE_OPTIONS).or_insert_with(|| HeaderValue::from_static("nosniff"));
    headers.entry(header::X_FRAME_OPTIONS).or_insert_with(|| HeaderValue::from_static("DENY"));
    headers.entry(header::REFERRER_POLICY).or_insert_with(|| HeaderValue::from_static("same-origin"));
    response
}

// 通用静态文件处理函数
fn serve_asset<T: RustEmbed>(
    path: &str,
    prefix: &str,
    if_none_match: Option<&str>,
) -> Option<http::Response<axum::body::Body>> {
    let file_path = path.strip_prefix(prefix).unwrap_or(path).trim_start_matches('/');
    let file_path = if file_path.is_empty() { "index.html" } else { file_path };

    let mut asset = T::get(file_path)?;
    let mime = mime_guess::from_path(file_path).first_or_octet_stream();

    if file_path == "index.html" {
        let cfg = config::get();
        let title = cfg.site_title.trim();
        let html = std::str::from_utf8(&asset.data).ok()?;
        let mut html = html.replacen(
            "name=\"firefly-timezone\" content=\"Asia/Shanghai\"",
            &format!("name=\"firefly-timezone\" content=\"{}\"", config::timezone().name()), 1,
        );
        if !title.is_empty() && title != "流萤" {
            // Treat the configured title as text, including any HTML delimiters.
            let title = title.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;");
            html = html.replacen("<title>流萤</title>", &format!("<title>{title}</title>"), 1);
        }
        asset.data = std::borrow::Cow::Owned(html.into_bytes());
    }

    // Vite 只会给 assets/ 下的产物加内容哈希，dist 根目录的 index.html 和
    // public/ 复制过来的文件没有哈希。没有哈希就不能长缓存，否则更新后老客户端
    // 拿不到新文件；这类文件改用 no-cache + ETag 条件请求（命中即 304）。
    let cache_header = if file_path.starts_with("assets/") {
        "public, max-age=31536000, immutable"
    } else {
        "no-cache"
    };

    let etag = asset_etag(file_path, &asset.data);

    // --- 转成 Bytes ---
    // release 模式下 rust-embed 返回 Cow::Borrowed，走 from_static 零拷贝，避免每次请求复制资源。
    let body = axum::body::Body::from(match asset.data {
        std::borrow::Cow::Borrowed(data) => bytes::Bytes::from_static(data),
        std::borrow::Cow::Owned(data) => bytes::Bytes::from(data),
    });

    let builder = http::Response::builder()
        .header("Content-Type", mime.as_ref())
        .header("Cache-Control", cache_header)
        .header("ETag", etag.as_str());

    // 条件请求命中时返回 304：重复访问 index.html 只花一次空响应，
    // 不再重传整个 HTML。
    if if_none_match.is_some_and(|value| etag_matches(value, &etag)) {
        return builder
            .status(http::StatusCode::NOT_MODIFIED)
            .body(axum::body::Body::empty())
            .ok();
    }

    builder.status(http::StatusCode::OK).body(body).ok()
}

/// 从请求的 `If-None-Match` 判断 ETag 是否命中。同时接受 `W/` 弱校验前缀与
/// `*`，并按规范用逗号分隔的列表逐项比较。
fn etag_matches(header_value: &str, etag: &str) -> bool {
    header_value.split(',').any(|candidate| {
        let candidate = candidate.trim();
        candidate == "*" || candidate.strip_prefix("W/").unwrap_or(candidate).trim() == etag
    })
}

/// 静态资源 ETag：按内容 sha256 生成，只在该文件首次被请求时计算并缓存，
/// 之后复用。不用文件 mtime，避免同秒内的两次构建产生相同 ETag 而返回过期的
/// index.html —— 部署后必须让客户端拿到新资源。
///
/// release 下 rust-embed 把资源编译进二进制、内容不会变，因此可以永久缓存
/// 计算结果；debug 下 rust-embed 每次 `get` 都重新读盘，缓存会让改完前端后
/// 旧的 ETag 继续命中 304，所以 debug 不缓存、按当前内容现算。
fn asset_etag(file_path: &str, data: &[u8]) -> String {
    #[cfg(debug_assertions)]
    {
        let _ = file_path;
        hash_etag(data)
    }

    #[cfg(not(debug_assertions))]
    {
        type EtagCache = std::sync::Mutex<std::collections::HashMap<String, String>>;
        static CACHE: std::sync::OnceLock<EtagCache> = std::sync::OnceLock::new();
        let cache = CACHE.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()));

        if let Ok(guard) = cache.lock() {
            if let Some(etag) = guard.get(file_path) {
                return etag.clone();
            }
        }

        let etag = hash_etag(data);

        if let Ok(mut guard) = cache.lock() {
            guard.insert(file_path.to_string(), etag.clone());
        }
        etag
    }
}

/// 按内容计算 sha256 ETag，取前 16 字节（128 位）已足够，同时让 ETag 保持短小。
fn hash_etag(data: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(data);
    let mut etag = String::with_capacity(34);
    etag.push('"');
    for byte in &digest[..16] {
        use std::fmt::Write;
        let _ = write!(etag, "{byte:02x}");
    }
    etag.push('"');
    etag
}

#[cfg(test)]
mod tests {
    use axum::{
        extract::Request,
        http::{HeaderValue, StatusCode},
        middleware::{self, Next},
        response::Response,
    };
    use tower::ServiceExt;

    use super::routes;

    async fn mark_request_path(req: Request, next: Next) -> Response {
        let path = req.uri().path().to_string();
        let mut response = next.run(req).await;
        response.headers_mut().insert("x-test-path", HeaderValue::from_str(&path).unwrap());
        response
    }

    #[tokio::test]
    async fn retired_routes_return_not_found() {
        crate::config::init_test_config();
        for path in [
            "/monitor",
            "/monitor/",
            "/monitor/index.html",
            "/files",
            "/files/",
            "/files/index.html",
            "/auth/login",
            "/auth/index.html",
            "/api/assistant/actions/ws",
            "/api/assistant/actions/owner%2Frepo%3A1%3A1/dismiss",
            "/news/api/xiaohongshu/publications",
            "/news/api/xiaohongshu/config-status",
            "/news/api/xiaohongshu/assets/1/image",
            "/news/api/xiaohongshu/publications/1/assets",
            "/news/api/xiaohongshu/publications/1/attempts",
            "/request-logs",
            "/request-logs/",
            "/request-logs/index.html",
            "/api/request-logs",
            "/api/request-logs/1",
            "/api/request-logs/dates",
            "/api/request-logs/monthly",
            "/request-logs/api/request-logs",
        ] {
            let response =
                routes().oneshot(Request::builder().uri(path).body(axum::body::Body::empty()).unwrap()).await.unwrap();
            assert_eq!(response.status(), StatusCode::NOT_FOUND, "{path}");
        }
    }

    #[tokio::test]
    async fn publication_routes_are_module_owned() {
        crate::config::init_test_config();
        for path in ["/api/xiaohongshu/publications", "/api/xiaohongshu/config-status"] {
            let response =
                routes().oneshot(Request::builder().uri(path).body(axum::body::Body::empty()).unwrap()).await.unwrap();
            assert_eq!(response.status(), StatusCode::NOT_FOUND, "{path}");
        }
    }

    #[tokio::test]
    async fn special_days_routes_are_module_owned() {
        crate::config::init_test_config();
        for path in ["/api/special_days", "/api/special_days/month?year=2026&month=10"] {
            let response = routes()
                .oneshot(Request::builder().uri(path).body(axum::body::Body::empty()).unwrap())
                .await.unwrap();
            assert_eq!(response.status(), StatusCode::NOT_FOUND, "{path}");
        }
    }

    #[tokio::test]
    async fn ai_rankings_is_module_owned() {
        crate::config::init_test_config();
        let response = routes()
            .oneshot(Request::builder().uri("/api/assistant/ai-rankings")
                .body(axum::body::Body::empty()).unwrap())
            .await.unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn business_routes_are_module_owned() {
        crate::config::init_test_config();
        for (method, path) in [
            ("GET", "/api/assistant/finance/gold"),
            ("GET", "/api/assistant/finance/fund"),
            ("POST", "/api/assistant/finance/fund/refresh"),
            ("GET", "/api/assistant/finance/portfolio"),
            ("GET", "/api/assistant/finance/status"),
            ("POST", "/api/assistant/finance/gold/refresh"),
            ("POST", "/api/assistant/finance/transactions"),
            ("DELETE", "/api/assistant/finance/transactions/1"),
            ("GET", "/api/assistant/todos"),
            ("POST", "/api/assistant/todos"),
            ("PATCH", "/api/assistant/todos/1"),
            ("DELETE", "/api/assistant/todos/1"),
            ("POST", "/api/assistant/todos/1/restore"),
            ("GET", "/api/assistant/pending-status"),
            ("GET", "/api/metrics"),
            ("GET", "/api/metrics/status"),
            ("GET", "/api/metrics/history"),
        ] {
            let response = routes().oneshot(Request::builder().method(method).uri(path)
                .body(axum::body::Body::empty()).unwrap()).await.unwrap();
            // The core catch-all serves GET only; removed write routes are 405.
            assert!(matches!(response.status(), StatusCode::NOT_FOUND | StatusCode::METHOD_NOT_ALLOWED), "{method} {path}: {}", response.status());
        }
    }

    #[tokio::test]
    async fn unknown_paths_pass_through_global_middleware() {
        crate::config::init_test_config();
        let response = routes()
            .layer(middleware::from_fn(mark_request_path))
            .oneshot(Request::builder().uri("/does-not-exist").body(axum::body::Body::empty()).unwrap())
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert_eq!(response.headers().get("x-test-path").unwrap(), "/does-not-exist");
    }
}

#[cfg(test)]
mod route_tests {
    use axum::{
        body::Body,
        http::{Request, StatusCode},
    };
    use tower::ServiceExt;

    /// 未登录的预览请求必须被 `Session` 拦截（401），而不是落到前端 fallback。
    #[tokio::test]
    async fn preview_route_requires_a_session() {
        crate::config::init_test_config();
        let response = super::app()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/api/assistant/conversations/1/files/preview?path=index.html")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }
}
