//! [`TaskStore`] — the persistence seam behind A2A's `tasks/*` methods.
//!
//! # Why this is a trait
//!
//! AgentCore terminates a container abruptly, and the default [`InMemoryTaskStore`]
//! loses every task with the microVM. A2A's `tasks/get` and `tasks/resubscribe` exist
//! precisely so a client can come back to a task after a disconnect, which is only
//! meaningful across container lifetimes if the tasks outlive one. This trait is that
//! seam: implement it over a real database and pass it to
//! [`AgentCoreServerBuilder::task_store`](crate::AgentCoreServerBuilder::task_store).
//!
//! # The `subscribe` contract
//!
//! `subscribe(id, from)` replays the backlog from `from` (inclusive) and then tails
//! live appends, ending when the task reaches a terminal state — **with no gap at the
//! seam**. An event appended between the backlog read and the start of the wait must
//! still be delivered.
//!
//! Closing that window is a matter of ordering, and it is the one thing a future edit
//! is likely to get wrong. Each poll must, in this exact order:
//!
//! 1. create the [`Notified`](tokio::sync::Notify) future and **enable** it;
//! 2. lock and read the events at or after the cursor, plus the terminal flag;
//! 3. yield any events found, advancing the cursor;
//! 4. otherwise, if terminal, end the stream;
//! 5. otherwise await the already-enabled `Notified` and loop.
//!
//! Enabling before the read is what makes step 5 safe: `Notify` records a permit for an
//! enabled future, so a `notify_waiters` that fires between steps 2 and 5 is remembered
//! rather than missed. Reading first and enabling afterwards reintroduces exactly the
//! lost wakeup that `subscribe_does_not_lose_a_fast_appended_event` guards.

use std::{
    collections::{HashMap, VecDeque},
    sync::Arc,
};

use async_trait::async_trait;
use futures_util::stream::{self, BoxStream, StreamExt as _};
use tokio::sync::{Mutex, Notify};

use crate::{
    a2a::types::{now_rfc3339, Task, TaskEvent, TaskState},
    error::AgentCoreError,
};

/// Maximum events retained per task by [`InMemoryTaskStore`].
///
/// Task-count eviction alone does not bound memory: one long streaming run appends an
/// event per token and would grow without limit inside a single retained task. Once a
/// task exceeds this, its oldest events are dropped and a `subscribe` cursor pointing
/// into the dropped range clamps forward to the oldest retained event.
pub const MAX_EVENTS_PER_TASK: usize = 512;

/// Storage for A2A tasks and their event logs.
///
/// Implementations must be safe to share across concurrent requests.
///
/// The subtle requirement is [`subscribe`](TaskStore::subscribe)'s: it replays a
/// backlog and then tails live appends, and an event appended between those two phases
/// must still be delivered. An implementation registers its wakeup *before* reading the
/// backlog, never after — reading first leaves a window in which an append fires a
/// notification nobody is waiting on yet, and that event is then lost until the next
/// one happens to arrive.
#[async_trait]
pub trait TaskStore: Send + Sync {
    /// Insert a newly-created task.
    ///
    /// # Errors
    ///
    /// Returns [`AgentCoreError::Internal`] if the backing store fails.
    async fn create(&self, task: Task) -> Result<(), AgentCoreError>;

    /// Fetch a task by id, or `Ok(None)` when no such task exists.
    ///
    /// An unknown id is *not* an error here — `tasks/get` turns the `None` into the A2A
    /// `TaskNotFoundError` itself, and a durable store legitimately returns `None` for a
    /// task that has aged out.
    ///
    /// # Errors
    ///
    /// Returns [`AgentCoreError::Internal`] if the backing store fails.
    async fn get(&self, id: &str) -> Result<Option<Task>, AgentCoreError>;

    /// Atomically move a task from `expected` to `next`.
    ///
    /// Returns `Ok(true)` when the swap happened and `Ok(false)` when the task's current
    /// state was not `expected` — the caller lost a race and **must not** retry blindly.
    /// This is what keeps a late `tasks/cancel` from overwriting a run that already
    /// completed.
    ///
    /// # Errors
    ///
    /// Returns [`AgentCoreError::NotFound`] if the task does not exist, or
    /// [`AgentCoreError::Internal`] if the backing store fails.
    async fn update_state(
        &self,
        id: &str,
        expected: TaskState,
        next: TaskState,
    ) -> Result<bool, AgentCoreError>;

    /// Replace a task's artifacts.
    ///
    /// Called once a run has produced its output, so that a task fetched later by
    /// `tasks/get` carries the same artifacts the original `message/send` returned.
    ///
    /// # Errors
    ///
    /// Returns [`AgentCoreError::NotFound`] if the task does not exist, or
    /// [`AgentCoreError::Internal`] if the backing store fails.
    async fn set_artifacts(
        &self,
        id: &str,
        artifacts: Vec<crate::Artifact>,
    ) -> Result<(), AgentCoreError>;

    /// Append one event to a task's log, returning the sequence number assigned to it.
    ///
    /// The `seq` field of the supplied event is ignored; the store assigns it.
    ///
    /// # Errors
    ///
    /// Returns [`AgentCoreError::NotFound`] if the task does not exist, or
    /// [`AgentCoreError::Internal`] if the backing store fails.
    async fn append_event(&self, id: &str, event: TaskEvent) -> Result<u64, AgentCoreError>;

    /// Stream a task's events from `from` (**inclusive**), continuing with live appends
    /// until the task reaches a terminal state, at which point the stream ends.
    ///
    /// A `from` pointing at events already evicted clamps forward to the oldest retained
    /// event rather than erroring.
    ///
    /// # Errors
    ///
    /// Returns [`AgentCoreError::NotFound`] if the task does not exist, or
    /// [`AgentCoreError::Internal`] if the backing store fails.
    async fn subscribe(
        &self,
        id: &str,
        from: u64,
    ) -> Result<BoxStream<'static, TaskEvent>, AgentCoreError>;
}

/// One stored task: the task itself, its bounded event log, and the wakeup handle
/// shared with every subscriber.
struct Record {
    /// The task as last written.
    task: Task,
    /// Retained events, oldest first, bounded by [`MAX_EVENTS_PER_TASK`].
    events: VecDeque<TaskEvent>,
    /// Sequence number of `events.front()`; advances as old events are evicted.
    first_seq: u64,
    /// Sequence number the next append will receive.
    next_seq: u64,
    /// Woken on every append and on every state change, so a subscriber blocked in the
    /// live tail observes both new events and terminality.
    notify: Arc<Notify>,
}

/// Mutable interior of [`InMemoryTaskStore`].
struct Inner {
    /// Tasks by id.
    tasks: HashMap<String, Record>,
    /// Insertion order, used to evict the oldest task once `max_tasks` is exceeded.
    order: VecDeque<String>,
    /// Upper bound on retained tasks.
    max_tasks: usize,
}

/// Bounded in-memory [`TaskStore`] — the default when no store is configured.
///
/// Everything is lost when the container stops. That is acceptable for a stateless
/// request/response deployment and *not* acceptable if clients rely on
/// `tasks/resubscribe` across container restarts; supply a durable implementation via
/// [`AgentCoreServerBuilder::task_store`](crate::AgentCoreServerBuilder::task_store) for
/// that case.
pub struct InMemoryTaskStore {
    /// Behind an [`Arc`] rather than owned inline so `subscribe`'s `'static` stream can
    /// hold its own handle and re-lock on every poll without borrowing `self`.
    inner: Arc<Mutex<Inner>>,
}

impl InMemoryTaskStore {
    /// A store retaining at most `max_tasks` tasks, evicting the oldest beyond that.
    ///
    /// A `max_tasks` of zero is raised to one: a store that can retain nothing would
    /// evict every task the instant it was created, making `tasks/get` useless.
    pub fn new(max_tasks: usize) -> Self {
        Self {
            inner: Arc::new(Mutex::new(Inner {
                tasks: HashMap::new(),
                order: VecDeque::new(),
                max_tasks: max_tasks.max(1),
            })),
        }
    }
}

impl Default for InMemoryTaskStore {
    fn default() -> Self {
        Self::new(1024)
    }
}

#[async_trait]
impl TaskStore for InMemoryTaskStore {
    async fn create(&self, task: Task) -> Result<(), AgentCoreError> {
        let mut inner = self.inner.lock().await;
        let id = task.id.clone();
        inner.tasks.insert(
            id.clone(),
            Record {
                task,
                events: VecDeque::new(),
                first_seq: 0,
                next_seq: 0,
                notify: Arc::new(Notify::new()),
            },
        );
        inner.order.push_back(id);
        while inner.order.len() > inner.max_tasks {
            if let Some(evicted) = inner.order.pop_front() {
                // Wake any subscriber before dropping the record, so a stream tailing an
                // evicted task terminates instead of hanging on a `Notify` nobody holds.
                if let Some(record) = inner.tasks.remove(&evicted) {
                    record.notify.notify_waiters();
                }
            }
        }
        Ok(())
    }

    async fn get(&self, id: &str) -> Result<Option<Task>, AgentCoreError> {
        let inner = self.inner.lock().await;
        Ok(inner.tasks.get(id).map(|r| r.task.clone()))
    }

    async fn update_state(
        &self,
        id: &str,
        expected: TaskState,
        next: TaskState,
    ) -> Result<bool, AgentCoreError> {
        let mut inner = self.inner.lock().await;
        let record = inner
            .tasks
            .get_mut(id)
            .ok_or_else(|| AgentCoreError::NotFound(format!("task {id}")))?;
        if record.task.status.state != expected {
            return Ok(false);
        }
        record.task.status.state = next;
        record.task.status.timestamp = now_rfc3339();
        record.notify.notify_waiters();
        Ok(true)
    }

    async fn set_artifacts(
        &self,
        id: &str,
        artifacts: Vec<crate::Artifact>,
    ) -> Result<(), AgentCoreError> {
        let mut inner = self.inner.lock().await;
        let record = inner
            .tasks
            .get_mut(id)
            .ok_or_else(|| AgentCoreError::NotFound(format!("task {id}")))?;
        record.task.artifacts = artifacts;
        Ok(())
    }

    async fn append_event(&self, id: &str, event: TaskEvent) -> Result<u64, AgentCoreError> {
        let mut inner = self.inner.lock().await;
        let record = inner
            .tasks
            .get_mut(id)
            .ok_or_else(|| AgentCoreError::NotFound(format!("task {id}")))?;
        let seq = record.next_seq;
        record.next_seq += 1;
        record.events.push_back(TaskEvent {
            seq,
            payload: event.payload,
        });
        while record.events.len() > MAX_EVENTS_PER_TASK {
            record.events.pop_front();
            record.first_seq += 1;
        }
        record.notify.notify_waiters();
        Ok(seq)
    }

    async fn subscribe(
        &self,
        id: &str,
        from: u64,
    ) -> Result<BoxStream<'static, TaskEvent>, AgentCoreError> {
        let notify = {
            let inner = self.inner.lock().await;
            let record = inner
                .tasks
                .get(id)
                .ok_or_else(|| AgentCoreError::NotFound(format!("task {id}")))?;
            Arc::clone(&record.notify)
        };

        // `unfold` needs owned state, and the trait's stream is `'static`, so the stream
        // cannot borrow `self`. Every poll re-locks through this handle instead.
        let store = Arc::clone(&self.inner);
        let id = id.to_owned();

        Ok(stream::unfold(
            (store, id, notify, from),
            |(store, id, notify, mut cursor)| async move {
                // The `Notified` future borrows `notify`, so the decision is computed
                // here and acted on *after* the loop — returning from inside would move
                // `notify` while that borrow is still live.
                let batch = loop {
                    // (1) Enable the wakeup BEFORE reading. See the module docs: this is
                    // what makes an append racing step (2) a remembered permit rather
                    // than a lost wakeup.
                    let notified = notify.notified();
                    tokio::pin!(notified);
                    notified.as_mut().enable();

                    // (2) Read the backlog at/after the cursor plus the terminal flag.
                    let read = {
                        let inner = store.lock().await;
                        inner.tasks.get(&id).map(|record| {
                            if cursor < record.first_seq {
                                tracing::debug!(
                                    target: "paigasus::runtime_agentcore::a2a",
                                    task_id = %id,
                                    requested = cursor,
                                    clamped_to = record.first_seq,
                                    "subscribe cursor pointed at evicted events; clamping forward"
                                );
                                cursor = record.first_seq;
                            }
                            let batch: Vec<TaskEvent> = record
                                .events
                                .iter()
                                .filter(|e| e.seq >= cursor)
                                .cloned()
                                .collect();
                            (batch, record.task.status.state.is_terminal())
                        })
                    };

                    // Evicted mid-tail: nothing further can ever arrive.
                    let Some((batch, terminal)) = read else {
                        break None;
                    };

                    // (3) Yield what we found, advancing past it.
                    if !batch.is_empty() {
                        break Some(batch);
                    }

                    // (4) Nothing pending and the task is finished: end the stream.
                    if terminal {
                        break None;
                    }

                    // (5) Wait for the next append or state change, then re-read.
                    notified.await;
                }?;

                let next_cursor = batch.last().map_or(cursor, |e| e.seq + 1);
                Some((stream::iter(batch), (store, id, notify, next_cursor)))
            },
        )
        .flatten()
        .boxed())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::a2a::types::{Task, TaskKind, TaskState, TaskStatus};

    fn task(id: &str) -> Task {
        Task {
            id: id.to_owned(),
            context_id: "ctx".to_owned(),
            status: TaskStatus {
                state: TaskState::Submitted,
                timestamp: "2026-08-08T00:00:00Z".to_owned(),
            },
            artifacts: vec![],
            kind: TaskKind::Task,
        }
    }

    fn ev(n: u64) -> TaskEvent {
        TaskEvent {
            seq: 0,
            payload: serde_json::json!({"n": n}),
        }
    }

    #[tokio::test]
    async fn get_on_an_unknown_id_is_ok_none() {
        let s = InMemoryTaskStore::new(8);
        assert!(s.get("nope").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn mutating_an_unknown_id_is_not_found() {
        let s = InMemoryTaskStore::new(8);
        let err = s
            .update_state("nope", TaskState::Submitted, TaskState::Working)
            .await
            .unwrap_err();
        assert!(matches!(err, AgentCoreError::NotFound(_)));
        let err = s.append_event("nope", ev(1)).await.unwrap_err();
        assert!(matches!(err, AgentCoreError::NotFound(_)));
        // `unwrap_err` needs `T: Debug`, and a boxed stream is not `Debug`.
        let Err(err) = s.subscribe("nope", 0).await else {
            panic!("expected NotFound when subscribing to an unknown task");
        };
        assert!(matches!(err, AgentCoreError::NotFound(_)));
    }

    #[tokio::test]
    async fn append_event_returns_monotonic_sequence_numbers() {
        let s = InMemoryTaskStore::new(8);
        s.create(task("t")).await.unwrap();
        assert_eq!(s.append_event("t", ev(1)).await.unwrap(), 0);
        assert_eq!(s.append_event("t", ev(2)).await.unwrap(), 1);
        assert_eq!(s.append_event("t", ev(3)).await.unwrap(), 2);
    }

    #[tokio::test]
    async fn update_state_is_a_compare_and_swap() {
        let s = InMemoryTaskStore::new(8);
        s.create(task("t")).await.unwrap();
        assert!(s
            .update_state("t", TaskState::Submitted, TaskState::Working)
            .await
            .unwrap());
        // The expected state no longer matches, so the swap must be refused.
        assert!(!s
            .update_state("t", TaskState::Submitted, TaskState::Canceled)
            .await
            .unwrap());
        assert_eq!(
            s.get("t").await.unwrap().unwrap().status.state,
            TaskState::Working
        );
    }

    /// The cancel-vs-completion race (§5.7): once the driver has written `Completed`,
    /// a late cancel must lose and leave the task completed.
    #[tokio::test]
    async fn a_late_cancel_loses_to_a_completed_task() {
        let s = InMemoryTaskStore::new(8);
        s.create(task("t")).await.unwrap();
        s.update_state("t", TaskState::Submitted, TaskState::Working)
            .await
            .unwrap();
        assert!(s
            .update_state("t", TaskState::Working, TaskState::Completed)
            .await
            .unwrap());
        assert!(!s
            .update_state("t", TaskState::Working, TaskState::Canceled)
            .await
            .unwrap());
        assert_eq!(
            s.get("t").await.unwrap().unwrap().status.state,
            TaskState::Completed
        );
    }

    #[tokio::test]
    async fn subscribe_replays_the_backlog_then_ends_at_the_terminal() {
        let s = InMemoryTaskStore::new(8);
        s.create(task("t")).await.unwrap();
        s.append_event("t", ev(1)).await.unwrap();
        s.append_event("t", ev(2)).await.unwrap();
        s.update_state("t", TaskState::Submitted, TaskState::Completed)
            .await
            .unwrap();

        let events: Vec<TaskEvent> = s.subscribe("t", 0).await.unwrap().collect().await;
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].seq, 0);
        assert_eq!(events[1].seq, 1);
    }

    #[tokio::test]
    async fn subscribe_honours_the_from_cursor_inclusively() {
        let s = InMemoryTaskStore::new(8);
        s.create(task("t")).await.unwrap();
        for n in 0..4 {
            s.append_event("t", ev(n)).await.unwrap();
        }
        s.update_state("t", TaskState::Submitted, TaskState::Completed)
            .await
            .unwrap();
        let events: Vec<TaskEvent> = s.subscribe("t", 2).await.unwrap().collect().await;
        assert_eq!(events.len(), 2, "from is inclusive");
        assert_eq!(events[0].seq, 2);
    }

    /// The lost-wakeup guard, mirroring `runtime-axum`'s `EventLog` regression test: an
    /// event appended immediately after `subscribe` returns must still be delivered.
    #[tokio::test]
    async fn subscribe_does_not_lose_a_fast_appended_event() {
        let s = Arc::new(InMemoryTaskStore::new(8));
        s.create(task("t")).await.unwrap();
        let stream = s.subscribe("t", 0).await.unwrap();

        let writer = Arc::clone(&s);
        tokio::spawn(async move {
            writer.append_event("t", ev(99)).await.unwrap();
            writer
                .update_state("t", TaskState::Submitted, TaskState::Completed)
                .await
                .unwrap();
        });

        let events: Vec<TaskEvent> = stream.collect().await;
        assert_eq!(events.len(), 1, "the fast append must not be lost");
        assert_eq!(events[0].payload["n"], 99);
    }

    #[tokio::test]
    async fn live_tail_delivers_events_appended_after_subscription() {
        let s = Arc::new(InMemoryTaskStore::new(8));
        s.create(task("t")).await.unwrap();
        s.append_event("t", ev(1)).await.unwrap();
        let stream = s.subscribe("t", 0).await.unwrap();

        let writer = Arc::clone(&s);
        tokio::spawn(async move {
            for n in 2..5 {
                writer.append_event("t", ev(n)).await.unwrap();
            }
            writer
                .update_state("t", TaskState::Submitted, TaskState::Completed)
                .await
                .unwrap();
        });

        let events: Vec<TaskEvent> = stream.collect().await;
        assert_eq!(events.len(), 4, "backlog plus live events, no gap");
        let seqs: Vec<u64> = events.iter().map(|e| e.seq).collect();
        assert_eq!(seqs, vec![0, 1, 2, 3], "no duplicates and no gaps");
    }

    /// A `tasks/get` after a `message/send` must report the same artifacts the send
    /// returned, so the store — not just the response — has to carry them.
    #[tokio::test]
    async fn set_artifacts_is_visible_to_a_later_get() {
        use crate::a2a::types::{Artifact, Part};

        let s = InMemoryTaskStore::new(8);
        s.create(task("t")).await.unwrap();
        s.set_artifacts(
            "t",
            vec![Artifact {
                artifact_id: "a1".to_owned(),
                name: "agent_response".to_owned(),
                parts: vec![Part::Text {
                    text: "hello".to_owned(),
                }],
            }],
        )
        .await
        .unwrap();

        let got = s.get("t").await.unwrap().unwrap();
        assert_eq!(got.artifacts.len(), 1);
        assert!(matches!(
            &got.artifacts[0].parts[0],
            Part::Text { text } if text == "hello"
        ));

        let err = s.set_artifacts("nope", vec![]).await.unwrap_err();
        assert!(matches!(err, AgentCoreError::NotFound(_)));
    }

    #[tokio::test]
    async fn the_task_count_is_bounded_by_lru_eviction() {
        let s = InMemoryTaskStore::new(2);
        s.create(task("a")).await.unwrap();
        s.create(task("b")).await.unwrap();
        s.create(task("c")).await.unwrap();
        assert!(s.get("a").await.unwrap().is_none(), "oldest task evicted");
        assert!(s.get("c").await.unwrap().is_some());
    }

    #[tokio::test]
    async fn per_task_events_are_bounded_and_the_cursor_clamps() {
        let s = InMemoryTaskStore::new(4);
        s.create(task("t")).await.unwrap();
        for n in 0..(MAX_EVENTS_PER_TASK as u64 + 10) {
            s.append_event("t", ev(n)).await.unwrap();
        }
        s.update_state("t", TaskState::Submitted, TaskState::Completed)
            .await
            .unwrap();
        let events: Vec<TaskEvent> = s.subscribe("t", 0).await.unwrap().collect().await;
        assert_eq!(events.len(), MAX_EVENTS_PER_TASK);
        assert_eq!(
            events[0].seq, 10,
            "an evicted cursor clamps to the oldest retained event"
        );
    }
}
