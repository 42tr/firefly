use anyhow::anyhow;
use async_trait::async_trait;
use g::{Tool, ToolBehavior, ToolError, ToolSpec};
use schemars::{schema_for, JsonSchema};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::config;

/// 生成 OpenAI 兼容的 tool parameters schema。
/// 部分 provider（如 DashScope）要求 object 类型必须带 properties 字段，
/// 且不接受 $schema 等 JSON Schema 元字段，这里统一清洗。
fn openai_tool_schema<T: JsonSchema>() -> Value {
    let mut schema = serde_json::to_value(schema_for!(T)).unwrap();
    if let Value::Object(map) = &mut schema {
        map.remove("$schema");
        map.remove("title");
        map.entry("properties".to_string()).or_insert_with(|| json!({}));
    }
    schema
}

#[derive(Clone, Debug, Serialize)]
pub struct ToolDefinition {
    pub name: String,
    pub title: String,
    pub description: String,
    pub input_schema: Value,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ToolAttachment {
    pub file_name: String,
    pub content_type: String,
    pub file_size: i64,
    pub storage_path: String,
}

#[derive(Clone, Debug)]
pub struct ToolResponse {
    pub value: Value,
    pub attachments: Vec<ToolAttachment>,
}

impl ToolResponse {
    pub fn new(value: Value) -> Self {
        Self { value, attachments: Vec::new() }
    }

}

#[derive(Clone, Debug)]
pub struct ToolContext {
    pub user_id: String,
}

fn wrap_tool_response(resp: ToolResponse) -> Value {
    let attachments = resp.attachments.iter().map(|a| serde_json::to_value(a).unwrap()).collect::<Vec<_>>();
    json!({ "response": resp.value, "attachments": attachments })
}

#[derive(Clone)]
struct McpAgentTool {
    definition: ToolDefinition,
    ctx: ToolContext,
}

#[async_trait]
impl Tool for McpAgentTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: self.definition.name.clone(),
            description: self.definition.description.clone(),
            input_schema: self.definition.input_schema.clone(),
            behavior: ToolBehavior::default(),
        }
    }

    async fn call(&self, _context: g::ToolContext, input: Value) -> Result<Value, ToolError> {
        let response = call_tool(&self.definition.name, input, Some(&self.ctx))
            .await
            .map_err(|err| ToolError::new(format!("{:#}", err)))?;
        Ok(wrap_tool_response(response))
    }
}

pub fn build_tool_set(ctx: ToolContext) -> Vec<std::sync::Arc<dyn Tool>> {
    list_tools()
        .into_iter()
        .map(|definition| {
            std::sync::Arc::new(McpAgentTool { definition, ctx: ctx.clone() }) as std::sync::Arc<dyn Tool>
        })
        .collect()
}

pub fn list_tools() -> Vec<ToolDefinition> {
    let mut tools = Vec::new();
    if config::try_get().map(|cfg| cfg.assistant_http_tool_enabled).unwrap_or(false) {
        tools.push(ToolDefinition {
            name: "http_request".to_string(),
            title: "HTTP 请求".to_string(),
            description:
                "发起一次 HTTP 请求并返回状态码与响应体。按接口的 URL、方法、参数说明调用此工具执行。\
                3xx 重定向不会自动跟随，如需跳转请对 Location 再次调用。"
                    .to_string(),
            input_schema: openai_tool_schema::<firefly_safe_http::Request>(),
        });
    }
    tools
}

pub async fn call_tool(name: &str, arguments: Value, _ctx: Option<&ToolContext>) -> anyhow::Result<ToolResponse> {
    match name {
        "http_request" => {
            if !config::get().assistant_http_tool_enabled {
                return Err(anyhow!("http_request 工具未启用（ASSISTANT_HTTP_TOOL_ENABLED=false）"));
            }
            let request: firefly_safe_http::Request =
                serde_json::from_value(arguments).map_err(|err| anyhow!("参数格式错误: {}", err))?;
            let allowed_hosts = config::get()
                .assistant_http_tool_allowed_hosts
                .split(',')
                .map(str::trim)
                .filter(|host| !host.is_empty())
                .map(str::to_owned)
                .collect::<Vec<_>>();
            let response = firefly_safe_http::execute(request, &allowed_hosts).await?;
            Ok(ToolResponse::new(response))
        }
        _ => Err(anyhow!("Unknown tool: {}", name)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn g_tool_set_matches_public_tool_definitions() {
        let ctx = ToolContext { user_id: "test-user".to_string() };
        let mut actual = build_tool_set(ctx).into_iter().map(|tool| tool.spec()).collect::<Vec<_>>();
        let mut expected = list_tools();
        actual.sort_by(|left, right| left.name.cmp(&right.name));
        expected.sort_by(|left, right| left.name.cmp(&right.name));

        assert_eq!(actual.len(), expected.len());
        for (actual, expected) in actual.iter().zip(expected.iter()) {
            assert_eq!(actual.name, expected.name);
            assert_eq!(actual.description, expected.description);
            assert_eq!(actual.input_schema, expected.input_schema);
        }
    }

}
