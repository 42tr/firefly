use std::{
    collections::HashMap,
    process::Stdio,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, LazyLock, Mutex,
    },
    time::{Duration, Instant},
};

use anyhow::{anyhow, Context};

use serde_json::{json, Value};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::{Child, ChildStdin, Command},
    sync::{broadcast, oneshot, Mutex as AsyncMutex},
    time::timeout,
};
use tracing::{debug, error, info, trace, warn, Level};

mod process_control;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);
const INTERRUPT_TIMEOUT: Duration = Duration::from_secs(15);
const NOTIFICATION_CAPACITY: usize = 4096;
pub struct CodexAppServer {
    stdin: AsyncMutex<ChildStdin>,
    // Keeping the child in the manager keeps kill_on_drop effective for the
    // lifetime of the process-wide client.
    _child: AsyncMutex<Child>,
    process_group: crate::process_control::ProcessGroup,
    pending: Mutex<HashMap<u64, oneshot::Sender<anyhow::Result<Value>>>>,
    thread_configurations: Mutex<HashMap<String, Value>>,
    notifications: broadcast::Sender<Value>,
    next_id: AtomicU64,
    // Set by the stdout reader once the process exits, so the next caller respawns it.
    closed: AtomicBool,
}

struct PendingRequest<'a> {
    server: &'a CodexAppServer,
    id: u64,
}

#[derive(Debug, thiserror::Error)]
#[error("{0}")]
struct RpcError(String);

impl Drop for PendingRequest<'_> {
    fn drop(&mut self) {
        self.server.pending.lock().unwrap().remove(&self.id);
    }
}

// A cancelled partial write would corrupt the next NDJSON frame. Retire the
// connection before another caller can use it, including cancellation by Drop.
struct MessageWrite<'a> {
    server: &'a CodexAppServer,
    complete: bool,
}

impl Drop for MessageWrite<'_> {
    fn drop(&mut self) {
        if !self.complete { self.server.close("Codex 消息写入中断"); }
    }
}

struct InitializationGuard(Arc<CodexAppServer>, bool);

impl Drop for InitializationGuard {
    fn drop(&mut self) {
        if !self.1 { self.0.close("Codex 初始化未完成"); }
    }
}

// An abandoned turn/start may already have started work even if its response
// never arrived. Retire that server when the caller cannot obtain the turn ID.
struct TurnStartGuard(Arc<CodexAppServer>, bool);

impl Drop for TurnStartGuard {
    fn drop(&mut self) {
        if !self.1 { self.0.close("Codex turn/start 未完成，停止未确认的执行"); }
    }
}

static CLIENT: AsyncMutex<Option<Arc<CodexAppServer>>> = AsyncMutex::const_new(None);

// stderr also carries regular tracing events. Strip terminal formatting before
// reading the level, and keep unstructured diagnostics visible as warnings.
fn app_server_log_record(line: &str) -> Option<(Level, String)> {
    static ANSI: LazyLock<regex::Regex> = LazyLock::new(|| {
        regex::Regex::new(r"\x1b(?:\[[0-?]*[ -/]*[@-~]|\][^\x07\x1b]*(?:\x07|\x1b\\))")
            .expect("valid terminal escape regex")
    });
    let plain = ANSI.replace_all(line, "");
    let plain = plain.trim();
    if plain.is_empty() {
        return None;
    }
    let mut fields = plain.split_whitespace();
    let first = fields.next()?;
    let level = if chrono::DateTime::parse_from_rfc3339(first).is_ok() {
        fields.next().unwrap_or("")
    } else {
        first
    };
    let level = level.trim_end_matches(':').parse::<Level>().unwrap_or(Level::WARN);
    // Transient transport recovery is reported through turn events. Preserve
    // terminal failures and all other stderr diagnostics in the service log.
    if level == Level::WARN && (
        plain.contains("codex_core::responses_retry: stream disconnected - retrying sampling request")
        || plain.contains("codex_core::client: falling back to HTTP")
    ) {
        return None;
    }
    Some((level, plain.to_owned()))
}

const HISTORY_WARNING_WINDOW: Duration = Duration::from_secs(5);

struct HistoryWarningBatch {
    turn_id: String,
    repeated: u64,
    deadline: Instant,
}

#[derive(Default)]
struct AppServerLogBatch {
    history: Option<HistoryWarningBatch>,
}

impl AppServerLogBatch {
    // History reconstruction may report every discarded item in the same turn.
    // Keep the first diagnostic, then count only this specific warning in a
    // bounded window. All other warnings and errors remain visible immediately.
    fn push(&mut self, line: &str, now: Instant) -> Vec<(Level, String)> {
        let mut records = self.flush_expired(now);
        let Some((level, message)) = app_server_log_record(line) else { return records };
        let turn_id = (level == Level::WARN)
            .then(|| message.split_once("codex_app_server_protocol::protocol::thread_history: "))
            .flatten()
            .and_then(|(_, body)| body.strip_prefix("dropping turn-scoped item for unknown turn id `"))
            .and_then(|body| body.split_once('`'))
            .map(|(turn_id, _)| turn_id);
        if let Some(turn_id) = turn_id {
            if let Some(batch) = self.history.as_mut().filter(|batch| batch.turn_id == turn_id) {
                batch.repeated += 1;
                return records;
            }
            records.extend(self.flush());
            self.history = Some(HistoryWarningBatch {
                turn_id: turn_id.to_owned(),
                repeated: 0,
                deadline: now + HISTORY_WARNING_WINDOW,
            });
        } else {
            records.extend(self.flush());
        }
        records.push((level, message));
        records
    }

    fn flush_expired(&mut self, now: Instant) -> Vec<(Level, String)> {
        if self.history.as_ref().is_some_and(|batch| now >= batch.deadline) {
            self.flush()
        } else {
            Vec::new()
        }
    }

    fn flush(&mut self) -> Vec<(Level, String)> {
        self.history.take().filter(|batch| batch.repeated > 0).map(|batch| {
            vec![(Level::WARN, format!(
                "codex_app_server_protocol::protocol::thread_history: merged {} additional warnings about dropped items for unknown turn id `{}`",
                batch.repeated, batch.turn_id
            ))]
        }).unwrap_or_default()
    }
}

fn log_app_server_records(records: Vec<(Level, String)>) {
    for (level, message) in records {
        match level {
            Level::ERROR => error!("codex app-server: {}", message),
            Level::WARN => warn!("codex app-server: {}", message),
            Level::INFO => info!("codex app-server: {}", message),
            Level::DEBUG => debug!("codex app-server: {}", message),
            Level::TRACE => trace!("codex app-server: {}", message),
        }
    }
}

pub async fn app_server() -> anyhow::Result<Arc<CodexAppServer>> {
    reuse_or_spawn_client(&CLIENT, CodexAppServer::spawn).await
}

async fn reuse_or_spawn_client<F, Fut>(slot: &AsyncMutex<Option<Arc<CodexAppServer>>>, spawn: F) -> anyhow::Result<Arc<CodexAppServer>>
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = anyhow::Result<Arc<CodexAppServer>>>,
{
    let mut client = slot.lock().await;
    if let Some(server) = client.as_ref().filter(|server| !server.closed.load(Ordering::Acquire)) {
        return Ok(server.clone());
    }
    let server = spawn().await?;
    *client = Some(server.clone());
    Ok(server)
}

impl CodexAppServer {
    async fn spawn() -> anyhow::Result<Arc<Self>> {
        let mut command = Command::new("codex");
        command.arg("app-server")
            // Settings changes explicitly unsubscribe one idle thread before resuming it.
            .args(["-c", "thread_unload_delay_secs=0"])
            // Keep Firefly's filter independent from Codex's verbose span logs.
            .env("RUST_LOG", "warn")
            .env("NO_COLOR", "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        Self::spawn_command(command, REQUEST_TIMEOUT).await
    }

    async fn spawn_command(mut command: Command, request_timeout: Duration) -> anyhow::Result<Arc<Self>> {
        let mut child = command.kill_on_drop(true).process_group(0).spawn()
            .context("启动 codex app-server 失败，请确认 Codex CLI 已安装")?;
        let process_group = crate::process_control::ProcessGroup::new(child.id().context("Codex 缺少进程编号")?);

        let stdin = child.stdin.take().ok_or_else(|| anyhow!("codex app-server 缺少 stdin"))?;
        let stdout = child.stdout.take().ok_or_else(|| anyhow!("codex app-server 缺少 stdout"))?;
        let stderr = child.stderr.take().ok_or_else(|| anyhow!("codex app-server 缺少 stderr"))?;
        let (notifications, _) = broadcast::channel(NOTIFICATION_CAPACITY);
        let server = Self {
            stdin: AsyncMutex::new(stdin),
            _child: AsyncMutex::new(child),
            process_group,
            pending: Mutex::new(HashMap::new()),
            thread_configurations: Mutex::new(HashMap::new()),
            notifications,
            next_id: AtomicU64::new(1),
            closed: AtomicBool::new(false),
        };
        let server = Arc::new(server);
        let mut initialization = InitializationGuard(server.clone(), false);

        let stdout_server = Arc::downgrade(&server);
        tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                if line.trim().is_empty() {
                    continue;
                }
                let Some(server) = stdout_server.upgrade() else { break };
                match serde_json::from_str::<Value>(&line) {
                    Ok(message) => server.handle_message(message).await,
                    Err(err) => warn!("无法解析 codex app-server 消息: {}", err),
                }
            }
            if let Some(server) = stdout_server.upgrade() {
                server.close("codex app-server 连接已关闭");
            }
        });

        tokio::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            let mut batch = AppServerLogBatch::default();
            let mut tick = tokio::time::interval(Duration::from_secs(1));
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                tokio::select! {
                    line = lines.next_line() => match line {
                        Ok(Some(line)) => log_app_server_records(batch.push(&line, Instant::now())),
                        _ => break,
                    },
                    _ = tick.tick() => log_app_server_records(batch.flush_expired(Instant::now())),
                }
            }
            log_app_server_records(batch.flush());
        });

        let init_result = server
            .request_with_timeout(
                "initialize",
                json!({
                    "clientInfo": {
                        "name": "firefly_assistant",
                        "title": "Firefly Assistant",
                        "version": env!("CARGO_PKG_VERSION")
                    }
                }),
                request_timeout,
            )
            .await;
        if let Err(error) = init_result {
            server.shutdown().await;
            return Err(error);
        }
        if let Err(error) = server.notify("initialized", json!({})).await {
            server.shutdown().await;
            return Err(error);
        }
        initialization.1 = true;
        Ok(server)
    }

    fn close(&self, message: &str) {
        if self.closed.swap(true, Ordering::AcqRel) { return; }
        self.process_group.kill();
        let pending = std::mem::take(&mut *self.pending.lock().unwrap());
        for (_, sender) in pending { let _ = sender.send(Err(anyhow!("{}", message))); }
        let _ = self.notifications.send(json!({"method": "server/closed", "params": {"message": message}}));
    }

    async fn shutdown(&self) {
        self.close("codex app-server 已停止");
        let _ = timeout(Duration::from_secs(5), async { self._child.lock().await.wait().await }).await;
    }

    async fn handle_message(&self, message: Value) {
        if let Some(id) = message.get("id").and_then(Value::as_u64) {
            // Server requests and client requests have independent ID spaces.
            let sender = if message.get("method").is_none() { self.pending.lock().unwrap().remove(&id) } else { None };
            if let Some(sender) = sender {
                let response = if let Some(error) = message.get("error") {
                    Err(RpcError(error.get("message").and_then(Value::as_str).unwrap_or("Codex 请求失败").to_owned()).into())
                } else {
                    Ok(message.get("result").cloned().unwrap_or(Value::Null))
                };
                let _ = sender.send(response);
                return;
            }

            // approvalPolicy=never should prevent these requests. Declining an
            // unexpected server request avoids leaving a turn permanently stuck.
            if let Some(method) = message.get("method").and_then(Value::as_str) {
                let result = match method {
                    "item/commandExecution/requestApproval" | "item/fileChange/requestApproval" => {
                        json!({ "decision": "decline" })
                    }
                    "item/permissions/requestApproval" => json!({ "permissions": {} }),
                    "item/tool/requestUserInput" | "mcpServer/elicitation/request" => {
                        json!({ "action": "decline", "content": null })
                    }
                    _ => json!({}),
                };
                let _ = timeout(REQUEST_TIMEOUT, self.send_message(json!({ "id": id, "result": result }))).await;
                return;
            }
        }

        if message.get("method").is_some() {
            let _ = self.notifications.send(message);
        }
    }

    async fn send_message(&self, message: Value) -> anyhow::Result<()> {
        let line = serde_json::to_string(&message)?;
        let mut stdin = self.stdin.lock().await;
        if self.closed.load(Ordering::Acquire) { return Err(anyhow!("codex app-server 连接已关闭")); }
        let mut write = MessageWrite { server: self, complete: false };
        stdin.write_all(line.as_bytes()).await?;
        stdin.write_all(b"\n").await?;
        stdin.flush().await?;
        write.complete = true;
        Ok(())
    }

    async fn notify(&self, method: &str, params: Value) -> anyhow::Result<()> {
        timeout(REQUEST_TIMEOUT, self.send_message(json!({ "method": method, "params": params })))
            .await.context("Codex 通知写入超时")?
    }

    pub async fn request(&self, method: &str, params: Value) -> anyhow::Result<Value> {
        self.request_with_timeout(method, params, REQUEST_TIMEOUT).await
    }

    pub async fn request_with_timeout(&self, method: &str, params: Value, limit: Duration) -> anyhow::Result<Value> {
        if self.closed.load(Ordering::Acquire) { return Err(anyhow!("codex app-server 连接已关闭")); }
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (sender, receiver) = oneshot::channel();
        self.pending.lock().unwrap().insert(id, sender);
        let _pending = PendingRequest { server: self, id };
        timeout(limit, async {
            self.send_message(json!({ "id": id, "method": method, "params": params })).await?;
            receiver.await.context("Codex 请求通道已关闭")?
        }).await.with_context(|| format!("Codex 请求超时: {method}"))?
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Value> {
        self.notifications.subscribe()
    }

    pub async fn start_turn(self: &Arc<Self>, params: Value, limit: Duration) -> anyhow::Result<(String, broadcast::Receiver<Value>)> {
        let mut started = TurnStartGuard(self.clone(), false);
        let events = self.subscribe();
        let result = self.request_with_timeout("turn/start", params, limit).await;
        // An explicit RPC error confirms that start was rejected. A timeout or
        // transport failure leaves its execution state unknown and retires it.
        if result.as_ref().is_err_and(|error| error.downcast_ref::<RpcError>().is_some()) {
            started.1 = true;
        }
        let result = result?;
        let turn_id = result["turn"]["id"].as_str().map(ToOwned::to_owned)
            .ok_or_else(|| anyhow!("Codex 未返回 turn id"))?;
        started.1 = true;
        Ok((turn_id, events))
    }

    // Conversations archived by older Firefly versions become ordinary conversations.
    // Restore the native Codex session lazily when its first resume reports archival.
    pub async fn resume_stored_thread(&self, thread_id: &str, params: Value) -> anyhow::Result<Value> {
        match self.request("thread/resume", params.clone()).await {
            Ok(result) => Ok(result),
            Err(error) => {
                let archived = error.downcast_ref::<RpcError>()
                    .is_some_and(|error| error.0.contains(&format!("session {thread_id} is archived")));
                if !archived { return Err(error); }
                self.request("thread/unarchive", json!({ "threadId": thread_id })).await?;
                self.request("thread/resume", params).await
            }
        }
    }

    pub fn remember_thread_configuration(&self, thread_id: &str, configuration: Value) {
        self.thread_configurations.lock().unwrap().insert(thread_id.to_owned(), configuration);
    }

    /// Loaded Codex threads ignore resume overrides. Reload only the idle thread whose settings changed.
    pub async fn resume_configured_thread(&self, thread_id: &str, params: Value, configuration: Value) -> anyhow::Result<()> {
        let changed = self.thread_configurations.lock().unwrap().get(thread_id)
            .is_some_and(|previous| previous != &configuration);
        if changed { self.unload_idle_thread(thread_id).await?; }
        let mut result = self.resume_stored_thread(thread_id, params.clone()).await?;
        let matches = |result: &Value| ["modelProvider", "model"].iter().all(|field| {
            params[*field].as_str().is_none_or(|expected| result[*field].as_str() == Some(expected))
        });
        // Also handle a thread loaded before configuration tracking began.
        if !matches(&result) && !changed {
            self.unload_idle_thread(thread_id).await?;
            result = self.resume_stored_thread(thread_id, params.clone()).await?;
        }
        if !matches(&result) { return Err(anyhow!("Codex 未应用所选平台或模型，未启动新一轮任务")); }
        self.remember_thread_configuration(thread_id, configuration);
        Ok(())
    }

    async fn unload_idle_thread(&self, thread_id: &str) -> anyhow::Result<()> {
        let mut events = self.subscribe();
        let result = self.request("thread/unsubscribe", json!({ "threadId": thread_id })).await?;
        if result["status"] == "notLoaded" { return Ok(()); }
        timeout(INTERRUPT_TIMEOUT, async {
            loop {
                let event = events.recv().await.context("等待 Codex 会话卸载失败")?;
                if notification_method(&event) == Some("thread/closed")
                    && notification_params(&event)["threadId"].as_str() == Some(thread_id) {
                    return Ok::<_, anyhow::Error>(());
                }
            }
        }).await.context("Codex 会话未及时卸载，未启动新一轮任务")?
    }

    pub async fn read_turn(&self, thread_id: &str, turn_id: &str) -> anyhow::Result<Option<Value>> {
        let result = self.request("thread/turns/list", json!({
            "threadId": thread_id, "itemsView": "notLoaded", "limit": 10, "sortDirection": "desc"
        })).await?;
        Ok(result["data"].as_array()
            .and_then(|turns| turns.iter().find(|turn| turn["id"].as_str() == Some(turn_id)))
            .cloned())
    }

    pub async fn interrupt_turn(&self, thread_id: &str, turn_id: &str, limit: Duration) -> anyhow::Result<()> {
        let stopped = |turn: &Value| matches!(turn["status"].as_str(), Some("completed" | "failed" | "interrupted"));
        let mut events = self.subscribe();
        let confirmation = timeout(limit, async {
            if self.read_turn(thread_id, turn_id).await?.as_ref().is_some_and(stopped) {
                return Ok::<_, anyhow::Error>(());
            }
            self.request("turn/interrupt", json!({ "threadId": thread_id, "turnId": turn_id })).await?;
            let mut poll = tokio::time::interval_at(
                tokio::time::Instant::now() + Duration::from_millis(250), Duration::from_millis(250),
            );
            loop {
                tokio::select! {
                    event = events.recv() => match event {
                        Ok(event) if notification_method(&event) == Some("turn/completed")
                            && notification_params(&event)["turn"]["id"].as_str() == Some(turn_id) => return Ok(()),
                        Ok(event) if notification_method(&event) == Some("server/closed") => return Err(anyhow!("Codex 取消期间连接关闭")),
                        Err(broadcast::error::RecvError::Closed) => return Err(anyhow!("Codex 取消期间事件流关闭")),
                        _ => {}
                    },
                    turn = async { poll.tick().await; self.read_turn(thread_id, turn_id).await } => {
                        if turn?.as_ref().is_some_and(stopped) {
                            return Ok(());
                        }
                    }
                }
            }
        }).await;
        match confirmation {
            Ok(Ok(())) => Ok(()),
            error => {
                // A wedged app-server cannot confirm a per-turn stop. Killing
                // its group is the final fallback, and retires its shared client.
                warn!(thread_id, turn_id, ?error, "Codex 未确认 turn 停止，终止 app-server 进程组");
                self.close("Codex turn 停止未确认，app-server 已终止");
                timeout(Duration::from_secs(5), async { self._child.lock().await.wait().await }).await
                    .context("等待 Codex app-server 退出超时")?.context("等待 Codex app-server 退出失败")?;
                Ok(())
            }
        }
    }
}


pub fn notification_method(message: &Value) -> Option<&str> {
    message.get("method").and_then(Value::as_str)
}

pub fn notification_params(message: &Value) -> &Value {
    message.get("params").unwrap_or(&Value::Null)
}


pub async fn interrupt_existing_turn(thread_id:&str, turn_id:&str)->anyhow::Result<()> {
    let client = CLIENT.lock().await.clone();
    let Some(client) = client else { return Ok(()); };
    if client.closed.load(Ordering::Acquire) {
        client.shutdown().await;
        return Ok(());
    }
    client.interrupt_turn(thread_id, turn_id, INTERRUPT_TIMEOUT).await
}

#[cfg(test)]
mod tests {

    fn mock_command(script: &str) -> tokio::process::Command {
        let mut command = tokio::process::Command::new("python3");
        command.args(["-u", "-c", script]).stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped());
        command
    }

    async fn mock_server() -> std::sync::Arc<super::CodexAppServer> {
        let command = mock_command("import json,sys,time\nr=json.loads(sys.stdin.readline())\nprint(json.dumps({'id':r['id'],'result':{}}),flush=True)\ntime.sleep(60)");
        super::CodexAppServer::spawn_command(command, std::time::Duration::from_secs(2)).await.unwrap()
    }

    #[tokio::test]
    async fn resume_legacy_archived_thread_restores_before_retry() {
        let script = r#"import json,sys
steps = ['thread/resume', 'thread/unarchive', 'thread/resume']
index = 0
saved = None
for line in sys.stdin:
 r = json.loads(line)
 if 'id' not in r: continue
 response = {'result': {}}
 if r['method'] != 'initialize':
  assert r['method'] == steps[index]
  assert r['params']['threadId'] == 'thread'
  if index == 0:
   saved = r['params']
   response = {'error': {'message': 'session thread is archived; unarchive it first'}}
  if index == 2: assert r['params'] == saved
  index += 1
 print(json.dumps(dict(id=r['id'], **response)), flush=True)
"#;
        let server = super::CodexAppServer::spawn_command(mock_command(script), std::time::Duration::from_secs(2)).await.unwrap();
        server.resume_stored_thread("thread", serde_json::json!({
            "threadId": "thread", "cwd": "/workspace", "modelProvider": "relay", "model": "test-model"
        })).await.unwrap();
        assert!(!server.closed.load(std::sync::atomic::Ordering::Acquire));
        server.shutdown().await;
    }

    #[tokio::test]
    async fn changed_settings_reload_only_the_target_thread_and_unchanged_settings_reuse_it() {
        let script = r#"import json,sys
settings = {'modelProvider':'first', 'model':'first-model'}
loaded = True
unloads = 0
for line in sys.stdin:
 r = json.loads(line)
 if 'id' not in r: continue
 result = {}
 if r['method'] == 'thread/resume':
  assert r['params']['threadId'] == 'thread'
  if not loaded:
   settings = {key:r['params'].get(key) for key in ['modelProvider','model']}
   loaded = True
  result = settings
 elif r['method'] == 'thread/unsubscribe':
  assert r['params']['threadId'] == 'thread'
  unloads += 1
  loaded = False
  print(json.dumps({'method':'thread/closed','params':{'threadId':'unrelated'}}),flush=True)
  print(json.dumps({'method':'thread/closed','params':{'threadId':'thread'}}),flush=True)
  result = {'status':'unsubscribed'}
 elif r['method'] == 'test/unloads': result = {'count':unloads}
 print(json.dumps({'id':r['id'],'result':result}),flush=True)
"#;
        let server = super::CodexAppServer::spawn_command(mock_command(script), std::time::Duration::from_secs(2)).await.unwrap();
        let params = |provider, model| serde_json::json!({"threadId":"thread", "modelProvider":provider, "model":model});
        let first = params("first", "first-model");
        server.remember_thread_configuration("thread", first.clone());
        server.resume_configured_thread("thread", first.clone(), first.clone()).await.unwrap();
        assert_eq!(server.request("test/unloads", serde_json::json!({})).await.unwrap()["count"], 0);
        let next = params("second", "second-model");
        server.resume_configured_thread("thread", next.clone(), next.clone()).await.unwrap();
        server.resume_configured_thread("thread", next.clone(), next.clone()).await.unwrap();
        assert_eq!(server.request("test/unloads", serde_json::json!({})).await.unwrap()["count"], 1);
        // Returning to an inherited model/effort also requires a fresh resume.
        let defaults = serde_json::json!({"threadId":"thread"});
        server.resume_configured_thread("thread", defaults.clone(), defaults).await.unwrap();
        assert_eq!(server.request("test/unloads", serde_json::json!({})).await.unwrap()["count"], 2);
        server.shutdown().await;
    }

    #[tokio::test]
    async fn resume_detects_ignored_overrides_without_tracking_and_rejects_an_unapplied_selection() {
        let script = r#"import json,sys
for line in sys.stdin:
 r=json.loads(line)
 if 'id' not in r: continue
 result = {'modelProvider':'wrong', 'model':'wrong-model'}
 if r['method']=='thread/unsubscribe':
  print(json.dumps({'method':'thread/closed','params':{'threadId':'thread'}}),flush=True)
  result={'status':'unsubscribed'}
 print(json.dumps({'id':r['id'],'result':result}),flush=True)
"#;
        let server = super::CodexAppServer::spawn_command(mock_command(script), std::time::Duration::from_secs(2)).await.unwrap();
        let params = serde_json::json!({"threadId":"thread", "modelProvider":"desired", "model":"desired-model"});
        let error = server.resume_configured_thread("thread", params.clone(), params).await.unwrap_err();
        assert!(error.to_string().contains("未应用所选平台或模型"));
        assert!(!server.closed.load(std::sync::atomic::Ordering::Acquire));
        server.shutdown().await;
    }

    #[tokio::test]
    async fn resume_other_rpc_errors_do_not_restore_or_retry() {
        let script = r#"import json,sys
for line in sys.stdin:
 r = json.loads(line)
 if 'id' not in r: continue
 if r['method'] == 'initialize': response = {'result': {}}
 else:
  print(json.dumps({'method': 'test/request', 'params': {'method': r['method']}}), flush=True)
  response = {'error': {'message': 'at capacity'}}
 print(json.dumps(dict(id=r['id'], **response)), flush=True)
"#;
        let server = super::CodexAppServer::spawn_command(mock_command(script), std::time::Duration::from_secs(2)).await.unwrap();
        let mut events = server.subscribe();
        let error = server.resume_stored_thread("thread", serde_json::json!({"threadId": "thread"})).await.unwrap_err();
        assert!(error.downcast_ref::<super::RpcError>().is_some());
        assert_eq!(error.to_string(), "at capacity");
        assert_eq!(events.try_recv().unwrap()["params"]["method"], "thread/resume");
        assert!(events.try_recv().is_err());
        server.shutdown().await;
    }

    #[tokio::test]
    async fn rpc_deadline_includes_waiting_for_writer_and_cleans_pending() {
        let server = mock_server().await;
        let writer = server.stdin.lock().await;
        let error = server.request_with_timeout("test", serde_json::json!({}), std::time::Duration::from_millis(20)).await.unwrap_err();
        assert!(error.to_string().contains("超时"));
        assert!(server.pending.lock().unwrap().is_empty());
        drop(writer);
        server.shutdown().await;
    }

    #[tokio::test]
    async fn rpc_cancel_removes_pending_without_closing_an_intact_connection() {
        let server = mock_server().await;
        let requested = server.clone();
        let task = tokio::spawn(async move { requested.request("test", serde_json::json!({})).await });
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while server.pending.lock().unwrap().is_empty() { tokio::task::yield_now().await; }
        }).await.unwrap();
        // Let the tiny complete frame reach the pipe before cancelling its response.
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        task.abort();
        let _ = task.await;
        assert!(server.pending.lock().unwrap().is_empty());
        assert!(!server.closed.load(std::sync::atomic::Ordering::Acquire));
        server.shutdown().await;
    }

    #[tokio::test]
    async fn rpc_blocked_partial_write_retires_connection() {
        let server = mock_server().await;
        let result = server.request_with_timeout("test", serde_json::json!({"data":"x".repeat(2 * 1024 * 1024)}), std::time::Duration::from_millis(50)).await;
        assert!(result.is_err());
        assert!(server.pending.lock().unwrap().is_empty());
        assert!(server.closed.load(std::sync::atomic::Ordering::Acquire));
        server.shutdown().await;
    }

    #[tokio::test]
    async fn lost_start_response_stops_unknown_turn() {
        let server = mock_server().await;
        let result = server.start_turn(serde_json::json!({}), std::time::Duration::from_millis(30)).await;
        assert!(result.is_err());
        assert!(server.closed.load(std::sync::atomic::Ordering::Acquire));
        assert!(server.pending.lock().unwrap().is_empty());
        server.shutdown().await;
    }

    #[tokio::test]
    async fn rejected_turn_start_keeps_shared_server_available() {
        let command = mock_command("import json,sys\nfor line in sys.stdin:\n r=json.loads(line)\n if 'id' not in r: continue\n response={'error':{'message':'at capacity'}} if r['method']=='turn/start' else {'result':{}}\n print(json.dumps(dict(id=r['id'],**response)),flush=True)");
        let server = super::CodexAppServer::spawn_command(command, std::time::Duration::from_secs(2)).await.unwrap();
        assert!(server.start_turn(serde_json::json!({}), std::time::Duration::from_secs(2)).await.is_err());
        assert!(!server.closed.load(std::sync::atomic::Ordering::Acquire));
        server.shutdown().await;
    }

    #[tokio::test]
    async fn cancelled_turn_start_stops_unknown_turn() {
        let server = mock_server().await;
        let requested = server.clone();
        let task = tokio::spawn(async move { requested.start_turn(serde_json::json!({}), std::time::Duration::from_secs(2)).await });
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while server.pending.lock().unwrap().is_empty() { tokio::task::yield_now().await; }
        }).await.unwrap();
        task.abort();
        let _ = task.await;
        assert!(server.closed.load(std::sync::atomic::Ordering::Acquire));
        assert!(server.pending.lock().unwrap().is_empty());
        server.shutdown().await;
    }

    async fn interrupt_server(complete: bool) -> std::sync::Arc<super::CodexAppServer> {
        let script = format!("import json,sys,time\nfor line in sys.stdin:\n r=json.loads(line)\n if 'id' not in r: continue\n data={{'data':[{{'id':'turn','status':'inProgress'}}]}} if r['method']=='thread/turns/list' else {{}}\n print(json.dumps({{'id':r['id'],'result':data}}),flush=True)\n if r['method']=='turn/interrupt' and {complete}:\n  time.sleep(.05)\n  print(json.dumps({{'method':'turn/completed','params':{{'threadId':'thread','turn':{{'id':'turn','status':'interrupted'}}}}}}),flush=True)", complete = if complete { "True" } else { "False" });
        super::CodexAppServer::spawn_command(mock_command(&script), std::time::Duration::from_secs(2)).await.unwrap()
    }

    #[tokio::test]
    async fn interrupt_waits_for_terminal_event_after_acknowledgment() {
        let server = interrupt_server(true).await;
        let started = std::time::Instant::now();
        server.interrupt_turn("thread", "turn", std::time::Duration::from_secs(2)).await.unwrap();
        assert!(started.elapsed() >= std::time::Duration::from_millis(40));
        assert!(!server.closed.load(std::sync::atomic::Ordering::Acquire));
        server.shutdown().await;
    }

    #[tokio::test]
    async fn unconfirmed_interrupt_retires_server_before_returning() {
        let server = interrupt_server(false).await;
        server.interrupt_turn("thread", "turn", std::time::Duration::from_millis(40)).await.unwrap();
        assert!(server.closed.load(std::sync::atomic::Ordering::Acquire));
        assert!(server._child.lock().await.try_wait().unwrap().is_some());
    }

    #[tokio::test]
    async fn shared_client_reuses_live_server_and_replaces_closed_server() {
        let first = mock_server().await;
        let slot = tokio::sync::Mutex::new(Some(first.clone()));
        let reused = super::reuse_or_spawn_client(&slot, || async { panic!("live client must be reused") }).await.unwrap();
        assert!(std::sync::Arc::ptr_eq(&first, &reused));
        first.shutdown().await;
        let replacement = super::reuse_or_spawn_client(&slot, || async { Ok(mock_server().await) }).await.unwrap();
        assert!(!std::sync::Arc::ptr_eq(&first, &replacement));
        assert!(!replacement.closed.load(std::sync::atomic::Ordering::Acquire));
        replacement.shutdown().await;
    }

    #[tokio::test]
    async fn failed_handshake_terminates_and_reaps_child() {
        let directory = tempfile::tempdir().unwrap();
        let pidfile = directory.path().join("pid");
        // Reject initialize only after recording the PID. A short wall-clock
        // timeout can fire before Python starts under parallel test load.
        let mut command = mock_command("import json,os,sys,time\nopen(sys.argv[1],'w').write(str(os.getpid()))\nr=json.loads(sys.stdin.readline())\nprint(json.dumps({'id':r['id'],'error':{'message':'initialize rejected'}}),flush=True)\ntime.sleep(60)");
        command.arg(&pidfile);
        let result = super::CodexAppServer::spawn_command(command, std::time::Duration::from_secs(10)).await;
        assert!(result.is_err());
        let pid: i32 = std::fs::read_to_string(pidfile).unwrap().parse().unwrap();
        assert_eq!(unsafe { libc::kill(pid, 0) }, -1);
        assert_eq!(std::io::Error::last_os_error().raw_os_error(), Some(libc::ESRCH));
    }

    #[test]
    fn app_server_logs_preserve_levels_and_strip_terminal_formatting() {
        for level in [tracing::Level::TRACE, tracing::Level::DEBUG, tracing::Level::INFO, tracing::Level::WARN, tracing::Level::ERROR] {
            let line = format!("\x1b[2m2026-10-01T14:30:34.804373Z\x1b[0m \x1b[32m{level}\x1b[0m \x1b[1msession_loop\x1b[0m: codex_core::tasks: enter");
            let (parsed, message) = super::app_server_log_record(&line).unwrap();
            assert_eq!(parsed, level);
            assert_eq!(message, format!("2026-10-01T14:30:34.804373Z {level} session_loop: codex_core::tasks: enter"));
        }
        let (level, message) = super::app_server_log_record("\x1b]8;;https://example.com\x07ERROR: failed\x1b]8;;\x07").unwrap();
        assert_eq!(level, tracing::Level::ERROR);
        assert_eq!(message, "ERROR: failed");
    }

    #[test]
    fn app_server_logs_keep_unstructured_diagnostics_and_ignore_empty_lines() {
        for line in ["codex: failed to start", "startup failed: expected INFO", "2026-10-01T14:30:34Z missing level"] {
            assert_eq!(super::app_server_log_record(line), Some((tracing::Level::WARN, line.to_owned())));
        }
        for line in ["", "  \t", "\x1b[0m \x1b[2m"] {
            assert!(super::app_server_log_record(line).is_none());
        }
    }

    #[test]
    fn app_server_logs_suppress_transport_retries_and_keep_terminal_failures() {
        for message in [
            "codex_core::responses_retry: stream disconnected - retrying sampling request (1/5 in 180ms)... sampling_error=request timed out",
            "codex_core::responses_retry: stream disconnected - retrying sampling request (5/5 in 3.4s)... sampling_error=websocket closed",
            "codex_core::client: falling back to HTTP",
        ] {
            let line = format!("\x1b[2m2026-10-12T03:02:35Z\x1b[0m \x1b[33mWARN\x1b[0m {message}");
            assert!(super::app_server_log_record(&line).is_none());
            let error = format!("ERROR {message}");
            assert_eq!(super::app_server_log_record(&error), Some((tracing::Level::ERROR, error)));
        }
        for line in ["WARN codex_core::responses_retry: retry budget exhausted", "ERROR codex_core::client: stream failed", "WARN codex_core::shell_snapshot: snapshot failed"] {
            assert!(super::app_server_log_record(line).is_some());
        }
    }

    fn history_warning(turn: &str, item: usize) -> String {
        format!("2026-10-01T14:59:18Z WARN codex_app_server_protocol::protocol::thread_history: dropping turn-scoped item for unknown turn id `{turn}` item_id=\"exec-{item}\"")
    }

    #[test]
    fn history_warning_bursts_keep_first_detail_and_count_remaining_items() {
        let now = std::time::Instant::now();
        let mut batch = super::AppServerLogBatch::default();
        let first = history_warning("turn-a", 0);
        assert_eq!(batch.push(&first, now), vec![(tracing::Level::WARN, first)]);
        for item in 1..46 {
            assert!(batch.push(&history_warning("turn-a", item), now).is_empty());
        }
        let summary = batch.flush();
        assert_eq!(summary.len(), 1);
        assert_eq!(summary[0].0, tracing::Level::WARN);
        assert!(summary[0].1.contains("merged 45 additional warnings"));
        assert!(summary[0].1.contains("`turn-a`"));
        assert!(batch.flush().is_empty());
        batch.push(&history_warning("turn-a", 46), now);
        assert!(batch.flush().is_empty());
    }

    #[test]
    fn history_warning_batches_flush_on_other_turns_and_preserve_other_diagnostics() {
        let now = std::time::Instant::now();
        let mut batch = super::AppServerLogBatch::default();
        batch.push(&history_warning("turn-a", 0), now);
        batch.push(&history_warning("turn-a", 1), now);
        let next = history_warning("turn-b", 2);
        let records = batch.push(&next, now);
        assert_eq!(records.len(), 2);
        assert!(records[0].1.contains("`turn-a`"));
        assert_eq!(records[1], (tracing::Level::WARN, next));
        batch.push(&history_warning("turn-b", 3), now);
        let error = history_warning("turn-b", 4).replace("WARN", "ERROR");
        let records = batch.push(&error, now);
        assert_eq!(records.len(), 2);
        assert!(records[0].1.contains("`turn-b`"));
        assert_eq!(records[1], (tracing::Level::ERROR, error));
        for line in ["WARN codex_core::responses_retry: stream disconnected", "WARN codex_app_server_protocol::protocol::thread_history: another warning", "unstructured startup failure"] {
            for _ in 0..2 {
                assert_eq!(batch.push(line, now), vec![(tracing::Level::WARN, line.to_owned())]);
            }
        }
    }

    #[test]
    fn history_warning_batches_flush_when_idle_and_do_not_extend_under_load() {
        let now = std::time::Instant::now();
        let mut batch = super::AppServerLogBatch::default();
        batch.push(&history_warning("turn-a", 0), now);
        batch.push(&history_warning("turn-a", 1), now + std::time::Duration::from_secs(4));
        assert!(batch.flush_expired(now + std::time::Duration::from_secs(4)).is_empty());
        let records = batch.flush_expired(now + super::HISTORY_WARNING_WINDOW);
        assert_eq!(records.len(), 1);
        assert!(records[0].1.contains("merged 1 additional warnings"));
        assert_eq!(batch.push(&history_warning("turn-a", 2), now + super::HISTORY_WARNING_WINDOW).len(), 1);
        batch.push(&history_warning("turn-a", 3), now + super::HISTORY_WARNING_WINDOW);
        let records = batch.push(&history_warning("turn-a", 4), now + super::HISTORY_WARNING_WINDOW * 2);
        assert_eq!(records.len(), 2);
        assert!(records[0].1.contains("merged 1 additional warnings"));
        assert!(records[1].1.contains("exec-4"));
    }


}
