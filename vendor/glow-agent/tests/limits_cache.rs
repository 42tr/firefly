//! The injected HTTP client must serve both generation and provider-limit
//! discovery, and successful limits must persist across restarts.
#![cfg(feature = "limits")]

use axum::{
    http::HeaderMap,
    routing::{get, post},
    Json, Router,
};
use glow_agent::{Message, Model, ModelRequest, OpenAIModel};
use serde_json::{json, Value};

#[tokio::test]
async fn injected_client_is_used_for_generation_and_metadata_and_limits_are_persisted() {
    let app = Router::new()
        .route(
            "/models/fixture",
            get(|headers: HeaderMap| async move {
                assert_eq!(headers["x-client"], "injected-client");
                assert_eq!(headers["authorization"], "Bearer fixture-key");
                Json(json!({"id":"fixture","context_window":100000,"max_output_tokens":10000}))
            }),
        )
        .route(
            "/responses",
            post(|headers: HeaderMap, Json(body): Json<Value>| async move {
                assert_eq!(headers["x-client"], "injected-client");
                assert_eq!(headers["authorization"], "Bearer fixture-key");
                assert_eq!(body["model"], "fixture");
                Json(json!({"status":"completed","output":[{"type":"message",
                "content":[{"type":"output_text","text":"done"}]}]}))
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert("x-client", "injected-client".parse().unwrap());
    let client = reqwest::Client::builder()
        .no_proxy()
        .default_headers(headers)
        .build()
        .unwrap();
    let model = OpenAIModel::new("fixture-key", "fixture")
        .with_base_url(base)
        .with_http_client(client)
        .with_limits_cache(pool.clone());

    let response = model
        .generate(ModelRequest::new(vec![Message::user("hello")], vec![]))
        .await
        .unwrap();
    assert_eq!(response.message.text_content(), "done");
    let stored: Value = sqlx::query_scalar("SELECT limits FROM model_limits_cache")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(stored["window"], 100000);
    assert_eq!(stored["output"], 10000);
    server.abort();
}
