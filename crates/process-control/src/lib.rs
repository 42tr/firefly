//! Own a subprocess group so cancellation also stops shell/tool descendants.
//!
//! Callers must start the child with `process_group(0)`. [`ProcessGroup::kill`]
//! disarms the guard before signalling so a later `Drop` cannot signal a
//! recycled process-group ID. [`ProcessGroup::terminate`] is the optional
//! graceful first signal and intentionally leaves the guard armed for the
//! caller's later forceful [`ProcessGroup::kill`]. Callers that have reaped
//! the child through a normal exit should call [`ProcessGroup::disarm`]:
//! signalling a group whose leader was already waited for risks hitting a
//! recycled process-group ID.

use std::sync::atomic::{AtomicU32, Ordering};

pub struct ProcessGroup(AtomicU32);

impl ProcessGroup {
    pub fn new(pid: u32) -> Self {
        Self(AtomicU32::new(pid))
    }

    /// Ask the process group to terminate, retaining the guard for escalation.
    pub fn terminate(&self) {
        let pid = self.0.load(Ordering::Acquire);
        if pid != 0 {
            // Commands using this guard must be spawned with process_group(0).
            unsafe { libc::killpg(pid as libc::pid_t, libc::SIGTERM) };
        }
    }

    /// Stop tracking the group without signalling it. Call after the child has
    /// been reaped through a normal exit: the group ID may be recycled later,
    /// and descendants left behind by a successful exit are not ours to kill.
    pub fn disarm(&self) {
        self.0.store(0, Ordering::Release);
    }

    /// Forcefully stop the process group and disarm this guard.
    pub fn kill(&self) {
        self.kill_with(|pid| unsafe {
            libc::killpg(pid as libc::pid_t, libc::SIGKILL);
        });
    }

    fn kill_with(&self, kill: impl FnOnce(u32)) {
        // Disarm before signalling: later cancellation, close, and Drop must
        // never signal a recycled PGID after the original group has exited.
        let pid = self.0.swap(0, Ordering::AcqRel);
        if pid != 0 {
            kill(pid);
        }
    }
}

impl Drop for ProcessGroup {
    fn drop(&mut self) {
        self.kill();
    }
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
                scope.spawn(|| {
                    group.kill_with(|pid| {
                        assert_eq!(pid, 123);
                        calls.fetch_add(1, Ordering::Relaxed);
                    })
                });
            }
        });
        assert_eq!(calls.load(Ordering::Relaxed), 1);
        group.kill_with(|_| panic!("already signalled a process group"));
        assert_eq!(group.0.load(Ordering::Acquire), 0);
        drop(group);
    }
}
