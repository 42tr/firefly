//! Multiplex assistant state over one authenticated socket; never expose arbitrary API paths.
use super::{
    auth::Session,
    error::AppError,
};
use crate::assistant_events;
use axum::{
    extract::{
        ws::{Message, WebSocket},
        Path, Query, WebSocketUpgrade,
    },
    response::Response,
};
use chrono::Utc;
use serde::Deserialize;
use serde_json::{json, Value};
use std::{collections::BTreeMap, time::Duration};
use tower_cookies::Cookies;

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Topic {
    Conversations,
    Firefly,
    ActiveGeneration,
    Process,
}
impl Topic {
    fn domain(&self) -> &'static str {
        match self {
            Self::Conversations | Self::Firefly | Self::ActiveGeneration | Self::Process => "conversations",
        }
    }
    fn affected_by(&self, domain: &str) -> bool {
        self.domain() == domain
    }
}
#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum Command {
    Subscribe {
        id: String,
        topic: Topic,
        #[serde(default = "empty_params")]
        params: Value,
    },
    Unsubscribe {
        id: String,
    },
    Refresh {
        id: String,
    },
}
fn empty_params() -> Value {
    json!({})
}
struct Subscription {
    topic: Topic,
    params: Value,
    dirty: bool,
    last: Option<Value>,
    revision: u64,
    task: Option<tokio::task::AbortHandle>,
    /// A 4xx snapshot (e.g. a conversation deleted elsewhere) will not heal on its own;
    /// only an explicit resubscribe or refresh retries it.
    rejected: bool,
}

pub async fn connect(
    user: Session, cookies: Cookies, ws: WebSocketUpgrade,
) -> Result<Response, AppError> {
    let cookie = cookies.get("firefly_session").ok_or_else(|| AppError::unauthorized("Missing session"))?;
    let expires = super::jwt::validate(cookie.value()).map_err(|_| AppError::unauthorized("Invalid session"))?.exp;
    Ok(ws.max_message_size(4096).max_frame_size(4096).on_upgrade(move |socket| serve(socket, user.storage_scope().to_owned(), expires)))
}

async fn snapshot(user_id: &str, topic: &Topic, params: Value) -> Result<Value, AppError> {
    let user = || Session::for_storage(user_id);
    // Reuse authenticated read handlers, including their ownership checks and bounded pagination.
    let result: Result<Value, AppError> = async {
        Ok(match topic {
            Topic::Conversations => serde_json::to_value(
                super::assistant::list_conversations(user(), Query(serde_json::from_value(params)?)).await?.0,
            )?,
            Topic::Firefly => super::assistant::firefly::list(user(), Query(serde_json::from_value(params)?)).await?.0,
            Topic::ActiveGeneration => {
                super::assistant::get_active_generation(
                    user(),
                    Path(serde_json::from_value(params["conversation_id"].clone())?),
                )
                .await?
                .0
            }
            Topic::Process => serde_json::to_value(
                super::assistant::get_conversation_process(
                    user(),
                    Path(serde_json::from_value(params["conversation_id"].clone())?),
                    Query(serde_json::from_value(params)?),
                )
                .await?
                .0,
            )?,
        })
    }
    .await;
    result
}

async fn send(socket: &mut WebSocket, value: Value) -> bool {
    matches!(
        tokio::time::timeout(Duration::from_secs(10), socket.send(Message::Text(value.to_string().into()))).await,
        Ok(Ok(()))
    )
}

// Process outputs can be large. After the initial snapshot, send only changed
// records; reconnects always begin with a complete authoritative snapshot.
fn snapshot_message(id: &str, topic: &Topic, previous: Option<&Value>, data: &Value) -> Value {
    if matches!(topic, Topic::Process) {
        if let Some(previous) = previous {
            let old: BTreeMap<_, _> = previous["messages"].as_array().into_iter().flatten()
                .filter_map(|record| record["id"].as_i64().map(|id| (id, record))).collect();
            let new: BTreeMap<_, _> = data["messages"].as_array().into_iter().flatten()
                .filter_map(|record| record["id"].as_i64().map(|id| (id, record))).collect();
            let changed: Vec<_> = new.iter().filter(|(id, record)| old.get(id) != Some(record))
                .map(|(_, record)| *record).collect();
            let removed: Vec<_> = old.keys().filter(|id| !new.contains_key(id)).collect();
            return json!({"type":"snapshot_delta", "id":id, "data":{
                "conversation_id":data["conversation_id"], "job_id":data["job_id"],
                "messages":changed, "removed_ids":removed,
            }});
        }
    }
    json!({"type":"snapshot", "id":id, "data":data})
}
async fn serve(mut socket: WebSocket, user: String, expires: i64) {
    // Subscribe before any read. Lagged consumers receive fresh snapshots, never partial deltas.
    let mut updates = assistant_events::changes().subscribe();
    let mut subscriptions: BTreeMap<String, Subscription> = BTreeMap::new();
    let mut tasks = tokio::task::JoinSet::<(String, u64, Result<Value, AppError>)>::new();
    let mut revision = 0_u64;
    let mut flush = tokio::time::interval(Duration::from_millis(250));
    flush.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut heartbeat = tokio::time::interval(Duration::from_secs(25));
    heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut last_seen = tokio::time::Instant::now();
    let expiry = tokio::time::sleep(Duration::from_secs((expires - Utc::now().timestamp()).max(0) as u64));
    tokio::pin!(expiry);
    loop {
        tokio::select! {
            biased;
            _ = &mut expiry => break,
            message = socket.recv() => {
                match message {
                    Some(Ok(Message::Text(text))) => {
                        last_seen = tokio::time::Instant::now();
                        match serde_json::from_str::<Command>(&text) {
                            Ok(Command::Subscribe { id, topic, params }) if !id.is_empty() && id.len() <= 80 && params.is_object() && (subscriptions.len() < 64 || subscriptions.contains_key(&id)) => {
                                revision += 1;
                                if let Some(old) = subscriptions.insert(id, Subscription { topic, params, dirty: true, last: None, revision, task: None, rejected: false }) {
                                    if let Some(task) = old.task { task.abort(); }
                                }
                            }
                            Ok(Command::Unsubscribe { id }) => { if let Some(s) = subscriptions.remove(&id) { if let Some(task) = s.task { task.abort(); } } }
                            Ok(Command::Refresh { id }) => { if let Some(s) = subscriptions.get_mut(&id) { s.dirty = true; s.last = None; s.rejected = false; } }
                            _ => { if !send(&mut socket, json!({"type":"protocol_error","message":"Invalid subscription"})).await { break; } }
                        }
                    }
                    Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                    _ => last_seen = tokio::time::Instant::now(),
                }
            }
            event = updates.recv() => {
                match event {
                    Ok(event) if event.visible_to(&user) => for s in subscriptions.values_mut().filter(|s| !s.rejected && s.topic.affected_by(event.domain)) { s.dirty = true; },
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => for s in subscriptions.values_mut().filter(|s| !s.rejected) { s.dirty = true; },
                    _ => {}
                }
            }
            _ = heartbeat.tick() => {
                if last_seen.elapsed() > Duration::from_secs(75) { break; }
                if !send(&mut socket, json!({"type":"heartbeat","server_time":Utc::now().timestamp_millis()})).await { break; }
                if !matches!(tokio::time::timeout(Duration::from_secs(10), socket.send(Message::Ping(Vec::new().into()))).await, Ok(Ok(()))) { break; }
                // Retry snapshots that have not produced a value.
                for s in subscriptions.values_mut().filter(|s| !s.rejected) {
                    if s.last.is_none()
                        { s.dirty = true; }
                }
            }
            completed = tasks.join_next(), if !tasks.is_empty() => {
                if let Some(Ok((id, revision, result))) = completed {
                    if let Some(s) = subscriptions.get_mut(&id).filter(|s| s.revision == revision) {
                        s.task = None;
                        match result {
                            Ok(data) => {
                                if s.last.as_ref() != Some(&data) {
                                    if !send(&mut socket, snapshot_message(&id, &s.topic, s.last.as_ref(), &data)).await { break; }
                                    s.last = Some(data);
                                }
                            }
                            Err(err) => {
                                s.rejected = err.status().is_client_error();
                                if s.rejected {
                                    tracing::info!(topic = ?s.topic, params = %s.params, "assistant realtime snapshot rejected: {err:?}");
                                } else {
                                    tracing::warn!(topic = ?s.topic, params = %s.params, "assistant realtime snapshot failed: {err:?}");
                                }
                                s.last = None;
                                if !send(&mut socket, json!({"type":"subscription_error","id":id,"message":"实时状态暂时无法获取，请重试"})).await { break; }
                            }
                        }
                    }
                }
            }
            _ = flush.tick() => {
                for (id, s) in &mut subscriptions {
                    if !s.dirty || s.task.is_some() { continue; }
                    s.dirty = false;
                    let (id, revision, topic, params, user) = (id.clone(), s.revision, s.topic.clone(), s.params.clone(), user.clone());
                    s.task = Some(tasks.spawn(async move {
                        let result = tokio::time::timeout(Duration::from_secs(15), snapshot(&user, &topic, params)).await
                            .unwrap_or_else(|elapsed| Err(elapsed.into()));
                        (id, revision, result)
                    }));
                }
            }
        }
        if expires <= Utc::now().timestamp() {
            break;
        }
    }
    let _ = tokio::time::timeout(Duration::from_secs(1), socket.send(Message::Close(None))).await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn websocket_handshakes_require_sessions_but_not_origin_allowlists() {
        crate::config::init_test_config();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, super::super::app()).await.unwrap();
        });
        let client = reqwest::Client::builder().no_proxy().timeout(Duration::from_secs(5)).build().unwrap();
        let session = super::super::jwt::create(&super::super::jwt::Claims::new(1)).unwrap();
        let expired = super::super::jwt::create(&super::super::jwt::Claims::new(-1)).unwrap();
        for origin in [None, Some("null"), Some("http://192.168.1.3:5173"), Some("https://different.example")] {
            for (token, status) in [
                (Some(session.as_str()), reqwest::StatusCode::SWITCHING_PROTOCOLS),
                (None, reqwest::StatusCode::UNAUTHORIZED),
                (Some("invalid"), reqwest::StatusCode::UNAUTHORIZED),
                (Some(expired.as_str()), reqwest::StatusCode::UNAUTHORIZED),
            ] {
                let mut request = client.get(format!("http://{address}/api/assistant/ws"))
                    .header("connection", "upgrade")
                    .header("upgrade", "websocket")
                    .header("sec-websocket-version", "13")
                    .header("sec-websocket-key", "dGhlIHNhbXBsZSBub25jZQ==");
                if let Some(origin) = origin { request = request.header("origin", origin); }
                if let Some(token) = token { request = request.header("cookie", format!("firefly_session={token}")); }
                let response = request.send().await.unwrap();
                assert_eq!(response.status(), status, "Origin: {origin:?}");
            }
        }
        server.abort();
        let _ = server.await;
    }

    #[test]
    fn subscription_protocol_is_bounded_to_supported_read_topics() {
        assert!(serde_json::from_value::<Command>(json!({"type":"subscribe", "id":"a", "topic":"conversations", "params":{}}))
            .is_ok());
        for command in [
            json!({"type":"subscribe", "id":"a", "topic":"actions", "params":{}}),
            json!({"type":"subscribe", "id":"a", "topic":"metrics_traffic", "params":{}}),
            json!({"type":"subscribe", "id":"a", "topic":"todos", "params":{}}),
            json!({"type":"subscribe", "id":"a", "topic":"pending", "params":{}}),
            json!({"type":"subscribe", "id":"a", "topic":"system", "params":{}}),
            json!({"type":"subscribe", "id":"a", "topic":"metrics_history", "params":{}}),
            json!({"type":"subscribe", "id":"a", "topic":"arbitrary_sql", "params":{}}),
            json!({"type":"subscribe", "id":"a", "topic":"conversations", "user_id":"other"}),
            json!({"type":"delete", "id":"a"}),
        ] {
            assert!(serde_json::from_value::<Command>(command).is_err());
        }
        assert!(Topic::ActiveGeneration.affected_by("conversations"));
        assert!(Topic::Process.affected_by("conversations"));
        assert!(!Topic::Process.affected_by("modules"));
        assert!(!assistant_events::Change { user_id: Some("owner".into()), domain: "conversations" }.visible_to("other"));
    }

    #[test]
    fn process_pushes_only_changed_records_after_the_initial_snapshot() {
        let before = json!({"conversation_id":1, "job_id":"job", "messages":[
            {"id":1, "content":"large unchanged tool output"},
            {"id":2, "content":"pending"},
            {"id":3, "content":"removed"},
        ]});
        let after = json!({"conversation_id":1, "job_id":"job", "messages":[
            {"id":1, "content":"large unchanged tool output"},
            {"id":2, "content":"finished"},
            {"id":4, "content":"new result"},
        ]});
        assert_eq!(snapshot_message("p", &Topic::Process, None, &before)["data"], before);
        let delta = snapshot_message("p", &Topic::Process, Some(&before), &after);
        assert_eq!(delta["type"], "snapshot_delta");
        assert_eq!(delta["data"]["messages"], json!([
            {"id":2, "content":"finished"}, {"id":4, "content":"new result"},
        ]));
        assert_eq!(delta["data"]["removed_ids"], json!([3]));
        assert!(!delta.to_string().contains("large unchanged tool output"));
        assert_eq!(snapshot_message("c", &Topic::Conversations, Some(&before), &after)["type"], "snapshot");
    }

    #[tokio::test]
    async fn db_tests_realtime_snapshots_and_committed_events_are_user_scoped() {
        let pool = crate::db::test_pool().await;
        crate::db::set_test_pool(pool.clone());
        let mut events = assistant_events::changes().subscribe();
        let conversation = crate::assistant_store::create_conversation("owner", crate::assistant_store::NewConversation {
            system_prompt: "",
            title: Some("新的工作"),
            agent: "codex",
            codex_cwd: None,
            provider: None,
            model: None,
            reasoning_level: None,
            parallel: None,
        })
        .await
        .unwrap();
        crate::assistant_store::append_message(
            conversation.id,
            crate::assistant_store::StoredMessage {
                role: "user".into(),
                content: "测试任务".into(),
                tool_call_id: None,
                tool_calls: None,
                metadata: None,
                is_error: false,
                generation_job_id: None,
            },
        )
        .await
        .unwrap();
        crate::assistant_store::rename_conversation(conversation.id, "owner", "推送的新标题").await.unwrap();
        let owner = snapshot("owner", &Topic::Conversations, json!({})).await.unwrap();
        assert_eq!(owner["conversations"][0]["title"], "推送的新标题");
        assert!(snapshot("other", &Topic::Conversations, json!({})).await.unwrap()["conversations"]
            .as_array()
            .unwrap()
            .is_empty());
        let denied = snapshot("other", &Topic::ActiveGeneration, json!({"conversation_id":conversation.id})).await;
        assert!(denied.unwrap_err().status().is_client_error());
        let process_params = json!({"conversation_id":conversation.id, "start_id":1, "end_id":i64::MAX});
        let process = snapshot("owner", &Topic::Process, process_params.clone()).await.unwrap();
        assert_eq!(process["messages"][0]["content"], "测试任务");
        assert!(snapshot("other", &Topic::Process, process_params).await.unwrap_err().status().is_client_error());
        assert!(snapshot("owner", &Topic::Process, json!({"conversation_id":conversation.id})).await.unwrap_err().status().is_client_error());
        let mut own_domains = Vec::new();
        while let Ok(event) = events.try_recv() {
            if event.user_id.as_deref() == Some("owner") {
                assert!(!event.visible_to("other"));
                own_domains.push(event.domain);
            }
        }
        assert!(own_domains.contains(&"conversations"));
        // A new subscription after a disconnect reads authoritative data, not a stale event cache.
        crate::assistant_store::rename_conversation(conversation.id, "owner", "离线期间更新").await.unwrap();
        assert_eq!(
            snapshot("owner", &Topic::Conversations, json!({})).await.unwrap()["conversations"][0]["title"],
            "离线期间更新"
        );
        let active = snapshot("owner", &Topic::ActiveGeneration, json!({"conversation_id":conversation.id})).await;
        assert_eq!(active.unwrap()["active"], false);
    }
}
