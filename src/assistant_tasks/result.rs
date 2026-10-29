//! Model-facing access to the original, durable task result. Never replay a tool.
use anyhow::{anyhow, Context};
use async_trait::async_trait;
use g::{Tool, ToolBehavior, ToolError, ToolSpec};
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::{query_as, SqlitePool};

use super::TaskRecord;

const PAGE_BYTES: usize = 4096;

pub struct ReadTaskResultTool;

#[async_trait]
impl Tool for ReadTaskResultTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "read_task_result".into(),
            description: "Read a saved task result without rerunning its tool. Call this tool directly. Select a field using JSON Pointer, for example /structuredContent/body/prices, then page arrays with offset and limit. Each returned item is complete; next_offset continues the page. Use total to select the last records. Task previews list readable fields. Only tasks in your user/conversation/handoff scope are readable.".into(),
            input_schema: json!({"type":"object","properties":{
                "task_id":{"type":"string","minLength":1},
                "json_pointer":{"type":"string","default":"","description":"RFC 6901 pointer into the ORIGINAL result. Empty selects the root. Escape ~ as ~0 and / as ~1."},
                "offset":{"type":"integer","minimum":0,"default":0},
                "limit":{"type":"integer","minimum":1,"maximum":128,"default":32}
            },"required":["task_id"],"additionalProperties":false}),
            behavior: ToolBehavior { read_only: true, idempotent: true, parallel_safe: true },
        }
    }

    async fn call(&self, _: g::ToolContext, _: Value) -> Result<Value, ToolError> {
        Err(ToolError::new("The host must submit read_task_result as a task so it can enforce the execution's handoff scope"))
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Query {
    task_id: String,
    #[serde(default)]
    json_pointer: String,
    #[serde(default)]
    offset: usize,
    #[serde(default = "default_limit")]
    limit: usize,
}
fn default_limit() -> usize {
    32
}

pub(super) async fn read(
    pool: &SqlitePool,
    execution: &TaskRecord,
    arguments: Value,
) -> anyhow::Result<Value> {
    let args: Query = serde_json::from_value(arguments)?;
    anyhow::ensure!((1..=128).contains(&args.limit), "limit 必须为 1–128");
    validate_pointer(&args.json_pointer)?;
    let task: TaskRecord = query_as("SELECT * FROM assistant_tasks WHERE id=?1 AND user_id=?2 AND conversation_id=?3 AND (?4 IS NULL OR (generation_job_id=?5 AND scope_id=?4))")
        .bind(&args.task_id).bind(&execution.user_id).bind(execution.conversation_id)
        .bind(&execution.scope_id).bind(&execution.generation_job_id)
        .fetch_optional(pool).await?.ok_or_else(|| anyhow!("任务不存在或无权访问当前作用域之外的结果"))?;
    let result = task
        .result
        .as_ref()
        .context("任务尚无结果；请先等待任务结束")?;
    let selected = result
        .pointer(&args.json_pointer)
        .context("JSON Pointer 对应的字段不存在；请使用预览中的 readable_fields")?;
    let mut page = json!({"task_id":task.id,"status":task.status,"json_pointer":args.json_pointer,
        "data":null,"total":null,"next_offset":null,"page_bytes_limit":PAGE_BYTES});
    if let Some(array) = selected.as_array() {
        anyhow::ensure!(
            args.offset <= array.len(),
            "offset 超过数组长度 {}",
            array.len()
        );
        page["total"] = json!(array.len());
        page["offset"] = json!(args.offset);
        page["data"] = json!([]);
        for item in array.iter().skip(args.offset).take(args.limit) {
            page["data"].as_array_mut().unwrap().push(item.clone());
            let end = args.offset + page["data"].as_array().unwrap().len();
            page["next_offset"] = if end < array.len() {
                json!(end)
            } else {
                Value::Null
            };
            if serde_json::to_vec(&page)?.len() > PAGE_BYTES {
                page["data"].as_array_mut().unwrap().pop();
                anyhow::ensure!(
                    !page["data"].as_array().unwrap().is_empty(),
                    "单条结果超过 {PAGE_BYTES} 字节；请用更具体的 JSON Pointer 读取该条目的字段"
                );
                let end = args.offset + page["data"].as_array().unwrap().len();
                page["next_offset"] = json!(end);
                break;
            }
        }
    } else {
        anyhow::ensure!(args.offset == 0, "offset 仅适用于数组");
        page["data"] = selected.clone();
    }
    anyhow::ensure!(
        serde_json::to_vec(&page)?.len() <= PAGE_BYTES,
        "所选字段超过 {PAGE_BYTES} 字节；请用更具体的 JSON Pointer 选择字段或数组"
    );
    tracing::info!(job_id = %execution.generation_job_id, task_id = %task.id,
        json_pointer = %args.json_pointer, bytes = serde_json::to_vec(&page)?.len(), "saved task result read");
    Ok(page)
}

fn validate_pointer(pointer: &str) -> anyhow::Result<()> {
    anyhow::ensure!(
        pointer.is_empty() || pointer.starts_with('/'),
        "JSON Pointer 必须为空或以 / 开头"
    );
    let mut chars = pointer.chars();
    while let Some(c) = chars.next() {
        if c == '~' {
            anyhow::ensure!(
                matches!(chars.next(), Some('0' | '1')),
                "JSON Pointer 的 ~ 只能转义为 ~0 或 ~1"
            );
        }
    }
    Ok(())
}

pub(super) fn model_view(value: &Value) -> Value {
    let mut view = value.clone();
    if let Some(structured) = value.get("structuredContent") {
        if let Some(content) = view.get_mut("content").and_then(Value::as_array_mut) {
            content.retain(|part| {
                part.get("text")
                    .and_then(Value::as_str)
                    .and_then(|text| serde_json::from_str::<Value>(text).ok())
                    .as_ref()
                    != Some(structured)
            });
            if content.is_empty() {
                view.as_object_mut().unwrap().remove("content");
            }
        }
    }
    view
}

pub(super) fn fields(value: &Value) -> Vec<Value> {
    fn walk(value: &Value, path: String, depth: usize, result: &mut Vec<Value>) {
        if result.len() >= 6 || path.len() > 128 {
            return;
        }
        if let Some(object) = value.as_object().filter(|_| depth < 4) {
            for (key, child) in object {
                if path.is_empty() && key == "content" && value.get("structuredContent").is_some() {
                    continue;
                }
                walk(
                    child,
                    format!("{path}/{}", key.replace('~', "~0").replace('/', "~1")),
                    depth + 1,
                    result,
                );
            }
        } else {
            result.push(json!({"json_pointer":path,"type":if value.is_array(){"array"}else{"value"},"total":value.as_array().map(Vec::len)}));
        }
    }
    let mut result = Vec::new();
    // Structured data first; the MCP text envelope often duplicates all of it.
    if let Some(structured) = value.get("structuredContent") {
        walk(structured, "/structuredContent".into(), 1, &mut result);
    }
    if result.len() < 6 {
        walk(value, String::new(), 0, &mut result);
    }
    result.dedup();
    result
}

pub(super) fn reading_hint(task_id: &str, value: &Value) -> Value {
    let fields = fields(value);
    let pointer = fields
        .iter()
        .find(|field| field["type"] == "array")
        .and_then(|field| field["json_pointer"].as_str())
        .unwrap_or("");
    json!({"tool":"read_task_result","arguments":{"task_id":task_id,"json_pointer":pointer,"offset":0,"limit":32}})
}
