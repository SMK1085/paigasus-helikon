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
#[derive(Clone, Default)]
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
}
