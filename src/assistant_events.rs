//! Wake WebSocket subscribers after committed writes. Snapshots remain authoritative.
use std::sync::OnceLock;
use tokio::sync::broadcast;

#[derive(Clone, Debug)]
pub struct Change {
    pub user_id: Option<String>,
    pub domain: &'static str,
}

impl Change {
    pub fn visible_to(&self, user_id: &str) -> bool {
        self.user_id.as_deref().is_none_or(|owner| owner == user_id)
    }
}

pub fn changes() -> &'static broadcast::Sender<Change> {
    static CHANGES: OnceLock<broadcast::Sender<Change>> = OnceLock::new();
    CHANGES.get_or_init(|| broadcast::channel(256).0)
}

pub fn notify(user_id: Option<&str>, domain: &'static str) {
    let _ = changes().send(Change { user_id: user_id.map(str::to_owned), domain });
}
