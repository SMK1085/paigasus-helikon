//! Session management for the axum runtime.
//!
//! [`SessionProvider`] maps an optional `X-Session-Id` header value to a
//! [`paigasus_helikon_core::Session`].  [`InMemorySessionProvider`] is the
//! default implementation: it keeps a bounded FIFO map backed by
//! [`paigasus_helikon_core::MemorySession`].  Anonymous requests (`id = None`)
//! always receive a fresh, unshared session.
//!
//! [`SessionLocks`] is an internal helper used by the transport handlers to
//! serialise concurrent runs that share the same session id.

use std::{
    collections::{HashMap, VecDeque},
    sync::Arc,
};

use async_trait::async_trait;
use paigasus_helikon_core::{MemorySession, Session};
use tokio::sync::RwLock;

use crate::error::ServerError;

// ---------------------------------------------------------------------------
// Public trait
// ---------------------------------------------------------------------------

/// Maps an optional session identifier to a [`Session`] object.
///
/// Implementations must be cheaply cloneable (all provided by this crate wrap
/// an `Arc` internally) so that the axum state extractor can share one
/// instance across all handler tasks.
///
/// - `Some(id)` — return the existing session for `id`, creating one on the
///   first call.  Two calls with the same non-`None` `id` must return `Arc`s
///   that are pointer-equal (`Arc::ptr_eq`).
/// - `None` — return a fresh, anonymous session that is *not* stored and is
///   never pointer-equal to any other session.
#[async_trait]
pub trait SessionProvider: Send + Sync {
    /// Look up or create the session for `id`.
    async fn session(&self, id: Option<&str>) -> Result<Arc<dyn Session>, ServerError>;
}

// ---------------------------------------------------------------------------
// InMemorySessionProvider
// ---------------------------------------------------------------------------

/// A bounded, FIFO in-memory [`SessionProvider`] backed by
/// [`MemorySession`].
///
/// When the number of tracked sessions exceeds `max_sessions` the oldest
/// session (by insertion order) is evicted.  Anonymous sessions (`id = None`)
/// are never stored and never count toward the limit.
pub struct InMemorySessionProvider {
    max_sessions: usize,
    /// Guards both `map` and `order` together so eviction and insertion are
    /// atomic.
    inner: RwLock<InMemoryInner>,
}

struct InMemoryInner {
    map: HashMap<String, Arc<dyn Session>>,
    order: VecDeque<String>,
}

impl InMemorySessionProvider {
    /// Create a new provider that holds at most `max_sessions` sessions.
    ///
    /// # Panics
    ///
    /// Panics if `max_sessions` is zero.
    pub fn new(max_sessions: usize) -> Self {
        assert!(max_sessions > 0, "max_sessions must be > 0");
        Self {
            max_sessions,
            inner: RwLock::new(InMemoryInner {
                map: HashMap::new(),
                order: VecDeque::new(),
            }),
        }
    }

    /// Return the number of currently tracked (named) sessions.
    ///
    /// Available in test builds only.
    #[cfg(test)]
    #[allow(clippy::len_without_is_empty)]
    pub fn len(&self) -> usize {
        self.inner
            .try_read()
            .expect("lock not contended in tests")
            .map
            .len()
    }
}

#[async_trait]
impl SessionProvider for InMemorySessionProvider {
    async fn session(&self, id: Option<&str>) -> Result<Arc<dyn Session>, ServerError> {
        let Some(id) = id else {
            // Anonymous: fresh session, never stored.
            return Ok(Arc::new(MemorySession::new()) as Arc<dyn Session>);
        };

        // Fast path: read lock.
        {
            let inner = self.inner.read().await;
            if let Some(arc) = inner.map.get(id) {
                return Ok(Arc::clone(arc));
            }
        }

        // Slow path: write lock — insert and possibly evict.
        let mut inner = self.inner.write().await;

        // Double-check in case another writer raced us.
        if let Some(arc) = inner.map.get(id) {
            return Ok(Arc::clone(arc));
        }

        let session: Arc<dyn Session> = Arc::new(MemorySession::new());
        inner.map.insert(id.to_owned(), Arc::clone(&session));
        inner.order.push_back(id.to_owned());

        // Evict the oldest entry if over the limit.
        if inner.map.len() > self.max_sessions {
            if let Some(oldest) = inner.order.pop_front() {
                inner.map.remove(&oldest);
            }
        }

        Ok(session)
    }
}

// ---------------------------------------------------------------------------
// SessionLocks (pub(crate) — used by transport handlers in Task 10)
// ---------------------------------------------------------------------------

/// Per-session run serialisation locks.
///
/// Ensures that at most one request runs at a time for a given session id.
/// Anonymous requests (`id = None`) get a fresh throwaway lock each time.
///
/// **Bounded growth.** Each [`lock_for`](SessionLocks::lock_for) call
/// opportunistically prunes entries that are held *only* by the map
/// (`Arc::strong_count == 1`, i.e. no in-flight run is holding the lock), so the
/// map stays bounded by the number of concurrently-active sessions rather than
/// by the number of distinct session ids observed over the server's lifetime.
pub(crate) struct SessionLocks {
    map: std::sync::Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
}

impl SessionLocks {
    /// Create an empty lock map.
    pub(crate) fn new() -> Self {
        Self {
            map: std::sync::Mutex::new(HashMap::new()),
        }
    }

    /// Return the per-session lock for `id`.
    ///
    /// - `Some(id)` — return the shared lock for `id`, creating it on the
    ///   first call.  Two calls with the same `id` (while at least one caller
    ///   still holds the returned `Arc`) return pointer-equal `Arc`s.
    /// - `None` — return a fresh throwaway lock that is not shared with any
    ///   other call.
    ///
    /// Before resolving `id`, every entry whose lock is no longer held by any
    /// active run (`Arc::strong_count == 1`) is pruned, keeping the map bounded.
    pub(crate) fn lock_for(&self, id: Option<&str>) -> Arc<tokio::sync::Mutex<()>> {
        let Some(id) = id else {
            return Arc::new(tokio::sync::Mutex::new(()));
        };

        let mut map = self.map.lock().expect("SessionLocks mutex poisoned");
        // Opportunistic cleanup: drop entries held only by the map (no active
        // run is keeping the lock alive). An entry for `id` that is currently in
        // use (count > 1) is preserved, so concurrent same-id requests keep
        // serialising on a pointer-equal lock.
        map.retain(|_, lock| Arc::strong_count(lock) > 1);
        Arc::clone(
            map.entry(id.to_owned())
                .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(()))),
        )
    }

    /// Number of currently-tracked per-session locks. Test-only.
    #[cfg(test)]
    #[allow(clippy::len_without_is_empty)]
    pub(crate) fn len(&self) -> usize {
        self.map.lock().expect("SessionLocks mutex poisoned").len()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn same_id_returns_same_session_none_is_fresh() {
        let p = InMemorySessionProvider::new(16);
        let a = p.session(Some("s1")).await.unwrap();
        let b = p.session(Some("s1")).await.unwrap();
        assert!(Arc::ptr_eq(&a, &b));
        let anon1 = p.session(None).await.unwrap();
        let anon2 = p.session(None).await.unwrap();
        assert!(!Arc::ptr_eq(&anon1, &anon2)); // anonymous never shared / never stored
    }

    #[tokio::test]
    async fn bounded_map_evicts() {
        let p = InMemorySessionProvider::new(1);
        let _a = p.session(Some("s1")).await.unwrap();
        let _b = p.session(Some("s2")).await.unwrap(); // evicts s1
        assert_eq!(p.len(), 1); // expose a test-only len()
    }

    #[test]
    fn session_locks_same_id_ptr_eq() {
        let locks = SessionLocks::new();
        let l1 = locks.lock_for(Some("x"));
        let l2 = locks.lock_for(Some("x"));
        assert!(Arc::ptr_eq(&l1, &l2));
    }

    #[test]
    fn session_locks_none_distinct() {
        let locks = SessionLocks::new();
        let l1 = locks.lock_for(None);
        let l2 = locks.lock_for(None);
        assert!(!Arc::ptr_eq(&l1, &l2));
    }

    /// Once the only `Arc` to a session's lock is dropped, the next `lock_for`
    /// call prunes the now-unheld entry, keeping the map bounded.
    #[test]
    fn session_locks_prune_drops_unheld_entries() {
        let locks = SessionLocks::new();

        // Take and release the lock Arc for "a": after the scope, only the map
        // holds it (strong_count == 1).
        {
            let _la = locks.lock_for(Some("a"));
            assert_eq!(locks.len(), 1);
        }

        // Acquiring a lock for a different id prunes the now-unheld "a" entry.
        let _lb = locks.lock_for(Some("b"));
        assert_eq!(locks.len(), 1); // only "b" remains; "a" was pruned

        // Sanity: a still-held entry is NOT pruned by a later call.
        let _lb_alias = locks.lock_for(Some("b"));
        assert!(Arc::ptr_eq(&_lb, &_lb_alias));
        assert_eq!(locks.len(), 1);
    }
}
