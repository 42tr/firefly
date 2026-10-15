//! Discover provider limits rather than silently assigning a default window.
use std::{
    collections::HashMap,
    sync::{Arc, Mutex, OnceLock},
    time::{Duration, Instant},
};

use futures_util::StreamExt;
use g::ModelError;
use reqwest::Url;
use scraper::{Html, Selector};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use sqlx::{types::Json, SqlitePool};

#[derive(Clone)]
pub struct ModelBudget {
    source: Option<Arc<Query>>,
    cap: Option<usize>,
}

struct Query {
    base: String,
    key: String,
    model: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct Limits {
    window: usize,
    input: Option<usize>,
    output: Option<usize>,
    source: String,
}

struct Cached {
    at: Instant,
    result: Result<Limits, String>,
}
type Entry = Arc<tokio::sync::Mutex<Option<Cached>>>;
static CACHE: OnceLock<Mutex<HashMap<[u8; 32], Entry>>> = OnceLock::new();

impl ModelBudget {
    pub fn queried(base: &str, key: &str, model: &str, cap: Option<usize>) -> Self {
        Self {
            source: Some(Arc::new(Query {
                base: base.trim_end_matches('/').into(),
                key: key.into(),
                model: model.into(),
            })),
            cap,
        }
    }

    #[cfg(test)]
    pub(crate) fn fixed(cap: usize) -> Self {
        Self {
            source: None,
            cap: Some(cap),
        }
    }

    pub async fn resolve(&self) -> Result<usize, ModelError> {
        if self.cap.is_some_and(|cap| cap < 1024) {
            return Err(ModelError::new("显式上下文预算至少为 1024，请检查 Agent 配置中的输入预算"));
        }
        let Some(query) = &self.source else {
            return Ok(self.cap.expect("fixed budget"));
        };
        match query.cached().await {
            Ok(limits) => {
                let budget = input_budget(&limits, self.cap).map_err(ModelError::new)?;
                tracing::debug!(model = query.model, source = limits.source, window = limits.window,
                    output_reserve = ?limits.output, budget, "model input budget resolved");
                Ok(budget)
            }
            Err(reason) => match self.cap {
                Some(cap) => {
                    tracing::warn!(model = query.model, budget = cap, reason,
                        "model limits unavailable; using explicitly configured input budget");
                    Ok(cap)
                }
                None => Err(ModelError::new(format!(
                    "无法查询模型 {} 的上下文限制：{reason}。请检查模型信息接口，或在 Agent 配置中填写输入预算", query.model))),
            },
        }
    }
}

fn input_budget(limits: &Limits, cap: Option<usize>) -> Result<usize, String> {
    // Leave room for the provider's maximum output without imposing a request cap.
    // If metadata omits it, the safety margin below still reserves context space.
    let output = limits.output.unwrap_or_default();
    if output >= limits.window {
        return Err("查询到的模型最大输出长度不小于上下文窗口，请检查模型元数据".into());
    }
    let available = (limits.window - output).min(limits.input.unwrap_or(usize::MAX));
    let budget = available.saturating_sub((available / 20).max(64));
    let budget = budget.min(cap.unwrap_or(usize::MAX));
    if budget < 1024 {
        return Err("扣除输出空间及安全余量后，上下文预算不足 1024".into());
    }
    Ok(budget)
}

impl Query {
    fn cache_key(&self) -> [u8; 32] {
        // Length-delimited fields prevent ambiguous keys; credentials never appear in logs.
        let mut hash = Sha256::new();
        for field in [&self.base, &self.model, &self.key] {
            hash.update(field.len().to_le_bytes());
            hash.update(field.as_bytes());
        }
        hash.finalize().into()
    }

    async fn cached(&self) -> Result<Limits, String> {
        let key = self.cache_key();
        let entry = {
            let mut cache = CACHE.get_or_init(Mutex::default).lock().unwrap();
            if !cache.contains_key(&key) && cache.len() >= 128 {
                // Evict idle entries only: active queries must remain coalesced.
                if let Some(idle) = cache
                    .iter()
                    .find(|(_, value)| Arc::strong_count(value) == 1)
                    .map(|(key, _)| *key)
                {
                    cache.remove(&idle);
                }
            }
            cache
                .entry(key)
                .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(None)))
                .clone()
        };
        let mut cached = entry.lock().await;
        if let Some(value) = cached.as_ref() {
            let ttl = if value.result.is_ok() {
                Duration::from_secs(3600)
            } else {
                Duration::from_secs(30)
            };
            if value.at.elapsed() < ttl {
                return value.result.clone();
            }
        }
        // Persistent successes have no TTL: a restart or memory eviction must
        // not turn known limits into a dependency on provider/docs availability.
        let pool = crate::db::maybe_pool();
        if let Some(pool) = pool {
            match load_limits(pool, &key).await {
                Ok(Some(limits)) => {
                    tracing::debug!(
                        model = self.model,
                        source = limits.source,
                        window = limits.window,
                        "model limits loaded from database"
                    );
                    *cached = Some(Cached {
                        at: Instant::now(),
                        result: Ok(limits.clone()),
                    });
                    return Ok(limits);
                }
                Ok(None) => {}
                Err(error) => tracing::warn!(model = self.model, %error,
                    "failed to read persisted model limits; querying provider"),
            }
        }
        let result = self.fetch().await;
        // Persist only usable metadata; resolve still reports invalid provider
        // limits rather than silently replacing them with a configured cap.
        let valid = result
            .as_ref()
            .ok()
            .filter(|limits| input_budget(limits, None).is_ok());
        if let (Some(pool), Some(limits)) = (pool, valid) {
            if let Err(error) = save_limits(pool, &key, limits).await {
                tracing::warn!(model = self.model, %error,
                    "failed to persist model limits; using queried limits");
            }
        }
        match &result {
            Ok(limits) => {
                tracing::info!(model = self.model, source = limits.source, window = limits.window,
                max_input = ?limits.input, max_output = ?limits.output, "model limits discovered")
            }
            Err(reason) => {
                tracing::warn!(model = self.model, reason, "model limits discovery failed")
            }
        }
        *cached = Some(Cached {
            at: Instant::now(),
            result: result.clone(),
        });
        result
    }

    async fn fetch(&self) -> Result<Limits, String> {
        let base = Url::parse(&self.base).map_err(|_| "模型服务地址无效")?;
        if !matches!(base.scheme(), "http" | "https") {
            return Err("模型服务必须使用 HTTP 或 HTTPS".into());
        }
        let mut list = base.clone();
        list.set_path(&format!("{}/models", base.path().trim_end_matches('/')));
        list.set_query(None);
        let mut detail = list.clone();
        detail
            .path_segments_mut()
            .map_err(|_| "模型服务地址不支持路径")?
            .push(&self.model);
        let mut urls = vec![detail, list.clone()];
        let mut root_list = base.clone();
        root_list.set_path("/v1/models");
        root_list.set_query(None);
        if root_list != list {
            urls.push(root_list);
        }
        let aliyun = aliyun_host(base.host_str().unwrap_or_default());
        if aliyun {
            let mut native = base.clone();
            native.set_path("/api/v1/models");
            native.set_query(None);
            native
                .query_pairs_mut()
                .append_pair("model", &self.model)
                .append_pair("page_size", "100");
            urls.push(native);
        }
        let mut listed = false;
        for url in urls {
            if let Ok(body) = get(url.clone(), Some(&self.key)).await {
                if let Ok(value) = serde_json::from_slice::<Value>(&body) {
                    if let Some(info) = model_info(&value, &self.model) {
                        listed = true;
                        if let Some(limits) = parse_limits(info, url.to_string()) {
                            return Ok(limits);
                        }
                    }
                }
            }
        }
        // Public official docs are a source of metadata, not a table of guessed windows.
        // Only use them after this provider has confirmed the exact model ID.
        if aliyun
            && listed
            && self
                .model
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'))
        {
            let slug = self.model.to_ascii_lowercase().replace(['.', '_'], "-");
            let url =
                Url::parse(&format!("https://help.aliyun.com/zh/model-studio/{slug}")).unwrap();
            let body = get(url.clone(), None)
                .await
                .map_err(|_| "模型服务未返回窗口字段，官方模型文档查询失败")?;
            return parse_aliyun_doc(
                &String::from_utf8_lossy(&body),
                &self.model,
                url.to_string(),
            )
            .ok_or_else(|| "模型服务未返回窗口字段，官方文档未提供匹配模型的上下文限制".into());
        }
        Err("模型信息接口未返回匹配模型的上下文窗口".into())
    }
}

async fn load_limits(pool: &SqlitePool, key: &[u8; 32]) -> anyhow::Result<Option<Limits>> {
    let stored: Option<Json<Limits>> =
        sqlx::query_scalar("SELECT limits FROM model_limits_cache WHERE cache_key=?1")
            .bind(key.as_slice())
            .fetch_optional(pool)
            .await?;
    stored
        .map(|Json(limits)| {
            input_budget(&limits, None).map_err(anyhow::Error::msg)?;
            Ok(limits)
        })
        .transpose()
}

async fn save_limits(pool: &SqlitePool, key: &[u8; 32], limits: &Limits) -> anyhow::Result<()> {
    sqlx::query("INSERT INTO model_limits_cache(cache_key,limits) VALUES (?1,?2)
        ON CONFLICT(cache_key) DO UPDATE SET limits=excluded.limits, discovered_at=excluded.discovered_at")
        .bind(key.as_slice())
        .bind(Json(limits))
        .execute(pool)
        .await?;
    Ok(())
}

fn aliyun_host(host: &str) -> bool {
    host.ends_with(".maas.aliyuncs.com")
        || host == "dashscope.aliyuncs.com"
        || host == "dashscope-intl.aliyuncs.com"
        || host.ends_with(".dashscope.aliyuncs.com")
        || host.ends_with(".dashscope-intl.aliyuncs.com")
}

async fn get(url: Url, key: Option<&str>) -> Result<Vec<u8>, String> {
    let mut request = crate::http_client::client()
        .get(url)
        .timeout(Duration::from_secs(8));
    if let Some(key) = key {
        request = request.bearer_auth(key);
    }
    let response = request.send().await.map_err(|_| "模型信息查询连接失败")?;
    if !response.status().is_success() {
        return Err(format!(
            "模型信息接口返回 HTTP {}",
            response.status().as_u16()
        ));
    }
    let mut stream = response.bytes_stream();
    let mut body = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| "模型信息响应读取失败")?;
        if body.len().saturating_add(chunk.len()) > 4 * 1024 * 1024 {
            return Err("模型信息响应过大".into());
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

fn model_info<'a>(value: &'a Value, model: &str) -> Option<&'a Value> {
    fn matches(value: &Value, model: &str) -> bool {
        value
            .get("id")
            .or_else(|| value.get("model"))
            .and_then(Value::as_str)
            == Some(model)
    }
    if matches(value, model) {
        return Some(value);
    }
    for path in ["/data", "/output/models", "/models"] {
        if let Some(array) = value.pointer(path).and_then(Value::as_array) {
            if let Some(info) = array.iter().find(|info| matches(info, model)) {
                return Some(info);
            }
        }
    }
    value.get("data").filter(|value| matches(value, model))
}

fn positive(value: &Value) -> Option<usize> {
    value
        .as_u64()
        .or_else(|| value.as_str()?.parse().ok())
        .and_then(|n| usize::try_from(n).ok())
        .filter(|n| *n > 0)
}

fn parse_limits(info: &Value, source: String) -> Option<Limits> {
    fn minimum(info: &Value, fields: &[&str]) -> Option<usize> {
        std::iter::once(info)
            .chain(
                ["model_info", "limits", "metadata", "top_provider"]
                    .iter()
                    .filter_map(|name| info.get(name)),
            )
            .flat_map(|node| {
                fields
                    .iter()
                    .filter_map(move |name| node.get(name).and_then(positive))
            })
            .min()
    }
    Some(Limits {
        window: minimum(
            info,
            &[
                "context_window",
                "context_length",
                "max_context_length",
                "max_model_len",
            ],
        )?,
        input: minimum(
            info,
            &[
                "max_input_tokens",
                "max_input_length",
                "reasoning_max_input_tokens",
            ],
        ),
        output: minimum(
            info,
            &[
                "max_output_tokens",
                "max_completion_tokens",
                "reasoning_max_output_tokens",
            ],
        ),
        source,
    })
}

fn parse_aliyun_doc(page: &str, model: &str, source: String) -> Option<Limits> {
    let props = page.split_once("window.__ICE_PAGE_PROPS__=")?.1;
    let value = serde_json::Deserializer::from_str(props)
        .into_iter::<Value>()
        .next()?
        .ok()?;
    let content = value
        .pointer("/docDetailData/storeData/data/content")?
        .as_str()?;
    let html = Html::parse_fragment(content);
    let selector = |query| Selector::parse(query).unwrap();
    // Confirm exact identity; a redirect or similar model must not supply the limits.
    if !html
        .select(&selector("code"))
        .any(|node| node.text().collect::<String>().trim() == model)
    {
        return None;
    }
    let mut window = None;
    let mut input = None;
    let mut output = None;
    for row in html.select(&selector("tr")) {
        let cells: Vec<_> = row
            .select(&selector("td, th"))
            .map(|node| node.text().collect::<String>())
            .collect();
        for pair in cells.chunks_exact(2) {
            let number = pair[1]
                .trim()
                .replace([',', ' '], "")
                .parse::<usize>()
                .ok()
                .filter(|n| *n > 0);
            let slot = match pair[0].trim() {
                "上下文长度" => &mut window,
                "最大输入长度" | "最大输入长度（思考模式下）" => &mut input,
                "最大输出长度" | "最大输出长度（思考模式下）" => &mut output,
                _ => continue,
            };
            if let Some(number) = number {
                *slot = Some(slot.unwrap_or(usize::MAX).min(number));
            }
        }
    }
    Some(Limits {
        window: window?,
        input,
        output,
        source,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn exact_model_and_native_nullable_limits() {
        let data = json!({"output":{"models":[{"model":"other","model_info":{"context_window":999999}},
            {"model":"test","model_info":{"context_window":100000,"max_input_tokens":90000,
                "max_output_tokens":8000,"reasoning_max_input_tokens":null}}]}});
        let limits = parse_limits(model_info(&data, "test").unwrap(), "fixture".into()).unwrap();
        assert_eq!(limits.window, 100000);
        assert_eq!(input_budget(&limits, None).unwrap(), 85500);
        assert_eq!(input_budget(&limits, Some(20000)).unwrap(), 20000);
        assert!(model_info(&data, "unknown").is_none());
        assert!(parse_limits(&json!({"max_input_tokens":90000}), "fixture".into()).is_none());
        let limits = Limits {
            window: 1000000,
            input: None,
            output: None,
            source: "fixture".into(),
        };
        assert_eq!(input_budget(&limits, None).unwrap(), 950000);
        assert_eq!(input_budget(&limits, Some(2000000)).unwrap(), 950000);
        let limits = Limits {
            output: Some(200000),
            ..limits
        };
        assert_eq!(input_budget(&limits, None).unwrap(), 760000);
        assert!(input_budget(
            &Limits {
                output: Some(1000000),
                ..limits
            },
            None
        )
        .is_err());
    }

    #[test]
    fn official_doc_requires_exact_model_and_extracts_table() {
        let props = json!({"docDetailData":{"storeData":{"data":{"content":
            "<code>test-model</code><table><tr><td>上下文长度</td><td>100,000</td><td>最大输入长度</td><td>90000</td></tr><tr><td>最大输出长度</td><td>8000</td></tr></table>"}}}});
        let page = format!("<script>window.__ICE_PAGE_PROPS__={props};otherScript()</script>");
        let limits = parse_aliyun_doc(&page, "test-model", "fixture".into()).unwrap();
        assert_eq!(
            (limits.window, limits.input, limits.output),
            (100000, Some(90000), Some(8000))
        );
        assert!(parse_aliyun_doc(&page, "other-model", "fixture".into()).is_none());
        assert!(!aliyun_host("dashscope.aliyuncs.com.evil.example"));
    }

    #[tokio::test]
    async fn discovery_coalesces_and_reserves_provider_output_with_per_caller_input_caps() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let calls = Arc::new(AtomicUsize::new(0));
        let count = calls.clone();
        let app = axum::Router::new().route(
            "/models/test",
            axum::routing::get(move |headers: axum::http::HeaderMap| {
                let count = count.clone();
                async move {
                    assert!(matches!(
                        headers["authorization"].to_str().unwrap(),
                        "Bearer fixture-key" | "Bearer fixture-key2"
                    ));
                    count.fetch_add(1, Ordering::SeqCst);
                    axum::Json(
                        json!({"id":"test","context_window":100000,"max_output_tokens":10000}),
                    )
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let first = ModelBudget::queried(&base, "fixture-key", "test", None);
        let second = ModelBudget::queried(&base, "fixture-key", "test", Some(20000));
        let (a, b) = tokio::join!(first.resolve(), second.resolve());
        assert_eq!(a.unwrap(), 85500);
        assert_eq!(b.unwrap(), 20000);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        let different_account = ModelBudget::queried(&base, "fixture-key2", "test", None);
        assert_eq!(different_account.resolve().await.unwrap(), 85500);
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        let other_key = ModelBudget::queried(&base, "fixture-key2", "missing", Some(20000));
        assert_eq!(other_key.resolve().await.unwrap(), 20000);
        let no_default = ModelBudget::queried(&base, "fixture-key2", "missing", None);
        assert!(no_default.resolve().await.is_err());
        server.abort();
    }

    #[tokio::test]
    async fn persisted_limits_survive_restart_and_provider_outage_without_expiring() {
        use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
        let directory = tempfile::tempdir().unwrap();
        let url = format!(
            "sqlite://{}",
            directory.path().join("limits.sqlite3").display()
        );
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(crate::db::options(&url, true).unwrap())
            .await
            .unwrap();
        crate::db::MIGRATOR.run(&pool).await.unwrap();
        crate::db::set_test_pool(pool.clone());

        let calls = Arc::new(AtomicUsize::new(0));
        let count = calls.clone();
        let online = Arc::new(AtomicBool::new(true));
        let serving = online.clone();
        let app = axum::Router::new().route(
            "/models/test",
            axum::routing::get(move || {
                let count = count.clone();
                let serving = serving.clone();
                async move {
                    count.fetch_add(1, Ordering::SeqCst);
                    if !serving.load(Ordering::SeqCst) {
                        return Err(axum::http::StatusCode::SERVICE_UNAVAILABLE);
                    }
                    Ok(axum::Json(json!({"id":"test","context_window":100000,
                    "max_input_tokens":90000,"max_output_tokens":10000})))
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        // A caller's cap/fallback is not provider metadata and must not be saved.
        let capped = ModelBudget::queried(&base, "fixture-key", "test", Some(20000));
        assert_eq!(capped.resolve().await.unwrap(), 20000);
        let uncapped = ModelBudget::queried(&base, "fixture-key", "test", None);
        let key = uncapped.source.as_ref().unwrap().cache_key();
        let stored = load_limits(&pool, &key).await.unwrap().unwrap();
        assert_eq!(
            (stored.window, stored.input, stored.output),
            (100000, Some(90000), Some(10000))
        );

        // Even after the memory TTL expires, old durable metadata avoids HTTP.
        sqlx::query("UPDATE model_limits_cache SET discovered_at=1")
            .execute(&pool)
            .await
            .unwrap();
        {
            let entry = CACHE
                .get()
                .unwrap()
                .lock()
                .unwrap()
                .get(&key)
                .unwrap()
                .clone();
            entry.lock().await.as_mut().unwrap().at = Instant::now() - Duration::from_secs(3601);
        }
        assert_eq!(uncapped.resolve().await.unwrap(), 85500);
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        // Simulate a fresh process using the same file with no memory cache.
        pool.close().await;
        CACHE.get().unwrap().lock().unwrap().remove(&key);
        online.store(false, Ordering::SeqCst);
        server.abort();
        let _ = server.await;
        let reopened = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(crate::db::options(&url, false).unwrap())
            .await
            .unwrap();
        crate::db::set_test_pool(reopened.clone());
        assert_eq!(uncapped.resolve().await.unwrap(), 85500);
        assert_eq!(capped.resolve().await.unwrap(), 20000);
        assert_eq!(
            ModelBudget::queried(&base, "fixture-key", "test", Some(200000))
                .resolve()
                .await
                .unwrap(),
            85500
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        // Neither a different provider/model/credential nor explicit fallbacks
        // may reuse or populate this successful record while HTTP is offline.
        for other in [
            ModelBudget::queried(&format!("{base}/other"), "fixture-key", "test", None),
            ModelBudget::queried(&base, "fixture-key", "other", None),
            ModelBudget::queried(&base, "other-key", "test", None),
        ] {
            assert!(other.resolve().await.is_err());
        }
        assert_eq!(
            ModelBudget::queried(&base, "other-key", "test", Some(10000))
                .resolve()
                .await
                .unwrap(),
            10000
        );
        let count: i64 = sqlx::query_scalar("SELECT count(*) FROM model_limits_cache")
            .fetch_one(&reopened)
            .await
            .unwrap();
        assert_eq!(count, 1);
    }

    #[tokio::test]
    async fn invalid_persisted_limits_are_replaced_by_valid_provider_metadata() {
        let pool = crate::db::test_pool().await;
        crate::db::set_test_pool(pool.clone());
        let app = axum::Router::new()
            .route(
                "/models/test",
                axum::routing::get(|| async {
                    axum::Json(
                        json!({"id":"test","context_window":100000,"max_output_tokens":10000}),
                    )
                }),
            )
            .route(
                "/models/invalid",
                axum::routing::get(|| async {
                    axum::Json(
                        json!({"id":"invalid","context_window":100000,"max_output_tokens":100000}),
                    )
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let budget = ModelBudget::queried(&base, "fixture-key", "test", None);
        let key = budget.source.as_ref().unwrap().cache_key();
        let invalid = Limits {
            window: 100000,
            input: None,
            output: Some(100000),
            source: "invalid".into(),
        };
        save_limits(&pool, &key, &invalid).await.unwrap();
        assert_eq!(budget.resolve().await.unwrap(), 85500);
        let stored = load_limits(&pool, &key).await.unwrap().unwrap();
        assert_eq!(stored.output, Some(10000));
        assert_eq!(stored.source, format!("{base}/models/test"));
        let invalid_budget = ModelBudget::queried(&base, "fixture-key", "invalid", Some(20000));
        assert!(invalid_budget.resolve().await.is_err());
        let invalid_key = invalid_budget.source.as_ref().unwrap().cache_key();
        assert!(load_limits(&pool, &invalid_key).await.unwrap().is_none());
        server.abort();
    }

    #[tokio::test]
    #[ignore = "read-only live model metadata check; requires FIREFLY_MODEL_CONTEXT_CHECK_ENV"]
    async fn live_model_metadata() {
        dotenvy::from_path(std::env::var("FIREFLY_MODEL_CONTEXT_CHECK_ENV").unwrap()).unwrap();
        let model = std::env::var("OPENAI_MODEL").unwrap();
        let query = Query {
            base: std::env::var("OPENAI_BASE_URL").unwrap(),
            key: crate::llm::normalize_bearer_secret(&std::env::var("OPENAI_API_KEY").unwrap()),
            model,
        };
        let limits = query.cached().await.unwrap();
        let budget = input_budget(&limits, None).unwrap();
        println!(
            "model={} window={} input_budget={} source={}",
            query.model, limits.window, budget, limits.source
        );
        assert!(budget >= 1024);
    }
}
