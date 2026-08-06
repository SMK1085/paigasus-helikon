//! Run registry: in-flight and recently-completed runs, with TTL and count-cap retention.
//!
//! [`RunRegistry`] stores every run that was started by the actix server. Completed runs are
//! retained until they age out (TTL) or until the retained-run count exceeds `max_runs`
//! (FIFO-by-completion eviction). Live (non-terminal) runs are **never** evicted.

use crate::error::ServerError;
use crate::event_log::EventLog;
use paigasus_helikon_core::AgentEvent;
use std::{
    collections::{HashMap, VecDeque},
    sync::{Arc, Mutex, Once, RwLock},
    time::{Duration, Instant},
};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

// ── RunHandle ─────────────────────────────────────────────────────────────────

/// Everything the server needs to track a single run.
pub(crate) struct RunHandle {
    /// Name of the agent that owns this run.
    pub agent_name: String,
    /// Principal that started this run; `None` for an unbound run.
    ///
    /// The WebSocket events endpoint compares against this so a run's stream is
    /// readable only by its owner.
    pub principal: Option<String>,
    /// Append-only, bounded event log for this run.
    pub log: Arc<EventLog>,
    /// Cancellation token — drop or call `.cancel()` to abort the run.
    pub cancel: CancellationToken,
    /// Populated on the start-error path when the agent failed to launch before emitting any events.
    pub start_error: Mutex<Option<String>>,
    /// Set once the run enters a terminal state (via [`RunRegistry::note_terminal`]).
    pub terminal_at: Mutex<Option<Instant>>,
    /// When the run was created. Used by the sweeper to reclaim a run that never
    /// reaches a terminal state.
    pub created_at: Instant,
}

/// Public `error` text for a run that failed before emitting any event.
///
/// The detailed cause is logged once by the writer task; putting it in the
/// frame would leak it to every SSE and WebSocket subscriber (CWE-209).
const PUBLIC_RUN_FAILED_TO_START: &str = "run failed to start";

/// Public `error` text for a stream that ended without a terminal event.
const PUBLIC_RUN_NO_TERMINAL: &str = "run ended before producing a terminal event";

impl RunHandle {
    /// The synthetic terminal frame a streaming transport must emit when its
    /// subscribe stream ended without delivering a real `RunCompleted`/`RunFailed`.
    ///
    /// Returns `None` when a real terminal was already delivered (`saw_terminal`
    /// is `true`). Otherwise returns an [`AgentEvent::RunFailed`], carrying
    /// [`PUBLIC_RUN_FAILED_TO_START`] if the run failed to start, or
    /// [`PUBLIC_RUN_NO_TERMINAL`] otherwise (e.g. a stream that panicked or
    /// ended mid-run before any terminal event). Both are fixed public strings —
    /// the detailed cause never reaches this frame (CWE-209).
    pub(crate) fn synthetic_terminal_frame(&self, saw_terminal: bool) -> Option<AgentEvent> {
        if saw_terminal {
            return None;
        }
        let failed_to_start = self
            .start_error
            .lock()
            .expect("start_error mutex poisoned")
            .is_some();
        let error = if failed_to_start {
            PUBLIC_RUN_FAILED_TO_START
        } else {
            PUBLIC_RUN_NO_TERMINAL
        };
        // Note this logs the PUBLIC string, not the detail. The detail is logged
        // once by the writer task; this method runs once per subscriber, so
        // logging it here would duplicate it per subscriber and skip it entirely
        // for an unwatched run.
        tracing::warn!(
            agent = %self.agent_name,
            %error,
            "run ended without a real terminal event; synthesizing a RunFailed frame for the stream subscriber"
        );
        Some(AgentEvent::RunFailed {
            error: error.to_owned(),
        })
    }
}

// ── RegistryInner ─────────────────────────────────────────────────────────────

/// Mutable state inside [`RunRegistry`], protected by an [`RwLock`].
struct RegistryInner {
    /// All live and recently-completed runs, keyed by run id.
    runs: HashMap<Uuid, Arc<RunHandle>>,
    /// Insertion order of terminal runs (oldest → newest). Used for FIFO eviction.
    completion_order: VecDeque<Uuid>,
    /// Count of entries in `runs` whose `terminal_at` is `None`.
    ///
    /// Maintained rather than recomputed. Every mutation happens under the one
    /// `inner` write lock (`create` +1, `note_terminal` −1, `sweep` pass 0 −1)
    /// and `sweep` never removes a non-terminal run, so it cannot drift from the
    /// map. Scanning instead would hold the write lock while taking up to
    /// `max_runs + max_in_flight` mutexes, serialising against every concurrent
    /// `get`.
    live: usize,
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
    /// Maximum number of simultaneously in-flight (non-terminal) runs.
    max_in_flight: usize,
    /// Maximum wall-clock lifetime of a single run before the sweeper reclaims it.
    max_run_duration: Duration,
    /// Guards [`RunRegistry::spawn_sweeper`] so at most one background task is
    /// spawned, no matter how many actix workers call `configure()`.
    sweeper: Once,
}

impl RunRegistry {
    /// Create a new registry wrapped in an [`Arc`].
    ///
    /// * `ttl` – retention window after a run becomes terminal.
    /// * `max_runs` – cap on retained completed runs; oldest-completed runs are evicted first.
    /// * `max_events_per_run` – passed to each run's [`EventLog::new`].
    /// * `max_in_flight` – cap on simultaneously non-terminal runs; further
    ///   admission is refused once reached.
    /// * `max_run_duration` – wall-clock lifetime after which the sweeper
    ///   cancels and reclaims a run that never reached a terminal state.
    pub fn new(
        ttl: Duration,
        max_runs: usize,
        max_events_per_run: usize,
        max_in_flight: usize,
        max_run_duration: Duration,
    ) -> Arc<Self> {
        Arc::new(Self {
            inner: RwLock::new(RegistryInner {
                runs: HashMap::new(),
                completion_order: VecDeque::new(),
                live: 0,
            }),
            ttl,
            max_runs,
            max_events_per_run,
            max_in_flight,
            max_run_duration,
            sweeper: Once::new(),
        })
    }

    /// Mint a new run id, build its handle, insert it into the registry, and
    /// return both.
    ///
    /// `principal` is the identity that started the run; the events endpoint
    /// uses it to scope subscriptions.
    ///
    /// The run starts as non-terminal. Call [`note_terminal`](RunRegistry::note_terminal) once
    /// the run ends.
    ///
    /// # Errors
    ///
    /// [`ServerError::Unavailable`] when admitting the run would exceed
    /// `max_in_flight`. The check and the insert share one critical section, so
    /// there is no window in which two callers both see room for the last slot.
    pub fn create(
        &self,
        agent_name: String,
        principal: Option<String>,
        cancel: CancellationToken,
    ) -> Result<(Uuid, Arc<RunHandle>), ServerError> {
        let mut inner = self.inner.write().expect("RunRegistry RwLock poisoned");
        if inner.live >= self.max_in_flight {
            // The only server-side signal that the cap is biting; the caller's
            // 503 body is redacted.
            tracing::warn!(
                live = inner.live,
                cap = self.max_in_flight,
                "rejecting run: in-flight limit reached"
            );
            return Err(ServerError::Unavailable(
                "in-flight run limit reached".to_owned(),
            ));
        }
        let id = Uuid::new_v4();
        let handle = Arc::new(RunHandle {
            agent_name,
            principal,
            created_at: Instant::now(),
            log: Arc::new(EventLog::new(self.max_events_per_run)),
            cancel,
            start_error: Mutex::new(None),
            terminal_at: Mutex::new(None),
        });
        inner.runs.insert(id, Arc::clone(&handle));
        inner.live += 1;
        Ok((id, handle))
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
        // Stamp `terminal_at` and enqueue into `completion_order` in ONE critical
        // section so a concurrent `sweep` cannot observe a half-applied state
        // (stamped-but-not-enqueued, or vice versa). Lock order is `inner` →
        // `terminal_at`, matching `sweep`, so the two never deadlock.
        let mut inner = self.inner.write().expect("RunRegistry RwLock poisoned");
        let Some(handle) = inner.runs.get(&id).cloned() else {
            return;
        };
        let mut t = handle
            .terminal_at
            .lock()
            .expect("terminal_at mutex poisoned");
        if t.is_none() {
            *t = Some(now);
            drop(t);
            inner.completion_order.push_back(id);
            // `saturating_sub`, not `-=`: the three `live`-mutation sites (here,
            // `create`, and `sweep` pass 0) are proven not to drift today, but a
            // future fourth stamp site or a bug would otherwise wrap a `usize`
            // underflow to `usize::MAX` in release, permanently wedging the
            // admission check (`live >= max_in_flight` becomes always-true) with
            // no log and no recovery short of a restart. `debug_assert!` still
            // fails loudly in tests/debug builds so drift is caught, not masked.
            debug_assert!(inner.live > 0, "live run count underflow in note_terminal");
            inner.live = inner.live.saturating_sub(1);
        }
    }

    /// Evict stale runs in three passes.
    ///
    /// **Pass 0 – reclamation:** cancel and stamp terminal every non-terminal run
    /// whose `created_at + max_run_duration ≤ now`. This is what makes the
    /// in-flight cap (`max_in_flight`) safe: without it, a run that never
    /// terminates (`?mode=async` attaches no cancel guard, and
    /// `RunConfig::default().timeout` is `None`) would hold its slot for the
    /// process lifetime, and the cap would become a permanent-outage vector.
    ///
    /// **Pass 1 – TTL:** remove every terminal run whose `terminal_at + ttl ≤ now`.
    ///
    /// **Pass 2 – count cap:** while the number of still-present *terminal* runs exceeds
    /// `max_runs`, pop the front of the completion queue and evict it (skipping ids that
    /// were already removed by pass 1 or a previous cap iteration).
    ///
    /// Live (non-terminal, not-yet-overdue) runs are **never** evicted by pass 1
    /// or pass 2.
    ///
    /// `now` is passed explicitly so callers can inject a deterministic clock in tests.
    pub fn sweep(&self, now: Instant) {
        let mut inner = self.inner.write().expect("RunRegistry RwLock poisoned");
        let ttl = self.ttl;

        // Pass 0: reclaim runs that never terminated. Without this the in-flight
        // cap is a permanent-outage vector — `?mode=async` attaches no cancel
        // guard and `RunConfig::default().timeout` is `None`, so a wedged run
        // would hold its slot for the process lifetime.
        let overdue: Vec<Uuid> = inner
            .runs
            .iter()
            .filter(|(_, h)| {
                h.terminal_at
                    .lock()
                    .expect("terminal_at mutex poisoned")
                    .is_none()
                    && h.created_at
                        .checked_add(self.max_run_duration)
                        .is_some_and(|deadline| deadline <= now)
            })
            .map(|(id, _)| *id)
            .collect();
        for id in overdue {
            let Some(handle) = inner.runs.get(&id).cloned() else {
                continue;
            };
            handle.cancel.cancel();
            let mut t = handle
                .terminal_at
                .lock()
                .expect("terminal_at mutex poisoned");
            if t.is_none() {
                *t = Some(now);
                drop(t);
                inner.completion_order.push_back(id);
                // See the matching comment in `note_terminal`: saturating, plus a
                // debug-only assert, rather than a bare `-=` that could wrap.
                debug_assert!(inner.live > 0, "live run count underflow in sweep pass 0");
                inner.live = inner.live.saturating_sub(1);
                tracing::warn!(%id, agent = %handle.agent_name,
                               "reclaiming run that exceeded max_run_duration");
            }
        }

        // Pass 1: TTL eviction. Track evicted ids so we can also clean them from
        // `completion_order`, preventing unbounded deque growth across long uptimes.
        let mut evicted: std::collections::HashSet<Uuid> = std::collections::HashSet::new();
        inner.runs.retain(|id, handle| {
            let t = handle
                .terminal_at
                .lock()
                .expect("terminal_at mutex poisoned");
            match *t {
                // Keep if still within the TTL window or non-terminal. `checked_add`
                // rather than `+`: an overflowing deadline would panic here while the
                // write lock is held, poisoning the `RwLock` for every later caller.
                // An un-representable deadline is infinitely far away, so keep the run.
                Some(terminal_at)
                    if terminal_at
                        .checked_add(ttl)
                        .is_some_and(|deadline| deadline <= now) =>
                {
                    evicted.insert(*id);
                    false
                }
                _ => true,
            }
        });
        // Remove TTL-evicted ids from the completion queue to prevent memory leaks.
        if !evicted.is_empty() {
            inner.completion_order.retain(|id| !evicted.contains(id));
        }

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
    /// At most one task is spawned per registry instance (guarded by a
    /// [`Once`]). `configure()` runs once per actix worker, so this is called
    /// once per worker; the [`Once`] collapses those to a single sweeper. The
    /// task is spawned onto the process-wide runtime via `handle`, not onto the
    /// per-worker `actix-rt` runtime. It holds only a [`Weak`](std::sync::Weak)
    /// reference, so on its next tick after the registry is dropped it observes a
    /// failed upgrade and exits (no strong [`Arc`] outlives the registry itself).
    pub fn spawn_sweeper(self: &Arc<Self>, handle: &tokio::runtime::Handle) {
        let weak = Arc::downgrade(self);
        let handle = handle.clone();
        self.sweeper.call_once(move || {
            handle.spawn(async move {
                let mut interval = tokio::time::interval(Duration::from_secs(30));
                loop {
                    interval.tick().await;
                    match weak.upgrade() {
                        None => return, // Registry dropped; exit.
                        Some(reg) => reg.sweep(Instant::now()),
                    }
                }
            });
        });
    }
}

// ── Test helpers ──────────────────────────────────────────────────────────────

#[cfg(test)]
impl RunRegistry {
    /// Returns the current length of `completion_order` for leak-regression tests.
    fn completion_queue_len(&self) -> usize {
        self.inner.read().unwrap().completion_order.len()
    }

    /// True once [`spawn_sweeper`](RunRegistry::spawn_sweeper) has actually
    /// spawned its background task.
    ///
    /// `Once::is_completed` only returns `true` once its `call_once` closure
    /// has *returned* — i.e. after `handle.spawn(...)` inside it has already
    /// run — and a panicking closure poisons the `Once` rather than
    /// completing it, so this can't observe a completed-but-unspawned state:
    /// `spawn_sweeper`'s closure is the single unconditional
    /// `handle.spawn(...)` statement, no branch inside it can finish without
    /// spawning.
    ///
    /// `pub(crate)`, not private, so `server.rs`'s tests can use it to prove
    /// [`AgentServer::configure`](crate::server::AgentServer::configure)
    /// spawns the sweeper without needing to wait for its 30-second tick.
    pub(crate) fn sweeper_is_spawned(&self) -> bool {
        self.sweeper.is_completed()
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
        let reg = RunRegistry::new(
            Duration::from_secs(60),
            1024,
            1024,
            1024,
            Duration::from_secs(3600),
        );
        let (id, _h) = reg
            .create("a".into(), None, CancellationToken::new())
            .unwrap();
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
        let reg = RunRegistry::new(
            Duration::from_secs(3600),
            2,
            1024,
            1024,
            Duration::from_secs(3600),
        );
        let t0 = Instant::now();
        let ids: Vec<_> = (0..3)
            .map(|i| {
                let (id, _) = reg
                    .create("a".into(), None, CancellationToken::new())
                    .unwrap();
                reg.note_terminal(id, t0 + Duration::from_secs(i));
                id
            })
            .collect();
        reg.sweep(t0 + Duration::from_secs(3));
        assert!(reg.get(ids[0]).is_none()); // oldest-completed evicted
        assert!(reg.get(ids[1]).is_some()); // middle run survives (exactly one evicted)
        assert!(reg.get(ids[2]).is_some());
    }

    /// TTL eviction must also remove the evicted ids from `completion_order` so that the
    /// deque does not grow without bound across long server uptimes (regression for the
    /// completion-queue leak found in review).
    #[test]
    fn ttl_eviction_cleans_completion_queue() {
        let reg = RunRegistry::new(
            Duration::from_secs(60),
            1024,
            1024,
            1024,
            Duration::from_secs(3600),
        );
        let t0 = Instant::now();

        // Create and terminate three runs.
        for _ in 0..3 {
            let (id, _h) = reg
                .create("a".into(), None, CancellationToken::new())
                .unwrap();
            reg.note_terminal(id, t0);
        }
        assert_eq!(reg.completion_queue_len(), 3);

        // Sweep before TTL — nothing evicted, queue unchanged.
        reg.sweep(t0 + Duration::from_secs(59));
        assert_eq!(reg.completion_queue_len(), 3);

        // Sweep past TTL — all three runs evicted and queue must be empty.
        reg.sweep(t0 + Duration::from_secs(61));
        assert_eq!(reg.completion_queue_len(), 0);
    }

    /// `synthetic_terminal_frame` returns `None` once a real terminal was seen,
    /// and otherwise one of two fixed public strings — never the captured
    /// `start_error` detail, which stays confined to the log (CWE-209).
    #[test]
    fn synthetic_terminal_frame_branches() {
        let reg = RunRegistry::new(
            Duration::from_secs(60),
            16,
            16,
            16,
            Duration::from_secs(3600),
        );
        let (_id, h) = reg
            .create("a".into(), None, CancellationToken::new())
            .unwrap();

        assert!(h.synthetic_terminal_frame(true).is_none());

        match h.synthetic_terminal_frame(false) {
            Some(AgentEvent::RunFailed { error }) => {
                assert_eq!(error, "run ended before producing a terminal event");
            }
            other => panic!("expected generic RunFailed, got {other:?}"),
        }

        *h.start_error.lock().unwrap() = Some("boom".to_owned());
        match h.synthetic_terminal_frame(false) {
            // Redacted: the detail lives in the log, not the frame.
            Some(AgentEvent::RunFailed { error }) => {
                assert_eq!(error, "run failed to start");
                assert!(!error.contains("boom"));
            }
            other => panic!("expected redacted RunFailed, got {other:?}"),
        }
    }

    /// The cap admits exactly `max_in_flight` live runs and refuses the next.
    #[test]
    fn cap_admits_then_rejects() {
        let reg = RunRegistry::new(
            Duration::from_secs(60),
            1024,
            1024,
            2,
            Duration::from_secs(3600),
        );
        let (_a, _ha) = reg
            .create("a".into(), None, CancellationToken::new())
            .unwrap();
        let (_b, _hb) = reg
            .create("a".into(), None, CancellationToken::new())
            .unwrap();
        assert!(reg
            .create("a".into(), None, CancellationToken::new())
            .is_err());
    }

    /// A terminal run frees its slot; a terminal-but-RETAINED run must not keep
    /// consuming one — that distinction is the entire point of the fix.
    #[test]
    fn terminal_runs_do_not_consume_slots() {
        let reg = RunRegistry::new(
            Duration::from_secs(3600),
            1024,
            1024,
            1,
            Duration::from_secs(3600),
        );
        let (id, _h) = reg
            .create("a".into(), None, CancellationToken::new())
            .unwrap();
        assert!(reg
            .create("a".into(), None, CancellationToken::new())
            .is_err());

        reg.note_terminal(id, Instant::now());
        // Still retained (TTL is an hour), but no longer in flight.
        assert!(reg.get(id).is_some());
        assert!(reg
            .create("a".into(), None, CancellationToken::new())
            .is_ok());
    }

    /// A run that never terminates is reclaimed once it exceeds
    /// `max_run_duration`, and its slot is reusable.
    #[test]
    fn sweep_reclaims_overdue_runs() {
        let reg = RunRegistry::new(
            Duration::from_secs(3600),
            1024,
            1024,
            1,
            Duration::from_secs(60),
        );
        let t0 = Instant::now();
        let (_id, handle) = reg
            .create("a".into(), None, CancellationToken::new())
            .unwrap();
        assert!(reg
            .create("a".into(), None, CancellationToken::new())
            .is_err());

        reg.sweep(t0 + Duration::from_secs(59));
        assert!(reg
            .create("a".into(), None, CancellationToken::new())
            .is_err());

        reg.sweep(t0 + Duration::from_secs(61));
        assert!(
            handle.cancel.is_cancelled(),
            "overdue run must be cancelled"
        );
        assert!(reg
            .create("a".into(), None, CancellationToken::new())
            .is_ok());
    }
}
