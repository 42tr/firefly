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

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ModelProfile {
    pub id: String,
    pub name: String,
    pub base_url: String,
    pub model: String,
    /// null preserves the key only for the same profile ID and service origin.
    #[serde(default)]
    pub api_key: Option<String>,
    #[serde(default)]
    pub context_tokens: Option<usize>,
    /// Accepted for GET/PUT round trips; the server always computes this value.
    #[serde(default, skip_serializing, rename = "key_configured")]
    pub _key_configured: bool,
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
            profiles: vec![ModelProfile {
                id: "default".into(),
                name: "默认模型".into(),
                base_url: std::env::var("OPENAI_BASE_URL")
                    .unwrap_or_else(|_| "https://api.openai.com/v1".into()),
                model: std::env::var("OPENAI_MODEL").unwrap_or_default(),
                api_key: Some(std::env::var("OPENAI_API_KEY").unwrap_or_default()),
                context_tokens: cfg.as_ref().and_then(|cfg| cfg.assistant_context_tokens),
                _key_configured: false,
            }],
            firefly: AgentDefinition {
                id: "firefly".into(),
                name: "Firefly".into(),
                description: "主助手".into(),
                instruction: cfg
                    .as_ref()
                    .map(|cfg| cfg.assistant_system_prompt.clone())
                    .unwrap_or_else(|| include_str!("../prompts/assistant.md").into()),
                model_profile: Some("default".into()),
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
        Ok(settings)
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
            (1..=32).contains(&self.profiles.len()),
            "需要 1–32 个模型配置"
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
            ensure!(
                profile.model.len() <= 256 && profile.base_url.len() <= 2048,
                "模型名称或服务地址过长"
            );
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
        ensure!(
            self.firefly.model_profile.is_some(),
            "请选择 Firefly 的模型配置"
        );
        let mut ids = BTreeSet::new();
        for agent in std::iter::once(&self.firefly).chain(&self.subagents) {
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
                agent.id == "firefly" || !agent.description.trim().is_empty(),
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
        ensure!(
            !profile.model.trim().is_empty(),
            "请在“更多功能 → Agent 配置”中填写模型名称"
        );
        let mut model = llm::AgentModel::new(
            llm::normalize_bearer_secret(profile.api_key.as_deref().unwrap_or_default()),
            &profile.model,
        )
        .with_http_client(crate::http_client::client().clone())
        .with_base_url(&profile.base_url)
        .with_context_budget(profile.context_tokens);
        if let Some(pool) = crate::db::maybe_pool() {
            model = model.with_limits_cache(pool.clone());
        }
        Ok(model)
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
    instruction.push_str("\n工具结果截断时，按 read_result 示例通过 run_tools 提交 read_task_result，使用 JSON Pointer 和 next_offset 读取完整字段。不要重复执行原工具或扫描文件、业务数据库寻找结果。模块数据以模块工具返回为准；行情图表标明实际数据截止日期，读取失败时如实说明。\n");
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
