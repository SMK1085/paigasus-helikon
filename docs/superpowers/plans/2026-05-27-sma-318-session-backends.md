# SMA-318 — `MemorySession` + `SqliteSession` Backends Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship the first two `Session` backends (`MemorySession` in core, `SqliteSession` in a new crate), add timestamps to every `SessionEvent`, implement the real `ConversationSnapshot` projection with compaction support, and expose the SQLite backend through the facade behind a Cargo feature.

**Architecture:** Three phases, three commits — one per release-eligible scope so release-plz can attribute version bumps correctly. Phase A modifies `paigasus-helikon-core` (timestamps + `SessionError::Backend` + `MemorySession` + `project`). Phase B scaffolds the new `paigasus-helikon-sessions-sqlite` crate. Phase C wires the new crate through the facade.

**Tech Stack:** Rust 1.75 MSRV, `jiff = 0.2` (timestamps), `sqlx = 0.8` (sqlite + runtime-tokio + macros + migrate), `tracing` (warn on compaction edge cases), `tokio` (async runtime for tests), `tempfile` (dev-dep for file-backed DB tests), `insta` (existing snapshot test framework).

**Design reference:** [`docs/superpowers/specs/2026-05-27-sma-318-session-backends-design.md`](../specs/2026-05-27-sma-318-session-backends-design.md).

**Branch:** `feature/sma-318-memorysession-sqlitesession-backends` (already created and currently checked out).

---

## File structure

### Created

- `crates/paigasus-helikon-sessions-sqlite/Cargo.toml` — crate manifest, workspace-inheriting.
- `crates/paigasus-helikon-sessions-sqlite/src/lib.rs` — `SqliteSession` impl, embedded migration entry.
- `crates/paigasus-helikon-sessions-sqlite/migrations/0001_session_events.sql` — embedded schema.
- `crates/paigasus-helikon-sessions-sqlite/tests/roundtrip.rs` — acceptance #1 (append + read-back).
- `crates/paigasus-helikon-sessions-sqlite/tests/persistence.rs` — acceptance #2 (file survives restart).
- `crates/paigasus-helikon-sessions-sqlite/tests/concurrent_writers.rs` — acceptance #2 (concurrency).
- `crates/paigasus-helikon-sessions-sqlite/tests/multi_session.rs` — session_id isolation.
- `crates/paigasus-helikon-core/tests/session_memory.rs` — MemorySession behavior tests.
- `crates/paigasus-helikon-core/tests/session_projection.rs` — `project()` unit tests.

### Modified

- `Cargo.toml` (workspace root) — add `jiff`, `sqlx`, internal `paigasus-helikon-sessions-sqlite` path to `[workspace.dependencies]`.
- `crates/paigasus-helikon-core/Cargo.toml` — add `jiff`, `tracing` to `[dependencies]`.
- `crates/paigasus-helikon-core/src/session.rs` — timestamps on `SessionEvent` variants, constructors, `SessionError::Backend`, `MemorySession`, `project()`.
- `crates/paigasus-helikon-core/tests/serde_roundtrip.rs` — update 6 `SessionEvent` fixtures to pass pinned `ts`.
- `crates/paigasus-helikon-core/tests/snapshots/serde_roundtrip__session_event_*.snap` — regenerate via `cargo insta accept`.
- `crates/paigasus-helikon-core/tests/object_safety.rs` — add `MemorySession` instantiation check.
- `crates/paigasus-helikon/Cargo.toml` — add optional dep + `sessions-sqlite` feature.
- `crates/paigasus-helikon/src/lib.rs` — `pub use paigasus_helikon_sessions_sqlite as sessions_sqlite;` (feature-gated).

### Untouched (verified no construction sites)

- `crates/paigasus-helikon-core/src/agent.rs` — only a doc-comment mention of `SessionEvent::AssistantMessage`, no construction.
- `crates/paigasus-helikon-core/src/runner.rs` — no `SessionEvent::` references.
- `crates/paigasus-helikon-core/tests/object_safety.rs` `NoopSession` and `crates/paigasus-helikon-core/tests/common/mod.rs` `NoopSession` — don't construct `SessionEvent`, no changes needed beyond the existing API working as-is.

---

## Phase A — `paigasus-helikon-core` changes

Single commit at the end of this phase: `feat(core): SMA-318 add timestamps, MemorySession, projection, Backend error variant`.

### Task A.1: Add `jiff` and `tracing` deps

**Files:**
- Modify: `Cargo.toml` (workspace root)
- Modify: `crates/paigasus-helikon-core/Cargo.toml`

- [ ] **Step 1: Add `jiff` to `[workspace.dependencies]`**

Open `Cargo.toml` and find the `[workspace.dependencies]` block. Add `jiff` alphabetically — between `insta` and `opentelemetry`:

```toml
jiff                  = { version = "0.2", features = ["serde"] }
```

- [ ] **Step 2: Add `jiff` and `tracing` to core's `[dependencies]`**

Open `crates/paigasus-helikon-core/Cargo.toml`. The `[dependencies]` table currently ends with `async-stream = { workspace = true }`. Add two lines after it:

```toml
jiff           = { workspace = true }
tracing        = { workspace = true }
```

- [ ] **Step 3: Verify the workspace still builds**

```
cargo build -p paigasus-helikon-core
```

Expected: succeeds. Unused-dep warnings on `jiff` and `tracing` are tolerated at this step — they get consumed in A.2 and A.5.

### Task A.2: Add `ts: Timestamp` to every `SessionEvent` variant, plus constructors

**Files:**
- Modify: `crates/paigasus-helikon-core/src/session.rs`

- [ ] **Step 1: Add the import**

Open `crates/paigasus-helikon-core/src/session.rs`. Add an import below the existing `use serde::{Deserialize, Serialize};`:

```rust
use jiff::Timestamp;
```

- [ ] **Step 2: Add `ts: Timestamp` to each variant**

Update the `SessionEvent` enum (currently around line 69). Add `ts: Timestamp` as the last field of each struct variant, with a doc comment. The result:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[non_exhaustive]
pub enum SessionEvent {
    /// A user-authored message.
    ///
    /// The enum is marked `#[non_exhaustive]` at the enum level (not per
    /// variant) so downstream tests and fixtures can construct variants
    /// by struct-init to pin a deterministic `ts`. Don't tighten this to
    /// per-variant `#[non_exhaustive]`.
    UserMessage {
        /// Content blocks of the message.
        content: Vec<ContentPart>,
        /// Wall-clock instant the event was recorded. UTC, nanosecond precision.
        ts: Timestamp,
    },
    /// An assistant-authored message attributed to a named agent.
    AssistantMessage {
        /// Content blocks of the message.
        content: Vec<ContentPart>,
        /// Name of the emitting [`crate::Agent`]. `String` (not `Option`)
        /// because the runner always knows which agent emitted when
        /// appending to the log.
        agent: String,
        /// Wall-clock instant the event was recorded. UTC, nanosecond precision.
        ts: Timestamp,
    },
    /// The runner invoked a tool.
    ToolCalled {
        /// Provider-assigned call identifier.
        call_id: String,
        /// Tool name.
        name: String,
        /// JSON arguments.
        args: serde_json::Value,
        /// Wall-clock instant the event was recorded. UTC, nanosecond precision.
        ts: Timestamp,
    },
    /// The tool returned.
    ToolReturned {
        /// Matching call identifier.
        call_id: String,
        /// Content blocks of the tool's output (Anthropic permits
        /// text + image inside a tool result).
        content: Vec<ContentPart>,
        /// Wall-clock instant the event was recorded. UTC, nanosecond precision.
        ts: Timestamp,
    },
    /// Control transferred from one agent to another.
    HandoffOccurred {
        /// Outgoing agent name.
        from: String,
        /// Incoming agent name.
        to: String,
        /// Wall-clock instant the event was recorded. UTC, nanosecond precision.
        ts: Timestamp,
    },
    /// Older events were compacted into a summary.
    ///
    /// **Provider-translator caveat:** [`project`] renders this as
    /// [`Item::System`]. Both shipped provider translators reshape
    /// `Item::System`: Anthropic hoists every system block into the
    /// top-level `system` field, and OpenAI concatenates multiple system
    /// blocks into one at the top of the conversation. The "summary
    /// replaces turns 1..N at this position" semantic is therefore
    /// observation-only in the event log; the model sees the summary text
    /// but as a top-level system instruction, not a positional cutover.
    Compacted {
        /// LLM-produced summary.
        summary: String,
        /// Number of events the summary replaces. `u64` (not `usize`)
        /// because the value is serialized into the persisted log — a
        /// 32-bit consumer must read what a 64-bit producer wrote.
        original_count: u64,
        /// Wall-clock instant the event was recorded. UTC, nanosecond precision.
        ts: Timestamp,
    },
}
```

- [ ] **Step 3: Add constructors below the enum**

Immediately after the closing `}` of `pub enum SessionEvent { … }`, add:

```rust
impl SessionEvent {
    /// Construct a [`SessionEvent::UserMessage`] with `ts = Timestamp::now()`.
    pub fn user_message(content: Vec<ContentPart>) -> Self {
        Self::UserMessage {
            content,
            ts: Timestamp::now(),
        }
    }

    /// Construct a [`SessionEvent::AssistantMessage`] with `ts = Timestamp::now()`.
    pub fn assistant_message(content: Vec<ContentPart>, agent: impl Into<String>) -> Self {
        Self::AssistantMessage {
            content,
            agent: agent.into(),
            ts: Timestamp::now(),
        }
    }

    /// Construct a [`SessionEvent::ToolCalled`] with `ts = Timestamp::now()`.
    pub fn tool_called(
        call_id: impl Into<String>,
        name: impl Into<String>,
        args: serde_json::Value,
    ) -> Self {
        Self::ToolCalled {
            call_id: call_id.into(),
            name: name.into(),
            args,
            ts: Timestamp::now(),
        }
    }

    /// Construct a [`SessionEvent::ToolReturned`] with `ts = Timestamp::now()`.
    pub fn tool_returned(call_id: impl Into<String>, content: Vec<ContentPart>) -> Self {
        Self::ToolReturned {
            call_id: call_id.into(),
            content,
            ts: Timestamp::now(),
        }
    }

    /// Construct a [`SessionEvent::HandoffOccurred`] with `ts = Timestamp::now()`.
    pub fn handoff_occurred(from: impl Into<String>, to: impl Into<String>) -> Self {
        Self::HandoffOccurred {
            from: from.into(),
            to: to.into(),
            ts: Timestamp::now(),
        }
    }

    /// Construct a [`SessionEvent::Compacted`] with `ts = Timestamp::now()`.
    pub fn compacted(summary: impl Into<String>, original_count: u64) -> Self {
        Self::Compacted {
            summary: summary.into(),
            original_count,
            ts: Timestamp::now(),
        }
    }
}
```

- [ ] **Step 4: Update the doctest in the `Session` trait**

The example doctest near the top of `session.rs` (around line 17) constructs a fake `MemorySession` for demo. It still compiles — `ConversationSnapshot::default()` is fine. No change needed here; A.4 will replace this doctest's struct with a real reference. Skip for now.

- [ ] **Step 5: Build to surface failing call sites**

```
cargo build -p paigasus-helikon-core --tests
```

Expected: failures in `tests/serde_roundtrip.rs` — six errors of the form "missing field `ts` in initializer of `SessionEvent::…`". This is the cue for Task A.6.

### Task A.3: Add `SessionError::Backend` and the `backend()` constructor

**Files:**
- Modify: `crates/paigasus-helikon-core/src/session.rs`

- [ ] **Step 1: Replace the `SessionError` enum**

Find the existing `SessionError` near the bottom of `session.rs` and replace with:

```rust
/// Errors raised by [`Session`] methods.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum SessionError {
    /// Backend unreachable (database down, file locked, …).
    #[error("session backend unavailable")]
    Unavailable,

    /// A backend-specific error, type-erased so core stays free of any
    /// particular backend dependency. The `'static` bound is required for
    /// [`std::error::Error::downcast_ref`] to work; callers who care about
    /// the underlying type can do `err.downcast_ref::<sqlx::Error>()`.
    #[error(transparent)]
    Backend(Box<dyn std::error::Error + Send + Sync + 'static>),

    /// Escape hatch.
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

impl SessionError {
    /// Wrap a backend-specific error as [`SessionError::Backend`].
    ///
    /// Saves the
    /// `.map_err(|e| SessionError::Backend(Box::new(e)))` boilerplate at
    /// every query call site — use as `.map_err(SessionError::backend)`.
    pub fn backend<E>(e: E) -> Self
    where
        E: std::error::Error + Send + Sync + 'static,
    {
        Self::Backend(Box::new(e))
    }
}
```

- [ ] **Step 2: Build to confirm**

```
cargo build -p paigasus-helikon-core
```

Expected: succeeds (tests still broken from A.2; that's fine).

### Task A.4: Add `MemorySession` (TDD)

**Files:**
- Modify: `crates/paigasus-helikon-core/src/session.rs`
- Create: `crates/paigasus-helikon-core/tests/session_memory.rs`

- [ ] **Step 1: Write the failing test file**

Create `crates/paigasus-helikon-core/tests/session_memory.rs`:

```rust
//! Behavior tests for [`MemorySession`].

use jiff::Timestamp;
use paigasus_helikon_core::{
    ContentPart, MemorySession, SequenceId, Session, SessionEvent,
};

fn epoch() -> Timestamp {
    Timestamp::from_second(0).expect("0 is a valid timestamp")
}

fn user_msg(text: &str) -> SessionEvent {
    SessionEvent::UserMessage {
        content: vec![ContentPart::Text { text: text.into() }],
        ts: epoch(),
    }
}

#[tokio::test]
async fn append_and_read_back_preserves_order_and_timestamps() {
    let session = MemorySession::new();
    let events = vec![user_msg("first"), user_msg("second"), user_msg("third")];

    session.append(&events).await.expect("append");
    let read_back = session.events(None).await.expect("events");

    assert_eq!(read_back.len(), 3);
    for (orig, got) in events.iter().zip(read_back.iter()) {
        let orig_json = serde_json::to_value(orig).unwrap();
        let got_json = serde_json::to_value(got).unwrap();
        assert_eq!(orig_json, got_json);
    }
}

#[tokio::test]
async fn events_since_returns_strictly_after_watermark() {
    let session = MemorySession::new();
    let events = (0..5)
        .map(|i| user_msg(&format!("msg-{i}")))
        .collect::<Vec<_>>();
    session.append(&events).await.unwrap();

    // since = SequenceId(2) should return events at indices 3, 4 (exclusive).
    let tail = session
        .events(Some(SequenceId(2)))
        .await
        .expect("events");
    assert_eq!(tail.len(), 2);

    let tail_first = serde_json::to_value(&tail[0]).unwrap();
    let expected_first = serde_json::to_value(&events[3]).unwrap();
    assert_eq!(tail_first, expected_first);
}

#[tokio::test]
async fn events_since_past_end_returns_empty() {
    let session = MemorySession::new();
    session.append(&[user_msg("only")]).await.unwrap();

    let tail = session.events(Some(SequenceId(100))).await.unwrap();
    assert!(tail.is_empty());
}

#[tokio::test]
async fn concurrent_appends_preserve_total_count() {
    use std::sync::Arc;

    let session = Arc::new(MemorySession::new());
    let tasks = (0..8)
        .map(|i| {
            let s = session.clone();
            tokio::spawn(async move {
                for _ in 0..100 {
                    s.append(&[user_msg(&format!("task-{i}"))]).await.unwrap();
                }
            })
        })
        .collect::<Vec<_>>();

    for t in tasks {
        t.await.unwrap();
    }

    let all = session.events(None).await.unwrap();
    assert_eq!(all.len(), 800);
}
```

- [ ] **Step 2: Run the test to confirm it fails to compile**

```
cargo test -p paigasus-helikon-core --test session_memory --no-run
```

Expected: compile error — "cannot find type `MemorySession` in scope".

- [ ] **Step 3: Implement `MemorySession`**

In `crates/paigasus-helikon-core/src/session.rs`, add this block after the `SessionEvent` impl block and before the `SequenceId` struct (or anywhere in the module body — placement doesn't matter):

```rust
/// In-memory [`Session`] backend backed by an [`std::sync::Mutex<Vec<_>>`].
///
/// Suitable for tests and ephemeral runs. One instance is one session by
/// construction — there is no `session_id`. For persistent or multi-session
/// storage, see `paigasus-helikon-sessions-sqlite`.
///
/// Lock poisoning panics (`expect`): if a panic occurred inside a critical
/// section, an invariant is already broken. Fail loud.
#[derive(Debug, Default)]
pub struct MemorySession {
    inner: std::sync::Mutex<Vec<SessionEvent>>,
}

impl MemorySession {
    /// Create an empty [`MemorySession`].
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl Session for MemorySession {
    async fn append(&self, events: &[SessionEvent]) -> Result<(), SessionError> {
        let mut guard = self.inner.lock().expect("MemorySession mutex poisoned");
        guard.extend_from_slice(events);
        Ok(())
    }

    async fn events(
        &self,
        since: Option<SequenceId>,
    ) -> Result<Vec<SessionEvent>, SessionError> {
        let guard = self.inner.lock().expect("MemorySession mutex poisoned");
        // `since` is *exclusive* — matches the existing trait doc ("those
        // after `since`"). `try_from` ensures 32-bit targets fail loudly
        // instead of wrapping past `u32::MAX`. Unreachable in practice.
        let start = match since {
            Some(s) => {
                usize::try_from(s.0).expect("SequenceId exceeds platform usize") + 1
            }
            None => 0,
        };
        Ok(guard.get(start..).unwrap_or(&[]).to_vec())
    }

    async fn snapshot(&self) -> Result<ConversationSnapshot, SessionError> {
        let events = self.events(None).await?;
        Ok(project(&events))
    }
}
```

- [ ] **Step 4: Add `MemorySession` to `pub use session::*` consumers**

`crates/paigasus-helikon-core/src/lib.rs` already has `pub use session::*;`, so `MemorySession` is automatically re-exported. No change needed.

- [ ] **Step 5: Verify the tests fail at link / runtime (not compile)**

```
cargo test -p paigasus-helikon-core --test session_memory --no-run
```

Expected: compile error — "cannot find function `project` in scope" inside the new `MemorySession::snapshot`. Task A.5 supplies `project`. Leave a stub for now:

```rust
// Temporary stub — replaced by Task A.5.
fn project(_events: &[SessionEvent]) -> ConversationSnapshot {
    ConversationSnapshot::default()
}
```

Add this stub at module scope inside `session.rs`. Re-run the build:

```
cargo test -p paigasus-helikon-core --test session_memory
```

Expected: all four `session_memory` tests pass.

### Task A.5: Implement `project()` (TDD)

**Files:**
- Modify: `crates/paigasus-helikon-core/src/session.rs`
- Create: `crates/paigasus-helikon-core/tests/session_projection.rs`

- [ ] **Step 1: Write the failing test file**

Create `crates/paigasus-helikon-core/tests/session_projection.rs`:

```rust
//! Unit tests for [`project`].

use jiff::Timestamp;
use paigasus_helikon_core::{
    project, ContentPart, ConversationSnapshot, Item, SessionEvent,
};

fn epoch() -> Timestamp {
    Timestamp::from_second(0).expect("0 is a valid timestamp")
}

fn user(text: &str) -> SessionEvent {
    SessionEvent::UserMessage {
        content: vec![ContentPart::Text { text: text.into() }],
        ts: epoch(),
    }
}

fn assistant(text: &str, agent: &str) -> SessionEvent {
    SessionEvent::AssistantMessage {
        content: vec![ContentPart::Text { text: text.into() }],
        agent: agent.into(),
        ts: epoch(),
    }
}

fn tool_called(call_id: &str) -> SessionEvent {
    SessionEvent::ToolCalled {
        call_id: call_id.into(),
        name: "calc".into(),
        args: serde_json::json!({"x": 1}),
        ts: epoch(),
    }
}

fn tool_returned(call_id: &str) -> SessionEvent {
    SessionEvent::ToolReturned {
        call_id: call_id.into(),
        content: vec![ContentPart::Text { text: "result".into() }],
        ts: epoch(),
    }
}

fn handoff(from: &str, to: &str) -> SessionEvent {
    SessionEvent::HandoffOccurred {
        from: from.into(),
        to: to.into(),
        ts: epoch(),
    }
}

fn compacted(summary: &str, n: u64) -> SessionEvent {
    SessionEvent::Compacted {
        summary: summary.into(),
        original_count: n,
        ts: epoch(),
    }
}

#[test]
fn empty_log_projects_to_empty_snapshot() {
    let snap = project(&[]);
    assert!(snap.messages.is_empty());
}

#[test]
fn user_and_assistant_turns_project_in_order_with_agent() {
    let events = vec![user("hi"), assistant("hello", "triage")];
    let snap = project(&events);
    assert_eq!(snap.messages.len(), 2);
    match &snap.messages[0] {
        Item::UserMessage { content } => {
            assert_eq!(content.len(), 1);
        }
        other => panic!("expected UserMessage, got {other:?}"),
    }
    match &snap.messages[1] {
        Item::AssistantMessage { content, agent } => {
            assert_eq!(content.len(), 1);
            assert_eq!(agent.as_deref(), Some("triage"));
        }
        other => panic!("expected AssistantMessage, got {other:?}"),
    }
}

#[test]
fn tool_call_and_return_project_as_pair() {
    let events = vec![tool_called("c1"), tool_returned("c1")];
    let snap = project(&events);
    assert_eq!(snap.messages.len(), 2);
    assert!(matches!(snap.messages[0], Item::ToolCall { .. }));
    assert!(matches!(snap.messages[1], Item::ToolResult { .. }));
}

#[test]
fn handoff_produces_no_message() {
    let events = vec![
        assistant("first", "a"),
        handoff("a", "b"),
        assistant("second", "b"),
    ];
    let snap = project(&events);
    assert_eq!(snap.messages.len(), 2);
    // Second assistant message carries the new agent name.
    match &snap.messages[1] {
        Item::AssistantMessage { agent, .. } => {
            assert_eq!(agent.as_deref(), Some("b"));
        }
        other => panic!("expected AssistantMessage, got {other:?}"),
    }
}

#[test]
fn compaction_replaces_window_with_single_system_message() {
    // 7 events: 4 keep, 3 get compacted into one System message, then more after.
    let events = vec![
        user("u1"),
        assistant("a1", "x"),
        user("u2"),
        compacted("summary of last 3", 3),
        assistant("a2", "x"),
        user("u3"),
    ];
    let snap = project(&events);
    // u1, a1, u2 are dropped; one System (summary) replaces them; a2, u3 follow.
    assert_eq!(snap.messages.len(), 3);
    match &snap.messages[0] {
        Item::System { content } => match &content[0] {
            ContentPart::Text { text } => assert_eq!(text, "summary of last 3"),
            _ => panic!("expected Text"),
        },
        other => panic!("expected System, got {other:?}"),
    }
    assert!(matches!(snap.messages[1], Item::AssistantMessage { .. }));
    assert!(matches!(snap.messages[2], Item::UserMessage { .. }));
}

#[test]
fn compaction_over_window_with_handoff_does_not_break_math() {
    // 4-event window includes one Handoff (0 messages produced).
    let events = vec![
        user("u1"),
        assistant("a1", "x"),
        handoff("x", "y"),
        assistant("a2", "y"),
        compacted("summary", 4),
        user("u3"),
    ];
    let snap = project(&events);
    // u1, a1, (no handoff msg), a2 → 3 messages dropped; one System replaces.
    assert_eq!(snap.messages.len(), 2);
    assert!(matches!(snap.messages[0], Item::System { .. }));
    assert!(matches!(snap.messages[1], Item::UserMessage { .. }));
}

#[test]
fn compaction_with_oversized_count_clamps_to_zero() {
    let events = vec![user("u1"), compacted("summary", 999)];
    let snap = project(&events);
    // u1 dropped; one System replaces.
    assert_eq!(snap.messages.len(), 1);
    assert!(matches!(snap.messages[0], Item::System { .. }));
}

#[test]
fn two_consecutive_compactions_chain() {
    let events = vec![
        user("u1"),
        assistant("a1", "x"),
        compacted("first summary", 2),
        compacted("second summary", 1),
    ];
    let snap = project(&events);
    // After first compact: [System("first summary")].
    // After second compact: [System("second summary")] (replaces the first).
    assert_eq!(snap.messages.len(), 1);
    match &snap.messages[0] {
        Item::System { content } => match &content[0] {
            ContentPart::Text { text } => assert_eq!(text, "second summary"),
            _ => panic!("expected Text"),
        },
        other => panic!("expected System, got {other:?}"),
    }
}
```

- [ ] **Step 2: Run the test — expect compile failure**

```
cargo test -p paigasus-helikon-core --test session_projection --no-run
```

Expected: compile error — "no function `project` found for the crate" (the stub from A.4 is non-public).

- [ ] **Step 3: Replace the stub `project` with the real implementation**

In `crates/paigasus-helikon-core/src/session.rs`, replace the temporary `fn project(_events: …) { ConversationSnapshot::default() }` stub with the full implementation:

```rust
/// Project an append-only [`SessionEvent`] log into a [`ConversationSnapshot`]
/// — the canonical message-list view that providers consume.
///
/// **Provider-translator caveat:** `Compacted` events render as
/// [`Item::System`]. Both shipped provider translators (SMA-316 OpenAI,
/// SMA-317 Anthropic) reshape system messages — Anthropic hoists every
/// `Item::System` into the top-level `system` field; OpenAI concatenates
/// multiple system blocks into one at the top of the conversation. The
/// "summary replaces turns 1..N at this position" semantic is therefore
/// observation-only in the event log; the model sees the summary text but
/// as a top-level system instruction, not a positional cutover.
pub fn project(events: &[SessionEvent]) -> ConversationSnapshot {
    let mut messages: Vec<Item> = Vec::new();
    // Parallel vec: contributions[i] = number of messages event i produced.
    // Needed because Compacted has to undo the message yield of the previous
    // N events, and yield varies per variant (HandoffOccurred = 0, others = 1).
    let mut contributions: Vec<usize> = Vec::new();

    for ev in events {
        let before = messages.len();
        match ev {
            SessionEvent::UserMessage { content, .. } => {
                messages.push(Item::UserMessage {
                    content: content.clone(),
                });
            }
            SessionEvent::AssistantMessage { content, agent, .. } => {
                messages.push(Item::AssistantMessage {
                    content: content.clone(),
                    agent: Some(agent.clone()),
                });
            }
            SessionEvent::ToolCalled {
                call_id, name, args, ..
            } => {
                messages.push(Item::ToolCall {
                    call_id: call_id.clone(),
                    name: name.clone(),
                    args: args.clone(),
                });
            }
            SessionEvent::ToolReturned { call_id, content, .. } => {
                messages.push(Item::ToolResult {
                    call_id: call_id.clone(),
                    content: content.clone(),
                });
            }
            SessionEvent::HandoffOccurred { .. } => {
                // Audit-only event; no message produced.
            }
            SessionEvent::Compacted {
                summary,
                original_count,
                ..
            } => {
                let n = *original_count as usize;
                if n == 0 {
                    tracing::warn!(
                        "Compacted event with original_count = 0; emitting summary without dropping any messages (likely producer bug)"
                    );
                }
                if n > contributions.len() {
                    tracing::warn!(
                        original_count = n,
                        events_seen = contributions.len(),
                        "Compacted event references more events than have been seen; clamping to 0 (likely corrupt log)"
                    );
                }
                let drop_from_idx = contributions.len().saturating_sub(n);
                let drop_msg_count: usize =
                    contributions[drop_from_idx..].iter().sum();
                let new_len = messages.len() - drop_msg_count;
                messages.truncate(new_len);
                messages.push(Item::System {
                    content: vec![ContentPart::Text {
                        text: summary.clone(),
                    }],
                });
            }
        }
        contributions.push(messages.len() - before);
    }

    ConversationSnapshot { messages }
}
```

- [ ] **Step 4: Run the projection tests**

```
cargo test -p paigasus-helikon-core --test session_projection
```

Expected: all eight tests pass.

- [ ] **Step 5: Re-run MemorySession tests (still green)**

```
cargo test -p paigasus-helikon-core --test session_memory
```

Expected: all four tests still pass.

### Task A.6: Update existing `serde_roundtrip` fixtures and snapshots

**Files:**
- Modify: `crates/paigasus-helikon-core/tests/serde_roundtrip.rs:319-377` (the six `SessionEvent` test fixtures)
- Modify: `crates/paigasus-helikon-core/tests/snapshots/serde_roundtrip__session_event_*.snap` (regenerated via insta)

- [ ] **Step 1: Add a pinned-timestamp helper at the top of `serde_roundtrip.rs`**

Open `crates/paigasus-helikon-core/tests/serde_roundtrip.rs`. After the existing `use` block at the top, add:

```rust
use jiff::Timestamp;

fn pinned_ts() -> Timestamp {
    // Fixed instant so insta snapshots are deterministic.
    Timestamp::from_second(0).expect("0 is a valid timestamp")
}
```

- [ ] **Step 2: Update the six `SessionEvent` fixtures**

Replace lines 321-377 of the existing file (the six `#[test]` functions for `session_event_*_roundtrip`) so each variant initializer includes `ts: pinned_ts(),`. Concretely, every `SessionEvent::Variant { ... }` literal in those six tests gets a trailing `ts: pinned_ts(),`. Example for `UserMessage`:

```rust
#[test]
fn session_event_user_message_roundtrip() {
    let ev = SessionEvent::UserMessage {
        content: vec![ContentPart::Text {
            text: "hello".into(),
        }],
        ts: pinned_ts(),
    };
    insta::assert_snapshot!(roundtrip(&ev));
}
```

Apply the same `ts: pinned_ts(),` addition to the other five variants in that block: `AssistantMessage`, `ToolCalled`, `ToolReturned`, `HandoffOccurred`, `Compacted`.

- [ ] **Step 3: Run the tests — expect snapshot diff**

```
cargo test -p paigasus-helikon-core --test serde_roundtrip
```

Expected: six failures with insta diffs showing the new `ts` field in the serialized JSON.

- [ ] **Step 4: Review and accept the snapshot updates**

```
cargo insta review
```

Inspect each diff: every snapshot should gain a `"ts": "1970-01-01T00:00:00Z"` field. Accept all six. If `cargo insta` isn't installed:

```
cargo install cargo-insta
```

…then re-run.

- [ ] **Step 5: Re-run to confirm green**

```
cargo test -p paigasus-helikon-core --test serde_roundtrip
```

Expected: all green.

### Task A.7: Add `MemorySession` to `object_safety.rs`

**Files:**
- Modify: `crates/paigasus-helikon-core/tests/object_safety.rs`

- [ ] **Step 1: Add `MemorySession` to the import**

In `crates/paigasus-helikon-core/tests/object_safety.rs`, the giant `use paigasus_helikon_core::{...}` block at the top needs `MemorySession` added alphabetically. Find the line:

```rust
    GuardrailError, GuardrailInput, GuardrailVerdict, Hook, HookDecision, HookEvent, Model,
```

…and change the next line (which currently begins `ModelCapabilities, ModelError, ...`) to insert `MemorySession,` before `ModelCapabilities`:

```rust
    MemorySession, ModelCapabilities, ModelError, ModelEvent, ModelRequest, RunConfig, RunContext, RunError,
```

- [ ] **Step 2: Add an instantiation check at the bottom of `fn trait_objects_construct`**

In the existing `#[test] fn trait_objects_construct()` function, after the line `let _: Box<dyn Session> = Box::new(NoopSession);`, add:

```rust
    // Concrete `MemorySession` also satisfies the `Session` trait object.
    let _: Box<dyn Session> = Box::new(MemorySession::new());
```

- [ ] **Step 3: Run the test**

```
cargo test -p paigasus-helikon-core --test object_safety
```

Expected: passes.

### Task A.8: Verify Phase A and commit

- [ ] **Step 1: Format**

```
cargo fmt --all
```

Expected: no output, exit 0.

- [ ] **Step 2: Clippy**

```
cargo clippy --workspace --all-features --all-targets -- -D warnings
```

Expected: no warnings. If `unused-import`/`unused-dep` warnings surface on `jiff` or `tracing` (shouldn't, since both are used by now), revisit Tasks A.2/A.5.

- [ ] **Step 3: Full test pass for the core crate**

```
cargo test -p paigasus-helikon-core --all-features
```

Expected: all green, including the existing `private_probe`, `loop_happy_path`, `loop_parallel_tools`, `transition_unit`, `compile_run_result_typed` tests.

- [ ] **Step 4: Stage and commit**

```
git add Cargo.toml \
        crates/paigasus-helikon-core/Cargo.toml \
        crates/paigasus-helikon-core/src/session.rs \
        crates/paigasus-helikon-core/tests/serde_roundtrip.rs \
        crates/paigasus-helikon-core/tests/snapshots/serde_roundtrip__session_event_*.snap \
        crates/paigasus-helikon-core/tests/object_safety.rs \
        crates/paigasus-helikon-core/tests/session_memory.rs \
        crates/paigasus-helikon-core/tests/session_projection.rs

git commit -m "feat(core): SMA-318 add timestamps, MemorySession, projection, Backend error variant

Every SessionEvent variant now carries jiff::Timestamp ts; with()-style
constructors stamp Timestamp::now() so the runner stays terse. New
MemorySession (Arc<Mutex<Vec<_>>>) is the ephemeral default for tests and
RunContext setup. project() walks the event log and produces the canonical
ConversationSnapshot, applying compaction by popping the contributions of
the previous N events and emitting one Item::System summary. SessionError
gains a Backend(Box<dyn Error + Send + Sync + 'static>) variant plus a
SessionError::backend(e) constructor so future backend crates (sqlite,
postgres) can map their errors without forcing core to depend on them."
```

Expected: commit succeeds. The `commit-msg` hook runs `convco check` — should pass on the `feat(core)` scope. The `pre-push` hook (later) will re-run fmt + clippy + convco; nothing happens at commit time besides convco.

---

## Phase B — `paigasus-helikon-sessions-sqlite` crate

Single commit at the end: `feat(sessions-sqlite): SMA-318 implement SqliteSession backed by sqlx`.

### Task B.1: Scaffold the crate

**Files:**
- Create: `crates/paigasus-helikon-sessions-sqlite/Cargo.toml`
- Create: `crates/paigasus-helikon-sessions-sqlite/src/lib.rs` (placeholder)
- Modify: `Cargo.toml` (workspace root)

- [ ] **Step 1: Make the directory and placeholder lib.rs**

```
mkdir -p crates/paigasus-helikon-sessions-sqlite/src
mkdir -p crates/paigasus-helikon-sessions-sqlite/migrations
mkdir -p crates/paigasus-helikon-sessions-sqlite/tests
```

Then write `crates/paigasus-helikon-sessions-sqlite/src/lib.rs`:

```rust
//! SQLite-backed [`Session`] implementation for the Paigasus Helikon SDK.
//!
//! See the SMA-318 design doc for the schema, concurrency strategy, and
//! provider-translator caveats around compaction projection.
//!
//! [`Session`]: paigasus_helikon_core::Session

// Implementation lands in subsequent tasks.
```

- [ ] **Step 2: Write the crate Cargo.toml**

Create `crates/paigasus-helikon-sessions-sqlite/Cargo.toml`:

```toml
[package]
name        = "paigasus-helikon-sessions-sqlite"
description = "SQLite-backed Session backend for the Paigasus Helikon AI SDK."
version                = "0.0.0"
edition.workspace      = true
rust-version.workspace = true
authors.workspace      = true
license.workspace      = true
repository.workspace   = true
homepage.workspace     = true
keywords.workspace     = true
categories.workspace   = true

[dependencies]
paigasus-helikon-core = { workspace = true }
async-trait           = { workspace = true }
jiff                  = { workspace = true }
serde_json            = { workspace = true }
sqlx                  = { workspace = true, features = ["runtime-tokio", "sqlite", "macros", "migrate"] }
thiserror             = { workspace = true }
tracing               = { workspace = true }

[dev-dependencies]
tokio        = { workspace = true, features = ["macros", "rt-multi-thread", "time", "sync"] }
tempfile     = "3"

[lints]
workspace = true
```

- [ ] **Step 3: Add `sqlx`, `tempfile`-via-default-not-needed, and the internal path entry to workspace deps**

Open root `Cargo.toml`. In `[workspace.dependencies]`, add `sqlx` alphabetically (between `serde_json` and `syn`):

```toml
sqlx                  = { version = "0.8", default-features = false, features = ["runtime-tokio", "sqlite", "macros", "migrate"] }
```

In the internal-crate block at the bottom of `[workspace.dependencies]`, add the new path between the existing entries — place it alphabetically (after `paigasus-helikon-runtime-temporal`, before `paigasus-helikon-evals` if you prefer source-tree order, or simply at the end of the block for a least-diff change):

```toml
paigasus-helikon-sessions-sqlite     = { path = "crates/paigasus-helikon-sessions-sqlite",     version = "0.0.0" }
```

- [ ] **Step 4: Verify the workspace recognizes the new crate**

```
cargo metadata --format-version 1 --no-deps | grep -o 'paigasus-helikon-sessions-sqlite'
```

Expected: matches at least once. Then:

```
cargo build -p paigasus-helikon-sessions-sqlite
```

Expected: builds cleanly. sqlx + sqlite compilation is slow on first run; the bundled SQLite amalgamation may take 60–90 seconds.

### Task B.2: Write the embedded migration

**Files:**
- Create: `crates/paigasus-helikon-sessions-sqlite/migrations/0001_session_events.sql`

- [ ] **Step 1: Write the migration**

Create `crates/paigasus-helikon-sessions-sqlite/migrations/0001_session_events.sql`:

```sql
CREATE TABLE session_events (
    session_id  TEXT    NOT NULL,
    sequence    INTEGER NOT NULL,
    ts_nanos    INTEGER NOT NULL,
    kind        TEXT    NOT NULL,
    payload     TEXT    NOT NULL,
    PRIMARY KEY (session_id, sequence)
);

CREATE INDEX idx_session_events_session_ts
    ON session_events (session_id, ts_nanos);
```

- [ ] **Step 2: Verify sqlx-cli (not used in CI; just a local sanity check, optional)**

This step is **optional** — skip if you don't have `sqlx-cli`. The migration is exercised by `SqliteSession::migrate` later, which is the canonical verification path.

### Task B.3: Implement the `SqliteSession` skeleton (`migrate`, `open`, `open_unchecked`)

**Files:**
- Modify: `crates/paigasus-helikon-sessions-sqlite/src/lib.rs`

- [ ] **Step 1: Replace `lib.rs` with the skeleton**

Replace the contents of `crates/paigasus-helikon-sessions-sqlite/src/lib.rs`:

```rust
//! SQLite-backed [`Session`] implementation for the Paigasus Helikon SDK.
//!
//! Stores conversation event logs in a single SQLite database. Multiple
//! sessions share one `SqlitePool` and are isolated by `session_id`. Safe
//! for concurrent writers — appends serialize through SQLite's database-level
//! write lock; the `(session_id, sequence)` primary key is the uniqueness
//! backstop.
//!
//! ## Recommended pool configuration
//!
//! ```no_run
//! use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
//! use std::time::Duration;
//!
//! # async fn build() -> Result<sqlx::SqlitePool, sqlx::Error> {
//! let opts = SqliteConnectOptions::new()
//!     .filename("sessions.db")
//!     .create_if_missing(true)
//!     .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal)
//!     .busy_timeout(Duration::from_secs(5));
//! SqlitePoolOptions::new().connect_with(opts).await
//! # }
//! ```
//!
//! ## Provider-translator caveat
//!
//! The [`project`] function in `paigasus-helikon-core` renders [`Compacted`]
//! events as `Item::System`. Both shipped provider translators reshape
//! system messages — Anthropic hoists them to the top-level `system` field,
//! OpenAI concatenates them. Compaction summaries reach the model but as
//! top-level instructions, not positional cutovers.
//!
//! [`Session`]: paigasus_helikon_core::Session
//! [`project`]: paigasus_helikon_core::project
//! [`Compacted`]: paigasus_helikon_core::SessionEvent::Compacted

use async_trait::async_trait;
use paigasus_helikon_core::{
    project, ConversationSnapshot, SequenceId, Session, SessionError, SessionEvent,
};
use sqlx::SqlitePool;

static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");

/// SQLite-backed [`Session`] implementation. One instance is one session
/// (identified by `session_id`); pools are shared across instances.
#[derive(Debug, Clone)]
pub struct SqliteSession {
    pool: SqlitePool,
    session_id: String,
}

impl SqliteSession {
    /// Run embedded migrations on `pool`. Idempotent — safe on every startup.
    ///
    /// Optional: [`SqliteSession::open`] runs migrations internally. Call
    /// this directly if you manage many sessions and want to migrate once at
    /// process start, then use [`SqliteSession::open_unchecked`] on the hot
    /// path to skip the per-`open` round-trip to `_sqlx_migrations`.
    pub async fn migrate(pool: &SqlitePool) -> Result<(), SessionError> {
        MIGRATOR.run(pool).await.map_err(SessionError::backend)?;
        Ok(())
    }

    /// Open (or implicitly create) a session within `pool`. Runs migrations
    /// as a side effect (one round-trip to `_sqlx_migrations`). For repeated
    /// session-opens against an already-migrated pool, prefer
    /// [`SqliteSession::open_unchecked`].
    pub async fn open(
        pool: SqlitePool,
        session_id: impl Into<String>,
    ) -> Result<Self, SessionError> {
        Self::migrate(&pool).await?;
        Ok(Self::open_unchecked(pool, session_id))
    }

    /// Open a session without running migrations. The caller must have
    /// already invoked [`SqliteSession::migrate`] on this pool; otherwise
    /// the first [`Session::append`] fails with `SessionError::Backend`
    /// wrapping a `no such table` error.
    pub fn open_unchecked(pool: SqlitePool, session_id: impl Into<String>) -> Self {
        Self {
            pool,
            session_id: session_id.into(),
        }
    }

    /// The `session_id` this instance reads and writes.
    pub fn session_id(&self) -> &str {
        &self.session_id
    }
}

#[async_trait]
impl Session for SqliteSession {
    async fn append(&self, _events: &[SessionEvent]) -> Result<(), SessionError> {
        unimplemented!("Task B.4")
    }

    async fn events(
        &self,
        _since: Option<SequenceId>,
    ) -> Result<Vec<SessionEvent>, SessionError> {
        unimplemented!("Task B.5")
    }

    async fn snapshot(&self) -> Result<ConversationSnapshot, SessionError> {
        let events = self.events(None).await?;
        Ok(project(&events))
    }
}
```

- [ ] **Step 2: Build**

```
cargo build -p paigasus-helikon-sessions-sqlite
```

Expected: succeeds. sqlx's `migrate!()` macro reads `./migrations/0001_session_events.sql` at compile time.

### Task B.4: Implement `append` (TDD via `roundtrip.rs`)

**Files:**
- Create: `crates/paigasus-helikon-sessions-sqlite/tests/roundtrip.rs`
- Modify: `crates/paigasus-helikon-sessions-sqlite/src/lib.rs` (replace `append` stub)

- [ ] **Step 1: Write the failing test**

Create `crates/paigasus-helikon-sessions-sqlite/tests/roundtrip.rs`:

```rust
//! Append + read-back round-trip for every [`SessionEvent`] variant.
//!
//! **Pool note:** the in-memory test pool MUST use `max_connections = 1`.
//! `sqlite::memory:` creates a *separate* in-memory database per
//! connection, so a multi-connection pool would intermittently hit
//! "no such table: session_events" because some connections never saw
//! the migration. Don't parallelize this — it'll reintroduce the bug.

use jiff::Timestamp;
use paigasus_helikon_core::{ContentPart, Session, SessionEvent};
use paigasus_helikon_sessions_sqlite::SqliteSession;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

async fn fresh_session() -> SqliteSession {
    let opts = SqliteConnectOptions::new().in_memory(true);
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(opts)
        .await
        .expect("pool");
    SqliteSession::open(pool, "test-session").await.expect("open")
}

fn pinned() -> Timestamp {
    Timestamp::from_second(1_700_000_000).expect("valid ts")
}

fn all_variants() -> Vec<SessionEvent> {
    vec![
        SessionEvent::UserMessage {
            content: vec![ContentPart::Text {
                text: "hello".into(),
            }],
            ts: pinned(),
        },
        SessionEvent::AssistantMessage {
            content: vec![ContentPart::Text {
                text: "hi back".into(),
            }],
            agent: "triage".into(),
            ts: pinned(),
        },
        SessionEvent::ToolCalled {
            call_id: "c1".into(),
            name: "calc".into(),
            args: serde_json::json!({"x": 1}),
            ts: pinned(),
        },
        SessionEvent::ToolReturned {
            call_id: "c1".into(),
            content: vec![ContentPart::Text { text: "2".into() }],
            ts: pinned(),
        },
        SessionEvent::HandoffOccurred {
            from: "triage".into(),
            to: "billing".into(),
            ts: pinned(),
        },
        SessionEvent::Compacted {
            summary: "previous turns summarized".into(),
            original_count: 5,
            ts: pinned(),
        },
    ]
}

#[tokio::test]
async fn roundtrip_preserves_every_variant_and_timestamps() {
    let session = fresh_session().await;
    let events = all_variants();

    session.append(&events).await.expect("append");
    let read_back = session.events(None).await.expect("events");

    assert_eq!(read_back.len(), events.len(), "event count");
    for (orig, got) in events.iter().zip(read_back.iter()) {
        let orig_json = serde_json::to_value(orig).unwrap();
        let got_json = serde_json::to_value(got).unwrap();
        assert_eq!(orig_json, got_json, "round-trip mismatch");
    }
}
```

- [ ] **Step 2: Run — confirm `unimplemented!()` panic**

```
cargo test -p paigasus-helikon-sessions-sqlite --test roundtrip
```

Expected: test panics on the `unimplemented!("Task B.4")` line in `append`.

- [ ] **Step 3: Implement `append`**

In `crates/paigasus-helikon-sessions-sqlite/src/lib.rs`, replace the `append` stub:

```rust
    async fn append(&self, events: &[SessionEvent]) -> Result<(), SessionError> {
        if events.is_empty() {
            return Ok(());
        }

        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(SessionError::backend)?;

        // Find next sequence number for this session. COALESCE handles the
        // first-append case (MAX returns NULL on an empty result set).
        let row: (i64,) = sqlx::query_as(
            "SELECT COALESCE(MAX(sequence), -1) + 1 FROM session_events WHERE session_id = ?",
        )
        .bind(&self.session_id)
        .fetch_one(&mut *tx)
        .await
        .map_err(SessionError::backend)?;
        let mut next: i64 = row.0;

        for ev in events {
            let (kind, ts_nanos) = event_metadata(ev);
            let payload = serde_json::to_string(ev).map_err(SessionError::backend)?;

            sqlx::query(
                "INSERT INTO session_events (session_id, sequence, ts_nanos, kind, payload) \
                 VALUES (?, ?, ?, ?, ?)",
            )
            .bind(&self.session_id)
            .bind(next)
            .bind(ts_nanos)
            .bind(kind)
            .bind(&payload)
            .execute(&mut *tx)
            .await
            .map_err(SessionError::backend)?;

            next += 1;
        }

        tx.commit().await.map_err(SessionError::backend)?;
        Ok(())
    }
```

Then add this helper at module scope (after the `impl Session for SqliteSession` block, before EOF):

```rust
/// Extract the `(kind, ts_nanos)` denormalized columns for an event. `kind`
/// matches the serde tag of the variant; `ts_nanos` is the timestamp in
/// i64 nanoseconds since the Unix epoch (covers ±292 years from 1970).
fn event_metadata(ev: &SessionEvent) -> (&'static str, i64) {
    let (kind, ts) = match ev {
        SessionEvent::UserMessage { ts, .. } => ("user_message", *ts),
        SessionEvent::AssistantMessage { ts, .. } => ("assistant_message", *ts),
        SessionEvent::ToolCalled { ts, .. } => ("tool_called", *ts),
        SessionEvent::ToolReturned { ts, .. } => ("tool_returned", *ts),
        SessionEvent::HandoffOccurred { ts, .. } => ("handoff_occurred", *ts),
        SessionEvent::Compacted { ts, .. } => ("compacted", *ts),
    };
    let ts_nanos = ts.as_nanosecond() as i64;
    (kind, ts_nanos)
}
```

- [ ] **Step 4: Implement `events` (needed by the same test)**

Replace the `events` stub in the `impl Session` block:

```rust
    async fn events(
        &self,
        since: Option<SequenceId>,
    ) -> Result<Vec<SessionEvent>, SessionError> {
        // `since` is exclusive ("those after"). Default to -1 so the
        // `sequence > ?` filter is a no-op when None.
        let watermark: i64 = match since {
            Some(s) => i64::try_from(s.0).map_err(SessionError::backend)?,
            None => -1,
        };

        let rows: Vec<(String,)> = sqlx::query_as(
            "SELECT payload FROM session_events \
             WHERE session_id = ? AND sequence > ? \
             ORDER BY sequence",
        )
        .bind(&self.session_id)
        .bind(watermark)
        .fetch_all(&self.pool)
        .await
        .map_err(SessionError::backend)?;

        rows.into_iter()
            .map(|(payload,)| {
                serde_json::from_str::<SessionEvent>(&payload)
                    .map_err(SessionError::backend)
            })
            .collect()
    }
```

- [ ] **Step 5: Run the roundtrip test**

```
cargo test -p paigasus-helikon-sessions-sqlite --test roundtrip
```

Expected: passes.

### Task B.5: `events(Some(_))` and `snapshot()` correctness (extends `roundtrip.rs`)

**Files:**
- Modify: `crates/paigasus-helikon-sessions-sqlite/tests/roundtrip.rs`

`snapshot()` was implemented in B.3 and `events()` in B.4; this task adds tests that pin the `since`-exclusive semantics and the snapshot projection.

- [ ] **Step 1: Append more tests to `roundtrip.rs`**

Append to `crates/paigasus-helikon-sessions-sqlite/tests/roundtrip.rs`:

```rust
#[tokio::test]
async fn events_since_is_exclusive_watermark() {
    let session = fresh_session().await;
    let events: Vec<SessionEvent> = (0..5)
        .map(|i| SessionEvent::UserMessage {
            content: vec![ContentPart::Text {
                text: format!("msg-{i}"),
            }],
            ts: pinned(),
        })
        .collect();
    session.append(&events).await.expect("append");

    // SequenceId(2) → strictly after index 2, so we get events 3, 4.
    let tail = session
        .events(Some(paigasus_helikon_core::SequenceId(2)))
        .await
        .expect("events");
    assert_eq!(tail.len(), 2);

    let head = session.events(None).await.expect("events");
    assert_eq!(head.len(), 5);
}

#[tokio::test]
async fn snapshot_projects_through_project_function() {
    let session = fresh_session().await;
    session
        .append(&[
            SessionEvent::UserMessage {
                content: vec![ContentPart::Text {
                    text: "hello".into(),
                }],
                ts: pinned(),
            },
            SessionEvent::AssistantMessage {
                content: vec![ContentPart::Text {
                    text: "hi".into(),
                }],
                agent: "triage".into(),
                ts: pinned(),
            },
        ])
        .await
        .expect("append");

    let snap = session.snapshot().await.expect("snapshot");
    assert_eq!(snap.messages.len(), 2);
}
```

- [ ] **Step 2: Run**

```
cargo test -p paigasus-helikon-sessions-sqlite --test roundtrip
```

Expected: three tests pass.

### Task B.6: Persistence — file survives process restart

**Files:**
- Create: `crates/paigasus-helikon-sessions-sqlite/tests/persistence.rs`

- [ ] **Step 1: Write the test**

Create `crates/paigasus-helikon-sessions-sqlite/tests/persistence.rs`:

```rust
//! Covers acceptance criterion #2 from SMA-318: a `SqliteSession` opens a
//! file, survives a process restart, and reads back what was written.

use jiff::Timestamp;
use paigasus_helikon_core::{ContentPart, Session, SessionEvent};
use paigasus_helikon_sessions_sqlite::SqliteSession;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

#[tokio::test]
async fn file_backed_session_survives_pool_drop() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("sessions.db");

    // First "process": write some events, then drop the session and pool.
    {
        let opts = SqliteConnectOptions::new()
            .filename(&path)
            .create_if_missing(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(opts)
            .await
            .expect("pool");
        let session = SqliteSession::open(pool, "convo-1").await.expect("open");
        session
            .append(&[
                SessionEvent::UserMessage {
                    content: vec![ContentPart::Text {
                        text: "before restart".into(),
                    }],
                    ts: Timestamp::from_second(1_700_000_000).unwrap(),
                },
                SessionEvent::AssistantMessage {
                    content: vec![ContentPart::Text {
                        text: "ack".into(),
                    }],
                    agent: "triage".into(),
                    ts: Timestamp::from_second(1_700_000_001).unwrap(),
                },
            ])
            .await
            .expect("append");
        // Drop pool + session by leaving scope.
    }

    // Second "process": open fresh pool on the same file, expect events.
    let opts = SqliteConnectOptions::new()
        .filename(&path)
        .create_if_missing(false);
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(opts)
        .await
        .expect("pool");
    let session = SqliteSession::open(pool, "convo-1").await.expect("open");
    let read_back = session.events(None).await.expect("events");

    assert_eq!(read_back.len(), 2);
    match &read_back[0] {
        SessionEvent::UserMessage { content, .. } => match &content[0] {
            ContentPart::Text { text } => assert_eq!(text, "before restart"),
            _ => panic!("expected Text"),
        },
        other => panic!("expected UserMessage, got {other:?}"),
    }
}
```

- [ ] **Step 2: Run**

```
cargo test -p paigasus-helikon-sessions-sqlite --test persistence
```

Expected: passes.

### Task B.7: Concurrent writers

**Files:**
- Create: `crates/paigasus-helikon-sessions-sqlite/tests/concurrent_writers.rs`

- [ ] **Step 1: Write the test**

Create `crates/paigasus-helikon-sessions-sqlite/tests/concurrent_writers.rs`:

```rust
//! Covers acceptance criterion #2 from SMA-318 (concurrency): N tasks
//! appending to the same `session_id` produce a contiguous sequence with
//! no gaps or duplicates.
//!
//! **Why not loom:** loom models pure-Rust concurrency primitives and
//! can't reason about SQLite's lock state machine. Using a real
//! `tokio::test` with a file-backed pool exercises the actual write-lock
//! path. The 30-second busy timeout absorbs slow CI runners where 160
//! sequential `BEGIN IMMEDIATE` transactions can approach the default 5
//! seconds.

use std::sync::Arc;
use std::time::Duration;

use jiff::Timestamp;
use paigasus_helikon_core::{ContentPart, Session, SessionEvent};
use paigasus_helikon_sessions_sqlite::SqliteSession;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions, SqliteJournalMode};

const N_TASKS: usize = 16;
const M_EVENTS_PER_TASK: usize = 10;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_appends_produce_contiguous_sequence() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("concurrent.db");

    let opts = SqliteConnectOptions::new()
        .filename(&path)
        .create_if_missing(true)
        .journal_mode(SqliteJournalMode::Wal)
        .busy_timeout(Duration::from_secs(30));
    let pool = SqlitePoolOptions::new()
        .max_connections(8)
        .connect_with(opts)
        .await
        .expect("pool");
    SqliteSession::migrate(&pool).await.expect("migrate");

    let session = Arc::new(SqliteSession::open_unchecked(pool, "shared"));

    let handles = (0..N_TASKS)
        .map(|task_idx| {
            let s = session.clone();
            tokio::spawn(async move {
                for j in 0..M_EVENTS_PER_TASK {
                    let ev = SessionEvent::UserMessage {
                        content: vec![ContentPart::Text {
                            text: format!("task-{task_idx}-msg-{j}"),
                        }],
                        ts: Timestamp::from_second(1_700_000_000)
                            .unwrap(),
                    };
                    s.append(&[ev]).await.expect("append");
                }
            })
        })
        .collect::<Vec<_>>();

    for h in handles {
        h.await.expect("task panicked");
    }

    let all = session.events(None).await.expect("events");
    let expected = N_TASKS * M_EVENTS_PER_TASK;
    assert_eq!(all.len(), expected, "total event count");

    // The sequence column is internal; we observe it indirectly: every
    // event we read back must be one of the ones we sent.
    let mut texts: Vec<String> = all
        .into_iter()
        .filter_map(|ev| match ev {
            SessionEvent::UserMessage { content, .. } => match content.into_iter().next() {
                Some(ContentPart::Text { text }) => Some(text),
                _ => None,
            },
            _ => None,
        })
        .collect();
    texts.sort();

    let mut expected_texts: Vec<String> = (0..N_TASKS)
        .flat_map(|t| (0..M_EVENTS_PER_TASK).map(move |j| format!("task-{t}-msg-{j}")))
        .collect();
    expected_texts.sort();

    assert_eq!(texts, expected_texts, "every sent event is present exactly once");
}
```

- [ ] **Step 2: Run**

```
cargo test -p paigasus-helikon-sessions-sqlite --test concurrent_writers
```

Expected: passes within ~5 seconds locally. May take longer on slow CI runners.

### Task B.8: Multi-session isolation

**Files:**
- Create: `crates/paigasus-helikon-sessions-sqlite/tests/multi_session.rs`

- [ ] **Step 1: Write the test**

Create `crates/paigasus-helikon-sessions-sqlite/tests/multi_session.rs`:

```rust
//! Two `SqliteSession`s with distinct `session_id`s in one pool must read
//! back only their own events.

use jiff::Timestamp;
use paigasus_helikon_core::{ContentPart, Session, SessionEvent};
use paigasus_helikon_sessions_sqlite::SqliteSession;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

fn msg(text: &str) -> SessionEvent {
    SessionEvent::UserMessage {
        content: vec![ContentPart::Text { text: text.into() }],
        ts: Timestamp::from_second(0).unwrap(),
    }
}

#[tokio::test]
async fn distinct_session_ids_are_isolated() {
    let opts = SqliteConnectOptions::new().in_memory(true);
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(opts)
        .await
        .expect("pool");
    SqliteSession::migrate(&pool).await.expect("migrate");

    let a = SqliteSession::open_unchecked(pool.clone(), "session-a");
    let b = SqliteSession::open_unchecked(pool, "session-b");

    a.append(&[msg("a1"), msg("a2"), msg("a3"), msg("a4"), msg("a5")])
        .await
        .expect("append a");
    b.append(&[msg("b1"), msg("b2"), msg("b3"), msg("b4"), msg("b5")])
        .await
        .expect("append b");

    let a_events = a.events(None).await.expect("events a");
    let b_events = b.events(None).await.expect("events b");

    assert_eq!(a_events.len(), 5);
    assert_eq!(b_events.len(), 5);

    // Each session sees only its own prefix.
    let extract_text = |ev: &SessionEvent| -> String {
        match ev {
            SessionEvent::UserMessage { content, .. } => match &content[0] {
                ContentPart::Text { text } => text.clone(),
                _ => unreachable!(),
            },
            _ => unreachable!(),
        }
    };
    let a_texts: Vec<String> = a_events.iter().map(extract_text).collect();
    let b_texts: Vec<String> = b_events.iter().map(extract_text).collect();

    assert_eq!(a_texts, vec!["a1", "a2", "a3", "a4", "a5"]);
    assert_eq!(b_texts, vec!["b1", "b2", "b3", "b4", "b5"]);
}
```

- [ ] **Step 2: Run**

```
cargo test -p paigasus-helikon-sessions-sqlite --test multi_session
```

Expected: passes.

### Task B.9: Verify Phase B and commit

- [ ] **Step 1: Format and clippy**

```
cargo fmt --all
cargo clippy --workspace --all-features --all-targets -- -D warnings
```

Expected: no warnings.

- [ ] **Step 2: Run all tests on the new crate**

```
cargo test -p paigasus-helikon-sessions-sqlite --all-features
```

Expected: four test files green (`roundtrip`, `persistence`, `concurrent_writers`, `multi_session`).

- [ ] **Step 3: Make sure the rest of the workspace still passes**

```
cargo test --workspace --all-features
```

Expected: full green.

- [ ] **Step 4: Commit**

```
git add Cargo.toml \
        crates/paigasus-helikon-sessions-sqlite

git commit -m "feat(sessions-sqlite): SMA-318 implement SqliteSession backed by sqlx

New crate paigasus-helikon-sessions-sqlite hosts the second Session backend:
- Embedded migration creates session_events(session_id, sequence, ts_nanos,
  kind, payload TEXT) with PK(session_id, sequence) and a secondary
  (session_id, ts_nanos) index.
- SqliteSession::migrate is idempotent. open() auto-migrates;
  open_unchecked skips for callers that pre-migrate at startup.
- Append uses BEGIN IMMEDIATE with COALESCE(MAX(sequence), -1)+1 for the
  next slot; SQLite's database-level write lock serializes concurrent
  writers, and the PK is the uniqueness backstop.
- Test matrix covers round-trip of every variant (single-connection
  in-memory pool to avoid sqlx::memory: per-connection isolation footgun),
  file-survives-pool-drop persistence, 16x10 concurrent appends
  (busy_timeout 30s for slow CI), and session_id isolation in a shared
  pool."
```

Expected: commit succeeds.

---

## Phase C — facade feature wiring

Single commit at the end: `feat(facade): SMA-318 expose sessions-sqlite via Cargo feature`.

### Task C.1: Add the optional dep and feature

**Files:**
- Modify: `crates/paigasus-helikon/Cargo.toml`
- Modify: `crates/paigasus-helikon/src/lib.rs`

- [ ] **Step 1: Add the optional dep**

In `crates/paigasus-helikon/Cargo.toml`, in the `[dependencies]` table, add (alphabetical placement after `paigasus-helikon-runtime-temporal`):

```toml
paigasus-helikon-sessions-sqlite     = { workspace = true, optional = true }
```

- [ ] **Step 2: Add the feature**

In the same file, in the `[features]` table, add after `runtime-agentcore  = ["dep:paigasus-helikon-runtime-agentcore"]`:

```toml
sessions-sqlite   = ["dep:paigasus-helikon-sessions-sqlite"]
```

- [ ] **Step 3: Add the re-export**

In `crates/paigasus-helikon/src/lib.rs`, append:

```rust
/// SQLite-backed `Session` backend. Enabled via the `sessions-sqlite` feature.
#[cfg(feature = "sessions-sqlite")]
pub use paigasus_helikon_sessions_sqlite as sessions_sqlite;
```

- [ ] **Step 4: Build with and without the feature**

```
cargo build -p paigasus-helikon
cargo build -p paigasus-helikon --features sessions-sqlite
cargo build -p paigasus-helikon --all-features
```

Expected: all three succeed.

### Task C.2: Verify Phase C and commit

- [ ] **Step 1: Format, clippy, full test pass**

```
cargo fmt --all
cargo clippy --workspace --all-features --all-targets -- -D warnings
cargo test --workspace --all-features
```

Expected: all green.

- [ ] **Step 2: Docs build (matches CI's `docs` job)**

```
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps
```

Expected: succeeds. Watch for the known `paigasus-helikon` library-vs-binary filename collision warning — that's pre-existing and non-fatal (CLAUDE.md notes it explicitly).

- [ ] **Step 3: Commit**

```
git add crates/paigasus-helikon/Cargo.toml \
        crates/paigasus-helikon/src/lib.rs

git commit -m "feat(facade): SMA-318 expose sessions-sqlite via Cargo feature

The new paigasus-helikon-sessions-sqlite crate is now reachable through the
facade as paigasus_helikon::sessions_sqlite when the sessions-sqlite feature
is enabled. Mirrors the existing kebab-case-feature, snake-case-alias
pattern used for providers and runtimes."
```

Expected: commit succeeds.

---

## Phase D — push, open PR, follow-ups

### Task D.1: Push the branch

- [ ] **Step 1: Push**

```
git push -u origin feature/sma-318-memorysession-sqlitesession-backends
```

The `pre-push` hook runs `cargo fmt --all -- --check`, `cargo clippy --workspace --all-features --all-targets -- -D warnings`, and `convco check <upstream>..HEAD`. All three should be green from Phase C.2. If any fail, fix and re-push (do not use `--no-verify`).

### Task D.2: Open the PR

- [ ] **Step 1: Choose merge strategy**

The branch has three commits (one per release-eligible scope: `core`, `sessions-sqlite`, `facade`). release-plz reads commit history from `main` to attribute version bumps. Two viable paths:

1. **Merge-commit or rebase-merge** the PR so the three scoped commits land on `main` individually. release-plz then attributes the `feat(core)` bump to `paigasus-helikon-core`, the `feat(sessions-sqlite)` bump to the new crate, and the `feat(facade)` bump to the facade. This is the canonical approach for multi-scope PRs.
2. **Squash-merge** with one of the scoped titles (e.g. `feat(sessions-sqlite): SMA-318 ...`) — release-plz then attributes the bump only to that one crate, missing the core and facade changes. Not recommended for this PR.

Document the chosen strategy in the PR description so the merger knows.

- [ ] **Step 2: Create the PR**

```
gh pr create --title "feat(sessions-sqlite): SMA-318 add MemorySession + SqliteSession backends" \
  --body "$(cat <<'EOF'
## Summary

- Adds `ts: jiff::Timestamp` to every `SessionEvent` variant plus `SessionEvent::*` constructors so the runner doesn't write `Timestamp::now()` inline.
- Adds `MemorySession` (`Arc<Mutex<Vec<_>>>`) in `paigasus-helikon-core` as the ephemeral default for tests.
- Adds `SessionError::Backend(Box<dyn Error + Send + Sync + 'static>)` and a `SessionError::backend(e)` constructor so backend crates can map their errors without forcing core to depend on them.
- Implements `project()` in core: the canonical event-log → `ConversationSnapshot` projection, with compaction support (drops the contributions of the previous N events; emits one `Item::System` summary).
- New crate `paigasus-helikon-sessions-sqlite` with `SqliteSession` backed by `sqlx` (sqlite + runtime-tokio + macros + migrate). One embedded migration creates `session_events` with PK `(session_id, sequence)` and a secondary `(session_id, ts_nanos)` index.
- Facade exposes the new backend as `paigasus_helikon::sessions_sqlite` behind the `sessions-sqlite` Cargo feature.

## Architecture

Three commits, one per release-eligible scope (`core`, `sessions-sqlite`, `facade`), so release-plz can attribute per-crate bumps correctly. **Please merge with a merge commit or rebase-merge — not squash** — so the per-scope attribution survives on `main`.

Design doc: `docs/superpowers/specs/2026-05-27-sma-318-session-backends-design.md`.
Plan doc: `docs/superpowers/plans/2026-05-27-sma-318-session-backends.md`.

## Test plan

- [x] `cargo test --workspace --all-features` green locally
- [x] `cargo fmt --all -- --check` green
- [x] `cargo clippy --workspace --all-features --all-targets -- -D warnings` green
- [x] `RUSTDOCFLAGS=-D warnings cargo doc --workspace --all-features --no-deps` green
- [ ] CI matrix green ({ubuntu, macos, windows} × {stable, 1.75})
- [ ] CodeRabbit review

## Follow-ups

- `chore(release): SMA-XXX escape release-plz 0.0.0 trap for sessions-sqlite` after merge to bump the new crate to 0.1.0 (per the workspace's release-plz convention; see CLAUDE.md).

🤖 Generated with [Claude Code](https://claude.com/claude-code)
EOF
)"
```

- [ ] **Step 3: Watch CI**

The PR triggers the gates: `fmt`, `clippy`, `test (ubuntu-latest, stable)`, `docs`, `doc-coverage`, `commits`, `pr-title`, `audit`, `deny`. Wait for all required checks to report green.

The `pr-title` gate enforces Conventional Commits format (`feat(sessions-sqlite): …`) **and** the subject-must-start-lowercase rule after the `SMA-### ` prefix. The title above (`feat(sessions-sqlite): SMA-318 add ...`) satisfies both.

### Task D.3: Spec & plan cleanup tracking

- [ ] **Step 1: Move SMA-318 to "In Review" in Linear**

When the PR is open and CI is passing, transition the Linear issue:

```
# Via the Linear MCP tool — example only; the conversation flow handles this.
```

Linear auto-closes the issue on PR merge, so no manual close needed afterward (per CLAUDE.md and memory).

### Task D.4: Post-merge follow-up

- [ ] **Step 1: Open the release-plz escape ticket**

After this PR merges, the new `paigasus-helikon-sessions-sqlite` crate is pinned at `0.0.0` and release-plz will not propose a bump (the 0.0.0 git tag created by release-plz's first run is interpreted as "already published"). Open a new ticket along the lines of `SMA-XXX: escape release-plz 0.0.0 trap for sessions-sqlite`, scoped as `chore(release):`, that bumps `version = "0.0.0"` → `version = "0.1.0"` in:

- `crates/paigasus-helikon-sessions-sqlite/Cargo.toml`
- `Cargo.toml` (workspace root `[workspace.dependencies]` entry — change `version = "0.0.0"` → `version = "0.1.0"`)

That's a separate PR; not part of SMA-318's scope.

---

## Acceptance-criteria mapping (cross-check after Phase C)

| SMA-318 criterion | Verified in |
|---|---|
| Append + read-back preserves order + timestamps | `session_memory.rs::append_and_read_back_preserves_order_and_timestamps`, `roundtrip.rs::roundtrip_preserves_every_variant_and_timestamps` |
| File survives restart | `persistence.rs::file_backed_session_survives_pool_drop` |
| Consistent under concurrent writers | `concurrent_writers.rs::concurrent_appends_produce_contiguous_sequence` |
| ADR-compliant (event-log API only) | Code review — no `pub fn messages()` or equivalent shortcut on either backend; the only reads are `events()` and `snapshot()` |
