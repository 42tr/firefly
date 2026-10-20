use std::{sync::Arc, time::Duration};

use serde_json::{json, Value};

use super::codex::{
    codex_turn_outcome, consume_codex_turn, is_codex_capacity_error, is_codex_retry_limit_error,
    is_codex_retryable_error, CODEX_CAPACITY_RETRY_DELAY,
};
use crate::assistant_job;

const CAPACITY_ERROR: &str = "Selected model is at capacity. Please try a different model.";
const RETRY_LIMIT_ERROR: &str = "exceeded retry limit, last status: 429 Too Many Requests";

async fn consume(notifications: Vec<Value>) -> anyhow::Result<()> {
    let job = Arc::new(assistant_job::GenerationJob::new(uuid::Uuid::new_v4(), 1, "test".into()));
    let (tx, rx) = tokio::sync::broadcast::channel(16);
    for notification in notifications {
        tx.send(notification).unwrap();
    }
    drop(tx);
    consume_codex_turn("thread", "turn", rx, 1, &job).await
}

fn error(message: &str, will_retry: bool) -> Value {
    json!({"method": "error", "params": {
        "threadId": "thread", "turnId": "turn", "willRetry": will_retry,
        "error": {"message": message}
    }})
}

fn completed(status: &str, message: Option<&str>) -> Value {
    json!({"method": "turn/completed", "params": {
        "threadId": "thread", "turn": {"id": "turn", "status": status,
            "error": message.map(|message| json!({"message": message}))}
    }})
}

#[test]
fn polled_turn_outcome_matches_completion_event() {
    assert!(codex_turn_outcome(&json!({"status": "completed"}), None, None).is_ok());
    let interrupted = codex_turn_outcome(&json!({"status": "interrupted"}), None, None).unwrap_err();
    assert_eq!(interrupted.to_string(), "Codex turn 已中断");
    let failed = json!({"status": "failed", "error": {"message": "boom"}});
    assert_eq!(codex_turn_outcome(&failed, Some("event"), None).unwrap_err().to_string(), "boom");
    let bare = json!({"status": "failed"});
    assert_eq!(codex_turn_outcome(&bare, None, Some(CAPACITY_ERROR)).unwrap_err().to_string(), CAPACITY_ERROR);
}

#[test]
fn capacity_detection_is_specific_and_case_insensitive() {
    assert_eq!(CODEX_CAPACITY_RETRY_DELAY, Duration::from_secs(10));
    assert!(is_codex_capacity_error(CAPACITY_ERROR));
    assert!(is_codex_capacity_error("SELECTED MODEL IS AT CAPACITY"));
    for message in ["quota exceeded", "invalid API key", "Codex 事件流已断开", RETRY_LIMIT_ERROR] {
        assert!(!is_codex_capacity_error(message));
    }
}

#[test]
fn retry_limit_detection_covers_exhausted_429() {
    assert!(is_codex_retry_limit_error(RETRY_LIMIT_ERROR));
    assert!(is_codex_retry_limit_error("Exceeded Retry Limit, last status: 503"));
    for message in [CAPACITY_ERROR, "quota exceeded", "Codex 事件流已断开"] {
        assert!(!is_codex_retry_limit_error(message));
    }
    assert!(is_codex_retryable_error(CAPACITY_ERROR));
    assert!(is_codex_retryable_error(RETRY_LIMIT_ERROR));
    assert!(!is_codex_retryable_error("invalid API key"));
}

#[tokio::test]
async fn retry_limit_error_waits_for_failed_turn_and_preserves_message() {
    let err = consume(vec![error(RETRY_LIMIT_ERROR, false), completed("failed", None)]).await.unwrap_err();
    assert_eq!(err.to_string(), RETRY_LIMIT_ERROR);
    assert!(is_codex_retryable_error(&err.to_string()));
}

#[tokio::test]
async fn capacity_error_waits_for_failed_turn_and_preserves_message() {
    let err = consume(vec![error(CAPACITY_ERROR, false), completed("failed", None)]).await.unwrap_err();
    assert_eq!(err.to_string(), CAPACITY_ERROR);
}

#[tokio::test]
async fn internal_retry_and_stale_completion_keep_current_turn_alive() {
    let mut stale = completed("failed", Some("old failure"));
    stale["params"]["turn"]["id"] = json!("previous-turn");
    consume(vec![error(CAPACITY_ERROR, true), stale, completed("completed", None)]).await.unwrap();
}

#[tokio::test]
async fn interrupted_turn_does_not_retry_an_earlier_capacity_error() {
    let err = consume(vec![error(CAPACITY_ERROR, false), completed("interrupted", None)]).await.unwrap_err();
    assert_eq!(err.to_string(), "Codex turn 已中断");
    assert!(!is_codex_capacity_error(&err.to_string()));
}
