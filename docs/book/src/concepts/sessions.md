# Sessions

A session models conversation persistence as an **append-only event log**, not
a flat message list. The log shape buys deterministic replay for evals, an audit
trail for regulated deployments, and event-sourcing-style durability — at the
cost of a projection step before a provider can read it.

## The `Session` trait

`Session` lives in `paigasus-helikon-core` (re-exported as
`paigasus_helikon::core::Session`). It is an `async_trait` with three methods:

```rust
#[async_trait]
pub trait Session: Send + Sync {
    async fn append(&self, events: &[SessionEvent]) -> Result<(), SessionError>;
    async fn events(&self, since: Option<SequenceId>) -> Result<Vec<SessionEvent>, SessionError>;
    async fn snapshot(&self) -> Result<ConversationSnapshot, SessionError>;
}
```

- `append` writes events to the end of the log.
- `events` reads the log; `since` is **exclusive** — `Some(SequenceId(n))`
  returns events strictly after position `n`, `None` returns the whole log.
- `snapshot` returns a `ConversationSnapshot` — the canonical
  `messages: Vec<Item>` view that providers consume. Both shipped backends
  implement it as `project` over `events(None)`.

A `SequenceId(pub u64)` is a monotonic position in one log. `SessionError` is a
`#[non_exhaustive]` enum with `Unavailable`, a type-erased
`Backend(Box<dyn Error + Send + Sync>)` variant, and an `Other(anyhow::Error)`
escape hatch. Backends wrap their own error type with the
`SessionError::backend(e)` helper.

## The event log

Each entry is a `SessionEvent` — a `#[non_exhaustive]`, serde-tagged enum
(`#[serde(tag = "type", rename_all = "snake_case")]`). Every variant carries a
`ts: jiff::Timestamp` recording when it was logged:

| Variant | Carries |
| --- | --- |
| `UserMessage` | `content: Vec<ContentPart>` |
| `AssistantMessage` | `content: Vec<ContentPart>`, `agent: String` |
| `ToolCalled` | `call_id`, `name`, `args: serde_json::Value` |
| `ToolReturned` | `call_id`, `content: Vec<ContentPart>` |
| `HandoffOccurred` | `from: String`, `to: String` |
| `Compacted` | `summary: String`, `original_count: u64` |

Constructor helpers stamp `ts = Timestamp::now()` for you:
`SessionEvent::user_message(content)`,
`SessionEvent::assistant_message(content, agent)`,
`SessionEvent::tool_called(call_id, name, args)`,
`SessionEvent::tool_returned(call_id, content)`,
`SessionEvent::handoff_occurred(from, to)`, and
`SessionEvent::compacted(summary, original_count)`.

### Projection

`project(events: &[SessionEvent]) -> ConversationSnapshot` folds the log into a
message list. Most variants map one-to-one to an `Item`; `HandoffOccurred` is
audit-only and yields no message; `Compacted` drops the `original_count`
preceding events' messages and emits the summary as an `Item::System`.

**Provider caveat:** a `Compacted` summary renders as `Item::System`, and both
shipped provider translators reshape system messages — Anthropic hoists them to
the top-level `system` field, OpenAI concatenates them at the top of the
conversation. The summary text reaches the model, but as a top-level
instruction, not a positional cutover.

## Shipped backends

### `MemorySession` (in core)

`MemorySession` is an in-memory backend backed by a `Mutex<Vec<SessionEvent>>`,
re-exported as `paigasus_helikon::core::MemorySession`. One instance is one
session — there is no `session_id`. It is the right default for tests and
ephemeral runs:

```rust
use std::sync::Arc;
use paigasus_helikon::core::MemorySession;

let session = Arc::new(MemorySession::new());
```

### `SqliteSession` (`paigasus-helikon-sessions-sqlite`)

For persistent or multi-session storage, the `sessions-sqlite` feature pulls in
`paigasus-helikon-sessions-sqlite` (re-exported as
`paigasus_helikon::sessions_sqlite`). `SqliteSession` stores logs in a single
SQLite database; many sessions share one `sqlx::SqlitePool` and are isolated by
`session_id`. The constructors are `async` and return
`Result<_, SessionError>`:

```rust
use std::sync::Arc;
use paigasus_helikon::sessions_sqlite::SqliteSession;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions, SqliteJournalMode};
use std::time::Duration;

let opts = SqliteConnectOptions::new()
    .filename("sessions.db")
    .create_if_missing(true)
    .journal_mode(SqliteJournalMode::Wal)
    .busy_timeout(Duration::from_secs(30));
let pool = SqlitePoolOptions::new().connect_with(opts).await?;

// `open` runs embedded migrations as a side effect, then opens the session.
let session = Arc::new(SqliteSession::open(pool, "user-42").await?);
```

`SqliteSession::open` runs the embedded migrations on every call (one
round-trip). When you manage many sessions against an already-migrated pool,
call `SqliteSession::migrate(&pool).await?` once at startup, then use
`SqliteSession::open_without_migrate(pool, session_id)` (synchronous, no
`Result`) on the hot path. `SqliteSession::session_id()` returns the id the
instance reads and writes.

Appends serialize through SQLite's database-level write lock (`BEGIN IMMEDIATE`
plus a `(session_id, sequence)` primary key), so the backend is safe for
concurrent writers.

## Plugging a session into a run

`RunContext` accepts any `Arc<dyn Session>` via the `.with_session(...)` setter.
The quickest path is `RunContext::ephemeral(())`, which already installs an
in-memory `MemorySession`. To substitute a persistent backend, call
`.with_session(...)` on the ephemeral context:

```rust
use std::sync::Arc;
use paigasus_helikon::core::{MemorySession, RunContext};

// Default: in-memory session.
let ctx: RunContext<()> = RunContext::ephemeral(());

// Persistent: swap the session backend.
// let ctx: RunContext<()> = RunContext::ephemeral(())
//     .with_session(Arc::new(SqliteSession::open(pool, "user-42").await?));
```

Any `Session` impl drops in via `.with_session(Arc::new(your_backend))`. Swap
`MemorySession` for `SqliteSession::open(pool, "user-42").await?` to persist
across process restarts. Tools do **not** see the session handle — persistence is
the runner's job, not a tool's.

## Run-lifecycle persistence

Loading prior history and writing new events is wired by the **runner**, not by
the agent loop. `TokioRunner` (the `runtime-tokio` feature) does it around each
run:

- **Before the run**, it calls `session.snapshot()`, prepends those messages to
  the run's `AgentInput`, and seeds a `SessionRecorder` with the new turn. A read
  failure is a hard error — the run cannot faithfully resume from an unreadable
  session.
- **During the run**, the recorder observes the agent's `AgentEvent` stream,
  accumulating assistant messages, tool calls/results, and handoffs as
  `SessionEvent`s.
- **After the run**, it drains the recorder and calls `session.append(...)`.
  Persistence here is best-effort: an append error is logged via `tracing` and
  never propagated, so the run's outcome is unaffected. `drain` also synthesizes
  a `ToolReturned` for any tool call interrupted mid-flight, so the log always
  projects to a provider-valid conversation.

Running an `Agent` directly (without a runner) executes against the session in
the `RunContext` but performs no automatic load-or-persist — that lifecycle
belongs to the runner. See [Agent loop](./agent-loop.md) and
[Multi-agent patterns](./multi-agent-patterns.md) for how runs are driven.
