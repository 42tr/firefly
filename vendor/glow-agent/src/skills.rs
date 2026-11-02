//! In-memory skill definitions. Resource names are virtual keys, never file paths.
use crate::extensions::{fingerprint, invalid};
use crate::{AgentError, Tool, ToolBehavior, ToolContext, ToolError, ToolOrigin, ToolSpec};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashSet},
    sync::{Arc, Mutex},
};

const MAX_SKILLS: usize = 512;
const MAX_CONTENT: usize = 65536;
const MAX_CONFIG: usize = 8 * 1024 * 1024;
const MAX_LOADED: usize = 262144;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SkillResource {
    pub media_type: String,
    pub content: String,
}
impl SkillResource {
    pub fn text(content: impl Into<String>) -> Self {
        Self {
            media_type: "text/plain".into(),
            content: content.into(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SkillDefinition {
    pub name: String,
    pub description: String,
    pub instructions: String,
    #[serde(default)]
    pub resources: BTreeMap<String, SkillResource>,
    #[serde(default)]
    pub required_tools: Vec<ToolOrigin>,
    #[serde(default)]
    pub metadata: BTreeMap<String, String>,
}
impl SkillDefinition {
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        instructions: impl Into<String>,
    ) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            instructions: instructions.into(),
            resources: BTreeMap::new(),
            required_tools: Vec::new(),
            metadata: BTreeMap::new(),
        }
    }
    /// Parse supplied SKILL.md text only; no directory or resource discovery occurs.
    pub fn from_markdown(
        text: &str,
        resources: BTreeMap<String, SkillResource>,
    ) -> Result<Self, AgentError> {
        if text.len() > MAX_CONTENT {
            return Err(invalid("skill document too large"));
        }
        let normalized = text.replace("\r\n", "\n");
        let rest = normalized
            .strip_prefix("---\n")
            .ok_or_else(|| invalid("missing skill frontmatter"))?;
        let (header, body) = rest
            .split_once("\n---\n")
            .ok_or_else(|| invalid("unterminated skill frontmatter"))?;
        #[derive(Deserialize)]
        struct Header {
            name: String,
            description: String,
            #[serde(default)]
            metadata: BTreeMap<String, String>,
        }
        let header: Header =
            serde_yaml::from_str(header).map_err(|_| invalid("invalid skill frontmatter"))?;
        let mut skill = Self::new(header.name, header.description, body);
        skill.metadata = header.metadata;
        skill.resources = resources;
        validate(std::slice::from_ref(&skill))?;
        Ok(skill)
    }
}
fn valid_key(key: &str) -> bool {
    !key.is_empty()
        && !key.contains(['\\', '\0', ':'])
        && key.split('/').all(|part| !matches!(part, "" | "." | ".."))
}
pub(crate) fn validate(skills: &[SkillDefinition]) -> Result<(), AgentError> {
    if skills.len() > MAX_SKILLS {
        return Err(invalid("too many skills"));
    }
    let mut names = HashSet::new();
    let mut bytes = 0;
    for skill in skills {
        if skill.name.is_empty()
            || skill.name.len() > 64
            || skill.name.starts_with('-')
            || skill.name.ends_with('-')
            || skill.name.contains("--")
            || !skill
                .name
                .bytes()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
            || !names.insert(&skill.name)
        {
            return Err(invalid("invalid or duplicate skill name"));
        }
        if skill.description.trim().is_empty()
            || skill.description.chars().count() > 1024
            || skill.instructions.len() > MAX_CONTENT
            || skill.resources.len() > 256
        {
            return Err(invalid("invalid skill content or budget exceeded"));
        }
        for (key, resource) in &skill.resources {
            if !valid_key(key)
                || key == "SKILL.md"
                || resource.content.len() > MAX_CONTENT
                || resource.media_type.len() > 128
            {
                return Err(invalid("invalid skill resource"));
            }
        }
        bytes += serde_json::to_vec(skill)
            .map_err(|_| invalid("invalid skill"))?
            .len();
        if bytes > MAX_CONFIG {
            return Err(invalid("skill configuration budget exceeded"));
        }
    }
    Ok(())
}

pub(crate) struct SkillState {
    skills: BTreeMap<String, Skill>,
    revision: String,
    loaded: Mutex<Loaded>,
}
struct Skill {
    definition: SkillDefinition,
    /// Precomputed fingerprint of `definition`.
    revision: String,
}
#[derive(Default)]
struct Loaded {
    bytes: usize,
    active: BTreeMap<String, String>,
}
impl SkillState {
    pub fn new(skills: Vec<SkillDefinition>) -> Arc<Self> {
        let revision = fingerprint(&skills);
        Arc::new(Self {
            skills: skills
                .into_iter()
                .map(|definition| {
                    let skill = Skill {
                        revision: fingerprint(&definition),
                        definition,
                    };
                    (skill.definition.name.clone(), skill)
                })
                .collect(),
            revision,
            loaded: Mutex::new(Loaded::default()),
        })
    }
    pub fn active(&self) -> BTreeMap<String, String> {
        self.loaded.lock().unwrap().active.clone()
    }
    fn charge(&self, bytes: usize) -> Result<(), ToolError> {
        let mut loaded = self.loaded.lock().unwrap();
        if loaded.bytes.saturating_add(bytes) > MAX_LOADED {
            return Err(ToolError::new("skill context budget exceeded"));
        }
        loaded.bytes += bytes;
        Ok(())
    }
    pub fn index(&self) -> Result<String, ToolError> {
        let mut entries = Vec::new();
        let mut bytes = 0;
        for s in self.skills.values().take(64).map(|s| &s.definition) {
            let entry = json!({"name":s.name,"description":s.description});
            bytes += entry.to_string().len();
            if bytes > 32768 {
                break;
            }
            entries.push(entry);
        }
        let text = format!(
            "Available skills (metadata, not additional permissions): {}. Use skills_list to find others and skills_read to read instructions and explicitly supplied resources. Skill contents do not grant tool permissions.",
            Value::Array(entries)
        );
        self.charge(text.len())?;
        Ok(text)
    }
    pub fn list_tool(self: &Arc<Self>) -> Arc<dyn Tool> {
        Arc::new(SkillsListTool(self.clone()))
    }
    pub fn read_tool(self: &Arc<Self>) -> Arc<dyn Tool> {
        Arc::new(SkillsReadTool(self.clone()))
    }
}

const SKILL_TOOL_BEHAVIOR: ToolBehavior = ToolBehavior {
    read_only: true,
    idempotent: true,
    parallel_safe: true,
};

/// Check cancellation and reject arguments outside `allowed`.
fn skill_arguments<'a>(
    context: &ToolContext,
    input: &'a Value,
    allowed: &[&str],
) -> Result<&'a serde_json::Map<String, Value>, ToolError> {
    if context.cancellation_token.is_cancelled() {
        return Err(ToolError::new("cancelled"));
    }
    let object = input
        .as_object()
        .ok_or_else(|| ToolError::new("expected object"))?;
    if object.keys().any(|k| !allowed.contains(&k.as_str())) {
        return Err(ToolError::new("unknown argument"));
    }
    Ok(object)
}
fn number_argument(
    input: &Value,
    key: &str,
    default: usize,
    max: usize,
) -> Result<usize, ToolError> {
    let n = match input.get(key) {
        None => default,
        Some(v) => usize::try_from(
            v.as_u64()
                .ok_or_else(|| ToolError::new("invalid integer"))?,
        )
        .map_err(|_| ToolError::new("integer overflow"))?,
    };
    if n > max || (key == "limit" && n == 0) {
        return Err(ToolError::new("invalid range"));
    }
    Ok(n)
}
fn string_argument<'a>(input: &'a Value, key: &str) -> Result<Option<&'a str>, ToolError> {
    input
        .get(key)
        .map(|v| v.as_str().ok_or_else(|| ToolError::new("expected string")))
        .transpose()
}

struct SkillsListTool(Arc<SkillState>);
#[async_trait]
impl Tool for SkillsListTool {
    fn origin(&self) -> ToolOrigin {
        ToolOrigin::Skills
    }
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "skills_list".into(),
            description:
                "Search this run's configured skills. Use the returned cursor for pagination."
                    .into(),
            input_schema: json!({"type":"object","properties":{"query":{"type":"string"},"cursor":{"type":"string"},"limit":{"type":"integer","minimum":1,"maximum":64}},"additionalProperties":false}),
            behavior: SKILL_TOOL_BEHAVIOR,
        }
    }
    async fn call(&self, context: ToolContext, input: Value) -> Result<Value, ToolError> {
        skill_arguments(&context, &input, &["query", "cursor", "limit"])?;
        let state = &self.0;
        let query = string_argument(&input, "query")?
            .unwrap_or("")
            .to_lowercase();
        let cursor_key = fingerprint(&(&state.revision, &query));
        let offset = match string_argument(&input, "cursor")? {
            None => 0,
            Some(cursor) => {
                let (key, offset) = cursor
                    .split_once(':')
                    .ok_or_else(|| ToolError::new("invalid cursor"))?;
                if key != cursor_key {
                    return Err(ToolError::new("stale cursor"));
                }
                offset
                    .parse::<usize>()
                    .map_err(|_| ToolError::new("invalid cursor"))?
            }
        };
        let limit = number_argument(&input, "limit", 32, 64)?;
        let matches: Vec<_> = state
            .skills
            .values()
            .filter(|s| {
                let d = &s.definition;
                d.name.contains(&query) || d.description.to_lowercase().contains(&query)
            })
            .collect();
        if offset > matches.len() {
            return Err(ToolError::new("cursor out of range"));
        }
        let entries: Vec<_> = matches
            .iter()
            .skip(offset)
            .take(limit)
            .map(|s| json!({"name":s.definition.name,"description":s.definition.description,"revision":s.revision}))
            .collect();
        let next = offset + entries.len();
        let result = json!({"skills":entries,"cursor":if next < matches.len() { Some(format!("{cursor_key}:{next}")) } else { None }});
        state.charge(result.to_string().len())?;
        Ok(result)
    }
}

struct SkillsReadTool(Arc<SkillState>);
#[async_trait]
impl Tool for SkillsReadTool {
    fn origin(&self) -> ToolOrigin {
        ToolOrigin::Skills
    }
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "skills_read".into(),
            description: "Read a configured skill or one of its explicit resource keys. Offsets are Unicode characters.".into(),
            input_schema: json!({"type":"object","properties":{"name":{"type":"string"},"path":{"type":"string"},"offset":{"type":"integer","minimum":0},"limit":{"type":"integer","minimum":1,"maximum":16384}},"required":["name"],"additionalProperties":false}),
            behavior: SKILL_TOOL_BEHAVIOR,
        }
    }
    async fn call(&self, context: ToolContext, input: Value) -> Result<Value, ToolError> {
        skill_arguments(&context, &input, &["name", "path", "offset", "limit"])?;
        let state = &self.0;
        let name =
            string_argument(&input, "name")?.ok_or_else(|| ToolError::new("missing skill name"))?;
        let skill = state
            .skills
            .get(name)
            .ok_or_else(|| ToolError::new("skill_not_found"))?;
        let definition = &skill.definition;
        let path = string_argument(&input, "path")?;
        let reads_body = matches!(path, None | Some("SKILL.md"));
        let text = match path {
            None | Some("SKILL.md") => &definition.instructions,
            Some(key) => {
                if !valid_key(key) {
                    return Err(ToolError::new("invalid resource key"));
                }
                let resource = definition
                    .resources
                    .get(key)
                    .ok_or_else(|| ToolError::new("resource_not_found"))?;
                if !resource.media_type.starts_with("text/")
                    && !matches!(
                        resource.media_type.as_str(),
                        "application/json" | "application/yaml"
                    )
                {
                    return Err(ToolError::new("unsupported_content"));
                }
                &resource.content
            }
        };
        let offset = number_argument(&input, "offset", 0, usize::MAX)?;
        let limit = number_argument(&input, "limit", 16384, 16384)?;
        let total = text.chars().count();
        if offset > total {
            return Err(ToolError::new("offset out of range"));
        }
        let content: String = text.chars().skip(offset).take(limit).collect();
        let next = offset + content.chars().count();
        let result = json!({"name":name,"revision":skill.revision,"path":path.unwrap_or("SKILL.md"),"content":content,"next_offset":if next < total { Some(next) } else { None },"resources":definition.resources.keys().collect::<Vec<_>>()});
        state.charge(result.to_string().len())?;
        if reads_body {
            state
                .loaded
                .lock()
                .unwrap()
                .active
                .insert(name.into(), skill.revision.clone());
        }
        Ok(result)
    }
}
