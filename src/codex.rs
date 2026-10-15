use std::{collections::HashMap,path::{Component,Path,PathBuf},sync::Mutex,time::{Duration,Instant}};
use anyhow::{anyhow, Context};
use futures_util::future::join_all;
use serde::Serialize;
use serde_json::{json, Value};
use tokio::{fs,sync::broadcast,time::timeout};

use crate::{config, http_client, llm};

use firefly_codex_transport::app_server;

const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);
/// /models 拉取超时：平台不可达时不拖慢 providers 接口。
const PROVIDER_MODELS_TIMEOUT: Duration = Duration::from_secs(10);
/// 瞬时失败（超时/连接错误/5xx/429）重试前的等待。
const PROVIDER_MODELS_RETRY_BACKOFF: Duration = Duration::from_millis(300);
/// /models 成功结果的进程内缓存时长。
const PROVIDER_MODELS_CACHE_TTL: Duration = Duration::from_secs(600);
/// /models 失败结果的缓存时长：只用于挡住短时间内的重复请求，避免一次网络抖动被放大成 10 分钟不可用。
const PROVIDER_MODELS_ERROR_TTL: Duration = Duration::from_secs(30);
/// 单个平台展示的模型数量上限，避免 relay 返回超长列表撑爆下拉框。
const PROVIDER_MODELS_LIMIT: usize = 200;


#[derive(Debug, Serialize)]
pub struct WorkspaceEntry {
    pub name: String,
    pub path: String,
}

#[derive(Debug, Serialize)]
pub struct WorkspaceListing {
    pub root: String,
    pub path: String,
    pub entries: Vec<WorkspaceEntry>,
}

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
    fn from_catalog(item: &Value) -> Option<Self> {
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
fn configured_provider_models() -> HashMap<String, Vec<String>> {
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

fn parse_model_providers(content: &str) -> ModelProviderListing {
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
struct ProviderModelsEntry {
    fetched_at: Instant,
    models: Vec<String>,
    error: Option<String>,
}

type ProviderModelsCache = Mutex<HashMap<String, ProviderModelsEntry>>;

/// provider → 缓存的 /models 结果。key 为 `base_url|env_key`，与环境变量解耦，便于测试。
static PROVIDER_MODELS_CACHE: std::sync::OnceLock<ProviderModelsCache> = std::sync::OnceLock::new();

fn provider_models_cache() -> &'static ProviderModelsCache {
    PROVIDER_MODELS_CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// 读取仍然有效的缓存条目：成功结果用长 TTL，失败结果只用短 TTL，让上游恢复后能很快重试。
fn cached_provider_models(cache_key: &str) -> Option<ProviderModelsEntry> {
    let cache = provider_models_cache().lock().unwrap();
    let entry = cache.get(cache_key)?;
    let ttl = if entry.error.is_some() { PROVIDER_MODELS_ERROR_TTL } else { PROVIDER_MODELS_CACHE_TTL };
    (entry.fetched_at.elapsed() < ttl).then(|| entry.clone())
}

/// 请求 OpenAI 兼容平台的模型列表；失败返回可展示给用户的原因，不再把失败伪装成“没有模型”。
///
/// 依次尝试 `{base_url}/models` 与 `{origin}/v1/models`：部分中转站（如 Codex 后端中继）
/// 的 base_url 只实现 Responses API，`/models` 会返回网站 HTML，而网关根路径的
/// `/v1/models` 才是标准的 OpenAI 兼容模型列表。
async fn fetch_provider_models(base_url: &str, env_key: &str) -> Result<Vec<String>, String> {
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
struct ModelsFetchError {
    message: String,
    transient: bool,
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
async fn fetch_models_once(
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
async fn fetch_models_attempt(
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
            let mut cache = provider_models_cache().lock().unwrap();
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

pub async fn workspace_root() -> anyhow::Result<PathBuf> {
    let path = PathBuf::from(config::get().workspace_root.clone());
    fs::create_dir_all(&path).await.context("创建 Agent workspace 失败")?;
    Ok(fs::canonicalize(path).await?)
}

/// 校验 workspace 相对路径：不允许绝对路径和 `..` 组件。
pub(crate) fn validate_relative_path(path: &str) -> anyhow::Result<()> {
    if Path::new(path).is_absolute() {
        return Err(anyhow!("Agent 工作目录必须是 workspace 下的相对路径"));
    }
    for component in Path::new(path).components() {
        if !matches!(component, Component::Normal(_) | Component::CurDir) {
            return Err(anyhow!("Agent 工作目录包含非法路径组件"));
        }
    }
    Ok(())
}

pub async fn resolve_workspace_path(path: Option<&str>) -> anyhow::Result<(PathBuf, String)> {
    let root = workspace_root().await?;
    let relative = path.unwrap_or("").trim();
    validate_relative_path(relative)?;
    let candidate = if relative.is_empty() { root.clone() } else { root.join(relative) };
    let candidate = fs::canonicalize(&candidate).await.context("Agent 工作目录不存在")?;
    if !candidate.starts_with(&root) {
        return Err(anyhow!("Agent 工作目录必须位于 workspace 内"));
    }
    if !fs::metadata(&candidate).await?.is_dir() {
        return Err(anyhow!("Agent 工作目录不是目录"));
    }
    let display = candidate.strip_prefix(&root).unwrap_or(Path::new("")).to_string_lossy().replace('\\', "/");
    Ok((candidate, display))
}

pub async fn list_workspace(path: Option<&str>) -> anyhow::Result<WorkspaceListing> {
    let (directory, relative) = resolve_workspace_path(path).await?;
    let root = workspace_root().await?;
    let mut entries = Vec::new();
    let mut reader = fs::read_dir(directory).await?;
    while let Some(entry) = reader.next_entry().await? {
        let entry_path = entry.path();
        let resolved = match fs::canonicalize(&entry_path).await {
            Ok(path) if path.starts_with(&root) => path,
            _ => continue,
        };
        if !fs::metadata(&resolved).await.map(|meta| meta.is_dir()).unwrap_or(false) {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        let child_path = if relative.is_empty() { name.clone() } else { format!("{}/{}", relative, name) };
        entries.push(WorkspaceEntry { name, path: child_path });
    }
    entries.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(WorkspaceListing { root: root.to_string_lossy().into_owned(), path: relative, entries })
}

pub async fn create_workspace_directory(parent: Option<&str>, name: &str) -> anyhow::Result<WorkspaceEntry> {
    let name = name.trim();
    let component = Path::new(name).components().next();
    if name.is_empty() || Path::new(name).components().count() != 1 || !matches!(component, Some(Component::Normal(_)))
    {
        return Err(anyhow!("目录名称只能包含一个有效的目录名"));
    }
    let (directory, relative) = resolve_workspace_path(parent).await?;
    let child = directory.join(name);
    fs::create_dir(&child).await.context("创建 Agent 工作目录失败")?;
    let child_path = if relative.is_empty() { name.to_string() } else { format!("{}/{}", relative, name) };
    Ok(WorkspaceEntry { name: name.to_string(), path: child_path })
}

pub async fn resolve_workspace_file(path: &str) -> anyhow::Result<(PathBuf, String)> {
    let root = workspace_root().await?;
    let relative = path.trim();
    if relative.is_empty() {
        return Err(anyhow!("文件路径不能为空"));
    }
    let candidate_path = if Path::new(relative).is_absolute() {
        let absolute = PathBuf::from(relative);
        if !absolute.starts_with(&root) {
            return Err(anyhow!("workspace 文件路径非法"));
        }
        absolute
    } else {
        validate_relative_path(relative)?;
        root.join(relative)
    };
    let candidate = fs::canonicalize(candidate_path).await.context("workspace 文件不存在")?;
    if !candidate.starts_with(&root) {
        return Err(anyhow!("workspace 文件路径非法"));
    }
    if !fs::metadata(&candidate).await?.is_file() {
        return Err(anyhow!("workspace 路径不是文件"));
    }
    let display = candidate.strip_prefix(&root).unwrap_or(Path::new("/")).to_string_lossy().replace('\\', "/");
    Ok((candidate, display))
}

/// 选择生效模型：对话绑定 > 全局 CODEX_MODEL > 交给 codex 读取 config.toml 默认。
fn effective_model(conversation_model: Option<&str>) -> Option<String> {
    if let Some(model) = conversation_model.map(str::trim).filter(|model| !model.is_empty()) {
        return Some(model.to_string());
    }
    let global = config::try_get().map(|config| config.codex_model.trim().to_string()).unwrap_or_default();
    if global.is_empty() {
        None
    } else {
        Some(global)
    }
}

pub struct ConversationSettings {
    pub provider: String,
    pub model: String,
    pub reasoning_level: String,
}

/// Resume inherits saved settings when an override is omitted. Resolve defaults explicitly so clearing a selection takes effect.
pub async fn resolve_conversation_settings(
    cwd: &Path, provider: Option<&str>, model: Option<&str>, reasoning_level: Option<&str>,
) -> anyhow::Result<ConversationSettings> {
    let client = app_server().await?;
    let defaults = client.request("config/read", json!({"cwd":cwd, "includeLayers":false})).await?;
    let configured = &defaults["config"];
    let provider = provider.filter(|value| !value.is_empty())
        .or(configured["model_provider"].as_str()).unwrap_or("openai").to_owned();
    let model = effective_model(model).or_else(|| configured["model"].as_str().map(ToOwned::to_owned));
    let effort = reasoning_level.filter(|value| !value.is_empty())
        .or(configured["model_reasoning_effort"].as_str());
    let catalog = if model.is_none() || effort.is_none() {
        let result = client.request("model/list", json!({"limit":100, "includeHidden":true})).await?;
        result["data"].as_array().ok_or_else(|| anyhow!("Codex 未返回默认模型信息"))?.clone()
    } else { Vec::new() };
    resolve_settings_from_sources(provider, model, effort, &catalog)
}

fn resolve_settings_from_sources(
    provider: String, model: Option<String>, effort: Option<&str>, catalog: &[Value],
) -> anyhow::Result<ConversationSettings> {
    let model = model.or_else(|| catalog.iter().find(|item| item["isDefault"] == true)
        .and_then(|item| item["model"].as_str()).map(ToOwned::to_owned))
        .ok_or_else(|| anyhow!("无法确定 Codex 默认模型，请在模型设置中选择模型"))?;
    let unprefixed = model.strip_prefix(&format!("{provider}/")).unwrap_or(&model);
    let info = catalog.iter().find(|item| item["model"] == model || item["model"] == unprefixed);
    let reasoning_level = effort.or_else(|| info.and_then(|item| item["defaultReasoningEffort"].as_str()))
        .unwrap_or_else(|| {
            if info.is_some_and(|item| item["supportedReasoningEfforts"].as_array().is_some_and(Vec::is_empty)) {
                "none"
            } else {
                // Use standard medium reasoning when a custom model has no catalog entry.
                "medium"
            }
        })
        .to_owned();
    Ok(ConversationSettings { provider, model, reasoning_level })
}

fn thread_configuration(provider: Option<&str>, model: Option<&str>) -> Value {
    json!({"provider":provider, "model":effective_model(model)})
}

pub async fn start_thread(cwd: &Path, provider: Option<&str>, model: Option<&str>) -> anyhow::Result<String> {
    let client = app_server().await?;
    let mut params = json!({
        "cwd": cwd,
        "approvalPolicy": "never",
        "sandbox": "danger-full-access",
        "serviceName": "firefly_assistant"
    });
    if let Some(provider) = provider.map(str::trim).filter(|provider| !provider.is_empty()) {
        params["modelProvider"] = json!(provider);
    }
    if let Some(model) = effective_model(model) {
        params["model"] = json!(model);
    }
    let result = client.request("thread/start", params).await?;
    let thread_id = result["thread"]["id"].as_str().ok_or_else(|| anyhow!("Codex 未返回 thread id"))?;
    client.remember_thread_configuration(thread_id, thread_configuration(provider, model));
    Ok(thread_id.to_owned())
}

pub async fn resume_thread(
    thread_id: &str, cwd: &Path, provider: Option<&str>, model: Option<&str>,
) -> anyhow::Result<()> {
    let client = app_server().await?;
    let mut params = json!({
        "threadId": thread_id,
        "cwd": cwd,
        "approvalPolicy": "never",
        "sandbox": "danger-full-access"
    });
    // Loaded threads must be unloaded before resume can apply changed configuration.
    if let Some(provider) = provider.map(str::trim).filter(|provider| !provider.is_empty()) {
        params["modelProvider"] = json!(provider);
    }
    if let Some(model) = effective_model(model) {
        params["model"] = json!(model);
    }
    client.resume_configured_thread(thread_id, params, thread_configuration(provider, model)).await?;
    Ok(())
}

pub async fn start_turn(
    thread_id: &str, cwd: &Path, input: Vec<Value>, reasoning_level: Option<&str>,
) -> anyhow::Result<(String, broadcast::Receiver<Value>)> {
    let client = app_server().await?;
    let params = turn_params(thread_id, cwd, input, reasoning_level);
    client.start_turn(params, REQUEST_TIMEOUT).await
}

fn turn_params(thread_id: &str, cwd: &Path, input: Vec<Value>, reasoning_level: Option<&str>) -> Value {
    let mut params = json!({
        "threadId": thread_id,
        "input": input,
        "cwd": cwd,
        "approvalPolicy": "never",
        "sandboxPolicy": {
            "type": "dangerFullAccess"
        }
    });
    if let Some(level) = reasoning_level.map(str::trim).filter(|level| !level.is_empty()) {
        params["effort"] = json!(level);
    }
    params
}

pub async fn interrupt_turn(thread_id: &str, turn_id: &str) -> anyhow::Result<()> {
    // Cancellation must never spawn a new server just to stop an old turn.
    firefly_codex_transport::interrupt_existing_turn(thread_id, turn_id).await
}

/// 按 turn 列出 thread 历史中的 items，用于恢复因广播滞后而丢失的工具完成事件。
pub async fn list_turn_items(thread_id: &str, turn_id: &str) -> anyhow::Result<Vec<Value>> {
    let client = app_server().await?;
    let mut items = Vec::new();
    let mut cursor = Value::Null;
    loop {
        let result = client
            .request("thread/items/list", json!({ "threadId": thread_id, "turnId": turn_id, "cursor": cursor }))
            .await?;
        if let Some(entries) = result["data"].as_array() {
            for entry in entries {
                if let Some(item) = entry.get("item") {
                    items.push(item.clone());
                }
            }
        }
        match result["nextCursor"].as_str() {
            Some(next) => cursor = json!(next),
            None => break,
        }
    }
    Ok(items)
}

/// 读取 thread 中指定 turn 的状态，用于广播事件丢失后判断 turn 是否已结束。
pub async fn read_turn(thread_id: &str, turn_id: &str) -> anyhow::Result<Option<Value>> {
    let client = app_server().await?;
    client.read_turn(thread_id, turn_id).await
}

pub fn notification_matches(message: &Value, thread_id: &str, turn_id: &str) -> bool {
    let params = &message["params"];
    params["threadId"].as_str() == Some(thread_id)
        || params["turnId"].as_str() == Some(turn_id)
        || params["turn"]["id"].as_str() == Some(turn_id)
}

pub fn notification_method(message: &Value) -> Option<&str> {
    message.get("method").and_then(Value::as_str)
}

pub fn notification_params(message: &Value) -> &Value {
    message.get("params").unwrap_or(&Value::Null)
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
