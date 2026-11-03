//! Provider model discovery for Agent settings. Never forward saved keys across origins.
use std::{collections::BTreeSet, time::Duration};
use axum::{Json, extract::DefaultBodyLimit, routing::post, Router};
use futures_util::{stream, StreamExt};
use serde::Deserialize;
use serde_json::Value;
use super::{auth::Session, error::AppError};
use crate::{agent_settings::Settings, db, llm};

mod aliyun;

#[derive(Deserialize)]
struct Request { base_url: String, api_key: Option<String>, profile_id: Option<String> }
type ModelInfo = crate::agent_settings::ProfileModel;

pub fn routes() -> Router {
    Router::new().route("/api/assistant/agent-settings/models", post(discover)).layer(DefaultBodyLimit::max(32 * 1024))
}
fn base_url(raw: &str) -> Result<reqwest::Url, AppError> {
    let url = reqwest::Url::parse(raw).map_err(|_| AppError::bad_request("模型服务地址无效"))?;
    if raw.len() > 2048 || !matches!(url.scheme(), "http" | "https") || url.host_str().is_none()
        || !url.username().is_empty() || url.password().is_some() || url.query().is_some() || url.fragment().is_some() {
        return Err(AppError::bad_request("模型服务地址须为不含凭据、查询或片段的 HTTP/HTTPS URL"));
    }
    Ok(url)
}
async fn discover(_: Session, Json(request): Json<Request>) -> Result<(axum::http::HeaderMap, Json<Value>), AppError> {
    let base = base_url(request.base_url.trim())?;
    if request.api_key.as_ref().is_some_and(|key| key.len() > 16384 || key.contains(['\r', '\n'])) {
        return Err(AppError::bad_request("API Key 格式无效"));
    }
    let key = if let Some(key) = request.api_key { llm::normalize_bearer_secret(&key) }
        else if let Some(id) = request.profile_id {
            Settings::load(db::pool()).await?.profiles.into_iter().find(|p| p.id == id)
                .filter(|p| reqwest::Url::parse(&p.base_url).ok().is_some_and(|old| old.origin() == base.origin()))
                .and_then(|p| p.api_key).unwrap_or_default()
        } else { String::new() };
    let client = reqwest::Client::builder().redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(10)).connect_timeout(Duration::from_secs(5)).build()
        .map_err(AppError::internal_server_error)?;
    let models = fetch_catalog(&client, base, &key).await.map_err(AppError::bad_request)?;
    let mut headers = axum::http::HeaderMap::new();
    headers.insert(axum::http::header::CACHE_CONTROL, "no-store".parse().unwrap());
    Ok((headers, Json(serde_json::json!({"models": models}))))
}
async fn get_json(client: &reqwest::Client, url: reqwest::Url, key: &str) -> Result<Value, String> {
    let mut request = client.get(url);
    if !key.is_empty() { request = request.bearer_auth(key); }
    let response = request.send().await.map_err(|_| "无法连接模型服务，请检查地址和网络".to_string())?;
    if !response.status().is_success() { return Err(format!("模型服务返回 HTTP {}", response.status().as_u16())); }
    let mut body = Vec::new();
    let mut bytes = response.bytes_stream();
    while let Some(chunk) = bytes.next().await {
        let chunk = chunk.map_err(|_| "读取模型信息失败".to_string())?;
        if body.len() + chunk.len() > 4 * 1024 * 1024 { return Err("模型信息响应过大".into()); }
        body.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&body).map_err(|_| "模型服务没有返回有效的 JSON".into())
}
fn positive(node: &Value, fields: &[&str]) -> Option<usize> {
    std::iter::once(node).chain(["model_info", "limits", "metadata", "top_provider"].iter().filter_map(|key| node.get(key)))
        .flat_map(|item| fields.iter().filter_map(move |field| item.get(*field)))
        .filter_map(|v| v.as_u64().or_else(|| v.as_str()?.parse().ok()))
        .filter_map(|v| usize::try_from(v).ok()).filter(|v| *v > 0 && *v <= 100_000_000).min()
}
fn info(id: String, value: &Value) -> ModelInfo {
    let context_length = positive(value, &["context_window", "context_length", "max_context_length", "max_model_len"]);
    let max_input = positive(value, &["max_input_tokens", "max_input_length", "reasoning_max_input_tokens"]);
    let output = positive(value, &["max_output_tokens", "max_completion_tokens", "reasoning_max_output_tokens"]);
    let input_budget = context_length.and_then(|window| {
        let available = window.checked_sub(output.unwrap_or(0))?.min(max_input.unwrap_or(usize::MAX));
        let budget = available.saturating_sub((available / 20).max(64));
        (budget >= 1024).then_some(budget)
    });
    let (reasoning_levels, default_reasoning_level) = reasoning(value);
    ModelInfo { id, context_length, input_budget, reasoning_levels, default_reasoning_level, ..Default::default() }
}

async fn enrich_limits(client: &reqwest::Client, base: &str, key: &str, mut model: ModelInfo) -> ModelInfo {
    let mut budget = g::ModelBudget::queried(client.clone(), base, key, &model.id, None);
    if let Some(pool) = db::maybe_pool() { budget = budget.with_limits_cache(pool.clone()); }
    if let Ok(limits) = budget.limits().await {
        if let Ok(input_budget) = limits.input_budget() {
            model.context_length = Some(limits.window);
            model.input_budget = Some(input_budget);
            model.context_source = Some(limits.source);
        }
    }
    model
}

fn reasoning(value: &Value) -> (Option<Vec<String>>, Option<String>) {
    if value.get("supports_reasoning").and_then(Value::as_bool) == Some(false)
        || value.pointer("/think_efforts/support").and_then(Value::as_bool) == Some(false) {
        return (Some(Vec::new()), None);
    }
    let levels = ["/think_efforts/valid_efforts", "/reasoning/effort/valid", "/supported_reasoning_efforts", "/supported_reasoning_levels", "/reasoning_levels"]
        .iter().find_map(|path| value.pointer(path).and_then(Value::as_array))
        .map(|levels| {
            let mut seen = BTreeSet::new();
            levels.iter().filter_map(|level| level.as_str().or_else(|| level.get("effort")?.as_str()))
                .filter(|level| !level.is_empty() && level.len() <= 64
                    && level.bytes().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, b'_' | b'-')))
                .filter(|level| seen.insert(*level)).take(32).map(str::to_owned).collect::<Vec<_>>()
        });
    let default = ["/think_efforts/default_effort", "/reasoning/effort/default", "/default_reasoning_level", "/default_reasoning_effort"]
        .iter().find_map(|path| value.pointer(path).and_then(Value::as_str))
        .filter(|level| levels.as_ref().is_some_and(|levels| levels.iter().any(|item| item == level)))
        .map(str::to_owned);
    (levels, default)
}
async fn fetch_catalog(client: &reqwest::Client, mut base: reqwest::Url, key: &str) -> Result<Vec<ModelInfo>, String> {
    let service_base = base.to_string();
    let host = base.host_str().unwrap_or_default();
    let aliyun = host.ends_with(".maas.aliyuncs.com") || host == "dashscope.aliyuncs.com"
        || host == "dashscope-intl.aliyuncs.com" || host.ends_with(".dashscope.aliyuncs.com")
        || host.ends_with(".dashscope-intl.aliyuncs.com");
    base.path_segments_mut().map_err(|_| "模型服务地址无效")?.pop_if_empty().push("models");
    // Some Responses-compatible relays expose the catalog at the origin's
    // /v1/models instead of beneath the configured Responses base path.
    // Never follow redirects or send credentials to a different origin.
    let mut fallback = base.clone();
    fallback.set_path("/v1/models");
    let first = get_json(client, base.clone(), key).await;
    let is_catalog = |value: &Value| value.get("data").or_else(|| value.get("models"))
        .or_else(|| value.pointer("/output/models")).is_some_and(Value::is_array);
    let should_retry = match &first {
        Ok(value) => !is_catalog(value),
        Err(error) => error == "模型服务返回 HTTP 404" || error == "模型服务返回 HTTP 405" || error == "模型服务没有返回有效的 JSON",
    };
    let list = if should_retry && fallback != base {
        let value = get_json(client, fallback.clone(), key).await?;
        base = fallback;
        value
    } else { first? };
    let entries = list.get("data").or_else(|| list.get("models")).or_else(|| list.pointer("/output/models"))
        .and_then(Value::as_array).ok_or("模型服务未返回模型列表，请检查 Base URL")?;
    if entries.len() > 5000 { return Err("模型数量过多，暂不支持超过 5000 个模型".into()); }
    let mut seen = BTreeSet::new();
    let mut models = Vec::new();
    for item in entries {
        let id = item.get("id").or_else(|| item.get("model")).and_then(Value::as_str).or_else(|| item.as_str());
        let Some(id) = id.filter(|s| !s.trim().is_empty() && s.len() <= 256 && !s.chars().any(char::is_control)) else { continue };
        if seen.insert(id.to_owned()) { models.push(info(id.to_owned(), item)); }
    }
    // Bound optional per-model requests, retaining all list entries and every detail
    // received before the deadline. Providers may have large catalogs.
    let requests = stream::iter(models.clone().into_iter().enumerate().filter(|(_, m)| m.input_budget.is_none() || m.reasoning_levels.is_none()).map(|(index, mut model)| {
        let client = client.clone(); let key = key.to_owned(); let mut url = base.clone(); let service_base = service_base.clone();
        async move {
            if let Ok(mut path) = url.path_segments_mut() { path.push(&model.id); } else { return (index, model); }
            let mut result = match get_json(&client, url, &key).await {
                Ok(detail) if detail.get("id").or_else(|| detail.get("model")).and_then(Value::as_str) == Some(model.id.as_str()) => {
                    let result = info(model.id.clone(), &detail);
                    model.context_length = result.context_length.or(model.context_length);
                    model.input_budget = result.input_budget.or(model.input_budget);
                    if result.reasoning_levels.is_some() {
                        model.reasoning_levels = result.reasoning_levels;
                        model.default_reasoning_level = result.default_reasoning_level.or(model.default_reasoning_level)
                            .filter(|level| model.reasoning_levels.as_ref().is_some_and(|levels| levels.contains(level)));
                    }
                    model
                }
                _ => model,
            };
            // Token Plan's compatible catalog only advertises IDs. Reuse the
            // runtime's native metadata / verified official-doc fallback.
            if aliyun && result.input_budget.is_none() {
                result = enrich_limits(&client, &service_base, &key, result).await;
            }
            (index, result)
        }
    })).buffer_unordered(8);
    let details = async {
        tokio::pin!(requests);
        let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
        while let Ok(Some((index, result))) = tokio::time::timeout_at(deadline, requests.next()).await {
            models[index] = result;
        }
    };
    let docs = async { if aliyun { Some(aliyun::load().await) } else { None } };
    let (_, docs) = tokio::join!(details, docs);
    if let Some(docs) = docs { docs.apply(&mut models); }
    Ok(models)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{extract::Path, routing::get};
    use serde_json::json;

    #[test]
    fn computes_budget_without_inventing_unknown_limits() {
        let a = info("a".into(), &json!({"context_length":100000,"max_output_tokens":10000,"max_input_tokens":85000}));
        assert_eq!(a.context_length, Some(100000));
        assert_eq!(a.input_budget, Some(80750));
        assert!(info("b".into(), &json!({"id":"b"})).context_length.is_none());
        assert!(info("c".into(), &json!({"context_window":4000,"max_output_tokens":5000})).input_budget.is_none());
        assert!(base_url("https://user:password@example.com/v1").is_err());
        assert!(base_url("https://example.com/v1?token=x").is_err());
    }

    #[test]
    fn reads_advertised_efforts_without_inventing_levels() {
        let kimi = info("k3.1-pro".into(), &json!({"think_efforts":{
            "support":true, "valid_efforts":["low","high","max"], "default_effort":"high"}}));
        assert_eq!(kimi.reasoning_levels.unwrap(), ["low", "high", "max"]);
        assert_eq!(kimi.default_reasoning_level.as_deref(), Some("high"));
        let nested = info("nested".into(), &json!({"reasoning":{"effort":{"valid":["low","max","low"],"default":"max"}}}));
        assert_eq!(nested.reasoning_levels.unwrap(), ["low", "max"]);
        assert_eq!(nested.default_reasoning_level.as_deref(), Some("max"));
        assert_eq!(reasoning(&json!({"supports_reasoning":false})), (Some(vec![]),None));
        assert_eq!(reasoning(&json!({"think_efforts":{"support":false,"valid_efforts":["high"]}})), (Some(vec![]),None));
        assert_eq!(reasoning(&json!({"supports_reasoning":true})), (None,None));
        assert_eq!(reasoning(&json!({"reasoning_levels":["high"],"default_reasoning_level":"medium"})), (Some(vec!["high".into()]),None));
    }

    #[tokio::test]
    async fn discovery_reuses_runtime_limits_with_provenance_and_safe_budget() {
        let pool = crate::db::test_pool().await;
        crate::db::set_test_pool(pool.clone());
        let app = Router::new().route("/v1/models/available", get(|| async {
            Json(json!({"id":"available", "context_window":1000000, "max_input_tokens":983616, "max_output_tokens":131072}))
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}/v1", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        let model = info("available".into(), &json!({"id":"available"}));
        let enriched = enrich_limits(&client, &base, "private-fixture", model.clone()).await;
        assert_eq!(enriched.context_length, Some(1000000));
        assert_eq!(enriched.input_budget, Some(825482));
        assert_eq!(enriched.context_source.as_deref(), Some(format!("{base}/models/available").as_str()));
        let count: i64 = sqlx::query_scalar("SELECT count(*) FROM model_limits_cache").fetch_one(&pool).await.unwrap();
        assert_eq!(count, 1);
        server.abort();
        let cached = enrich_limits(&client, &base, "private-fixture", model).await;
        assert_eq!(cached.context_length, enriched.context_length);
        assert_eq!(cached.context_source, enriched.context_source);
        assert!(g::ModelBudget::fixed(32000).limits().await.is_err());
    }

    #[tokio::test]
    async fn discovers_list_and_per_model_metadata_without_guessing() {
        let app = Router::new().route("/v1/models", get(|| async { Json(json!({"data":[{"id":"first","context_length":32000},{"id":"second"},{"id":"unknown"}]})) }))
            .route("/v1/models/{id}", get(|Path(id): Path<String>| async move {
                if id == "first" { Json(json!({"id":id,"think_efforts":{"support":true,"valid_efforts":["low","high","max"],"default_effort":"high"}})) }
                else if id == "second" { Json(json!({"id":id,"context_window":64000,"max_output_tokens":4000})) }
                else { Json(json!({"id":id})) }
            }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}/v1", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let result = fetch_catalog(&reqwest::Client::new(), base_url(&base).unwrap(), "key").await.unwrap();
        assert_eq!(result.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(), ["first", "second", "unknown"]);
        assert_eq!(result[0].context_length, Some(32000));
        assert_eq!(result[0].reasoning_levels.as_deref().unwrap(), ["low", "high", "max"]);
        assert_eq!(result[0].default_reasoning_level.as_deref(), Some("high"));
        assert_eq!(result[1].context_length, Some(64000));
        assert_eq!(result[1].input_budget, Some(57000));
        assert_eq!(result[2].context_length, None);
        server.abort();
    }
    #[tokio::test]
    async fn falls_back_to_origin_model_list_on_missing_relay_route() {
        let app = Router::new().route("/v1/models", get(|| async { Json(json!({"data":[{"id":"available","context_window":32000}]})) }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}/relay/v1", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let result = fetch_catalog(&reqwest::Client::new(), base_url(&base).unwrap(), "key").await.unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].id, "available");
        assert_eq!(result[0].context_length, Some(32000));
        server.abort();
    }

    #[tokio::test]
    async fn authentication_failure_does_not_try_another_path() {
        use std::sync::{Arc, atomic::{AtomicUsize, Ordering}};
        let calls = Arc::new(AtomicUsize::new(0));
        let seen = calls.clone();
        let app = Router::new()
            .route("/relay/v1/models", get(|| async { axum::http::StatusCode::UNAUTHORIZED }))
            .route("/v1/models", get(move || { let seen = seen.clone(); async move { seen.fetch_add(1, Ordering::SeqCst); Json(json!({"data":[]})) } }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}/relay/v1", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let result = fetch_catalog(&reqwest::Client::new(), base_url(&base).unwrap(), "key").await;
        assert!(matches!(result, Err(message) if message.contains("401")));
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        server.abort();
    }

}
