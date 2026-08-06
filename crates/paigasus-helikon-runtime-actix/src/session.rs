//! Session management for the actix runtime.
//!
//! [`SessionProvider`] maps a [`SessionKey`] — the authenticated principal plus
//! the caller-supplied `X-Session-Id` header value — to a
//! [`paigasus_helikon_core::Session`].  [`InMemorySessionProvider`] is the
//! default implementation: it keeps a bounded FIFO map backed by
//! [`paigasus_helikon_core::MemorySession`].  Anonymous requests
//! (`key.id == None`) always receive a fresh, unshared session.
//!
//! [`SessionLocks`] is an internal helper used by the transport handlers to
//! serialise concurrent runs that share the same session key.

use std::{
    collections::{HashMap, VecDeque},
    sync::Arc,
};

use async_trait::async_trait;
use paigasus_helikon_core::{MemorySession, Session};
use tokio::sync::RwLock;

use crate::error::ServerError;

// ---------------------------------------------------------------------------
// SessionKey
// ---------------------------------------------------------------------------

/// The compound identity a session is resolved under.
///
/// # Security
///
/// A [`SessionProvider`] that keys on [`id`](SessionKey::id) **alone** remains
/// vulnerable to CWE-639: any admitted caller who learns or guesses another
/// caller's id reaches their conversation. Key on
/// [`storage_key`](SessionKey::storage_key), or on both fields together.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub struct SessionKey<'a> {
    /// The authenticated principal, when one was established.
    pub principal: Option<&'a str>,
    /// The caller-supplied `X-Session-Id`, when present.
    pub id: Option<&'a str>,
}

impl<'a> SessionKey<'a> {
    /// Construct a key.
    ///
    /// Required because the struct is `#[non_exhaustive]`, so external crates
    /// cannot build one with a struct literal. Being `#[non_exhaustive]` is what
    /// lets a future third component be added without another breaking change.
    pub fn new(principal: Option<&'a str>, id: Option<&'a str>) -> Self {
        Self { principal, id }
    }

    /// A collision-free single-string key, for providers whose backend needs one
    /// (Postgres, Redis, a filesystem path).
    ///
    /// Returns `None` for an anonymous request (`id` is `None`), which must not
    /// be stored at all.
    ///
    /// The principal is length-prefixed so that no two distinct
    /// `(principal, id)` pairs can produce the same string. A plain
    /// `format!("{principal}:{id}")` would let `("a:b", "c")` and
    /// `("a", "b:c")` collide, reintroducing the very IDOR this type exists to
    /// close.
    pub fn storage_key(&self) -> Option<String> {
        let id = self.id?;
        let principal = self.principal.unwrap_or("");
        Some(format!("{}:{}:{}", principal.len(), principal, id))
    }
}

// ---------------------------------------------------------------------------
// Public trait
// ---------------------------------------------------------------------------

/// Maps a [`SessionKey`] to a [`Session`] object.
///
/// Implementations must be cheaply cloneable (all provided by this crate wrap
/// an `Arc` internally) so that the actix state extractor can share one
/// instance across all handler tasks.
///
/// - `key.id == Some(_)` — return the existing session for that key, creating
///   one on the first call. Two calls with an equal key must return `Arc`s that
///   are pointer-equal (`Arc::ptr_eq`).
/// - `key.id == None` — return a fresh, anonymous session that is *not* stored
///   and is never pointer-equal to any other session.
///
/// # Security — key on the principal, not just the id
///
/// `key.id` comes straight from the request's `X-Session-Id` header, so it is
/// attacker-chosen. **A provider that uses it as its sole lookup key lets any
/// admitted caller who learns or guesses another caller's id read and append to
/// that conversation (CWE-639).** Use
/// [`SessionKey::storage_key`](SessionKey::storage_key), which combines both
/// components unambiguously.
#[async_trait]
pub trait SessionProvider: Send + Sync {
    /// Look up or create the session for `key`.
    async fn session(&self, key: SessionKey<'_>) -> Result<Arc<dyn Session>, ServerError>;
}

/// Owned form of the compound key: `(principal, id)`.
///
/// A tuple, deliberately. Concatenating the two components into one string
/// would let `("a:b", "c")` and `("a", "b:c")` collide; a tuple has no encoding
/// to get wrong.
type OwnedKey = (Option<String>, String);

// ---------------------------------------------------------------------------
// InMemorySessionProvider
// ---------------------------------------------------------------------------

/// A bounded, FIFO in-memory [`SessionProvider`] backed by
/// [`MemorySession`].
///
/// When the number of tracked sessions exceeds `max_sessions` the oldest
/// session (by insertion order) is evicted.  Anonymous sessions
/// (`key.id == None`) are never stored and never count toward the limit.
///
/// # Security
///
/// This provider keys on the full [`SessionKey`] — the authenticated principal
/// *and* the caller-supplied `X-Session-Id` — so two principals presenting the
/// same id resolve to two different sessions.
///
/// **Known limitation.** `max_sessions` is a single global FIFO bound, so one
/// principal creating `max_sessions` distinct ids evicts every other
/// principal's session, silently resetting their conversations. This is a
/// cross-tenant availability concern, not a disclosure one — the compound key
/// still prevents any caller from *reading* another's session.
pub struct InMemorySessionProvider {
    max_sessions: usize,
    /// Guards both `map` and `order` together so eviction and insertion are
    /// atomic.
    inner: RwLock<InMemoryInner>,
}

struct InMemoryInner {
    map: HashMap<OwnedKey, Arc<dyn Session>>,
    order: VecDeque<OwnedKey>,
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
    async fn session(&self, key: SessionKey<'_>) -> Result<Arc<dyn Session>, ServerError> {
        let Some(id) = key.id else {
            // Anonymous: fresh session, never stored, regardless of principal.
            return Ok(Arc::new(MemorySession::new()) as Arc<dyn Session>);
        };
        let owned: OwnedKey = (key.principal.map(str::to_owned), id.to_owned());

        // Fast path: read lock.
        {
            let inner = self.inner.read().await;
            if let Some(arc) = inner.map.get(&owned) {
                return Ok(Arc::clone(arc));
            }
        }

        // Slow path: write lock — insert and possibly evict.
        let mut inner = self.inner.write().await;

        // Double-check in case another writer raced us.
        if let Some(arc) = inner.map.get(&owned) {
            return Ok(Arc::clone(arc));
        }

        let session: Arc<dyn Session> = Arc::new(MemorySession::new());
        inner.map.insert(owned.clone(), Arc::clone(&session));
        inner.order.push_back(owned);

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
// SessionLocks (pub(crate) — used by the transport handlers)
// ---------------------------------------------------------------------------

/// Per-session run serialisation locks.
///
/// Ensures that at most one request runs at a time for a given session key.
/// Anonymous requests (`key.id == None`) get a fresh throwaway lock each time.
///
/// **Bounded growth.** Each [`lock_for`](SessionLocks::lock_for) call
/// opportunistically prunes entries that are held *only* by the map
/// (`Arc::strong_count == 1`, i.e. no in-flight run is holding the lock), so the
/// map stays bounded by the number of concurrently-active sessions rather than
/// by the number of distinct session ids observed over the server's lifetime.
pub(crate) struct SessionLocks {
    map: std::sync::Mutex<HashMap<OwnedKey, Arc<tokio::sync::Mutex<()>>>>,
}

impl SessionLocks {
    /// Create an empty lock map.
    pub(crate) fn new() -> Self {
        Self {
            map: std::sync::Mutex::new(HashMap::new()),
        }
    }

    /// Return the per-session lock for `key`.
    ///
    /// Keyed on the SAME compound identity as the session store, and not
    /// optionally so: if the lock map kept keying on the bare id, two principals
    /// using one id would serialise against each other — a cross-tenant stall
    /// and a timing oracle on the other principal's traffic.
    ///
    /// - `key.id == Some(_)` — return the shared lock for that key, creating it
    ///   on the first call.  Two calls with an equal key (while at least one
    ///   caller still holds the returned `Arc`) return pointer-equal `Arc`s.
    /// - `key.id == None` — return a fresh throwaway lock that is not shared
    ///   with any other call.
    ///
    /// Before resolving `key`, every entry whose lock is no longer held by any
    /// active run (`Arc::strong_count == 1`) is pruned, keeping the map bounded.
    pub(crate) fn lock_for(&self, key: SessionKey<'_>) -> Arc<tokio::sync::Mutex<()>> {
        let Some(id) = key.id else {
            return Arc::new(tokio::sync::Mutex::new(()));
        };
        let owned: OwnedKey = (key.principal.map(str::to_owned), id.to_owned());

        let mut map = self.map.lock().expect("SessionLocks mutex poisoned");
        // Opportunistic cleanup: drop entries held only by the map (no active
        // run is keeping the lock alive). An entry for `key` that is currently
        // in use (count > 1) is preserved, so concurrent same-key requests keep
        // serialising on a pointer-equal lock.
        map.retain(|_, lock| Arc::strong_count(lock) > 1);
        Arc::clone(
            map.entry(owned)
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

    /// Session affinity within one principal: the same key resolves to the same
    /// session object.
    #[tokio::test]
    async fn same_key_returns_same_session() {
        let p = InMemorySessionProvider::new(16);
        let a = p.session(SessionKey::new(None, Some("s1"))).await.unwrap();
        let b = p.session(SessionKey::new(None, Some("s1"))).await.unwrap();
        assert!(Arc::ptr_eq(&a, &b));
    }

    /// The IDOR itself: one id, two principals, two different sessions.
    #[tokio::test]
    async fn same_id_different_principals_are_isolated() {
        let p = InMemorySessionProvider::new(16);
        let alice = p
            .session(SessionKey::new(Some("alice"), Some("s1")))
            .await
            .unwrap();
        let mallory = p
            .session(SessionKey::new(Some("mallory"), Some("s1")))
            .await
            .unwrap();
        assert!(!Arc::ptr_eq(&alice, &mallory));

        // Positive control: the affinity guarantee still holds within a principal.
        let alice_again = p
            .session(SessionKey::new(Some("alice"), Some("s1")))
            .await
            .unwrap();
        assert!(Arc::ptr_eq(&alice, &alice_again));
    }

    /// A principal-less caller must not reach a named principal's session under
    /// the same id either — `None` is its own namespace, not a wildcard.
    #[tokio::test]
    async fn absent_principal_is_its_own_namespace() {
        let p = InMemorySessionProvider::new(16);
        let anon = p.session(SessionKey::new(None, Some("s1"))).await.unwrap();
        let alice = p
            .session(SessionKey::new(Some("alice"), Some("s1")))
            .await
            .unwrap();
        assert!(!Arc::ptr_eq(&anon, &alice));

        // …and the empty-string principal is distinct from an absent one, so a
        // caller cannot spoof the principal-less namespace with `""`.
        let empty = p
            .session(SessionKey::new(Some(""), Some("s1")))
            .await
            .unwrap();
        assert!(!Arc::ptr_eq(&anon, &empty));
    }

    /// A naive `"{principal}:{id}"` key would make these two collide. A tuple
    /// key cannot, and neither can the length-prefixed `storage_key`.
    #[tokio::test]
    async fn concatenation_collision_is_impossible() {
        let p = InMemorySessionProvider::new(16);
        let a = p
            .session(SessionKey::new(Some("a:b"), Some("c")))
            .await
            .unwrap();
        let b = p
            .session(SessionKey::new(Some("a"), Some("b:c")))
            .await
            .unwrap();
        assert!(!Arc::ptr_eq(&a, &b), "keys collided");

        assert_ne!(
            SessionKey::new(Some("a:b"), Some("c")).storage_key(),
            SessionKey::new(Some("a"), Some("b:c")).storage_key(),
        );
        assert_eq!(SessionKey::new(Some("a"), None).storage_key(), None);
    }

    /// Anonymous requests are never stored and never shared, whatever the
    /// principal is.
    #[tokio::test]
    async fn anonymous_is_always_fresh() {
        let p = InMemorySessionProvider::new(16);
        let a = p
            .session(SessionKey::new(Some("alice"), None))
            .await
            .unwrap();
        let b = p
            .session(SessionKey::new(Some("alice"), None))
            .await
            .unwrap();
        assert!(!Arc::ptr_eq(&a, &b));
        assert_eq!(p.len(), 0, "anonymous sessions are never stored");
    }

    /// Eviction still respects `max_sessions` with compound keys.
    #[tokio::test]
    async fn bounded_map_evicts() {
        let p = InMemorySessionProvider::new(1);
        let _a = p
            .session(SessionKey::new(Some("alice"), Some("s1")))
            .await
            .unwrap();
        let _b = p
            .session(SessionKey::new(Some("alice"), Some("s2")))
            .await
            .unwrap();
        assert_eq!(p.len(), 1);
    }

    /// Locks must be keyed on the same compound identity, or one principal can
    /// stall another by squatting a guessed id.
    ///
    /// Both `Arc`s are held simultaneously ON PURPOSE: `lock_for` prunes entries
    /// whose `Arc::strong_count` is 1, so dropping the first before taking the
    /// second would make this assertion hold even against a buggy bare-id
    /// implementation.
    #[test]
    fn locks_are_isolated_by_principal() {
        let locks = SessionLocks::new();
        let alice = locks.lock_for(SessionKey::new(Some("alice"), Some("s1")));
        let mallory = locks.lock_for(SessionKey::new(Some("mallory"), Some("s1")));
        assert!(!Arc::ptr_eq(&alice, &mallory));

        // Positive control, also with both held.
        let alice_again = locks.lock_for(SessionKey::new(Some("alice"), Some("s1")));
        assert!(Arc::ptr_eq(&alice, &alice_again));
        drop((alice, mallory, alice_again));
    }

    /// An anonymous request never shares a lock with anything.
    #[test]
    fn session_locks_none_distinct() {
        let locks = SessionLocks::new();
        let l1 = locks.lock_for(SessionKey::new(None, None));
        let l2 = locks.lock_for(SessionKey::new(None, None));
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
            let _la = locks.lock_for(SessionKey::new(None, Some("a")));
            assert_eq!(locks.len(), 1);
        }

        // Acquiring a lock for a different id prunes the now-unheld "a" entry.
        let _lb = locks.lock_for(SessionKey::new(None, Some("b")));
        assert_eq!(locks.len(), 1); // only "b" remains; "a" was pruned

        // Sanity: a still-held entry is NOT pruned by a later call.
        let _lb_alias = locks.lock_for(SessionKey::new(None, Some("b")));
        assert!(Arc::ptr_eq(&_lb, &_lb_alias));
        assert_eq!(locks.len(), 1);
    }
}
