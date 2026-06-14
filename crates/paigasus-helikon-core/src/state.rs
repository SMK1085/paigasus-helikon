//! Run-scoped, in-memory coordination state for workflow agents (SMA-325).
//!
//! [`SessionState`] is a key→JSON scratchpad shared across the sub-agents of a
//! single run; [`ActionsHandle`] is a control side-channel a tool uses to signal
//! the enclosing driver (today: `escalate`). Both mirror the [`crate::FailureSlot`]
//! pattern: an `Arc<Mutex<…>>` carried on [`crate::RunContext`], projected into
//! [`crate::ToolContext`], written inside, read after the stream drains. Neither
//! is persisted to the `Session` log.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// A run-scoped, in-memory key→JSON store shared across a run's sub-agents.
///
/// Cloning shares the underlying store (it is an `Arc` handle). `ParallelAgent`
/// branches write **disjoint** keys, so the brief per-write lock never contends
/// meaningfully. **Not** persisted to the `Session` event log.
#[derive(Clone, Default, Debug)]
pub struct SessionState(Arc<Mutex<HashMap<String, serde_json::Value>>>);

impl SessionState {
    /// Construct an empty store.
    pub fn new() -> Self {
        Self::default()
    }

    /// Read a value by key, cloned out of the store.
    pub fn get(&self, key: &str) -> Option<serde_json::Value> {
        self.0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(key)
            .cloned()
    }

    /// Insert or overwrite a value.
    pub fn set(&self, key: impl Into<String>, value: impl Into<serde_json::Value>) {
        self.0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(key.into(), value.into());
    }

    /// `true` if the key is present.
    pub fn contains_key(&self, key: &str) -> bool {
        self.0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains_key(key)
    }

    /// Every key currently in the store, in arbitrary order.
    pub fn keys(&self) -> Vec<String> {
        self.0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .keys()
            .cloned()
            .collect()
    }

    /// Atomically increment the `u64` at `key` if it is below `max`.
    ///
    /// Reads the value at `key` (absent or non-`u64` ⇒ treated as `0`); if it
    /// is `< max`, stores `value + 1` and returns `true`; otherwise leaves it
    /// untouched and returns `false`. The read-compare-write happens under a
    /// single lock hold, so concurrent callers racing on the same key never
    /// collectively exceed `max`.
    pub fn increment_u64_if_below(&self, key: &str, max: u64) -> bool {
        let mut guard = self.0.lock().unwrap_or_else(|e| e.into_inner());
        let current = guard.get(key).and_then(|v| v.as_u64()).unwrap_or(0);
        if current < max {
            guard.insert(key.to_owned(), serde_json::Value::from(current + 1));
            true
        } else {
            false
        }
    }
}

/// Control signals a tool can raise to the enclosing driver.
///
/// The faithful port of ADK's `EventActions`. Today it carries one signal,
/// `escalate`; `#[non_exhaustive]` so it can grow (`skip_summarization`,
/// `transfer_to_agent`, …) without a breaking change.
#[non_exhaustive]
#[derive(Debug, Default, Clone)]
pub struct EventActions {
    /// Request that the enclosing `LoopAgent` stop iterating.
    pub escalate: bool,
}

/// Cloneable handle a tool uses to raise [`EventActions`] signals.
///
/// `LoopAgent` reads [`ActionsHandle::is_escalated`] after a sub-agent run
/// drains — the same write-inside / read-after-drain discipline as
/// [`crate::FailureSlot`].
#[derive(Clone, Default, Debug)]
pub struct ActionsHandle(Arc<Mutex<EventActions>>);

impl ActionsHandle {
    /// Construct a handle with no signals raised.
    pub fn new() -> Self {
        Self::default()
    }

    /// Raise the `escalate` signal.
    pub fn escalate(&self) {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).escalate = true;
    }

    /// `true` once any holder of this handle has called [`ActionsHandle::escalate`].
    pub fn is_escalated(&self) -> bool {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).escalate
    }

    /// Clone the current [`EventActions`] out for inspection.
    pub fn snapshot(&self) -> EventActions {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

#[cfg(test)]
mod tests {
    use super::SessionState;
    use serde_json::json;

    #[test]
    fn set_get_roundtrip() {
        let s = SessionState::new();
        assert!(s.get("k").is_none());
        s.set("k", "v");
        assert_eq!(s.get("k"), Some(json!("v")));
        assert!(s.contains_key("k"));
    }

    #[test]
    fn clone_shares_store() {
        let a = SessionState::new();
        let b = a.clone();
        b.set("x", 1);
        assert_eq!(a.get("x"), Some(json!(1)));
    }

    #[test]
    fn keys_lists_all() {
        let s = SessionState::new();
        s.set("a", 1);
        s.set("b", 2);
        let mut k = s.keys();
        k.sort();
        assert_eq!(k, vec!["a".to_owned(), "b".to_owned()]);
    }

    #[test]
    fn increment_if_below_edge_cases() {
        let s = SessionState::new();

        // Absent key ⇒ treated as 0; first admits store 1, then 2.
        assert!(s.increment_u64_if_below("k", 2));
        assert_eq!(s.get("k").and_then(|v| v.as_u64()), Some(1));
        assert!(s.increment_u64_if_below("k", 2));
        assert_eq!(s.get("k").and_then(|v| v.as_u64()), Some(2));

        // At the cap ⇒ false, value unchanged.
        assert!(!s.increment_u64_if_below("k", 2));
        assert_eq!(s.get("k").and_then(|v| v.as_u64()), Some(2));

        // max = 0 ⇒ always false, nothing stored.
        assert!(!s.increment_u64_if_below("zero", 0));
        assert!(s.get("zero").is_none());

        // Non-u64 value ⇒ treated as 0, overwritten with 1.
        s.set("garbage", "not a number");
        assert!(s.increment_u64_if_below("garbage", 1));
        assert_eq!(s.get("garbage").and_then(|v| v.as_u64()), Some(1));
    }

    #[test]
    fn increment_if_below_is_atomic_under_contention() {
        use std::thread;

        const MAX: u64 = 1000;
        const THREADS: usize = 64;
        let s = SessionState::new();

        // Every thread races to admit on the same key until the cap is hit,
        // tallying how many admits (true) it saw.
        let handles: Vec<_> = (0..THREADS)
            .map(|_| {
                let s = s.clone();
                thread::spawn(move || {
                    let mut local = 0u64;
                    while s.increment_u64_if_below("uses", MAX) {
                        local += 1;
                    }
                    local
                })
            })
            .collect();

        let total: u64 = handles.into_iter().map(|h| h.join().unwrap()).sum();

        // Exactly MAX admits across all threads, and the stored counter lands
        // on MAX — never above it. A non-atomic get/set would overshoot here.
        assert_eq!(total, MAX, "exactly MAX admits across all threads");
        assert_eq!(s.get("uses").and_then(|v| v.as_u64()), Some(MAX));
    }

    use super::ActionsHandle;

    #[test]
    fn escalate_sets_flag() {
        let a = ActionsHandle::new();
        assert!(!a.is_escalated());
        a.escalate();
        assert!(a.is_escalated());
    }

    #[test]
    fn actions_clone_shares_slot() {
        let a = ActionsHandle::new();
        let b = a.clone();
        b.escalate();
        assert!(a.is_escalated(), "a clone observes the escalate");
    }

    #[test]
    fn snapshot_reflects_escalate() {
        let a = ActionsHandle::new();
        a.escalate();
        assert!(a.snapshot().escalate);
    }
}
