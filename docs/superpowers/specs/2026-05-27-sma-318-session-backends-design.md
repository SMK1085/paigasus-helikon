# SMA-318 — `MemorySession` + `SqliteSession` backends

**Linear:** [SMA-318](https://linear.app/smaschek/issue/SMA-318/memorysession-sqlitesession-backends)
**Branch:** `feature/sma-318-memorysession-sqlitesession-backends`
**References:**
- [Sessions (Notion)](https://www.notion.so/355830e8fbaa81d79e15d62ac40954e8)
- ADR — *Session is an append-only event log, not a message list*

## Goal

Ship the first two `Session` backends and the supporting type changes that subsequent backends (Postgres, Redis) will inherit. After this ticket:

- Every `SessionEvent` variant carries a wall-clock `ts`.
- `MemorySession` is the ephemeral default used by tests and `RunContext::new(NoopSession)` replacements.
- `SqliteSession` persists conversation event logs across process restarts, isolated per `session_id`, with safe concurrent appends.
- `ConversationSnapshot` is the real event-log → message-list projection that providers consume; compaction is applied at projection time.

## Non-goals

- `CompactingSession<S>` wrapper that drives LLM-based summarization (separate ticket).
- `PostgresSession` / `RedisSession` (separate tickets).
- Snapshot caching / incremental projection (cheap enough to recompute; future optimization).
- `sqlx-cli` in CI — migrations are embedded via `sqlx::migrate!()`.

## Crate layout

| Crate | Change |
|---|---|
| `paigasus-helikon-core` | Modify `src/session.rs`: timestamps on `SessionEvent`, `MemorySession`, real `project()` for `ConversationSnapshot`, new `SessionError::Backend` variant. New dep: `jiff`. |
| `paigasus-helikon-sessions-sqlite` (**new**) | Houses `SqliteSession`, the embedded migration, and crate-level tests. Deps: `paigasus-helikon-core`, `sqlx` (sqlite + runtime-tokio + macros + migrate), `jiff`, `async-trait`, `thiserror`, `serde_json`. |
| `paigasus-helikon` (facade) | New Cargo feature `sessions-sqlite` activating an optional dep on the new crate, with the kebab→snake `pub use` alias `sessions_sqlite`. |
| `Cargo.toml` (workspace) | New `[workspace.dependencies]` entries: `jiff = { version = "0.2", features = ["serde"] }`, `sqlx = { version = "0.8", default-features = false, features = ["runtime-tokio", "sqlite", "macros", "migrate"] }`, internal path entry for `paigasus-helikon-sessions-sqlite` at `version = "0.0.0"`. |

The new crate starts at `version = "0.0.0"` per the workspace's release-plz escape rule (see CLAUDE.md). The 0.0.0 → 0.1.0 bump is a follow-up `chore(release): SMA-XXX escape release-plz 0.0.0 trap for sessions-sqlite` commit after the impl PR merges.

## `SessionEvent` timestamp migration

Every variant gains `ts: jiff::Timestamp` (UTC, nanosecond precision, single canonical instant — `Zoned` is for human-facing time and we don't want zone data in the log).

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[non_exhaustive]
pub enum SessionEvent {
    UserMessage { content: Vec<ContentPart>, ts: Timestamp },
    AssistantMessage { content: Vec<ContentPart>, agent: String, ts: Timestamp },
    ToolCalled { call_id: String, name: String, args: serde_json::Value, ts: Timestamp },
    ToolReturned { call_id: String, content: Vec<ContentPart>, ts: Timestamp },
    HandoffOccurred { from: String, to: String, ts: Timestamp },
    Compacted { summary: String, original_count: u64, ts: Timestamp },
}

impl SessionEvent {
    pub fn user_message(content: Vec<ContentPart>) -> Self { /* ts = Timestamp::now() */ }
    pub fn assistant_message(content: Vec<ContentPart>, agent: impl Into<String>) -> Self { … }
    pub fn tool_called(call_id: impl Into<String>, name: impl Into<String>, args: serde_json::Value) -> Self { … }
    pub fn tool_returned(call_id: impl Into<String>, content: Vec<ContentPart>) -> Self { … }
    pub fn handoff_occurred(from: impl Into<String>, to: impl Into<String>) -> Self { … }
    pub fn compacted(summary: impl Into<String>, original_count: u64) -> Self { … }
}
```

**Why constructors:** the runner uses these so it never has to write `Timestamp::now()` inline; tests can still use struct-init syntax to pin a deterministic `ts`.

**Why `#[non_exhaustive]` stays:** adding a struct-variant field is breaking *without* `non_exhaustive`. With it, downstream pattern matchers using `..` keep compiling.

**Serde shape:** `jiff::Timestamp` with the `serde` feature serializes as an RFC 3339 string (`"2026-05-27T04:50:12.268000000Z"`). Human-readable in JSON; sorts correctly as text.

**Touch-up sites:** `crates/paigasus-helikon-core/src/agent.rs`, `runner.rs`, and every test under `crates/paigasus-helikon-core/tests/` that constructs `SessionEvent` literals. Mechanical, will be enumerated in the implementation plan.

## `MemorySession`

In `crates/paigasus-helikon-core/src/session.rs`.

```rust
#[derive(Debug, Default)]
pub struct MemorySession {
    inner: std::sync::Mutex<Vec<SessionEvent>>,
}

impl MemorySession {
    pub fn new() -> Self { Self::default() }
}

#[async_trait]
impl Session for MemorySession {
    async fn append(&self, events: &[SessionEvent]) -> Result<(), SessionError> {
        let mut guard = self.inner.lock().expect("MemorySession mutex poisoned");
        guard.extend_from_slice(events);
        Ok(())
    }

    async fn events(&self, since: Option<SequenceId>) -> Result<Vec<SessionEvent>, SessionError> {
        let guard = self.inner.lock().expect("MemorySession mutex poisoned");
        // `since` is *exclusive* — matches the existing trait doc ("those after `since`").
        let start = since.map(|s| s.0 as usize + 1).unwrap_or(0);
        Ok(guard.get(start..).unwrap_or(&[]).to_vec())
    }

    async fn snapshot(&self) -> Result<ConversationSnapshot, SessionError> {
        let events = self.events(None).await?;
        Ok(project(&events))
    }
}
```

**Choices:**

- **`std::sync::Mutex`, not `tokio::sync::Mutex`.** Critical sections are pure CPU (push, slice-clone) with no `await` while holding. Sync Mutex is faster and works outside a tokio context.
- **`SequenceId` semantics:** 0-indexed position; `since` is **exclusive** (matches the existing trait doc "those after `since`"). `events(Some(SequenceId(5)))` returns events at index > 5 (the 7th event onward). `events(None)` returns all. Same convention applies to `SqliteSession`.
- **Poisoning:** `.expect()` panics. Lock poisoning means a panic occurred inside a critical section — an invariant is already broken. Fail loud.
- **No `session_id`.** One `MemorySession` instance is one session, by construction.
- Joins `pub use session::*` in `lib.rs`; gets a `///` doc comment so `missing_docs` passes.

## `SqliteSession`

In `crates/paigasus-helikon-sessions-sqlite/src/lib.rs`.

### Schema

`crates/paigasus-helikon-sessions-sqlite/migrations/0001_session_events.sql`:

```sql
CREATE TABLE session_events (
    session_id  TEXT    NOT NULL,
    sequence    INTEGER NOT NULL,
    ts_nanos    INTEGER NOT NULL,
    kind        TEXT    NOT NULL,
    payload     TEXT    NOT NULL,
    PRIMARY KEY (session_id, sequence)
);
```

- The PK `(session_id, sequence)` satisfies the ticket's "indexes on (session_id, sequence)" requirement and acts as the uniqueness backstop for concurrent appends.
- `ts_nanos` is `jiff::Timestamp::as_nanosecond()` truncated to `i64`. `i64` of nanoseconds covers ±292 years from 1970; truncation only kicks in past year 2262.
- `kind` and `ts_nanos` are denormalized for ad-hoc querying (e.g., "all tool calls in the last hour"). Source of truth for round-tripping is `payload` (JSON of the full `SessionEvent`).

### API

```rust
pub struct SqliteSession {
    pool: SqlitePool,
    session_id: String,
}

impl SqliteSession {
    /// Idempotent; safe on every startup.
    pub async fn migrate(pool: &SqlitePool) -> Result<(), SessionError> { … }

    /// Open (or implicitly create) a session within the given pool.
    pub fn open(pool: SqlitePool, session_id: impl Into<String>) -> Self { … }

    pub fn session_id(&self) -> &str { &self.session_id }
}

#[async_trait]
impl Session for SqliteSession { /* append / events / snapshot */ }
```

`SqlitePool` is cheap-clone (internally `Arc<_>`); multi-session sharing is just `pool.clone()`.

### Concurrency strategy for `append`

One `BEGIN IMMEDIATE` transaction per call. Inside:

1. `SELECT COALESCE(MAX(sequence), -1) + 1 FROM session_events WHERE session_id = ?` — next sequence number for this session.
2. `INSERT` each event with `sequence = next, next+1, …`, `ts_nanos = ev.ts().as_nanosecond() as i64`, `kind = <variant tag>`, `payload = serde_json::to_string(ev)?`.
3. Commit.

SQLite serializes writers via a database-level write lock, so concurrent `BEGIN IMMEDIATE` transactions queue naturally. WAL mode (set by the pool's `SqliteConnectOptions::journal_mode(WAL)`) keeps readers non-blocking. The PK is the backstop: if two transactions raced past the lock, the second INSERT fails with a UNIQUE violation that propagates as `SessionError::Backend`.

### Reads

- **`events(since)`:** `SELECT sequence, payload FROM session_events WHERE session_id = ? AND sequence > ? ORDER BY sequence` (where `?` defaults to `-1` when `since` is None, so the filter is a no-op). Deserialize each `payload` as `SessionEvent`.
- **`snapshot()`:** `events(None)` → `project()`.

### Pool configuration

Caller-supplied. We document the recommended `SqliteConnectOptions::new().filename(path).journal_mode(WAL).busy_timeout(Duration::from_secs(5))` in the crate-level rustdoc. The API takes a pre-built `SqlitePool` so consumers control connection counts and lifecycle.

### Error mapping

`SessionError` (in core) gains a typed-but-erased variant:

```rust
#[non_exhaustive]
pub enum SessionError {
    Unavailable,
    #[error(transparent)] Backend(Box<dyn std::error::Error + Send + Sync>),
    #[error(transparent)] Other(#[from] anyhow::Error),
}
```

Core stays sqlx-free. `SqliteSession` maps `sqlx::Error` via `.map_err(|e| SessionError::Backend(Box::new(e)))`. Callers who care downcast: `err.downcast_ref::<sqlx::Error>()`.

## `ConversationSnapshot` projection

Free function `pub fn project(events: &[SessionEvent]) -> ConversationSnapshot` in `paigasus-helikon-core::session`. Both `MemorySession::snapshot` and `SqliteSession::snapshot` call it.

### Algorithm

```rust
fn project(events: &[SessionEvent]) -> ConversationSnapshot {
    let mut messages: Vec<Item> = Vec::new();
    let mut contributions: Vec<usize> = Vec::new();   // messages produced per event

    for ev in events {
        let before = messages.len();
        match ev {
            SessionEvent::UserMessage { content, .. } => {
                messages.push(Item::UserMessage { content: content.clone() });
            }
            SessionEvent::AssistantMessage { content, agent, .. } => {
                messages.push(Item::AssistantMessage {
                    content: content.clone(),
                    agent: Some(agent.clone()),
                });
            }
            SessionEvent::ToolCalled { call_id, name, args, .. } => {
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
            SessionEvent::HandoffOccurred { .. } => { /* no Item produced */ }
            SessionEvent::Compacted { summary, original_count, .. } => {
                let n = *original_count as usize;
                let drop_from_idx = contributions.len().saturating_sub(n);
                let drop_msg_count: usize = contributions[drop_from_idx..].iter().sum();
                messages.truncate(messages.len() - drop_msg_count);
                messages.push(Item::System {
                    content: vec![ContentPart::Text { text: summary.clone() }],
                });
            }
        }
        contributions.push(messages.len() - before);
    }

    ConversationSnapshot { messages }
}
```

### Why a parallel `contributions` vec

Events have varying message yield. `HandoffOccurred` contributes 0; a normal turn contributes 1. When `Compacted` says "drop the last N events' worth of messages," we can't `truncate(len - N)` — we need the sum of their actual contributions.

### Edge cases

- `original_count = 0` → no-op pop, summary still appended (degenerate but valid).
- `original_count` > events seen so far → `saturating_sub` clamps to 0; every preceding message is replaced by the summary. Permissive — best-effort projection, no error.
- Two `Compacted` events in a row → the second compacts the first's summary plus whatever is in its window.
- A `Compacted` window that includes a `HandoffOccurred` → the handoff's 0-contribution doesn't break the pop math.

## Testing strategy

### Layout

```
crates/paigasus-helikon-core/tests/
  session_memory.rs              # MemorySession behavior
  session_projection.rs          # project() unit tests, no backend needed

crates/paigasus-helikon-sessions-sqlite/tests/
  roundtrip.rs                   # acceptance #1
  persistence.rs                 # acceptance #2 (file survives restart)
  concurrent_writers.rs          # acceptance #2 (concurrency)
  multi_session.rs               # session_id isolation
```

### `project()` unit tests (core)

Pure function, no async, no fixtures. Cases:

- Empty log → empty snapshot.
- N user/assistant turns → N messages in order, agent attribution preserved.
- Tool-call + tool-return pair → both messages in order.
- Handoff between two assistant messages → no system marker emitted; the next assistant message carries the new `agent`.
- Compaction over a 3-event window in the middle of a 7-event log → 1 system message + 4 untouched messages.
- Compaction over a window that includes a Handoff → handoff's 0-contribution doesn't break the math.
- `original_count` > events seen → clamped, doesn't panic.
- Two consecutive `Compacted` events → second compacts the first.

### `MemorySession` tests (core)

- Round-trip: construct one of each `SessionEvent` variant with pinned `ts`, `append`, `events(None)`, assert deep equality (acceptance #1 for the memory backend).
- `events(Some(SequenceId(3)))` returns events at index > 3 (exclusive watermark).
- Two `tokio::spawn`-ed tasks each append 100 events; total count is 200 (Mutex correctness sanity check).
- Add `MemorySession` to `crates/paigasus-helikon-core/tests/object_safety.rs` as an instantiation check.

### `SqliteSession` tests (new crate)

- **`roundtrip.rs`:** in-memory pool (`sqlite::memory:`), `SqliteSession::migrate`, then for each `SessionEvent` variant: construct with pinned `Timestamp`, append, read back, assert `serde_json::Value`-equality. Covers acceptance #1 including the nanos-as-i64 timestamp encoding.
- **`persistence.rs`:** file-backed DB in a `tempfile::tempdir()`, append events, drop the `SqliteSession` and `SqlitePool`, re-open the same file with fresh objects, assert events are intact. Covers acceptance #2 (restart survival).
- **`concurrent_writers.rs`:** single shared `SqlitePool` against a tempfile DB. Spawn `N=16` tasks each appending `M=10` events to the same `session_id`. After `join_all`: total count is `N*M`, `sequence` values are exactly `0..(N*M)` with no gaps or duplicates, every event matches one of the ones we sent. Covers acceptance #2 (concurrency).
- **`multi_session.rs`:** two `SqliteSession`s with different `session_id`s in one pool, each appends 5 events; assert `events(None)` on each returns only its own 5 in order.

**On loom:** the ticket lists loom as an option. Loom models pure-Rust concurrency primitives and can't reason about SQLite's lock state machine; a `tokio::test` with real concurrent writers against an actual DB file is the right tool here. Documented in `concurrent_writers.rs` rustdoc.

### Acceptance #3 (ADR compliance)

Structural rather than test-driven. Audit: no `pub fn messages(&self) -> &[Item]` or equivalent shortcut on either backend. The only ways to read are `events()` and `snapshot()`. CodeRabbit catches accidental regressions on review.

### CI surface

All new tests run under `cargo test --workspace --all-features` on `{ubuntu, macos, windows} × {stable, 1.75}`. sqlx bundles the SQLite amalgamation; no extra system dep. If Windows trips on file-locking semantics in `persistence.rs`, fallback is `#[cfg(unix)]` with a documented note — not pre-emptively gated.

## Acceptance-criteria mapping

| Criterion | Where verified |
|---|---|
| Append + read-back preserves order and timestamps | `session_memory.rs` (memory), `roundtrip.rs` (sqlite) |
| File survives restart | `persistence.rs` |
| Consistent under concurrent writers | `concurrent_writers.rs` |
| ADR-compliant (event-log API only) | Code review + grep audit |

## Open follow-ups (not in this ticket)

- Bump `paigasus-helikon-sessions-sqlite` from 0.0.0 → 0.1.0 in a `chore(release):` commit after merge (SMA-XXX follow-up).
- `CompactingSession<S>` wrapper.
- `PostgresSession`, `RedisSession`.
- Snapshot caching / incremental projection if profiling shows it's hot.
