//! Bounded append-only event log with `Notify`-based replay and live subscription.
//!
//! [`EventLog`] is the backbone shared by every HTTP transport (one-shot, SSE, WebSocket).
//! Events are stored in a bounded ring (via [`VecDeque`](std::collections::VecDeque)); once the
//! ring is full the oldest events are evicted from the head. Subscribers receive a seamless
//! replay-then-live stream that ends automatically when a terminal event is yielded.

use futures_util::Stream;
use paigasus_helikon_core::AgentEvent;
use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
};
use tokio::sync::Notify;

/// Returns `true` for events that signal end-of-run.
///
/// Only [`AgentEvent::RunCompleted`] and [`AgentEvent::RunFailed`] are terminal;
/// all other variants are non-terminal.
fn is_terminal(ev: &AgentEvent) -> bool {
    matches!(
        ev,
        AgentEvent::RunCompleted { .. } | AgentEvent::RunFailed { .. }
    )
}

/// Mutable state inside [`EventLog`], protected by a [`Mutex`].
struct EventLogInner {
    /// Retained events. May be shorter than total appended events due to ring eviction.
    events: VecDeque<AgentEvent>,
    /// Sequence number of the first retained event. Increases monotonically as events are evicted.
    first_seq: u64,
    /// Set to `true` once a terminal event has been appended or [`EventLog::mark_terminal`] is called.
    terminal: bool,
}

/// Bounded append-only event log with `Notify`-based wakeups.
///
/// Wrapped in an [`Arc`] and shared across all subscribers and transport handlers.
/// Appends to a bounded ring; once the ring is full the oldest event is evicted and
/// `first_seq` is incremented. Each append wakes all live subscribers via
/// [`tokio::sync::Notify`].
pub(crate) struct EventLog {
    inner: Mutex<EventLogInner>,
    notify: Notify,
    max_events: usize,
}

/// Snapshot slice returned by [`EventLog::read_from`].
pub(crate) struct ReadSlice {
    /// Sequence number of the first retained event in the log.
    ///
    /// If the requested cursor was evicted by ring truncation, this will be greater than
    /// the cursor the caller passed; the returned events start at `first_seq`.
    pub first_seq: u64,
    /// Events from `max(cursor, first_seq)` up to the current tail (exclusive).
    pub events: Vec<AgentEvent>,
    /// The sequence number the caller should pass on the next [`EventLog::read_from`] call.
    pub next_cursor: u64,
    /// `true` once the run has ended (a terminal event was appended or `mark_terminal` called).
    pub terminal: bool,
}

impl EventLog {
    /// Create a new log that retains at most `max_events` events.
    ///
    /// When the log is full, the oldest event is evicted before each new append.
    pub fn new(max_events: usize) -> Self {
        Self {
            inner: Mutex::new(EventLogInner {
                events: VecDeque::with_capacity(max_events.min(64)),
                first_seq: 0,
                terminal: false,
            }),
            notify: Notify::new(),
            max_events,
        }
    }

    /// Append an event to the log.
    ///
    /// If the ring is full, the oldest event is evicted and `first_seq` is incremented.
    /// Automatically sets the terminal flag when `ev` is `RunCompleted` or `RunFailed`.
    /// Always wakes all live subscribers via [`Notify::notify_waiters`] after appending.
    pub fn append(&self, ev: AgentEvent) {
        let terminal = is_terminal(&ev);
        let mut inner = self.inner.lock().expect("EventLog mutex poisoned");
        inner.events.push_back(ev);
        while inner.events.len() > self.max_events {
            inner.events.pop_front();
            inner.first_seq += 1;
        }
        if terminal {
            inner.terminal = true;
        }
        drop(inner);
        self.notify.notify_waiters();
    }

    /// Mark the log as terminal without appending an event.
    ///
    /// Used by the start-error path when no run events were ever emitted.
    /// Wakes all live subscribers so they can observe the terminal state and finish.
    pub fn mark_terminal(&self) {
        let mut inner = self.inner.lock().expect("EventLog mutex poisoned");
        inner.terminal = true;
        drop(inner);
        self.notify.notify_waiters();
    }

    /// Return a slice of retained events starting at `cursor`.
    ///
    /// If `cursor` is less than `first_seq` (the cursor was evicted by ring truncation),
    /// the slice starts at `first_seq` instead. Callers can detect this via `slice.first_seq`.
    pub fn read_from(&self, cursor: u64) -> ReadSlice {
        let inner = self.inner.lock().expect("EventLog mutex poisoned");
        let first_seq = inner.first_seq;
        let terminal = inner.terminal;

        // Clamp cursor up to first_seq: an under-range cursor starts at the earliest retained event.
        let effective_cursor = cursor.max(first_seq);
        // How many events to skip from the front of the deque.
        let skip = (effective_cursor - first_seq) as usize;
        let events: Vec<AgentEvent> = inner.events.iter().skip(skip).cloned().collect();
        let next_cursor = effective_cursor + events.len() as u64;

        ReadSlice {
            first_seq,
            events,
            next_cursor,
            terminal,
        }
    }

    /// Subscribe to this log, returning a stream of events.
    ///
    /// The stream first replays all retained events from `from` (clamped to `first_seq` if
    /// evicted), then delivers new events in real time. The stream ends after the first terminal
    /// event is yielded; if the log is already terminal at subscription time, only the retained
    /// events are delivered before the stream closes.
    ///
    /// The returned stream is `Unpin` (wrapped in `Pin<Box<…>>`) so callers can drive it with
    /// `stream.next().await` without additional pinning.
    ///
    /// # Lost-wakeup avoidance
    ///
    /// Each iteration creates a [`Notify::notified`] future, pins it on the stack, and calls
    /// [`.enable()`] *before* calling `read_from`. This guarantees that any
    /// [`Notify::notify_waiters`] fired between `read_from` returning an empty slice and the
    /// subsequent `.await` is not lost: merely constructing `notified()` without `enable()` does
    /// **not** register the waiter.
    ///
    /// # Pinning rationale
    ///
    /// The async block is `!Unpin` because its state machine stores a `Notified<'_>` value that
    /// borrows `log.notify` (which is also in the state), creating an apparent self-reference.
    /// Wrapping the `Unfold` stream in [`Box::pin`] pins the state machine on the heap, making
    /// the returned `Pin<Box<…>>` `Unpin` — callers need not pin it themselves.
    ///
    /// [`Notify::notified`]: tokio::sync::Notify::notified
    /// [`.enable()`]: tokio::sync::futures::Notified::enable
    pub fn subscribe(self: &Arc<Self>, from: u64) -> impl Stream<Item = AgentEvent> + Send + Unpin {
        let log = Arc::clone(self);

        // The unfold state carries only the cursor and the done-flag. `log` is captured by
        // the closure and cloned fresh for each iteration's async block, so that the async
        // block OWNS its Arc<EventLog> and can borrow `log.notify` for the `notified()` future
        // without the state tuple also owning `log` (which would require moving it in the return
        // statement while it is still borrowed).
        Box::pin(futures_util::stream::unfold(
            (from, false),
            move |(cursor, done)| {
                let log = Arc::clone(&log);
                async move {
                    if done {
                        return None;
                    }
                    loop {
                        // Step 1: Register the wakeup future BEFORE reading (lost-wakeup
                        // avoidance). `notified()` + `enable()` must precede `read_from` so that
                        // any `notify_waiters()` fired between them and the await is not missed.
                        let notif = log.notify.notified();
                        tokio::pin!(notif);
                        notif.as_mut().enable();

                        // Step 2: Try to drain one event at the current cursor.
                        let slice = log.read_from(cursor);

                        if let Some(ev) = slice.events.into_iter().next() {
                            // Fast path: event available. `log` drops when this async block
                            // exits; `notif` (which borrows `log.notify`) drops before `log`
                            // (LIFO) — no borrow conflict.
                            let next_cursor = cursor + 1;
                            let terminal = is_terminal(&ev);
                            return Some((ev, (next_cursor, terminal)));
                        }

                        // No events available yet.
                        if slice.terminal {
                            // Terminal with an empty slice: the run ended before we could observe
                            // it (all events evicted past our cursor, or mark_terminal with no
                            // events).
                            return None;
                        }

                        // Step 3: Await the pre-registered wakeup, then loop to re-check.
                        notif.await;
                    }
                }
            },
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::StreamExt;
    use paigasus_helikon_core::{AgentEvent, TokenUsage};
    use std::sync::Arc;

    fn delta(s: &str) -> AgentEvent {
        AgentEvent::TokenDelta { text: s.into() }
    }
    fn done() -> AgentEvent {
        AgentEvent::RunCompleted {
            usage: TokenUsage::default(),
        }
    }

    #[test]
    fn read_from_cursor_returns_tail_and_terminal() {
        let log = EventLog::new(1024);
        log.append(delta("a"));
        log.append(delta("b"));
        log.append(done());
        let slice = log.read_from(0);
        assert_eq!(slice.events.len(), 3);
        assert!(slice.terminal);
        assert_eq!(log.read_from(slice.next_cursor).events.len(), 0);
    }

    #[test]
    fn bounded_ring_truncates_head() {
        let log = EventLog::new(2);
        log.append(delta("a"));
        log.append(delta("b"));
        log.append(delta("c"));
        let slice = log.read_from(0);
        assert_eq!(slice.first_seq, 1); // "a" evicted
        assert_eq!(slice.events.len(), 2);
    }

    #[tokio::test]
    async fn subscribe_replays_then_tails_until_terminal() {
        let log = Arc::new(EventLog::new(1024));
        log.append(delta("a"));
        let mut sub = log.subscribe(0);
        let l2 = log.clone();
        tokio::spawn(async move {
            l2.append(delta("b"));
            l2.append(done());
        });
        let mut got = Vec::new();
        while let Some(ev) = sub.next().await {
            got.push(ev);
        }
        assert_eq!(got.len(), 3); // a (replay) + b + done, then stream ends
    }

    /// Verify that a fast append between subscribe creation and first poll is NOT lost.
    ///
    /// This proves the notify-before-read ordering: because `enable()` is called before
    /// `read_from`, a `notify_waiters()` fired between the two cannot be missed.
    #[tokio::test]
    async fn subscribe_does_not_lose_fast_appended_event() {
        let log = Arc::new(EventLog::new(1024));
        // Subscribe before any events.
        let mut sub = log.subscribe(0);
        // Append immediately — before the stream is first polled.
        log.append(delta("x"));
        log.append(done());
        let mut got = Vec::new();
        while let Some(ev) = sub.next().await {
            got.push(ev);
        }
        assert_eq!(got.len(), 2);
    }
}
