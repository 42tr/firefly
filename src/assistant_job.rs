use std::{
    collections::BTreeMap,
    sync::{
        atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering},
        Arc, Mutex, MutexGuard,
    },
};

use axum::response::sse::Event;
use serde_json::{json, Value};
use tokio::sync::{broadcast, mpsc};
use tokio_util::sync::CancellationToken;
use tracing::error;
use uuid::Uuid;

use crate::assistant_store;

const JOB_BROADCAST_CAPACITY: usize = 1024;
const JOB_EVENT_LOG_CAPACITY: usize = 10000;
const JOB_EVENT_LOG_MAX_BYTES: u64 = 4 * 1024 * 1024;

pub const STATUS_RUNNING: u8 = 0;
pub const STATUS_COMPLETED: u8 = 1;
pub const STATUS_FAILED: u8 = 2;

/// 获取互斥锁；即使锁因持有者 panic 而“中毒”，也恢复内部数据继续使用，
/// 避免一次 panic 导致所有后续 `unwrap()` 级联崩溃。
/// 这些锁的临界区都很短（纯内存操作，从不跨 await 持有），因此使用 std 锁即可。
fn lock_unpoisoned<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|err| err.into_inner())
}

#[derive(Debug, Clone)]
pub struct EventEntry {
    pub event: String,
    pub data: Value,
    bytes: u64,
}

/// 代表一次 assistant 回复的生成任务。
///
/// 任务在内存中维护事件日志与广播通道，允许浏览器断线后重新连接并回放事件。
/// 任务状态会同步写入数据库，保证服务端重启后也能知道某对话是否“应该”还有进行中的生成
///（虽然事件日志会丢失，但最终消息已在 DB 中持久化）。
pub struct GenerationJob {
    pub id: Uuid,
    pub started_at: chrono::DateTime<chrono::Utc>,
    /// Distinguishes re-executions of the same durable job after a restart.
    pub execution_id: Uuid,
    pub snapshot_required: AtomicBool,
    pub conversation_id: i64,
    pub user_id: String,
    /// 原子状态：STATUS_RUNNING / STATUS_COMPLETED / STATUS_FAILED
    pub status: AtomicU8,
    /// 顺序递增的事件序列号。每 emit 一条事件 +1。
    pub seq_counter: AtomicU64,
    /// 广播通知唤醒订阅者；实际事件按序从 event_log 读取。
    pub broadcast_tx: broadcast::Sender<()>,
    /// 环形事件日志，用于断线后回放。seq 单调递增，容量有上限。
    pub event_log: Mutex<BTreeMap<u64, EventEntry>>,
    event_log_bytes: AtomicU64,
    /// 失败原因（仅在 status == STATUS_FAILED 时有效）
    pub error_message: Mutex<Option<String>>,
    cancellation: CancellationToken,
    terminal_gate: tokio::sync::Mutex<()>,
}

impl GenerationJob {
    pub fn new(id: Uuid, conversation_id: i64, user_id: String) -> Self {
        let (broadcast_tx, _rx) = broadcast::channel(JOB_BROADCAST_CAPACITY);
        Self {
            id,
            started_at: chrono::Utc::now(),
            execution_id: Uuid::new_v4(),
            snapshot_required: AtomicBool::new(false),
            conversation_id,
            user_id,
            status: AtomicU8::new(STATUS_RUNNING),
            seq_counter: AtomicU64::new(0),
            broadcast_tx,
            event_log: Mutex::new(BTreeMap::new()),
            event_log_bytes: AtomicU64::new(0),
            error_message: Mutex::new(None),
            cancellation: CancellationToken::new(),
            terminal_gate: tokio::sync::Mutex::new(()),
        }
    }

    pub fn is_done(&self) -> bool {
        let status = self.status.load(Ordering::SeqCst);
        status == STATUS_COMPLETED || status == STATUS_FAILED
    }

    pub fn status_name(&self) -> &'static str {
        match self.status.load(Ordering::SeqCst) {
            STATUS_RUNNING if self.cancellation.is_cancelled() => "cancelling",
            STATUS_RUNNING => "running",
            STATUS_COMPLETED => "completed",
            STATUS_FAILED => "failed",
            _ => "unknown",
        }
    }

    /// 发送一条事件到所有订阅者，并记录到事件日志。
    /// 返回分配的序列号。没有实时订阅者时只保留事件日志，供后续连接回放。
    ///
    /// 事件的 `data` JSON 会自动注入 `seq` 字段，便于前端去重/断点续传。
    pub fn emit(&self, event: &str, data: Value) -> u64 {
        let data = if matches!(event, "tool_call" | "tool_result") { tool_event_summary(data) } else { data };
        let mut log = lock_unpoisoned(&self.event_log);
        let seq = self.seq_counter.fetch_add(1, Ordering::SeqCst) + 1;
        let occurred_at = chrono::Utc::now();
        let data_with_seq = match data {
            Value::Object(mut map) => {
                map.insert("seq".to_string(), Value::Number(serde_json::Number::from(seq)));
                map.insert("execution_id".to_string(), json!(self.execution_id));
                map.insert("occurred_at".to_string(), json!(occurred_at));
                Value::Object(map)
            }
            other => {
                json!({ "seq": seq, "execution_id": self.execution_id, "occurred_at": occurred_at, "payload": other })
            }
        };

        let bytes = (event.len() + data_with_seq.to_string().len()) as u64;
        let entry = EventEntry { event: event.to_string(), data: data_with_seq, bytes };
        let mut retained_bytes = self.event_log_bytes.load(Ordering::Relaxed) + bytes;
        log.insert(seq, entry);
        while log.len() > JOB_EVENT_LOG_CAPACITY || retained_bytes > JOB_EVENT_LOG_MAX_BYTES {
            if let Some((_, removed)) = log.pop_first() {
                retained_bytes -= removed.bytes;
            } else {
                break;
            }
        }
        self.event_log_bytes.store(retained_bytes, Ordering::Relaxed);
        // Notifications are only wake-ups; the ordered log is the source of truth.
        let _ = self.broadcast_tx.send(());

        seq
    }

    /// 标记任务成功完成。
    pub async fn complete(&self) {
        self.finish("completed", None, false).await;
    }

    /// 标记任务失败。
    pub async fn fail(&self, message: String) {
        self.finish("failed", Some(message), false).await;
    }

    pub fn cancellation_token(&self) -> CancellationToken {
        self.cancellation.clone()
    }

    /// The worker retains its database claim until execution and cleanup stop.
    pub async fn request_cancellation(&self) -> anyhow::Result<()> {
        let _guard = self.terminal_gate.lock().await;
        if self.is_done() {
            return Ok(());
        }
        // Persist intent before acknowledging the request. A restart between
        // stopping execution and the terminal commit must not resume this turn.
        assistant_store::request_generation_cancellation(self.id, &self.user_id).await?;
        self.cancellation.cancel();
        Ok(())
    }

    pub async fn cancel(&self) {
        self.cancellation.cancel();
        self.finish("failed", Some("用户已停止生成".into()), true).await;
    }

    /// Only the worker finalizes jobs. A failed commit must not release the
    /// conversation claim or advertise completion to subscribers.
    async fn finish(&self, status: &str, message: Option<String>, cancelled: bool) {
        let _guard = self.terminal_gate.lock().await;
        if self.is_done() {
            return;
        }
        // Cancellation may have arrived after the worker checked its token but
        // before acquiring this gate. Never overwrite an accepted stop request.
        let cancelled = cancelled || self.cancellation.is_cancelled();
        let (status, message) =
            if cancelled { ("failed", Some("用户已停止生成".to_string())) } else { (status, message) };
        let mut retry_seconds = 1;
        loop {
            match assistant_store::update_generation_status(self.id, status, message.as_deref()).await {
                Ok(()) => break,
                Err(err) => {
                    error!(job_id = %self.id, status, "failed to persist terminal status; retaining claim: {err:#}");
                    let delay = if cfg!(test) {
                        std::time::Duration::from_millis(10)
                    } else {
                        std::time::Duration::from_secs(retry_seconds)
                    };
                    tokio::time::sleep(delay).await;
                    retry_seconds = (retry_seconds * 2).min(30);
                }
            }
        }
        // The terminal event identifies the persisted output actually replayed
        // to this device, even if a newer turn has already entered the queue.
        let last_message_id: Option<i64> = sqlx::query_scalar(
            "SELECT MAX(id) FROM assistant_messages WHERE generation_job_id = ?1 AND conversation_id = ?2",
        )
        .bind(self.id)
        .bind(self.conversation_id)
        .fetch_one(crate::db::pool())
        .await
        .unwrap_or_else(|err| {
            error!(job_id = %self.id, "failed to load completed read position: {err:#}");
            None
        });
        *lock_unpoisoned(&self.error_message) = message.clone();
        self.status.store(if status == "completed" { STATUS_COMPLETED } else { STATUS_FAILED }, Ordering::SeqCst);
        if !cancelled {
            if let Some(message) = message {
                self.emit("error", json!({ "message": message }));
            }
        }
        tracing::info!(job_id = %self.id, elapsed_ms = (chrono::Utc::now() - self.started_at).num_milliseconds(), status, "assistant foreground finished");
        self.emit("assistant_done", json!({ "done": true, "cancelled": cancelled, "last_message_id": last_message_id }));
    }
}

/// Tool I/O belongs to the authenticated detail endpoint, never SSE replay.
fn tool_event_summary(data: Value) -> Value {
    let mut summary = serde_json::Map::new();
    for key in ["id", "name", "agent", "parent_id", "status", "is_error", "message_id", "attachments"] {
        if let Some(value) = data.get(key) { summary.insert(key.into(), value.clone()); }
    }
    // Reply artifacts have a separate display contract, excluding tool diagnostics.
    if data["is_error"] != true && (data["name"] == "image_generation" || data["result"]["type"] != "image") {
        if let Some(presentation) = tool_presentation(&data["result"]) {
            summary.insert("presentation".into(), presentation);
        }
    }
    summary.insert("detail_omitted".into(), json!(true));
    Value::Object(summary)
}

pub(crate) fn tool_presentation(output: &Value) -> Option<Value> {
    let output = output.get("result").unwrap_or(output);
    if output["type"] == "image" && output["error"].is_null() && output["failure"].is_null()
        && output["image_url"].as_str().is_some_and(|url| url.starts_with("/api/assistant/attachments/by-file/")) {
        return Some(json!({"type":"image","image_url":output["image_url"]}));
    }
    let option = if output["type"] == "echarts" { output.get("option") }
        else if let Some(option) = output.get("echarts_option") { Some(option) }
        else if output.get("option").is_some() && output.get("series").is_some() { Some(output) }
        else { None }?;
    Some(json!({"type":"echarts","option":option,"title":output["title"]}))
}

/// 管理内存中的生成任务。
#[derive(Default)]
pub struct JobRegistry {
    jobs: Mutex<BTreeMap<Uuid, Arc<GenerationJob>>>,
}

impl JobRegistry {
    pub fn new() -> Self {
        Self { jobs: Mutex::new(BTreeMap::new()) }
    }

    #[cfg(test)]
    pub fn insert(&self, job: Arc<GenerationJob>) {
        let mut jobs = lock_unpoisoned(&self.jobs);
        jobs.insert(job.id, job);
    }

    pub fn get(&self, job_id: Uuid) -> Option<Arc<GenerationJob>> {
        let jobs = lock_unpoisoned(&self.jobs);
        jobs.get(&job_id).cloned()
    }

    pub fn find_active_by_conversation(&self, conversation_id: i64) -> Option<Arc<GenerationJob>> {
        let jobs = lock_unpoisoned(&self.jobs);
        jobs.values().find(|j| j.conversation_id == conversation_id && !j.is_done()).cloned()
    }

    pub fn remove_execution(&self, job_id: Uuid, execution_id: Uuid) {
        let mut jobs = lock_unpoisoned(&self.jobs);
        if jobs.get(&job_id).is_some_and(|job| job.execution_id == execution_id) {
            jobs.remove(&job_id);
        }
    }

    #[cfg(test)]
    pub fn remove(&self, job_id: Uuid) {
        lock_unpoisoned(&self.jobs).remove(&job_id);
    }
}

/// 全局任务注册表。
pub fn global_registry() -> &'static JobRegistry {
    use std::sync::OnceLock;
    static REGISTRY: OnceLock<JobRegistry> = OnceLock::new();
    REGISTRY.get_or_init(JobRegistry::new)
}

/// 创建一个 SSE 流，先回放事件日志，再转发实时广播事件。
pub fn stream_for_job(
    job: Arc<GenerationJob>, start_seq: u64,
) -> mpsc::Receiver<Result<Event, std::convert::Infallible>> {
    let execution_id = job.execution_id;
    stream_for_job_from(job, start_seq, Some(execution_id))
}

pub fn stream_reset_event(job_id: Uuid, conversation_id: i64, execution_id: Option<Uuid>, reason: &str) -> Event {
    Event::default().event("stream_reset").data(
        json!({
            "job_id": job_id, "conversation_id": conversation_id,
            "execution_id": execution_id, "seq": 0, "reason": reason,
            "snapshot_required": true,
        })
        .to_string(),
    )
}

pub fn stream_for_job_from(
    job: Arc<GenerationJob>, start_seq: u64, execution_id: Option<Uuid>,
) -> mpsc::Receiver<Result<Event, std::convert::Infallible>> {
    let (tx, rx) = mpsc::channel(64);

    tokio::spawn(async move {
        if job.snapshot_required.load(Ordering::SeqCst) {
            let _ = tx
                .send(Ok(stream_reset_event(
                    job.id,
                    job.conversation_id,
                    Some(job.execution_id),
                    "execution_recovered",
                )))
                .await;
            return;
        }
        if execution_id.is_some_and(|id| id != job.execution_id) || (start_seq > 0 && execution_id.is_none()) {
            let _ = tx
                .send(Ok(stream_reset_event(job.id, job.conversation_id, Some(job.execution_id), "execution_changed")))
                .await;
            return;
        }
        // Subscribe before reading the log so an event cannot fall between the two.
        let mut notifications = job.broadcast_tx.subscribe();
        if tx
            .send(Ok(Event::default().event("stream_started").data(
                json!({
                    "job_id": job.id, "conversation_id": job.conversation_id,
                    "execution_id": job.execution_id, "seq": 0,
                    "started_at": job.started_at,
                })
                .to_string(),
            )))
            .await
            .is_err()
        {
            return;
        }
        let mut cursor = start_seq;
        loop {
            let (events, gap, terminal_seen) = {
                let log = lock_unpoisoned(&job.event_log);
                let latest = job.seq_counter.load(Ordering::SeqCst);
                let gap = cursor > latest
                    || match log.first_key_value() {
                        Some((&first, _)) => cursor < first.saturating_sub(1),
                        None => cursor < latest,
                    };
                let terminal_seen =
                    job.is_done() && log.iter().any(|(&seq, entry)| seq <= cursor && entry.event == "assistant_done");
                let events: Vec<_> = log
                    .range((std::ops::Bound::Excluded(cursor), std::ops::Bound::Unbounded))
                    .take(64)
                    .map(|(&seq, entry)| (seq, entry.clone()))
                    .collect();
                (events, gap, terminal_seen)
            };
            if gap {
                // The caller switches to authoritative persisted snapshots until
                // this turn ends; a non-atomic snapshot/cursor cannot replay safely.
                let _ = tx
                    .send(Ok(stream_reset_event(job.id, job.conversation_id, Some(job.execution_id), "log_expired")))
                    .await;
                return;
            }
            if terminal_seen {
                return;
            }
            if !events.is_empty() {
                for (seq, entry) in events {
                    let terminal = entry.event == "assistant_done";
                    let event = Event::default()
                        .id(format!("{}:{seq}", job.execution_id))
                        .event(entry.event)
                        .data(entry.data.to_string());
                    if tx.send(Ok(event)).await.is_err() {
                        return;
                    }
                    cursor = seq;
                    if terminal {
                        return;
                    }
                }
                continue;
            }
            tokio::select! {
                _ = tx.closed() => return,
                received = notifications.recv() => {
                    if matches!(received, Err(broadcast::error::RecvError::Closed)) {
                        return;
                    }
                    // Lagged notifications are harmless: replay starts at cursor.
                }
            }
        }
    });

    rx
}

/// Registers a job that has already been atomically claimed in the database.
pub fn register_claimed_job(record: &assistant_store::GenerationJobRecord) -> Arc<GenerationJob> {
    let mut jobs = lock_unpoisoned(&global_registry().jobs);
    if let Some(job) = jobs.get(&record.id).filter(|job| !job.is_done()) {
        return job.clone();
    }
    let mut job = GenerationJob::new(record.id, record.conversation_id, record.user_id.clone());
    job.started_at = record.started_at.unwrap_or(job.started_at);
    let job = Arc::new(job);
    // Conservatively require a snapshot until the worker has checked recovery
    // metadata; a subscriber may arrive while that database read is pending.
    job.snapshot_required.store(true, Ordering::SeqCst);
    jobs.insert(job.id, job.clone());
    job
}

/// 延迟清理已结束任务，允许短暂断线后回放终止事件。
pub fn cleanup_job(job_id: Uuid, execution_id: Uuid) {
    tokio::spawn(async move {
        // Keep terminal events briefly so a disconnect at completion can resume.
        tokio::time::sleep(std::time::Duration::from_secs(60)).await;
        // A workspace retry can reuse the durable job ID during this delay.
        global_registry().remove_execution(job_id, execution_id);
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::{to_bytes, Body},
        response::{IntoResponse, Sse},
    };
    use tokio_stream::wrappers::ReceiverStream;

    async fn collect(rx: mpsc::Receiver<Result<Event, std::convert::Infallible>>) -> String {
        let body: Body = Sse::new(ReceiverStream::new(rx)).into_response().into_body();
        let bytes =
            tokio::time::timeout(std::time::Duration::from_secs(5), to_bytes(body, usize::MAX)).await.unwrap().unwrap();
        String::from_utf8(bytes.to_vec()).unwrap()
    }

    #[tokio::test]
    async fn slow_subscriber_receives_all_events_and_terminal() {
        let job = Arc::new(GenerationJob::new(Uuid::new_v4(), 1, "1".into()));
        let rx = stream_for_job(job.clone(), 0);
        while job.broadcast_tx.receiver_count() == 0 {
            tokio::task::yield_now().await;
        }
        for i in 0..1500 {
            job.emit("delta", json!({"text": i}));
        }
        job.status.store(STATUS_COMPLETED, Ordering::SeqCst);
        job.emit("assistant_done", json!({"done": true}));
        let output = collect(rx).await;
        assert_eq!(output.matches("event: delta").count(), 1500);
        assert_eq!(output.matches("event: assistant_done").count(), 1);
        assert!(output.contains("\"seq\":1501"));
    }

    #[tokio::test]
    async fn replay_respects_cursor_and_handles_max_cursor() {
        let job = Arc::new(GenerationJob::new(Uuid::new_v4(), 1, "1".into()));
        job.emit("delta", json!({"text": "old"}));
        job.emit("delta", json!({"text": "new"}));
        job.status.store(STATUS_COMPLETED, Ordering::SeqCst);
        job.emit("assistant_done", json!({"done": true}));
        let output = collect(stream_for_job(job.clone(), 1)).await;
        assert!(!output.contains("old"));
        assert!(output.contains("new"));
        assert!(collect(stream_for_job(job, u64::MAX)).await.contains("event: stream_reset"));
    }

    #[tokio::test]
    async fn expired_replay_reports_gap() {
        let job = Arc::new(GenerationJob::new(Uuid::new_v4(), 1, "1".into()));
        for _ in 0..=JOB_EVENT_LOG_CAPACITY {
            job.emit("delta", json!({}));
        }
        let output = collect(stream_for_job(job, 0)).await;
        assert!(output.contains("event: stream_reset"));
        assert!(!output.contains("event: delta"));
        assert!(!output.contains("event: assistant_done"));
    }

    #[tokio::test]
    async fn replay_rejects_execution_changes_before_delivering_events() {
        let job = Arc::new(GenerationJob::new(Uuid::new_v4(), 1, "1".into()));
        job.emit("delta", json!({"text": "new execution"}));
        let output = collect(stream_for_job_from(job.clone(), 1, Some(Uuid::new_v4()))).await;
        assert!(output.contains("execution_changed"));
        assert!(!output.contains("event: delta"));
        assert!(collect(stream_for_job_from(job, 1, None)).await.contains("execution_changed"));
    }

    #[tokio::test]
    async fn replay_memory_is_bounded_by_bytes_even_for_one_oversized_event() {
        let job = Arc::new(GenerationJob::new(Uuid::new_v4(), 1, "1".into()));
        job.emit("delta", json!({"output": "a".repeat(JOB_EVENT_LOG_MAX_BYTES as usize + 1)}));
        assert_eq!(job.event_log_bytes.load(Ordering::Relaxed), 0);
        assert!(collect(stream_for_job(job.clone(), 0)).await.contains("log_expired"));
        for _ in 0..12 {
            job.emit("delta", json!({"output": "a".repeat(512 * 1024)}));
        }
        assert!(job.event_log_bytes.load(Ordering::Relaxed) <= JOB_EVENT_LOG_MAX_BYTES);
        assert!(lock_unpoisoned(&job.event_log).len() < 12);
    }

    #[test]
    fn tool_io_is_absent_from_live_and_replayed_events() {
        let job = GenerationJob::new(Uuid::new_v4(), 1, "owner".into());
        for event in ["tool_call", "tool_result"] {
            job.emit(event, json!({"id":"call","name":"command","agent":"programmer","parent_id":"parent",
                "arguments":{"secret":"private input"},"input":"private input","result":"private output",
                "output":"private output","error":"private error","is_error":true,"message_id":42}));
        }
        for entry in job.event_log.lock().unwrap().values() {
            assert_eq!(entry.data["id"], "call");
            assert_eq!(entry.data["parent_id"], "parent");
            assert_eq!(entry.data["is_error"], true);
            assert!(!entry.data.to_string().contains("private"));
            for field in ["arguments", "input", "result", "output", "error"] { assert!(entry.data.get(field).is_none()); }
        }
    }

    #[test]
    fn replay_log_retains_event_occurrence_times() {
        let job = GenerationJob::new(Uuid::new_v4(), 1, "owner".into());
        let seq = job.emit("tool_call", json!({"id":"call","name":"command"}));
        let entry = job.event_log.lock().unwrap().get(&seq).unwrap().clone();
        let occurred_at = chrono::DateTime::parse_from_rfc3339(entry.data["occurred_at"].as_str().unwrap()).unwrap();
        assert!(occurred_at >= job.started_at);
        job.emit("assistant_delta", json!({"delta":"reply"}));
        assert_eq!(job.event_log.lock().unwrap().get(&seq).unwrap().data["occurred_at"], entry.data["occurred_at"]);
    }

    async fn running_job() -> (sqlx::SqlitePool, Arc<GenerationJob>) {
        let pool = crate::db::test_pool().await;
        crate::db::set_test_pool(pool.clone());
        let conversation: i64 = sqlx::query_scalar(
            "INSERT INTO assistant_conversations(user_id,system_prompt) VALUES ('owner','') RETURNING id",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        let id = Uuid::new_v4();
        assistant_store::create_queued_user_turn(id, conversation, "owner", true, "hello".into(), &[]).await.unwrap();
        let record = assistant_store::claim_next_queued_generation(conversation, "owner").await.unwrap().unwrap();
        (pool, Arc::new(GenerationJob::new(record.id, conversation, "owner".into())))
    }

    #[tokio::test]
    async fn completed_read_position_excludes_a_later_queued_turn() {
        let (_pool, job) = running_job().await;
        let reply = assistant_store::append_message(job.conversation_id, assistant_store::StoredMessage {
            role: "assistant".into(), content: "finished reply".into(), tool_call_id: None,
            tool_calls: None, metadata: None, is_error: false, generation_job_id: Some(job.id),
        }).await.unwrap();
        let (queued, _) = assistant_store::create_queued_user_turn(
            Uuid::new_v4(), job.conversation_id, "owner", true, "next".into(), &[],
        ).await.unwrap();
        assert!(queued.id > reply.id);
        job.complete().await;
        let log = lock_unpoisoned(&job.event_log);
        let terminal = log.values().find(|entry| entry.event == "assistant_done").unwrap();
        assert_eq!(terminal.data["last_message_id"], reply.id);
    }

    #[tokio::test]
    async fn cancellation_keeps_claim_until_worker_finalizes() {
        let (_pool, job) = running_job().await;
        assistant_store::create_queued_user_turn(
            Uuid::new_v4(),
            job.conversation_id,
            "owner",
            true,
            "next".into(),
            &[],
        )
        .await
        .unwrap();
        job.request_cancellation().await.unwrap();
        assert_eq!(job.status_name(), "cancelling");
        assert!(!job.is_done());
        assert!(lock_unpoisoned(&job.event_log).is_empty());
        assert!(assistant_store::claim_next_queued_generation(job.conversation_id, "owner").await.unwrap().is_none());
        job.cancel().await;
        assert!(job.is_done());
        assert!(assistant_store::claim_next_queued_generation(job.conversation_id, "owner").await.unwrap().is_some());
    }

    #[tokio::test]
    async fn accepted_cancellation_survives_restart_and_wins_completion_race() {
        let (_pool, job) = running_job().await;
        job.request_cancellation().await.unwrap();
        // Even if the worker had selected success before the request arrived,
        // the terminal writer honors the accepted cancellation.
        job.complete().await;
        assert_eq!(job.status_name(), "failed");
        assert!(collect(stream_for_job(job, 0)).await.contains("\"cancelled\":true"));

        let (_pool, interrupted) = running_job().await;
        interrupted.request_cancellation().await.unwrap();
        let recovered = assistant_store::requeue_interrupted_generations().await.unwrap();
        assert_eq!((recovered.requeued, recovered.failed), (0, 1));
        let stored = assistant_store::get_generation_job(interrupted.id, "owner").await.unwrap().unwrap();
        assert_eq!(stored.status, "failed");
        assert_eq!(stored.error_message.as_deref(), Some("用户已停止生成"));
    }

    #[tokio::test]
    async fn failed_cancellation_commit_is_not_acknowledged() {
        let (pool, job) = running_job().await;
        sqlx::query("CREATE TRIGGER reject_cancel BEFORE UPDATE OF metadata ON assistant_messages BEGIN SELECT RAISE(ABORT, 'temporary failure'); END")
            .execute(&pool).await.unwrap();
        assert!(job.request_cancellation().await.is_err());
        assert!(!job.cancellation_token().is_cancelled());
        assert_eq!(job.status_name(), "running");
    }

    #[tokio::test]
    async fn retry_replaces_terminal_execution_and_old_cleanup_cannot_remove_it() {
        let (_pool, job) = running_job().await;
        let record = assistant_store::get_generation_job(job.id, "owner").await.unwrap().unwrap();
        let first = register_claimed_job(&record);
        assert!(Arc::ptr_eq(&first, &register_claimed_job(&record)));
        first.status.store(STATUS_FAILED, Ordering::SeqCst);
        let retry = register_claimed_job(&record);
        assert_ne!(first.execution_id, retry.execution_id);
        assert!(!retry.is_done());
        global_registry().remove_execution(record.id, first.execution_id);
        assert!(Arc::ptr_eq(&retry, &global_registry().get(record.id).unwrap()));
        global_registry().remove_execution(record.id, retry.execution_id);
        assert!(global_registry().get(record.id).is_none());
    }

    #[tokio::test]
    async fn completion_retries_durability_before_emitting_terminal_once() {
        let (pool, job) = running_job().await;
        sqlx::query("CREATE TRIGGER reject_finish BEFORE UPDATE ON assistant_generation_jobs WHEN NEW.status = 'completed' BEGIN SELECT RAISE(ABORT, 'temporary failure'); END")
            .execute(&pool).await.unwrap();
        // Keep polling persistence while this test reads the DB and removes the
        // rejection trigger; suspending it mid-transaction can retain a write lock.
        let completing_job = job.clone();
        let mut complete = tokio::spawn(async move { completing_job.complete().await });
        assert!(tokio::time::timeout(std::time::Duration::from_millis(40), &mut complete).await.is_err());
        assert!(!job.is_done());
        assert!(lock_unpoisoned(&job.event_log).is_empty());
        assert_eq!(assistant_store::get_generation_job(job.id, "owner").await.unwrap().unwrap().status, "running");
        sqlx::query("DROP TRIGGER reject_finish").execute(&pool).await.unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(2), &mut complete).await.unwrap().unwrap();
        tokio::join!(job.complete(), job.fail("late failure".into()), job.cancel());
        assert_eq!(job.status_name(), "completed");
        assert_eq!(assistant_store::get_generation_job(job.id, "owner").await.unwrap().unwrap().status, "completed");
        assert_eq!(lock_unpoisoned(&job.event_log).values().filter(|e| e.event == "assistant_done").count(), 1);
    }
}
