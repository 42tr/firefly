//! Bounded task output and historical snapshots for model context.
use serde_json::{json, Value};

use super::{result, TASK_PAGE_SIZE, TASK_PAYLOAD_BYTES};

// Budgets are measured after JSON escaping; a truncated string must remain
// valid UTF-8 and cannot expand an output budget through control characters.
pub(super) fn text_preview(text: &str, budget: usize, tail: bool) -> (String, bool) {
    if serde_json::to_string(text).expect("serialize text").len() <= budget {
        return (text.to_owned(), false);
    }
    let bytes = budget.saturating_sub(2) / 6;
    let preview = if tail {
        let mut start = text.len().saturating_sub(bytes);
        while !text.is_char_boundary(start) { start += 1; }
        &text[start..]
    } else {
        let mut end = bytes.min(text.len());
        while !text.is_char_boundary(end) { end -= 1; }
        &text[..end]
    };
    (preview.to_owned(), true)
}

pub(super) fn result_preview(value: &Value, budget: usize) -> Value {
    let view = result::model_view(value);
    let serialized = serde_json::to_string(&view).expect("serialize task result");
    if serialized.len() <= budget { return view; }
    let data = serde_json::to_string(view.get("structuredContent").unwrap_or(&view)).expect("serialize preview");
    let mut preserved = json!({});
    for key in ["isError", "error", "warning"] {
        if let Some(field) = view.get(key) {
            if field.to_string().len() <= 128 { preserved[key] = field.clone(); }
        }
    }
    for pointer in ["/structuredContent/status", "/structuredContent/error", "/structuredContent/warning", "/structuredContent/body/error", "/structuredContent/body/warning"] {
        if let Some(field)=view.pointer(pointer).filter(|value|!value.is_null() && value.to_string().len()<=128) { preserved[pointer]=field.clone(); }
    }
    if let Some(content) = view["content"].as_array() {
        let media: Vec<_> = content.iter().filter(|part| part["type"] != "text").take(4)
            .map(|part| {
                let mut descriptor=json!({"type":part["type"],"mimeType":part["mimeType"],"data_omitted":true});
                for key in ["name","uri","filename"] {
                    if let Some(field)=part.get(key).filter(|value|value.to_string().len()<=128) { descriptor[key]=field.clone(); }
                }
                if let Some(uri)=part.pointer("/resource/uri").filter(|value|value.to_string().len()<=128) { descriptor["uri"]=uri.clone(); }
                descriptor
            }).collect();
        if !media.is_empty() { preserved["content"] = json!(media); }
    }
    let kept = preserved.to_string().len();
    let (preview, _) = text_preview(&data, budget.saturating_sub(128 + kept), false);
    json!({"truncated":true,"original_bytes":value.to_string().len(),"preview":preview,"preserved":preserved})
}

/// Compact legacy task snapshots only for model context, preserving the stored
/// transcript and task details. Older get_tasks replies can contain megabytes
/// of already delivered history, before the current paging rules existed.
pub(crate) fn bounded_snapshot_history(value: Value) -> Value {
    let Some(completed) = value["completed"].as_array() else { return value; };
    let Some(pending) = value["pending"].as_array() else { return value; };
    if (completed.is_empty() && pending.is_empty())
        || !completed.iter().chain(pending).all(|task| task["task_id"].is_string() && task["status"].is_string())
        || value.to_string().len() <= 64 * 1024 {
        return value;
    }
    let done_start = completed.len().saturating_sub(TASK_PAGE_SIZE);
    let pending_len = pending.len().min(TASK_PAGE_SIZE);
    let payload = TASK_PAYLOAD_BYTES / (completed.len() - done_start + pending_len).max(1);
    let compact = |task: &Value| {
        let mut summary = json!({});
        for key in ["id", "task_id", "tool", "title", "status", "detached", "parent_task_id", "scope_id", "started_at", "deadline_at", "error"] {
            if let Some(field) = task.get(key) {
                summary[key] = if matches!(key, "title" | "error" | "scope_id") && field.is_string() {
                    json!(text_preview(field.as_str().unwrap(), 512, false).0)
                } else { field.clone() };
            }
        }
        if let Some(result) = task.get("result") { summary["result"] = result_preview(result, payload / 2); }
        let output = if let Some(output) = task["output_preview"].as_str() { output.to_owned() } else {
            task["recent_output"].as_array().into_iter().flatten()
                .filter_map(|event| event["data"]["text"].as_str()).collect()
        };
        let (preview, truncated) = text_preview(&output, payload / 2, true);
        summary["output_preview"] = json!(preview);
        summary["output_truncated"] = json!(truncated || task["output_truncated"] == true);
        summary
    };
    json!({
        "completed":completed[done_start..].iter().map(compact).collect::<Vec<_>>(),
        "pending":pending[..pending_len].iter().map(compact).collect::<Vec<_>>(),
        "historical_snapshot_truncated":true,
        "omitted_completed":done_start,"omitted_pending":pending.len()-pending_len,
        "hint":"历史任务结果仅保留有界概况，完整记录仍在任务详情中；不应根据此旧快照判断当前任务状态。"
    })
}
