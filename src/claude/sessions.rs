//! Conversation-scoped CLI ownership, independent of browser connections.
use std::{
    collections::HashMap,
    sync::{Arc, Mutex, OnceLock},
    time::{Duration, Instant},
};

use tokio::sync::{Mutex as AsyncMutex, OwnedMutexGuard};

use super::{ClaudeTurn, PreparedTurn};

const IDLE_TTL: Duration = Duration::from_secs(60);
const MAX_IDLE_SESSIONS: usize = 4;
type Key = (String, i64);
type Slot = Arc<AsyncMutex<Option<Cached>>>;
static SESSIONS: OnceLock<Mutex<HashMap<Key, Slot>>> = OnceLock::new();

struct Cached {
    turn: ClaudeTurn,
    identity: [u8; 32],
    last_used: Instant,
}

pub struct Lease {
    guard: Option<OwnedMutexGuard<Option<Cached>>>,
    slot: Slot,
    key: Key,
    pub prompt_id: String,
}

pub async fn acquire(
    user: &str,
    conversation: i64,
    prepared: PreparedTurn,
    prompt: &str,
) -> anyhow::Result<Lease> {
    let key = (user.to_owned(), conversation);
    let slot = {
        let mut sessions = SESSIONS
            .get_or_init(Mutex::default)
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        // Only an unleased process can be evicted. Active jobs keep their slot.
        if !sessions.contains_key(&key) && sessions.len() >= MAX_IDLE_SESSIONS {
            let idle = sessions
                .iter()
                .find(|(_, slot)| Arc::strong_count(slot) == 1 && slot.try_lock().is_ok())
                .map(|(key, _)| key.clone());
            if let Some(idle) = idle {
                sessions.remove(&idle);
            }
        }
        sessions
            .entry(key.clone())
            .or_insert_with(|| Arc::new(AsyncMutex::new(None)))
            .clone()
    };
    let mut guard = slot.clone().lock_owned().await;
    let reusable = match guard.as_mut() {
        Some(cached) => cached.identity == prepared.identity && cached.turn.is_running()?,
        None => false,
    };
    if !reusable {
        if let Some(mut cached) = guard.take() {
            cached.turn.interrupt().await?;
        }
        let identity = prepared.identity;
        *guard = Some(Cached {
            turn: prepared.spawn()?,
            identity,
            last_used: Instant::now(),
        });
    }
    // Own the invalidation guard before the first asynchronous write. Dropping
    // a cancelled acquire must also stop a partially written prompt's process.
    let mut lease = Lease {
        guard: Some(guard),
        slot,
        key,
        prompt_id: String::new(),
    };
    lease.prompt_id = lease.turn().send_prompt(prompt).await?;
    Ok(lease)
}

impl Lease {
    pub fn turn(&mut self) -> &mut ClaudeTurn {
        &mut self.guard.as_mut().unwrap().as_mut().unwrap().turn
    }

    pub async fn release(mut self, healthy: bool) -> anyhow::Result<()> {
        let mut guard = self.guard.take().unwrap();
        if !healthy {
            if let Some(mut cached) = guard.take() {
                cached.turn.interrupt().await?;
            }
        } else if let Some(cached) = guard.as_mut() {
            cached.last_used = Instant::now();
        }
        let slot = Arc::downgrade(&self.slot);
        let key = self.key.clone();
        drop(guard);
        tokio::spawn(async move {
            tokio::time::sleep(IDLE_TTL).await;
            let Some(slot) = slot.upgrade() else { return };
            let mut guard = slot.lock().await;
            if guard
                .as_ref()
                .is_some_and(|cached| cached.last_used.elapsed() < IDLE_TTL)
            {
                return;
            }
            if let Some(mut cached) = guard.take() {
                if let Err(error) = cached.turn.interrupt().await {
                    tracing::warn!(%error, "关闭空闲 Claude 会话失败");
                }
            }
            drop(guard);
            let mut sessions = SESSIONS
                .get()
                .unwrap()
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            if Arc::strong_count(&slot) == 2
                && sessions
                    .get(&key)
                    .is_some_and(|current| Arc::ptr_eq(current, &slot))
            {
                sessions.remove(&key);
            }
        });
        Ok(())
    }
}

impl Drop for Lease {
    fn drop(&mut self) {
        // An aborted worker must never leave active work available for reuse.
        if let Some(guard) = self.guard.as_mut() {
            guard.take();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn prepared(identity: &str) -> PreparedTurn {
        let mut command = tokio::process::Command::new("python3");
        command.env_clear().env("FIXTURE_IDENTITY", identity).args(["-u", "-c", r#"
import json,os,sys
for line in sys.stdin:
    value=json.loads(line)
    print(json.dumps({'pid':os.getpid(),'uuid':value['uuid'],'content':value['message']['content']}),flush=True)
"#]);
        PreparedTurn::for_test(command)
    }

    #[tokio::test]
    async fn same_conversation_reuses_idle_cli_but_provider_changes_and_other_owners_do_not() {
        let owner = uuid::Uuid::new_v4().to_string();
        let mut first = acquire(&owner, 1, prepared("one"), "first").await.unwrap();
        let pid = first.turn().next_event().await.unwrap().unwrap()["pid"].clone();
        first.release(true).await.unwrap();
        let mut second = acquire(&owner, 1, prepared("one"), "second").await.unwrap();
        let event = second.turn().next_event().await.unwrap().unwrap();
        assert_eq!(event["pid"], pid);
        assert_eq!(event["content"], "second");
        second.release(true).await.unwrap();
        let mut changed = acquire(&owner, 1, prepared("two"), "changed")
            .await
            .unwrap();
        assert_ne!(
            changed.turn().next_event().await.unwrap().unwrap()["pid"],
            pid
        );
        changed.release(false).await.unwrap();
        let mut other = acquire("another-owner", 1, prepared("one"), "other")
            .await
            .unwrap();
        assert_ne!(
            other.turn().next_event().await.unwrap().unwrap()["pid"],
            pid
        );
        other.release(false).await.unwrap();
    }

    #[tokio::test]
    async fn dropped_worker_invalidates_its_cli_instead_of_reusing_unfinished_work() {
        let owner = uuid::Uuid::new_v4().to_string();
        let mut first = acquire(&owner, 2, prepared("one"), "first").await.unwrap();
        let pid = first.turn().next_event().await.unwrap().unwrap()["pid"].clone();
        drop(first);
        let mut second = acquire(&owner, 2, prepared("one"), "second").await.unwrap();
        assert_ne!(
            second.turn().next_event().await.unwrap().unwrap()["pid"],
            pid
        );
        assert_ne!(pid, json!(null));
        second.release(false).await.unwrap();
    }
}
