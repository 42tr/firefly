use anyhow::{ensure, Context};
use chrono::{DateTime, Timelike, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

fn one() -> u32 {
    1
}
fn version() -> String {
    "0.1.0".into()
}
fn host_api() -> String {
    "1".into()
}
fn timeout() -> u64 {
    60
}
fn object() -> Value {
    json!({})
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    #[serde(default = "one")]
    pub manifest_version: u32,
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default = "version")]
    pub version: String,
    #[serde(default = "host_api")]
    pub host_api: String,
    #[serde(default)]
    pub description: String,
    pub mcp: Transport,
    #[serde(default)]
    pub config_schema: Option<String>,
    #[serde(default)]
    pub pages: Vec<Page>,
    #[serde(default)]
    pub indicator: Option<Indicator>,
    #[serde(default)]
    pub public: Option<Public>,
    #[serde(default)]
    pub jobs: Vec<Job>,
    #[serde(default)]
    pub allowed_tools: Option<Vec<String>>,
    #[serde(default = "timeout")]
    pub call_timeout_secs: u64,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(tag = "transport", rename_all = "snake_case", deny_unknown_fields)]
pub enum Transport {
    Stdio {
        command: String,
        #[serde(default)]
        args: Vec<String>,
        /// Explicit environment bindings only; never inherit the service's secrets.
        #[serde(default)]
        env: BTreeMap<String, String>,
        #[serde(default)]
        env_from: BTreeMap<String, String>,
    },
    #[serde(alias = "http")]
    StreamableHttp {
        url: String,
        #[serde(default)]
        bearer_token_env: Option<String>,
    },
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Public { pub prefix: String, pub tool: String }

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Page {
    pub id: String,
    pub title: String,
    pub open_tool: String,
    #[serde(default)]
    pub icon: Option<String>,
    #[serde(default = "object")]
    pub arguments: Value,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Indicator {
    pub tool: String,
    pub refresh_secs: u64,
    #[serde(default = "object")]
    pub arguments: Value,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Job {
    pub id: String,
    pub tool: String,
    pub cron: String,
    pub timezone: String,
    #[serde(default = "object")]
    pub arguments: Value,
    #[serde(default)]
    pub enabled_by_default: bool,
}

impl Job {
    pub fn next(&self, now: DateTime<Utc>) -> anyhow::Result<DateTime<Utc>> {
        ensure!(
            self.cron.len() <= 120 && self.cron.split_whitespace().count() == 5,
            "定时任务需要五段 cron"
        );
        let zone = self.timezone.parse::<chrono_tz::Tz>().context("时区无效")?;
        let cron = croner::Cron::new(&self.cron).parse().context("cron 无效")?;
        Ok(cron
            .find_next_occurrence(&now.with_timezone(&zone).with_nanosecond(0).unwrap(), false)
            .context("cron 没有下次执行时间")?
            .with_timezone(&Utc))
    }
}

pub fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
}

pub fn contained_file(root: &Path, relative: &str) -> anyhow::Result<PathBuf> {
    let path = root
        .join(relative)
        .canonicalize()
        .context("模块文件不存在")?;
    ensure!(
        path.starts_with(root) && path.is_file(),
        "模块资源必须位于模块目录内"
    );
    Ok(path)
}

pub fn read_limited(path: &Path) -> anyhow::Result<String> {
    ensure!(
        std::fs::metadata(path)?.len() <= 256 * 1024,
        "模块定义文件超过 256 KiB"
    );
    Ok(std::fs::read_to_string(path)?)
}

impl Manifest {
    pub fn load(path: &Path) -> anyhow::Result<(Self, PathBuf, Value)> {
        let mut definition: Self =
            toml::from_str(&read_limited(path)?).context("模块 TOML 格式无效")?;
        let root = path.parent().context("缺少模块目录")?.canonicalize()?;
        if let Some(public) = &definition.public {
            ensure!(public.prefix.starts_with('/') && (2..=80).contains(&public.prefix.len())
                && public.prefix[1..].bytes().all(|b| b.is_ascii_lowercase() || b == b'-')
                && !["/api", "/auth", "/health", "/assets"].contains(&public.prefix.as_str())
                && !public.tool.is_empty(), "公开下载前缀或工具名称无效");
        }
        ensure!(
            definition.manifest_version == 1 && definition.host_api == "1",
            "模块需要不兼容的宿主 API 版本"
        );
        ensure!(
            valid_id(&definition.id),
            "模块 ID 仅允许小写字母、数字和连字符，最多 64 字符"
        );
        if definition.name.is_empty() {
            definition.name = definition.id.clone();
        }
        ensure!(
            definition.name.chars().count() <= 100 && definition.description.len() <= 4096,
            "模块名称或说明过长"
        );
        ensure!(
            (1..=3600).contains(&definition.call_timeout_secs),
            "调用超时须为 1–3600 秒"
        );
        ensure!(
            definition.pages.len() <= 20 && definition.jobs.len() <= 20,
            "模块页面或任务过多"
        );
        let mut ids = std::collections::HashSet::new();
        for page in &definition.pages {
            ensure!(
                valid_id(&page.id) && ids.insert(&page.id),
                "页面 ID 无效或重复"
            );
            ensure!(
                !page.title.is_empty()
                    && page.title.chars().count() <= 100
                    && page.arguments.is_object(),
                "页面定义无效"
            );
        }
        ids.clear();
        if let Some(indicator) = &definition.indicator {
            ensure!(
                !indicator.tool.is_empty()
                    && indicator.tool.len() <= 128
                    && (30..=3600).contains(&indicator.refresh_secs)
                    && indicator.arguments.is_object(),
                "模块状态提示需要工具名称、30–3600 秒刷新间隔和对象参数"
            );
        }
        for job in &definition.jobs {
            ensure!(
                valid_id(&job.id) && ids.insert(&job.id) && job.arguments.is_object(),
                "任务定义无效或重复"
            );
            job.next(Utc::now())?;
        }
        match &definition.mcp {
            Transport::Stdio {
                command,
                args,
                env,
                env_from,
            } => {
                ensure!(!command.is_empty() && args.len() <= 64, "启动命令无效");
                ensure!(env.len() + env_from.len() <= 64, "环境变量过多");
                for key in env.keys().chain(env_from.keys()) {
                    ensure!(
                        !key.starts_with("FIREFLY_MODULE_") && !key.contains(['=', '\0']),
                        "环境变量名称无效或保留"
                    );
                }
            }
            Transport::StreamableHttp { url, .. } => {
                let url = reqwest::Url::parse(url).context("MCP URL 无效")?;
                ensure!(
                    matches!(url.scheme(), "http" | "https")
                        && url.username().is_empty()
                        && url.password().is_none()
                        && url.fragment().is_none(),
                    "MCP URL 仅支持无内嵌凭据的 HTTP(S) 地址"
                );
            }
        }
        let schema = match &definition.config_schema {
            Some(path) => serde_json::from_str(&read_limited(&contained_file(&root, path)?)?)
                .context("配置 Schema 无效")?,
            None => json!({"type":"object","properties":{},"additionalProperties":false}),
        };
        validate_schema(&schema)?;
        ensure!(schema["type"] == "object", "配置 Schema 顶层须为 object");
        validate_settings_schema(&schema)?;
        Ok((definition, root, schema))
    }

    pub fn allows(&self, name: &str) -> bool {
        self.allowed_tools
            .as_ref()
            .is_none_or(|tools| tools.iter().any(|t| t == name))
    }
}

/// Do not allow JSON Schema to fetch files or remote URLs during validation.
pub fn validate_schema(schema: &Value) -> anyhow::Result<()> {
    fn visit(value: &Value) -> anyhow::Result<()> {
        match value {
            Value::Object(map) => {
                for (key, value) in map {
                    if matches!(key.as_str(), "$ref" | "$dynamicRef" | "$recursiveRef") {
                        ensure!(
                            value.as_str().is_some_and(|v| v.starts_with('#')),
                            "不支持外部 Schema 引用"
                        );
                    }
                    visit(value)?;
                }
            }
            Value::Array(items) => {
                for item in items {
                    visit(item)?;
                }
            }
            _ => {}
        }
        Ok(())
    }
    visit(schema)?;
    jsonschema::validator_for(schema).map_err(|_| anyhow::anyhow!("JSON Schema 无效"))?;
    Ok(())
}

pub fn public_settings(schema: &Value, settings: &Value) -> Value {
    if schema["writeOnly"] == true || schema["format"] == "password" {
        return Value::Null;
    }
    match settings {
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(k, v)| (k.clone(), public_settings(&schema["properties"][k], v)))
                .collect(),
        ),
        Value::Array(items) => Value::Array(
            items
                .iter()
                .map(|v| public_settings(&schema["items"], v))
                .collect(),
        ),
        _ => settings.clone(),
    }
}

// The configuration editor/redactor supports inline property trees. Reject
// constructs it cannot faithfully edit or redact, rather than exposing a secret
// defined through a reference or conditional branch.
pub(super) fn validate_settings_schema(value: &Value) -> anyhow::Result<()> {
    if let Some(map) = value.as_object() {
        for key in map.keys() {
            ensure!(
                !matches!(
                    key.as_str(),
                    "$ref"
                        | "$dynamicRef"
                        | "$recursiveRef"
                        | "allOf"
                        | "anyOf"
                        | "oneOf"
                        | "if"
                        | "then"
                        | "else"
                        | "not"
                        | "patternProperties"
                        | "dependentSchemas"
                        | "dependencies"
                        | "prefixItems"
                        | "additionalItems"
                        | "contains"
                        | "unevaluatedProperties"
                        | "unevaluatedItems"
                ),
                "设置 Schema 请使用内联 properties 和统一 items，暂不支持引用或组合条件"
            );
        }
        if let Some(extra) = map.get("additionalProperties") {
            ensure!(
                extra.is_boolean(),
                "设置 Schema 不支持 additionalProperties 子 Schema"
            );
        }
        if let Some(properties) = map.get("properties").and_then(Value::as_object) {
            for child in properties.values() {
                validate_settings_schema(child)?;
            }
        }
        if let Some(items) = map.get("items") {
            ensure!(!items.is_array(), "设置 Schema 暂不支持元组数组");
            validate_settings_schema(items)?;
        }
    }
    Ok(())
}

pub fn merge_settings(schema: &Value, current: &Value, patch: &Value) -> Value {
    if patch.is_null() && (schema["writeOnly"] == true || schema["format"] == "password") {
        return current.clone();
    }
    if let Some(map) = patch.as_object() {
        let mut result = serde_json::Map::new();
        for (key, value) in map {
            result.insert(
                key.clone(),
                merge_settings(&schema["properties"][key], &current[key], value),
            );
        }
        Value::Object(result)
    } else if let Some(items) = patch.as_array() {
        Value::Array(
            items
                .iter()
                .enumerate()
                .map(|(index, value)| merge_settings(&schema["items"], &current[index], value))
                .collect(),
        )
    } else {
        patch.clone()
    }
}
