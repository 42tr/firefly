//! Data-defined agents. Model clients and tool bindings belong to one turn.
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
    time::Duration,
};

use anyhow::{ensure, Context};
use g::{Agent, Model, Tool};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::SqlitePool;

use crate::{assistant_tasks, config, llm, mcp};

#[derive(Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReasoningOptions {
    pub reasoning_levels: Vec<String>,
    pub default_reasoning_level: Option<String>,
}

impl ReasoningOptions {
    fn valid(&self) -> bool {
        valid_reasoning_levels(&self.reasoning_levels)
            && self.default_reasoning_level.as_ref().is_none_or(|level| self.reasoning_levels.contains(level))
    }
}

fn valid_reasoning_levels(levels: &[String]) -> bool {
    levels.len() <= 32 && levels.iter().all(|level| !level.is_empty() && level.len() <= 64
        && level.bytes().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, b'_' | b'-')))
        && levels.iter().collect::<BTreeSet<_>>().len() == levels.len()
}

#[derive(Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProfileModel {
    pub id: String,
    #[serde(default)]
    pub context_length: Option<usize>,
    #[serde(default)]
    pub context_source: Option<String>,
    #[serde(default)]
    pub input_budget: Option<usize>,
    /// None means the service did not advertise effort controls; [] explicitly disables them.
    #[serde(default)]
    pub reasoning_levels: Option<Vec<String>>,
    #[serde(default)]
    pub default_reasoning_level: Option<String>,
    /// Protocol-specific capabilities from provider documentation. Never infer
    /// Anthropic effort controls from the provider's Responses controls.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub reasoning_by_protocol: BTreeMap<String, ReasoningOptions>,
}

impl ProfileModel {
    pub fn reasoning_options(&self, protocol: &str) -> (Option<&Vec<String>>, Option<&str>) {
        if let Some(options) = self.reasoning_by_protocol.get(protocol) {
            (Some(&options.reasoning_levels), options.default_reasoning_level.as_deref())
        } else {
            (self.reasoning_levels.as_ref(), self.default_reasoning_level.as_deref())
        }
    }
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ModelProfile {
    pub id: String,
    pub name: String,
    /// Which runtime consumes this profile. `shared` can be selected by any agent.
    #[serde(default = "default_profile_agent")]
    pub agent: String,
    /// Wire protocol used by the runtime adapter.
    #[serde(default = "default_profile_protocol")]
    pub protocol: String,
    pub base_url: String,
    /// Optional same-origin Anthropic endpoint for Claude Code.
    #[serde(default)]
    pub claude_base_url: Option<String>,
    pub model: String,
    #[serde(default, deserialize_with = "deserialize_models")]
    pub models: Vec<ProfileModel>,
    #[serde(default)]
    pub reasoning_levels: Vec<String>,
    #[serde(default = "default_model_discovery")]
    pub model_discovery: String,
    /// null preserves the key only for the same profile ID and service origin.
    #[serde(default)]
    pub api_key: Option<String>,
    #[serde(default)]
    pub context_tokens: Option<usize>,
    /// Accepted for GET/PUT round trips; the server always computes this value.
    #[serde(default, skip_serializing, rename = "key_configured")]
    pub _key_configured: bool,
}

impl Default for ModelProfile {
    fn default() -> Self { Self { id:String::new(), name:String::new(), agent:default_profile_agent(),
        protocol:default_profile_protocol(), base_url:String::new(), claude_base_url:None, model:String::new(),
        models:vec![], reasoning_levels:vec![], model_discovery:default_model_discovery(), api_key:None,
        context_tokens:None, _key_configured:false } }
}

impl ModelProfile {
    pub fn supports(&self, runtime: &str) -> bool {
        (self.agent == runtime || self.agent == "shared") && match runtime {
            "firefly" => self.protocol == "responses",
            "codex" => matches!(self.protocol.as_str(), "responses" | "openai"),
            "claude" => self.protocol == "anthropic" || self.agent == "shared",
            _ => false,
        }
    }
    pub fn model_ids(&self) -> Vec<String> {
        let mut models: Vec<_> = self.models.iter().map(|m| m.id.clone()).collect();
        if !self.model.trim().is_empty() && !models.contains(&self.model) { models.push(self.model.clone()); }
        models
    }
    pub fn default_model(&self) -> Option<&str> {
        (!self.model.trim().is_empty()).then_some(self.model.as_str())
            .or_else(|| self.models.first().map(|m| m.id.as_str()))
    }
    pub fn input_budget(&self, model: &str) -> Option<usize> {
        if model == self.model && self.context_tokens.is_some() { self.context_tokens }
        else { self.models.iter().find(|m| m.id == model).and_then(|m| m.input_budget) }
    }
    /// Use only advertised levels. Unknown capabilities leave effort to the service.
    pub fn reasoning_level(&self, model: &str, requested: Option<&str>, allowed: Option<&[&str]>) -> Option<String> {
        self.reasoning_level_for_protocol(model, requested, allowed, &self.protocol)
    }
    pub fn reasoning_level_for_protocol(&self, model: &str, requested: Option<&str>, allowed: Option<&[&str]>, protocol: &str) -> Option<String> {
        let info = self.models.iter().find(|m| m.id == model);
        let (levels, default) = info.map(|info| info.reasoning_options(protocol)).unwrap_or_default();
        let levels = levels
            .or_else(|| (!self.reasoning_levels.is_empty()).then_some(&self.reasoning_levels))?;
        let supported = |level: &&str| levels.iter().any(|item| item == level)
            && allowed.is_none_or(|allowed| allowed.contains(level));
        requested.filter(supported)
            .or_else(|| default.filter(supported))
            .or_else(|| levels.iter().map(String::as_str).find(supported))
            .map(str::to_owned)
    }
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AgentDefinition {
    pub id: String,
    pub name: String,
    pub description: String,
    pub instruction: String,
    #[serde(default)]
    pub model_profile: Option<String>,
    /// Overrides the selected provider's default model; children may inherit the root model.
    #[serde(default)]
    pub model: Option<String>,
    /// None follows the model default, or the root effort when sharing its model.
    #[serde(default)]
    pub reasoning_level: Option<String>,
    pub tools: Vec<String>,
    pub enabled: bool,
    pub timeout_secs: u64,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    pub revision: i64,
    pub profiles: Vec<ModelProfile>,
    pub firefly: AgentDefinition,
    pub subagents: Vec<AgentDefinition>,
    #[serde(default)]
    pub codex_api_profile: Option<String>,
    #[serde(default)]
    pub claude_api_profile: Option<String>,
}

fn default_profile_agent() -> String { "shared".into() }
fn default_profile_protocol() -> String { "responses".into() }
fn default_model_discovery() -> String { "auto".into() }

impl From<String> for ProfileModel {
    fn from(id: String) -> Self { Self { id, ..Default::default() } }
}
impl From<&str> for ProfileModel {
    fn from(id: &str) -> Self { id.to_owned().into() }
}
fn deserialize_models<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<Vec<ProfileModel>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Entry { Id(String), Metadata(ProfileModel) }
    Ok(Vec::<Entry>::deserialize(deserializer)?.into_iter().map(|entry| match entry {
        Entry::Id(id) => id.into(), Entry::Metadata(info) => info,
    }).collect())
}

#[derive(Serialize)]
pub struct ToolEntry {
    pub name: String,
    pub description: String,
    pub source: String,
    pub read_only: bool,
}

pub type ToolMap = BTreeMap<String, Arc<dyn Tool>>;

fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 40
        && id
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, b'_' | b'-'))
}

impl Settings {
    fn defaults() -> Self {
        let cfg = config::try_get();
        let mut tools = vec!["command", "fetch_activity_records"];
        if cfg
            .as_ref()
            .is_some_and(|cfg| cfg.assistant_http_tool_enabled)
        {
            tools.push("http_request");
        }
        Self {
            revision: 1,
            profiles: Vec::new(),
            firefly: AgentDefinition {
                id: "firefly".into(),
                name: "Firefly".into(),
                description: "主助手".into(),
                instruction: cfg
                    .as_ref()
                    .map(|cfg| cfg.assistant_system_prompt.clone())
                    .unwrap_or_else(|| include_str!("../prompts/assistant.md").into()),
                model_profile: None,
                model: None,
                reasoning_level: None,
                tools: tools.into_iter().map(str::to_owned).collect(),
                enabled: true,
                timeout_secs: std::env::var("ASSISTANT_RUN_TIMEOUT_SECS")
                    .ok()
                    .and_then(|v| v.parse().ok())
                    .filter(|v| *v > 0)
                    .unwrap_or(86400),
            },
            // There are deliberately no built-in child-agent definitions.
            subagents: Vec::new(),
            codex_api_profile: None,
            claude_api_profile: None,
        }
    }

    pub async fn load(pool: &SqlitePool) -> anyhow::Result<Self> {
        let row: Option<(i64, String)> =
            sqlx::query_as("SELECT revision,settings FROM assistant_agent_settings WHERE id=1")
                .fetch_optional(pool)
                .await?;
        if let Some((revision, settings)) = row {
            let mut settings: Self = serde_json::from_str(&settings)?;
            settings.revision = revision;
            // Older releases stored the Kimi Claude profile in the generic
            // model slot before Claude API profiles had an explicit agent and
            // protocol. Keep those installations usable after the schema
            // gained the new fields.
            settings.migrate_legacy_claude_profiles();
            settings.normalize_catalogs();
            return Ok(settings);
        }
        let mut defaults = Self::defaults();
        // Preserve the existing direct module tools when bootstrapping an old instance.
        defaults.firefly.tools.extend(
            crate::modules::agent_tools()
                .await
                .iter()
                .map(|tool| tool.spec().name),
        );
        defaults.firefly.tools.sort();
        defaults.firefly.tools.dedup();
        sqlx::query("INSERT OR IGNORE INTO assistant_agent_settings(id,revision,settings,updated_at) VALUES(1,1,?,?)")
            .bind(serde_json::to_string(&defaults)?).bind(chrono::Utc::now().timestamp_millis())
            .execute(pool).await?;
        let (revision, stored): (i64, String) =
            sqlx::query_as("SELECT revision,settings FROM assistant_agent_settings WHERE id=1")
                .fetch_one(pool)
                .await?;
        let mut settings: Self = serde_json::from_str(&stored)?;
        settings.revision = revision;
        settings.migrate_legacy_claude_profiles();
        settings.normalize_catalogs();
        Ok(settings)
    }

    /// Upgrade the legacy Kimi Code profile shape in memory. The migration is
    /// intentionally narrow: only the Kimi coding endpoint with a K3.1 Pro
    /// model is migrated, so unrelated Responses profiles keep their old
    /// behavior. A later settings save persists the normalized fields.
    pub(crate) fn migrate_legacy_claude_profiles(&mut self) -> bool {
        let legacy_indices: Vec<_> = self
            .profiles
            .iter()
            .enumerate()
            .filter_map(|(index, profile)| {
                let base_url = profile.base_url.to_ascii_lowercase();
                let model = profile.model.trim().to_ascii_lowercase();
                (profile.agent == "firefly"
                    && profile.protocol == "responses"
                    && base_url.contains("kimi")
                    && base_url.contains("/coding")
                    && (model == "k3.1-pro" || model == "k3.1-pro-highspeed"))
                .then_some(index)
            })
            .collect();
        let mut migrated = false;
        for index in legacy_indices {
            let original = self.profiles[index].clone();
            let mut profile = original.clone();
            profile.agent = "claude".into();
            profile.protocol = "anthropic".into();
            if profile.models.is_empty() && !profile.model.trim().is_empty() {
                profile.models.push(profile.model.trim().into());
            }
            if profile.reasoning_levels.is_empty() {
                profile.reasoning_levels = vec!["low".into(), "high".into(), "max".into()];
            }
            if profile.model_discovery.is_empty() {
                profile.model_discovery = "manual".into();
            }

            // The old profile may still be selected by Firefly. Keep it as a
            // Responses profile and create a separate Claude profile rather
            // than changing the runtime of the existing Firefly agent.
            let used_by_firefly = self.firefly.model_profile.as_deref() == Some(original.id.as_str())
                || self
                    .subagents
                    .iter()
                    .any(|agent| agent.model_profile.as_deref() == Some(original.id.as_str()));
            if used_by_firefly {
                if self.profiles.iter().any(|item| {
                    item.agent == "claude"
                        && item.protocol == "anthropic"
                        && (item.id == original.name || item.name == original.name)
                }) {
                    continue;
                }
                let mut id = original.name.trim().to_ascii_lowercase();
                id.retain(|value| {
                    value.is_ascii_lowercase() || value.is_ascii_digit() || matches!(value, '_' | '-')
                });
                if !valid_id(&id) || self.profiles.iter().any(|item| item.id == id) {
                    id = format!("claude-{}", original.id);
                }
                let base_id = id.clone();
                let mut suffix = 2;
                while self.profiles.iter().any(|item| item.id == id) {
                    id = format!("{}-{}", base_id, suffix);
                    suffix += 1;
                }
                profile.id = id;
                self.profiles.push(profile);
            } else {
                self.profiles[index] = profile;
            }
            migrated = true;
        }
        if self.claude_api_profile.is_none() {
            if let Some(profile) = self
                .profiles
                .iter()
                .find(|profile| profile.agent == "claude" && profile.protocol == "anthropic")
            {
                self.claude_api_profile = Some(profile.id.clone());
                migrated = true;
            }
        }
        migrated
    }

    fn normalize_catalogs(&mut self) {
        for profile in &mut self.profiles {
            if !profile.model.is_empty() && !profile.models.iter().any(|model| model.id == profile.model) {
                profile.models.push(profile.model.clone().into());
            }
            for model in &mut profile.models {
                if model.reasoning_levels.is_none() && !profile.reasoning_levels.is_empty() {
                    model.reasoning_levels = Some(profile.reasoning_levels.clone());
                    model.default_reasoning_level = profile.reasoning_levels.first().cloned();
                }
            }
        }
    }

    pub fn public(&self) -> Value {
        let mut value = json!(self);
        for (profile, public) in self
            .profiles
            .iter()
            .zip(value["profiles"].as_array_mut().unwrap())
        {
            public["key_configured"] =
                json!(profile.api_key.as_ref().is_some_and(|key| !key.is_empty()));
            public["api_key"] = Value::Null;
        }
        value
    }

    pub fn validate(&self) -> anyhow::Result<()> {
        ensure!(
            self.profiles.len() <= 32,
            "最多配置 32 个模型"
        );
        ensure!(self.subagents.len() <= 50, "最多配置 50 个子 agent");
        ensure!(
            serde_json::to_vec(self)?.len() <= 1024 * 1024,
            "Agent 配置总大小不能超过 1 MiB"
        );
        ensure!(
            self.firefly.id == "firefly" && self.firefly.enabled,
            "Firefly 主 agent 必须保留并启用"
        );
        let mut profiles = BTreeSet::new();
        for profile in &self.profiles {
            ensure!(
                valid_id(&profile.id) && profiles.insert(&profile.id),
                "模型配置 ID 无效或重复"
            );
            ensure!(
                !profile.name.trim().is_empty() && profile.name.chars().count() <= 100,
                "模型配置名称须为 1–100 字"
            );
            ensure!(
                matches!(profile.agent.as_str(), "firefly" | "codex" | "claude" | "shared"),
                "API 配置的 Agent 类型无效"
            );
            ensure!(
                matches!(profile.protocol.as_str(), "responses" | "openai" | "anthropic"),
                "API 配置的协议无效"
            );
            ensure!(
                matches!(profile.model_discovery.as_str(), "auto" | "manual"),
                "模型目录模式无效"
            );
            ensure!(profile.reasoning_levels.len() <= 16 && profile.reasoning_levels.iter().all(|level| {
                !level.trim().is_empty() && level.len() <= 64 && !level.chars().any(char::is_whitespace)
            }), "API 配置包含无效推理级别");
            let url = reqwest::Url::parse(&profile.base_url).context("模型服务地址无效")?;
            ensure!(
                matches!(url.scheme(), "http" | "https")
                    && url.host_str().is_some()
                    && url.username().is_empty()
                    && url.password().is_none()
                    && url.query().is_none()
                    && url.fragment().is_none(),
                "模型服务地址须为不含凭据、查询或片段的 HTTP/HTTPS URL"
            );
            if let Some(raw) = profile.claude_base_url.as_deref().filter(|v| !v.is_empty()) {
                let claude = reqwest::Url::parse(raw).context("Claude 接口地址无效")?;
                ensure!(raw.len() <= 2048 && claude.origin() == url.origin()
                    && claude.username().is_empty() && claude.password().is_none()
                    && claude.query().is_none() && claude.fragment().is_none(),
                    "Claude 接口地址须与模型服务同源且不含凭据、查询或片段");
            }
            ensure!(
                profile.model.len() <= 256 && profile.base_url.len() <= 2048,
                "模型名称或服务地址过长"
            );
            let mut model_ids = BTreeSet::new();
            ensure!(profile.models.len() <= 5000 && profile.models.iter().all(|m| {
                !m.id.trim().is_empty() && m.id.len() <= 256 && !m.id.chars().any(char::is_control)
                    && model_ids.insert(&m.id)
                    && m.context_length.is_none_or(|n| (1..=100_000_000).contains(&n))
                    && m.context_source.as_ref().is_none_or(|source| source.len() <= 2048
                        && reqwest::Url::parse(source).is_ok_and(|url| matches!(url.scheme(), "http" | "https")
                            && url.host_str().is_some() && url.username().is_empty() && url.password().is_none()))
                    && m.input_budget.is_none_or(|n| (1024..=100_000_000).contains(&n)
                        && m.context_length.is_none_or(|window| n <= window))
                    && m.reasoning_levels.as_ref().is_none_or(|levels| valid_reasoning_levels(levels))
                    && m.default_reasoning_level.as_ref().is_none_or(|level|
                        m.reasoning_levels.as_ref().is_some_and(|levels| levels.contains(level)))
                    && m.reasoning_by_protocol.len() <= 3 && m.reasoning_by_protocol.iter().all(|(protocol, options)|
                        matches!(protocol.as_str(), "responses" | "anthropic" | "openai") && options.valid())
            }), "模型目录包含无效、重复或过多的模型，或上下文长度无效");
            ensure!(
                profile
                    .api_key
                    .as_ref()
                    .is_none_or(|key| key.len() <= 16384 && !key.contains(['\r', '\n'])),
                "API Key 格式无效"
            );
            ensure!(
                profile
                    .context_tokens
                    .is_none_or(|tokens| (1024..=100_000_000).contains(&tokens)),
                "输入预算须为 1024–100000000，或留空自动查询"
            );
        }
        for (runtime, id) in [("codex", &self.codex_api_profile), ("claude", &self.claude_api_profile)] {
            if let Some(id) = id {
                ensure!(self.profiles.iter().any(|profile| profile.id == *id && profile.supports(runtime)),
                    "API 配置不存在或协议与 Agent 不匹配");
            }
        }
        let mut ids = BTreeSet::new();
        for agent in std::iter::once(&self.firefly).chain(&self.subagents) {
            ensure!(agent.model.as_deref().is_none_or(|m| !m.trim().is_empty() && m.len() <= 256
                && !m.chars().any(char::is_control)), "Agent 模型名称格式无效");
            ensure!(agent.reasoning_level.as_deref().is_none_or(|level| !level.is_empty() && level.len() <= 64
                && level.bytes().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, b'_' | b'-'))),
                "Agent 推理级别格式无效");
            ensure!(
                valid_id(&agent.id) && ids.insert(&agent.id),
                "Agent ID 无效或重复"
            );
            ensure!(
                !agent.name.trim().is_empty() && agent.name.chars().count() <= 100,
                "Agent 名称须为 1–100 字"
            );
            ensure!(
                agent.description.len() <= 4096 && agent.instruction.len() <= 128 * 1024,
                "Agent 职责说明或提示词过长"
            );
            ensure!(
                agent.id == "firefly" || !agent.enabled || !agent.description.trim().is_empty(),
                "子 agent 需要职责说明，供主 agent 选择委派"
            );
            ensure!(
                (1..=604800).contains(&agent.timeout_secs),
                "执行期限须为 1–604800 秒"
            );
            ensure!(
                agent
                    .model_profile
                    .as_ref()
                    .is_none_or(|id| profiles.contains(id)),
                "Agent 引用了不存在的模型配置"
            );
            if let Some(id) = &agent.model_profile {
                ensure!(self.profiles.iter().any(|profile| profile.id == *id && profile.supports("firefly")),
                    "Firefly 需要 Responses API 配置");
            }
            let mut tools = BTreeSet::new();
            ensure!(
                agent.tools.len() <= 500
                    && agent.tools.iter().all(|tool| !tool.is_empty()
                        && tool.len() <= 128
                        && tools.insert(tool)
                        && !tool.starts_with("handoff_to_")
                        && ![
                            "run_tools",
                            "wait_tools",
                            "get_tasks",
                            "cancel_tools",
                            "detach_tools"
                        ]
                        .contains(&tool.as_str())),
                "工具列表包含重复、无效或保留名称"
            );
        }
        Ok(())
    }

    pub async fn save(mut self, pool: &SqlitePool) -> anyhow::Result<Option<Self>> {
        self.validate()?;
        let previous = Self::load(pool).await?;
        if self.revision != previous.revision {
            return Ok(None);
        }
        for profile in &mut self.profiles {
            if profile.api_key.is_none() {
                profile.api_key = previous
                    .profiles
                    .iter()
                    .find(|old| old.id == profile.id)
                    .filter(|old| {
                        reqwest::Url::parse(&old.base_url)
                            .ok()
                            .zip(reqwest::Url::parse(&profile.base_url).ok())
                            .is_some_and(|(old, new)| old.origin() == new.origin())
                    })
                    .and_then(|old| old.api_key.clone())
                    .or_else(|| Some(String::new()));
            } else if let Some(key) = &mut profile.api_key {
                *key = llm::normalize_bearer_secret(key);
            }
        }
        self.revision += 1;
        let updated = sqlx::query("UPDATE assistant_agent_settings SET revision=?,settings=?,updated_at=? WHERE id=1 AND revision=?")
            .bind(self.revision).bind(serde_json::to_string(&self)?)
            .bind(chrono::Utc::now().timestamp_millis()).bind(previous.revision)
            .execute(pool).await?.rows_affected();
        if updated == 1 { crate::codex::refresh_profiles(&self.profiles).await; }
        Ok((updated == 1).then_some(self))
    }

    pub fn profile(&self, agent: &AgentDefinition) -> anyhow::Result<&ModelProfile> {
        let id = agent
            .model_profile
            .as_ref()
            .or(self.firefly.model_profile.as_ref())
            .context("请选择 Firefly 的模型配置")?;
        self.profiles
            .iter()
            .find(|profile| &profile.id == id)
            .context("模型配置不存在")
    }

    pub fn model(&self, agent: &AgentDefinition) -> anyhow::Result<llm::AgentModel> {
        let profile = self.profile(agent)?;
        let name = self.model_name(agent)?;
        ensure!(profile.supports("firefly"), "Firefly 需要 Responses API 配置");
        let mut model = llm::AgentModel::new(
            llm::normalize_bearer_secret(profile.api_key.as_deref().unwrap_or_default()),
            name,
        )
        .with_http_client(crate::http_client::client().clone())
        .with_base_url(&profile.base_url)
        .with_reasoning_effort(self.reasoning_level(agent)?)
        .with_context_budget(profile.input_budget(name));
        if let Some(pool) = crate::db::maybe_pool() {
            model = model.with_limits_cache(pool.clone());
        }
        Ok(model)
    }

    pub fn model_name<'a>(&'a self, agent: &'a AgentDefinition) -> anyhow::Result<&'a str> {
        agent.model.as_deref().or_else(|| {
            (agent.id != "firefly" && agent.model_profile.is_none()).then_some(self.firefly.model.as_deref()).flatten()
        }).or_else(|| self.profile(agent).ok().and_then(ModelProfile::default_model))
            .context("请在 Agent 配置中选择模型")
    }

    pub fn reasoning_level(&self, agent: &AgentDefinition) -> anyhow::Result<Option<String>> {
        let profile = self.profile(agent)?;
        let model = self.model_name(agent)?;
        let requested = agent.reasoning_level.as_deref().or_else(|| {
            (agent.id != "firefly" && self.profile(&self.firefly).is_ok_and(|root| root.id == profile.id)
                && self.model_name(&self.firefly).is_ok_and(|root| root == model))
                .then_some(self.firefly.reasoning_level.as_deref()).flatten()
        });
        Ok(profile.reasoning_level(model, requested, None))
    }

    pub fn child_agents(
        &self,
        tools: &ToolMap,
        backend: Arc<assistant_tasks::Backend>,
    ) -> (Vec<Agent>, Vec<String>) {
        let mut agents = Vec::new();
        let mut unavailable = Vec::new();
        for definition in self.subagents.iter().filter(|agent| agent.enabled) {
            let result = self.model(definition).and_then(|model| {
                ensure!(
                    definition.tools.iter().all(|name| tools.contains_key(name)),
                    "所选工具有未启用或不可用项"
                );
                Ok(build_agent(
                    definition,
                    model.with_context_compaction(),
                    tools,
                    backend.clone(),
                ))
            });
            match result {
                Ok(agent) => agents.push(agent),
                Err(error) => unavailable.push(format!("{}：{error}", definition.name)),
            }
        }
        (agents, unavailable)
    }
}

pub fn build_agent(
    definition: &AgentDefinition,
    model: Arc<dyn Model>,
    tools: &ToolMap,
    backend: Arc<assistant_tasks::Backend>,
) -> Agent {
    Agent::new(model)
        .name(&definition.id)
        .description(&definition.description)
        .instruction(tool_instruction(definition))
        .with_limits(g::RunLimits {
            max_tool_calls: usize::MAX,
            timeout: Duration::from_secs(definition.timeout_secs),
        })
        .tools(selected_tools(definition, tools))
        .with_task_backend(backend)
}

pub fn selected_tools(definition: &AgentDefinition, tools: &ToolMap) -> Vec<Arc<dyn Tool>> {
    let mut selected: Vec<_> = definition.tools.iter().filter_map(|name| tools.get(name).cloned()).collect();
    if !selected.iter().any(|tool| tool.spec().name == "read_task_result") {
        selected.push(Arc::new(assistant_tasks::ReadTaskResultTool));
    }
    selected
}

pub fn tool_instruction(definition: &AgentDefinition) -> String {
    let mut instruction = definition.instruction.clone();
    if definition.tools.iter().any(|tool| tool == "command") {
        instruction.push_str(crate::agent::FILE_SEARCH_INSTRUCTION);
    }
    instruction.push_str("\n工具结果截断时，按 read_result 示例直接调用 read_task_result，使用 JSON Pointer 和 next_offset 读取完整字段。不要重复执行原工具或扫描文件、业务数据库寻找结果。模块数据以模块工具返回为准；行情图表标明实际数据截止日期，读取失败时如实说明。\n");
    instruction
}

pub fn routing_instruction(agents: &[Agent]) -> String {
    let mut instruction = "\n\n# 当前可用子 agent\n仅以本轮提供的 handoff 工具为准，历史提示中的旧路由不代表当前能力。\n".to_owned();
    if agents.is_empty() {
        instruction.push_str("当前未启用任何子 agent。\n");
    }
    instruction.push_str("委派时传入完整目标、工作目录、验收条件、必要上下文、用户已有授权与约束。子 agent 不自动收到完整聊天历史。关键事实无法确定时澄清，不猜测项目或扩大授权。收到 partial 或 blocked 结果时如实说明未完成部分。\n");
    instruction
}

pub async fn available_tools(context: mcp::ToolContext) -> ToolMap {
    let mut tools: Vec<Arc<dyn Tool>> = vec![
        Arc::new(assistant_tasks::command::CommandTool),
        Arc::new(assistant_tasks::ReadTaskResultTool),
        Arc::new(crate::agent::activity::FetchActivityRecordsTool {
            user_id: context.user_id.clone(),
        }),
    ];
    tools.extend(mcp::build_tool_set(context));
    tools.extend(crate::modules::agent_tools().await);
    tools
        .into_iter()
        .map(|tool| (tool.spec().name, tool))
        .collect()
}

pub fn catalog(tools: &ToolMap) -> Vec<ToolEntry> {
    tools
        .values()
        .map(|tool| {
            let spec = tool.spec();
            ToolEntry {
                source: if spec.name.starts_with("mcp__") {
                    "扩展模块".into()
                } else {
                    "Firefly".into()
                },
                name: spec.name,
                description: spec.description,
                read_only: spec.behavior.read_only,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests;
