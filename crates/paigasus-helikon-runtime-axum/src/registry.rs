//! Run registry: in-flight and recently-completed runs, with TTL and count-cap retention.
//!
//! [`RunRegistry`] stores every run that was started by the axum server. Completed runs are
//! retained until they age out (TTL) or until the retained-run count exceeds `max_runs`
//! (FIFO-by-completion eviction). Live (non-terminal) runs are **never** evicted.

use crate::event_log::EventLog;
use std::{
    collections::{HashMap, VecDeque},
    sync::{Arc, Mutex, RwLock},
    time::{Duration, Instant},
};
use tokio::sync::OnceCell;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

// ── RunHandle ─────────────────────────────────────────────────────────────────

/// Everything the server needs to track a single run.
pub(crate) struct RunHandle {
    /// Name of the agent that owns this run.
    pub agent_name: String,
    /// Append-only, bounded event log for this run.
    pub log: Arc<EventLog>,
    /// Cancellation token — drop or call `.cancel()` to abort the run.
    pub cancel: CancellationToken,
    /// Populated on the start-error path when the agent failed to launch before emitting any events.
    pub start_error: Mutex<Option<String>>,
    /// Set once the run enters a terminal state (via [`RunRegistry::note_terminal`]).
    pub terminal_at: Mutex<Option<Instant>>,
}

// ── RegistryInner ─────────────────────────────────────────────────────────────

/// Mutable state inside [`RunRegistry`], protected by an [`RwLock`].
struct RegistryInner {
    /// All live and recently-completed runs, keyed by run id.
    runs: HashMap<Uuid, Arc<RunHandle>>,
    /// Insertion order of terminal runs (oldest → newest). Used for FIFO eviction.
    completion_order: VecDeque<Uuid>,
}

// ── RunRegistry ───────────────────────────────────────────────────────────────

/// Registry of in-flight and recently-completed runs with TTL and count-cap retention.
///
/// Always constructed behind an [`Arc`] (see [`RunRegistry::new`]).
pub(crate) struct RunRegistry {
    inner: RwLock<RegistryInner>,
    /// How long a completed run is retained after becoming terminal.
    ttl: Duration,
    /// Maximum number of *completed* runs to retain simultaneously.
    max_runs: usize,
    /// [`EventLog`] capacity for each newly-created run.
    max_events_per_run: usize,
    /// Guards [`RunRegistry::spawn_sweeper`] so at most one background task is spawned.
    sweeper_once: OnceCell<()>,
}

impl RunRegistry {
    /// Create a new registry wrapped in an [`Arc`].
    ///
    /// * `ttl` – retention window after a run becomes terminal.
    /// * `max_runs` – cap on retained completed runs; oldest-completed runs are evicted first.
    /// * `max_events_per_run` – passed to each run's [`EventLog::new`].
    pub fn new(ttl: Duration, max_runs: usize, max_events_per_run: usize) -> Arc<Self> {
        Arc::new(Self {
            inner: RwLock::new(RegistryInner {
                runs: HashMap::new(),
                completion_order: VecDeque::new(),
            }),
            ttl,
            max_runs,
            max_events_per_run,
            sweeper_once: OnceCell::new(),
        })
    }

    /// Mint a new run id, build its handle, insert it into the registry, and return both.
    ///
    /// The run starts as non-terminal. Call [`note_terminal`](RunRegistry::note_terminal) once
    /// the run ends.
    pub fn create(&self, agent_name: String, cancel: CancellationToken) -> (Uuid, Arc<RunHandle>) {
        let id = Uuid::new_v4();
        let handle = Arc::new(RunHandle {
            agent_name,
            log: Arc::new(EventLog::new(self.max_events_per_run)),
            cancel,
            start_error: Mutex::new(None),
            terminal_at: Mutex::new(None),
        });
        let mut inner = self.inner.write().expect("RunRegistry RwLock poisoned");
        inner.runs.insert(id, Arc::clone(&handle));
        (id, handle)
    }

    /// Look up a run by id. Returns `None` if it has been evicted or never existed.
    pub fn get(&self, id: Uuid) -> Option<Arc<RunHandle>> {
        let inner = self.inner.read().expect("RunRegistry RwLock poisoned");
        inner.runs.get(&id).cloned()
    }

    /// Stamp the run as terminal at `now` and record it in the completion queue.
    ///
    /// Idempotent: calling more than once for the same id is a no-op after the first call.
    /// `now` is passed explicitly so callers can inject a deterministic clock in tests.
    pub fn note_terminal(&self, id: Uuid, now: Instant) {
        // Clone the Arc so we release the map borrow before mutating completion_order.
        let handle = {
            let inner = self.inner.read().expect("RunRegistry RwLock poisoned");
            inner.runs.get(&id).cloned()
        };
        let Some(handle) = handle else { return };

        let mut t = handle
            .terminal_at
            .lock()
            .expect("terminal_at mutex poisoned");
        if t.is_none() {
            *t = Some(now);
            drop(t);
            // Now push into completion_order (requires write lock).
            let mut inner = self.inner.write().expect("RunRegistry RwLock poisoned");
            inner.completion_order.push_back(id);
        }
    }

    /// Evict stale runs in two passes.
    ///
    /// **Pass 1 – TTL:** remove every terminal run whose `terminal_at + ttl ≤ now`.
    ///
    /// **Pass 2 – count cap:** while the number of still-present *terminal* runs exceeds
    /// `max_runs`, pop the front of the completion queue and evict it (skipping ids that
    /// were already removed by pass 1 or a previous cap iteration).
    ///
    /// Live (non-terminal) runs are **never** evicted by either pass.
    ///
    /// `now` is passed explicitly so callers can inject a deterministic clock in tests.
    pub fn sweep(&self, now: Instant) {
        let mut inner = self.inner.write().expect("RunRegistry RwLock poisoned");
        let ttl = self.ttl;

        // Pass 1: TTL eviction.
        inner.runs.retain(|_id, handle| {
            let t = handle
                .terminal_at
                .lock()
                .expect("terminal_at mutex poisoned");
            match *t {
                // Keep if still within the TTL window or non-terminal.
                Some(terminal_at) => terminal_at + ttl > now,
                None => true,
            }
        });

        // Pass 2: count-cap eviction (FIFO by completion order).
        let mut terminal_count = inner
            .runs
            .values()
            .filter(|h| {
                h.terminal_at
                    .lock()
                    .expect("terminal_at mutex poisoned")
                    .is_some()
            })
            .count();

        while terminal_count > self.max_runs {
            // Pop from the front; skip ids already evicted (by pass 1 or an earlier iteration).
            let candidate = loop {
                match inner.completion_order.pop_front() {
                    None => break None,
                    Some(id) if inner.runs.contains_key(&id) => break Some(id),
                    Some(_already_gone) => continue,
                }
            };
            match candidate {
                None => break, // Safety valve: no more candidates.
                Some(id) => {
                    inner.runs.remove(&id);
                    terminal_count -= 1;
                }
            }
        }
    }

    /// Spawn a background task that calls [`sweep`](RunRegistry::sweep) every 30 seconds.
    ///
    /// At most one task is spawned per registry instance (guarded by a [`OnceCell`]).
    /// The task holds a [`Weak`] reference so it automatically exits when the registry is dropped
    /// (i.e., no [`Arc`] remainders outside the spawned task itself).
    pub fn spawn_sweeper(self: &Arc<Self>) {
        // `OnceCell::set` succeeds exactly once; subsequent calls return `Err` which we ignore.
        if self.sweeper_once.set(()).is_ok() {
            let weak = Arc::downgrade(self);
            tokio::spawn(async move {
                let mut interval = tokio::time::interval(Duration::from_secs(30));
                loop {
                    interval.tick().await;
                    match weak.upgrade() {
                        None => return, // Registry dropped; exit.
                        Some(reg) => reg.sweep(Instant::now()),
                    }
                }
            });
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// A completed run must not be evicted before its TTL expires, and must be evicted
    /// once the clock has advanced past `terminal_at + ttl`.
    #[test]
    fn ttl_evicts_after_deadline() {
        let reg = RunRegistry::new(Duration::from_secs(60), 1024, 1024);
        let (id, _h) = reg.create("a".into(), CancellationToken::new());
        let t0 = Instant::now();
        reg.note_terminal(id, t0);
        reg.sweep(t0 + Duration::from_secs(59));
        assert!(reg.get(id).is_some());
        reg.sweep(t0 + Duration::from_secs(61));
        assert!(reg.get(id).is_none());
    }

    /// When the number of completed runs exceeds `max_runs`, the oldest-completed run must be
    /// evicted first, regardless of which run finished last.
    #[test]
    fn count_cap_evicts_oldest_completed_first() {
        let reg = RunRegistry::new(Duration::from_secs(3600), 2, 1024);
        let t0 = Instant::now();
        let ids: Vec<_> = (0..3)
            .map(|i| {
                let (id, _) = reg.create("a".into(), CancellationToken::new());
                reg.note_terminal(id, t0 + Duration::from_secs(i));
                id
            })
            .collect();
        reg.sweep(t0 + Duration::from_secs(3));
        assert!(reg.get(ids[0]).is_none()); // oldest-completed evicted
        assert!(reg.get(ids[2]).is_some());
    }
}
