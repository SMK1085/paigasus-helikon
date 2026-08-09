//! [`CancelRegistry`] — maps a live A2A task id to the `CancellationToken` driving it.
//!
//! `tasks/cancel` needs a way to reach an in-flight run from a task id. Tokens are
//! registered when a run is spawned and removed by the same detached task that owns the
//! run's lifetime, so the map cannot outlive its runs.
//!
//! A task present in the store but absent here has no live run in *this* container —
//! with a durable [`TaskStore`](crate::TaskStore) that means another microVM ran it, and
//! `tasks/cancel` answers `-32002` rather than pretending to have cancelled anything.

use std::{collections::HashMap, sync::Mutex};

use paigasus_helikon_core::CancellationToken;

/// Live-run cancellation tokens, keyed by A2A task id.
#[derive(Default)]
pub(crate) struct CancelRegistry {
    inner: Mutex<HashMap<String, CancellationToken>>,
}

impl CancelRegistry {
    /// Record the token driving `task_id`'s run.
    pub(crate) fn register(&self, task_id: String, token: CancellationToken) {
        self.lock().insert(task_id, token);
    }

    /// Fire the token for `task_id`, returning whether one was registered.
    ///
    /// `false` means there is no live run here to cancel — the caller must report that
    /// rather than claiming a cancellation that never happened.
    pub(crate) fn cancel(&self, task_id: &str) -> bool {
        let token = self.lock().remove(task_id);
        match token {
            Some(token) => {
                token.cancel();
                true
            }
            None => false,
        }
    }

    /// Drop `task_id`'s entry, called by a run's owning task once it finishes.
    pub(crate) fn remove(&self, task_id: &str) {
        self.lock().remove(task_id);
    }

    /// Lock the map, recovering from poisoning.
    ///
    /// A panicking run must not wedge every later cancel: the map is a plain lookup with
    /// no invariant spanning entries, so the data behind a poisoned lock is still sound.
    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, CancellationToken>> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancelling_a_registered_task_fires_its_token() {
        let reg = CancelRegistry::default();
        let token = CancellationToken::new();
        reg.register("t1".to_owned(), token.clone());
        assert!(reg.cancel("t1"));
        assert!(token.is_cancelled());
    }

    #[test]
    fn cancelling_an_unregistered_task_reports_false() {
        let reg = CancelRegistry::default();
        assert!(
            !reg.cancel("nope"),
            "no live run means nothing was cancelled"
        );
    }

    #[test]
    fn removed_tasks_are_no_longer_cancellable() {
        let reg = CancelRegistry::default();
        reg.register("t1".to_owned(), CancellationToken::new());
        reg.remove("t1");
        assert!(!reg.cancel("t1"));
    }
}
