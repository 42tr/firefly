//! Codex app-server thread and turn protocol helpers.
use std::{path::Path, time::Duration};
use anyhow::anyhow;
use serde_json::{json, Value};
use tokio::sync::broadcast;

use firefly_codex_transport::app_server;
use super::settings::effective_model;

const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);

pub(crate) fn thread_configuration(provider: Option<&str>, model: Option<&str>) -> Value {
    json!({"provider":provider, "model":effective_model(model)})
}

pub async fn start_thread(cwd: &Path, provider: Option<&str>, model: Option<&str>) -> anyhow::Result<(String, Option<String>)> {
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
    let thread_name = result["thread"]["name"].as_str().map(ToOwned::to_owned);
    client.remember_thread_configuration(thread_id, thread_configuration(provider, model));
    Ok((thread_id.to_owned(), thread_name))
}

pub async fn resume_thread(
    thread_id: &str, cwd: &Path, provider: Option<&str>, model: Option<&str>,
) -> anyhow::Result<Option<String>> {
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
    let result = client.resume_configured_thread(thread_id, params, thread_configuration(provider, model)).await?;
    Ok(result["thread"]["name"].as_str().map(ToOwned::to_owned))
}

pub async fn read_thread_name(thread_id: &str) -> anyhow::Result<Option<String>> {
    let client = app_server().await?;
    let result = client
        .request("thread/read", json!({ "threadId": thread_id, "includeTurns": false }))
        .await?;
    Ok(result["thread"]["name"].as_str().map(ToOwned::to_owned))
}

pub async fn start_turn(
    thread_id: &str, cwd: &Path, input: Vec<Value>, reasoning_level: Option<&str>,
) -> anyhow::Result<(String, broadcast::Receiver<Value>)> {
    let client = app_server().await?;
    let params = turn_params(thread_id, cwd, input, reasoning_level);
    client.start_turn(params, REQUEST_TIMEOUT).await
}

pub(crate) fn turn_params(thread_id: &str, cwd: &Path, input: Vec<Value>, reasoning_level: Option<&str>) -> Value {
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
