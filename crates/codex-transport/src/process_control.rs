//! Own a subprocess group so cancellation also stops shell/tool descendants.
use std::sync::atomic::{AtomicU32, Ordering};

pub(crate) struct ProcessGroup(AtomicU32);

impl ProcessGroup {
    pub(crate) fn new(pid: u32) -> Self { Self(AtomicU32::new(pid)) }

    pub(crate) fn kill(&self) {
        // Commands using this guard must be spawned with process_group(0).
        // ESRCH means the group has already exited.
        self.kill_with(|pid| unsafe { libc::killpg(pid as libc::pid_t, libc::SIGKILL); });
    }

    fn kill_with(&self, kill: impl FnOnce(u32)) {
        // Disarm before signalling: later cancellation, close, and Drop must
        // never signal a recycled PGID after the original group has exited.
        let pid = self.0.swap(0, Ordering::AcqRel);
        if pid != 0 { kill(pid); }
    }
}

impl Drop for ProcessGroup {
    fn drop(&mut self) { self.kill(); }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn concurrent_kill_requests_signal_only_once_and_disarm_drop() {
        let group = ProcessGroup::new(123);
        let calls = AtomicU32::new(0);
        std::thread::scope(|scope| {
            for _ in 0..8 {
                scope.spawn(|| group.kill_with(|pid| {
                    assert_eq!(pid, 123);
                    calls.fetch_add(1, Ordering::Relaxed);
                }));
            }
        });
        assert_eq!(calls.load(Ordering::Relaxed), 1);
        group.kill_with(|_| panic!("already signalled a process group"));
        assert_eq!(group.0.load(Ordering::Acquire), 0);
        drop(group);
    }
}
