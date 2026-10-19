use super::{
    cached_provider_models, fetch_models_once, parse_model_providers, provider_models_cache,
    resolve_settings_from_sources, validate_relative_path, ProviderModelsEntry,
    PROVIDER_MODELS_ERROR_TTL,
};
use serde_json::json;
use std::time::{Duration, Instant};


#[test]
fn cleared_settings_resolve_current_defaults_and_model_specific_effort() {
    let catalog = vec![json!({
        "model":"default-model", "isDefault":true, "defaultReasoningEffort":"low",
        "supportedReasoningEfforts":[{"reasoningEffort":"low"}],
    })];
    let defaults = resolve_settings_from_sources("relay".into(), None, None, &catalog).unwrap();
    assert_eq!(defaults.model, "default-model");
    assert_eq!(defaults.reasoning_level, "low");
    let prefixed = resolve_settings_from_sources("relay".into(), Some("relay/default-model".into()), None, &catalog).unwrap();
    assert_eq!(prefixed.reasoning_level, "low");
    let explicit = resolve_settings_from_sources("openai".into(), Some("selected-model".into()), Some("ultra"), &catalog).unwrap();
    assert_eq!(explicit.provider, "openai");
    assert_eq!(explicit.model, "selected-model");
    assert_eq!(explicit.reasoning_level, "ultra");
    let without_reasoning = vec![json!({"model":"plain-model", "supportedReasoningEfforts":[]})];
    assert_eq!(resolve_settings_from_sources("relay".into(), Some("plain-model".into()), None, &without_reasoning).unwrap().reasoning_level, "none");
    assert!(resolve_settings_from_sources("relay".into(), None, None, &[]).is_err());
}

#[test]
fn catalog_preserves_model_reasoning_capabilities() {
    let entry = serde_json::json!({
        "model": " gpt-test ",
        "defaultReasoningEffort": "high",
        "supportedReasoningEfforts": [
            { "reasoningEffort": "low", "description": "Fast" },
            { "reasoningEffort": "high", "description": "Thorough" }
        ]
    });
    let model = super::OfficialModel::from_catalog(&entry).unwrap();
    assert_eq!(model.model, "gpt-test");
    assert_eq!(model.reasoning_levels, ["low", "high"]);
    assert_eq!(model.default_reasoning_level.as_deref(), Some("high"));
    let mut hidden = entry;
    hidden["hidden"] = serde_json::json!(true);
    assert!(super::OfficialModel::from_catalog(&hidden).is_none());
}

#[test]
fn turn_params_apply_reasoning_and_preserve_legacy_defaults() {
    let input = vec![serde_json::json!({"type": "text", "text": "hello"})];
    let params = super::turn_params("thread-1", std::path::Path::new("/workspace"), input.clone(), Some(" high "));
    assert_eq!(params["effort"], "high");
    assert_eq!(params["input"], serde_json::json!(input));
    for level in [None, Some(" ")] {
        let params = super::turn_params("thread-1", std::path::Path::new("/workspace"), vec![], level);
        assert!(params.get("effort").is_none());
    }
}

#[test]
fn parses_model_providers() {
    let content = r#"
model_provider = "platform_b"
model = "gpt-5"
model_reasoning_effort = "high"

[model_providers.platform_a]
name = "platform_a"
base_url = "https://a.example.com/v1"
env_key = "PLATFORM_A_KEY"
wire_api = "responses"

[model_providers.platform_b]
base_url = "https://b.example.com/v1"
"#;
    let listing = parse_model_providers(content);
    assert_eq!(listing.default.as_deref(), Some("platform_b"));
    assert_eq!(listing.default_reasoning_level.as_deref(), Some("high"));
    assert_eq!(listing.providers.len(), 2);
    assert_eq!(listing.providers[0].name, "platform_a");
    assert_eq!(listing.providers[0].base_url.as_deref(), Some("https://a.example.com/v1"));
    assert_eq!(listing.providers[0].env_key.as_deref(), Some("PLATFORM_A_KEY"));
    assert_eq!(listing.providers[1].name, "platform_b");
    assert_eq!(listing.providers[1].base_url.as_deref(), Some("https://b.example.com/v1"));
}

#[test]
fn parses_missing_providers_as_empty() {
    let listing = parse_model_providers("model = \"gpt-5\"");
    assert_eq!(listing.default, None);
    assert!(listing.providers.is_empty());
}

#[test]
fn rejects_broken_toml_as_empty() {
    let listing = parse_model_providers("this is [not toml");
    assert!(listing.providers.is_empty());
}

#[test]
fn effective_model_trims_and_defaults_to_none() {
    // 测试进程未加载全局配置：对话模型生效，空白/缺省返回 None。
    assert_eq!(super::effective_model(Some(" gpt-5 ")).as_deref(), Some("gpt-5"));
    assert_eq!(super::effective_model(Some("  ")).as_deref(), None);
    assert_eq!(super::effective_model(None).as_deref(), None);
}

#[test]
fn accepts_workspace_relative_paths() {
    assert!(validate_relative_path("").is_ok());
    assert!(validate_relative_path("project").is_ok());
    assert!(validate_relative_path("team/project").is_ok());
}

#[test]
fn rejects_paths_that_can_escape_workspace() {
    assert!(validate_relative_path("../project").is_err());
    assert!(validate_relative_path("team/../../project").is_err());
    assert!(validate_relative_path("/tmp/project").is_err());
}

#[tokio::test]
async fn models_endpoint_parses_json_and_reports_definitive_failures() {
    use axum::{http::StatusCode, routing::get, Router};

    let app = Router::new()
        .route(
            "/models",
            get(|headers: axum::http::HeaderMap| async move {
                assert_eq!(headers.get("authorization").unwrap(), "Bearer fixture-key");
                axum::Json(json!({"data":[{"id":"model-b"},{"id":"model-a"}]}))
            }),
        )
        .route(
            "/html/models",
            get(|| async { (StatusCode::OK, [("content-type", "text/html")], "<html>relay</html>") }),
        )
        .route("/missing/models", get(|| async { StatusCode::NOT_FOUND }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let budget = Duration::from_secs(5);

    let models = fetch_models_once(&format!("{base}/models"), "fixture-key", budget).await.unwrap();
    assert_eq!(models, vec!["model-a".to_string(), "model-b".to_string()]);

    // 中转站返回 HTML 与 404 都是确定性失败，交给调用方换下一个候选地址。
    let html = fetch_models_once(&format!("{base}/html/models"), "fixture-key", budget).await.unwrap_err();
    assert!(!html.transient);
    assert!(html.message.contains("JSON"), "{}", html.message);
    let missing = fetch_models_once(&format!("{base}/missing/models"), "fixture-key", budget).await.unwrap_err();
    assert!(!missing.transient);
    assert!(missing.message.contains("404"), "{}", missing.message);
    server.abort();
}

#[tokio::test]
async fn models_endpoint_retries_transient_failures_once() {
    use axum::{http::StatusCode, response::IntoResponse, routing::get, Router};
    use std::sync::atomic::{AtomicUsize, Ordering};

    static ATTEMPTS: AtomicUsize = AtomicUsize::new(0);
    ATTEMPTS.store(0, Ordering::SeqCst);
    let app = Router::new().route(
        "/models",
        get(|| async {
            if ATTEMPTS.fetch_add(1, Ordering::SeqCst) == 0 {
                return (StatusCode::SERVICE_UNAVAILABLE, "busy").into_response();
            }
            axum::Json(json!({"data":[{"id":"recovered"}]})).into_response()
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let models =
        fetch_models_once(&format!("{base}/models"), "fixture-key", Duration::from_secs(5)).await.unwrap();
    assert_eq!(models, vec!["recovered".to_string()]);
    assert_eq!(ATTEMPTS.load(Ordering::SeqCst), 2);
    server.abort();
}

#[test]
fn provider_models_cache_expires_failures_much_sooner_than_successes() {
    let key = "https://cache-fixture.example.com/v1|CACHE_FIXTURE_KEY";
    let aged = Instant::now() - PROVIDER_MODELS_ERROR_TTL - Duration::from_secs(1);
    provider_models_cache().lock().unwrap().insert(
        key.to_string(),
        ProviderModelsEntry { fetched_at: aged, models: vec!["model-a".into()], error: None },
    );
    // 成功结果在 10 分钟内仍然有效，不会因为一次失败就重拉。
    assert!(cached_provider_models(key).is_some());
    provider_models_cache().lock().unwrap().insert(
        key.to_string(),
        ProviderModelsEntry { fetched_at: aged, models: Vec::new(), error: Some("boom".into()) },
    );
    // 失败结果只缓存 30 秒：上游恢复后下一次请求就会重试。
    assert!(cached_provider_models(key).is_none());
    provider_models_cache().lock().unwrap().remove(key);
}
