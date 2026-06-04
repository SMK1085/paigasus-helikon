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
