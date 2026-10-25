//! Codex provider and model catalog discovery.
use std::{
    collections::HashMap,
    path::Path,
    sync::Mutex,
    time::{Duration, Instant},
};
use anyhow::anyhow;
use futures_util::future::join_all;
use serde::Serialize;
use serde_json::{json, Value};
use tokio::time::timeout;

use crate::{config, http_client, llm};
use firefly_codex_transport::app_server;

pub(crate) const PROVIDER_MODELS_TIMEOUT: Duration = Duration::from_secs(10);
pub(crate) const PROVIDER_MODELS_RETRY_BACKOFF: Duration = Duration::from_millis(300);
pub(crate) const PROVIDER_MODELS_CACHE_TTL: Duration = Duration::from_secs(600);
pub(crate) const PROVIDER_MODELS_ERROR_TTL: Duration = Duration::from_secs(30);
pub(crate) const PROVIDER_MODELS_LIMIT: usize = 200;

#[derive(Debug, Serialize)]
pub struct ModelProvider {
    pub name: String,
    pub base_url: Option<String>,
    /// API key 所在的环境变量名（config.toml 的 env_key），用于请求该平台的 /models。
    pub env_key: Option<String>,
    /// 该平台下前端可选的模型：优先取 CODEX_PROVIDER_MODELS 配置，否则来自平台 /models 接口；
    /// 为空表示该平台没有可用列表，前端回退为手工输入。
    pub models: Vec<String>,
    /// 最近一次 /models 拉取的失败原因；None 表示拉取成功，或该平台使用手工配置的模型列表。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub models_error: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ModelProviderListing {
    /// config.toml 中配置的全局默认 model_provider。
    pub default: Option<String>,
    /// config.toml 中配置的全局默认 model。
    pub default_model: Option<String>,
    pub default_reasoning_level: Option<String>,
    pub providers: Vec<ModelProvider>,
}

#[derive(Debug, Serialize)]
pub struct OfficialModel {
    pub model: String,
    pub reasoning_levels: Vec<String>,
    pub default_reasoning_level: Option<String>,
}

impl OfficialModel {
    pub(crate) fn from_catalog(item: &Value) -> Option<Self> {
        let model = item["model"].as_str()?.trim();
        if model.is_empty() || item["hidden"].as_bool() == Some(true) {
            return None;
        }
        let reasoning_levels = item["supportedReasoningEfforts"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|effort| effort["reasoningEffort"].as_str())
            .map(ToOwned::to_owned)
            .collect();
        Some(Self {
            model: model.to_owned(),
            reasoning_levels,
            default_reasoning_level: item["defaultReasoningEffort"].as_str().map(ToOwned::to_owned),
        })
    }
}

/// 解析 `CODEX_PROVIDER_MODELS`（`provider=m1,m2;provider2=m3`）为 provider → 模型列表。
pub(crate) fn configured_provider_models() -> HashMap<String, Vec<String>> {
    let mut map = HashMap::new();
    // 单元测试等场景下配置未加载，按未配置处理。
    let Some(config) = config::try_get() else { return map };
    for entry in config.codex_provider_models.split(';') {
        let Some((provider, models)) = entry.split_once('=') else { continue };
        let provider = provider.trim();
        if provider.is_empty() {
            continue;
        }
        let models =
            models.split(',').map(str::trim).filter(|model| !model.is_empty()).map(ToOwned::to_owned).collect();
        map.insert(provider.to_string(), models);
    }
    map
}

/// 读取 ~/.codex/config.toml 中的 `[model_providers.*]`，供前端按对话切换 API 平台。
/// 文件缺失或解析失败时返回空列表，调用方按“无可用平台”处理。
pub fn list_model_providers() -> ModelProviderListing {
    let Some(home) = std::env::var_os("HOME") else {
        return ModelProviderListing {
            default: None,
            default_model: None,
            default_reasoning_level: None,
            providers: Vec::new(),
        };
    };
    match std::fs::read_to_string(Path::new(&home).join(".codex/config.toml")) {
        Ok(content) => parse_model_providers(&content),
        Err(_) => ModelProviderListing {
            default: None,
            default_model: None,
            default_reasoning_level: None,
            providers: Vec::new(),
        },
    }
}

pub(crate) fn parse_model_providers(content: &str) -> ModelProviderListing {
    let empty = ModelProviderListing {
        default: None,
        default_model: None,
        default_reasoning_level: None,
        providers: Vec::new(),
    };
    let Ok(config) = toml::from_str::<toml::Value>(content) else { return empty };
    let default = config.get("model_provider").and_then(toml::Value::as_str).map(ToOwned::to_owned);
    let default_model = config.get("model").and_then(toml::Value::as_str).map(ToOwned::to_owned);
    let default_reasoning_level =
        config.get("model_reasoning_effort").and_then(toml::Value::as_str).map(ToOwned::to_owned);
    let configured_models = configured_provider_models();
    let mut providers = config
        .get("model_providers")
        .and_then(toml::Value::as_table)
        .map(|table| {
            table
                .iter()
                .map(|(name, value)| ModelProvider {
                    name: name.clone(),
                    base_url: value.get("base_url").and_then(toml::Value::as_str).map(ToOwned::to_owned),
                    env_key: value.get("env_key").and_then(toml::Value::as_str).map(ToOwned::to_owned),
                    models: configured_models.get(name).cloned().unwrap_or_default(),
                    models_error: None,
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    providers.sort_by(|left, right| left.name.cmp(&right.name));
    ModelProviderListing { default, default_model, default_reasoning_level, providers }
}

/// 一个平台最近一次 /models 拉取的结果：`error` 为 Some 时表示这次拉取失败，
/// `models` 保留上一次成功的结果，避免一次抖动就把下拉框变空。
#[derive(Clone)]
pub(crate) struct ProviderModelsEntry {
    pub(crate) fetched_at: Instant,
    pub(crate) models: Vec<String>,
    pub(crate) error: Option<String>,
}

type ProviderModelsCache = Mutex<HashMap<String, ProviderModelsEntry>>;

/// provider → 缓存的 /models 结果。key 为 `base_url|env_key`，与环境变量解耦，便于测试。
pub(crate) static PROVIDER_MODELS_CACHE: std::sync::OnceLock<ProviderModelsCache> = std::sync::OnceLock::new();

pub(crate) fn provider_models_cache() -> &'static ProviderModelsCache {
    PROVIDER_MODELS_CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// 读取仍然有效的缓存条目：成功结果用长 TTL，失败结果只用短 TTL，让上游恢复后能很快重试。
pub(crate) fn cached_provider_models(cache_key: &str) -> Option<ProviderModelsEntry> {
    let cache = provider_models_cache().lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let entry = cache.get(cache_key)?;
    let ttl = if entry.error.is_some() { PROVIDER_MODELS_ERROR_TTL } else { PROVIDER_MODELS_CACHE_TTL };
    (entry.fetched_at.elapsed() < ttl).then(|| entry.clone())
}

/// 请求 OpenAI 兼容平台的模型列表；失败返回可展示给用户的原因，不再把失败伪装成“没有模型”。
///
/// 依次尝试 `{base_url}/models` 与 `{origin}/v1/models`：部分中转站（如 Codex 后端中继）
/// 的 base_url 只实现 Responses API，`/models` 会返回网站 HTML，而网关根路径的
/// `/v1/models` 才是标准的 OpenAI 兼容模型列表。
pub(crate) async fn fetch_provider_models(base_url: &str, env_key: &str) -> Result<Vec<String>, String> {
    let api_key = std::env::var(env_key)
        .map(|key| llm::normalize_bearer_secret(&key))
        .map_err(|_| format!("环境变量 {env_key} 未设置"))?;
    if api_key.is_empty() {
        return Err(format!("环境变量 {env_key} 为空"));
    }
    let base = base_url.trim_end_matches('/');
    let mut candidates = vec![format!("{base}/models")];
    if let Ok(parsed) = reqwest::Url::parse(base) {
        let origin = parsed.origin().ascii_serialization();
        if origin != "null" {
            let fallback = format!("{origin}/v1/models");
            if fallback != candidates[0] {
                candidates.push(fallback);
            }
        }
    }
    // 平台确实返回了空列表时不算失败，只有所有候选地址都失败才报错。
    let mut empty_success = false;
    let mut first_error = None;
    for url in candidates {
        match fetch_models_once(&url, &api_key, PROVIDER_MODELS_TIMEOUT).await {
            Ok(models) if models.is_empty() => empty_success = true,
            Ok(models) => return Ok(models),
            Err(err) => {
                first_error.get_or_insert(err.message);
            }
        }
    }
    match (empty_success, first_error) {
        (true, _) => Ok(Vec::new()),
        (false, Some(message)) => Err(message),
        (false, None) => Err("没有可用的模型列表地址".to_string()),
    }
}

/// /models 拉取失败：区分瞬时失败（可重试）与确定性失败（换下一个候选地址）。
#[derive(Debug, Clone)]
pub(crate) struct ModelsFetchError {
    pub(crate) message: String,
    pub(crate) transient: bool,
}

impl ModelsFetchError {
    fn transient(message: impl Into<String>) -> Self {
        Self { message: message.into(), transient: true }
    }

    fn permanent(message: impl Into<String>) -> Self {
        Self { message: message.into(), transient: false }
    }
}

/// 单次请求 /models：只有「2xx 且 body 是含 data 数组的 JSON」才算成功，避免把
/// 中转站回退返回的 HTML 页面误当作可用结果。瞬时失败重试一次，躲开上游抖动。
pub(crate) async fn fetch_models_once(
    url: &str, api_key: &str, timeout_budget: Duration,
) -> Result<Vec<String>, ModelsFetchError> {
    match fetch_models_attempt(url, api_key, timeout_budget).await {
        Err(err) if err.transient => {
            tokio::time::sleep(PROVIDER_MODELS_RETRY_BACKOFF).await;
            fetch_models_attempt(url, api_key, timeout_budget).await
        }
        result => result,
    }
}

/// 单次尝试，并把 HTTP 层的失败归类成瞬时 / 确定性两种。
pub(crate) async fn fetch_models_attempt(
    url: &str, api_key: &str, timeout_budget: Duration,
) -> Result<Vec<String>, ModelsFetchError> {
    let request = http_client::client().get(url).header("Authorization", format!("Bearer {api_key}")).send();
    let response = match timeout(timeout_budget, request).await {
        Err(_) => return Err(ModelsFetchError::transient(format!("请求 {url} 超时"))),
        Ok(Err(err)) => return Err(ModelsFetchError::transient(format!("请求 {url} 失败: {err}"))),
        Ok(Ok(response)) => response,
    };
    let status = response.status();
    if status.is_server_error() || status == reqwest::StatusCode::TOO_MANY_REQUESTS {
        return Err(ModelsFetchError::transient(format!("{url} 返回 {status}")));
    }
    if !status.is_success() {
        return Err(ModelsFetchError::permanent(format!("{url} 返回 {status}")));
    }
    let Ok(body) = response.json::<Value>().await else {
        return Err(ModelsFetchError::permanent(format!("{url} 未返回模型 JSON")));
    };
    let Some(data) = body["data"].as_array() else {
        return Err(ModelsFetchError::permanent(format!("{url} 返回的 JSON 缺少 data 数组")));
    };
    let mut models =
        data.iter().filter_map(|item| item["id"].as_str().map(ToOwned::to_owned)).collect::<Vec<_>>();
    models.sort();
    models.dedup();
    models.truncate(PROVIDER_MODELS_LIMIT);
    Ok(models)
}

/// 为没有手工配置模型列表的平台填充可选模型：请求其 /models 接口。成功结果缓存 10 分钟；
/// 失败只缓存 30 秒并把原因回传前端，避免一次抖动变成长时间选不到模型。
pub async fn hydrate_provider_models(listing: &mut ModelProviderListing) {
    let fetches = listing.providers.iter().map(|provider| {
        let name = provider.name.clone();
        let base_url = provider.base_url.clone();
        let env_key = provider.env_key.clone();
        async move {
            let (Some(base_url), Some(env_key)) = (base_url, env_key) else {
                return (name, Vec::new(), None);
            };
            let cache_key = format!("{base_url}|{env_key}");
            if let Some(entry) = cached_provider_models(&cache_key) {
                return (name, entry.models, entry.error);
            }
            let (models, error) = match fetch_provider_models(&base_url, &env_key).await {
                Ok(models) => (models, None),
                Err(message) => {
                    tracing::warn!(provider = %name, base_url = %base_url, error = %message, "读取平台模型列表失败");
                    (Vec::new(), Some(message))
                }
            };
            let mut cache = provider_models_cache().lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            // 拉取失败时保留上一次成功的列表，前端仍可下拉，同时展示失败提示。
            let previous = cache.get(&cache_key).map(|entry| entry.models.clone()).unwrap_or_default();
            let models = if models.is_empty() && error.is_some() { previous } else { models };
            cache.insert(
                cache_key,
                ProviderModelsEntry { fetched_at: Instant::now(), models: models.clone(), error: error.clone() },
            );
            (name, models, error)
        }
    });
    let resolved = join_all(fetches).await;
    for provider in &mut listing.providers {
        // CODEX_PROVIDER_MODELS 手工配置优先；未配置的平台才用 /models 结果。
        if !provider.models.is_empty() {
            continue;
        }
        if let Some((_, models, error)) = resolved.iter().find(|(name, _, _)| name == &provider.name) {
            provider.models = models.clone();
            provider.models_error = error.clone();
        }
    }
}

/// 官方模型目录由 Codex 提供，避免在前端维护容易过期的模型名称。
pub async fn list_official_models() -> anyhow::Result<Vec<OfficialModel>> {
    let client = app_server().await?;
    let mut models = Vec::new();
    let mut cursor = None::<String>;
    loop {
        let result =
            client.request("model/list", json!({ "limit": 100, "cursor": cursor, "includeHidden": false })).await?;
        let data = result["data"].as_array().ok_or_else(|| anyhow!("Codex 未返回模型列表"))?;
        for item in data {
            if let Some(model) = OfficialModel::from_catalog(item) {
                if !models.iter().any(|existing: &OfficialModel| existing.model == model.model) {
                    models.push(model);
                }
            }
        }
        let next = result["nextCursor"].as_str().map(ToOwned::to_owned);
        if next.is_none() || next == cursor || models.len() >= PROVIDER_MODELS_LIMIT {
            break;
        }
        cursor = next;
    }
    models.truncate(PROVIDER_MODELS_LIMIT);
    Ok(models)
}
