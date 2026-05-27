# SMA-318 — `MemorySession` + `SqliteSession` backends review

**Reviewer:** Claude (staff-engineering review)
**Reviewed:** [`2026-05-27-sma-318-session-backends-design.md`](./2026-05-27-sma-318-session-backends-design.md)
**Date:** 2026-05-27
**Sources cross-checked:** Linear SMA-318, Notion "Sessions" + ADR-3 (Session is an append-only event log), the current `paigasus-helikon-core::session` module, `tests/object_safety.rs`, and the workspace `Cargo.toml`.

The spec is tight, well-scoped, and correctly aligned with the existing `Session` trait (the only design drift from ADR-3 — `ToolReturned { output: Value }` vs `ToolReturned { content: Vec<ContentPart> }` — is already settled in core and the spec follows the code). Most issues below are small; two will cause compile or test failures as written.

## Critical issues

### 1. `Box<dyn std::error::Error + Send + Sync>` is not downcastable as written

The spec defines:

```rust
#[error(transparent)] Backend(Box<dyn std::error::Error + Send + Sync>),
```

…and promises: "Callers who care downcast: `err.downcast_ref::<sqlx::Error>()`."

`<dyn Error>::downcast_ref::<T>()` requires `dyn Error + 'static`. Without the `'static` bound on the trait object, `downcast_ref` does not exist on the type. Code that tries to downcast won't compile.

**Fix**: add `+ 'static`:

```rust
Backend(Box<dyn std::error::Error + Send + Sync + 'static>),
```

This is the form `anyhow`, `eyre`, and `Box<dyn Error>`-style erasures all use. The `'static` bound also makes the type easier to plumb through `tokio::spawn` futures.

### 2. `sqlite::memory:` with a multi-connection pool will fail tests

§"Testing strategy" specifies `roundtrip.rs` uses an "in-memory pool (`sqlite::memory:`)". This is a well-known sqlx footgun: `sqlite::memory:` creates a **separate** in-memory database per connection. A `SqlitePool` with `min_connections > 1` (or `max_connections > 1` and varying load) hits "no such table: session_events" intermittently because some connections never saw the migration.

**Fix**: either

- Pin the test pool to `max_connections = 1` so all queries hit the same in-memory DB; or
- Use `sqlite:file::memory:?cache=shared` and configure the pool with `SqliteConnectOptions::in_memory(true)` plus the shared-cache flag.

Single-connection is simpler for tests. Document the choice in `roundtrip.rs` rustdoc; the next developer who tries to parallelize the test suite will reintroduce the bug otherwise.

### 3. Projecting `Compacted` as `Item::System` interacts badly with the provider translators

`project()` emits `Item::System { content: [Text { summary }] }` for each `Compacted` event. The model context then contains a system message inserted mid-conversation, intended as "this replaces turns 1..N."

But the SMA-316 OpenAI translator concatenates multiple `Item::System` items into one system block (per its translation table), and the SMA-317 Anthropic translator hoists every `Item::System` into Anthropic's single top-level `system` field — losing the positional meaning entirely. After the snapshot reaches the model:

- On Anthropic, the compaction summary becomes part of the global system prompt, applied to every turn, ahead of the *original* system prompt or concatenated with it. The "replaces turns 1..N" semantic is lost.
- On OpenAI, the compaction summary becomes part of a concatenated system message instead of a positional marker.

This isn't a SMA-318 implementation bug — it's the projection format colliding with downstream provider translation rules. SMA-318 either picks a different `Item` variant (e.g. `Item::AssistantMessage` with `agent: Some("__paigasus_summary__")`), or accepts the global-system behavior and **documents it loudly** in the `Compacted` event docstring and in the projection function rustdoc.

**Recommendation**: stay with `Item::System` but acknowledge the trade-off explicitly. Compaction is still useful (the model sees the summary text) — it just doesn't behave as a positional cutover. If positional behavior matters later (e.g. multi-summary sessions), revisit the projection format.

## Significant issues

### 4. `SessionError::Backend` boxing ergonomics are verbose

The spec proposes:

```rust
.map_err(|e| SessionError::Backend(Box::new(e)))
```

…at every backend call site. A typical `SqliteSession::append` will have 5–10 such call sites. The reviewer's PR diff is going to be one-line-per-query of boxing noise.

**Fix**: add a constructor:

```rust
impl SessionError {
    pub fn backend<E>(e: E) -> Self
    where E: std::error::Error + Send + Sync + 'static,
    { Self::Backend(Box::new(e)) }
}
```

Call sites become `.map_err(SessionError::backend)`. No `From` impl (which would conflict with `#[from] anyhow::Error`); no macro; same diff size as the boxing-free version.

### 5. `SqliteSession::open` is silently fragile without `migrate`

`open(pool, session_id) -> Self` is infallible and synchronous, so a user who skips `migrate(&pool).await` at startup gets a runtime "no such table" error on the first `append`. The error is mapped to `SessionError::Backend(...)` — recoverable in type but not in practice (the entire session backend is broken until the user notices and adds the missing call).

Three plausible fixes, in order of preference:

1. **Auto-migrate inside `open`**, making it `async fn open(pool, id) -> Result<Self, SessionError>`. `migrate!()` is idempotent. Cost: every `open` does one round-trip to check the migrations table. Acceptable for a session-open path that happens infrequently (once per session, not per request).
2. **Provide both:** `open_unchecked` (sync, infallible) and `open` (async, migrates). Document `open_unchecked` as "you must have migrated this pool already."
3. **Keep current shape but make the failure mode visible** — change `open`'s rustdoc to include a giant warning, and emit `tracing::warn!` from the first `append` if the table is missing (before the SQL error).

Option 1 is the right default for SMA-318's surface; option 2 keeps the perf-conscious path for embedded use cases.

### 6. `busy_timeout(5s)` is too tight for the concurrent-writers acceptance test

`concurrent_writers.rs` spawns 16 tasks × 10 events = 160 transactions. SQLite serializes writers; with `WAL` + `BEGIN IMMEDIATE`, each transaction takes ~5–50ms. On slow CI runners (Windows, ARM macOS under emulation), 160 sequential transactions can approach 5s wall-time, and the busy-timeout becomes an intermittent flake source.

**Fix**: either bump the test's busy timeout to 30s (or 60s), or scale down to 8 writers × 5 events. The wider timeout is cheaper to maintain than a flaky test.

The spec already notes "If Windows trips on file-locking semantics … fallback is `#[cfg(unix)]`" for `persistence.rs`. Apply the same caution proactively to `concurrent_writers.rs`.

### 7. `SequenceId(u64)` cast through `usize` and `i64`

Three implicit casts the spec doesn't address:

- `MemorySession::events`: `since.map(|s| s.0 as usize + 1)`. On 32-bit targets (Windows i686, embedded), `usize = u32`, so `u64 → usize` wraps silently past `u32::MAX`. Practically unreachable (4B events in one session is unrealistic) but a `try_into()` with a clear panic message is one line.
- `SqliteSession::append` storing sequence: the spec writes `INTEGER` (SQLite int64) but `SequenceId.0` is `u64`. Past `i64::MAX` (9 quintillion), the cast wraps to negative. Unreachable in practice.
- `ts_nanos` truncation: spec acknowledges the 2262 cliff explicitly. ✓ Documented.

**Recommendation**: add a `try_into` boundary at the `MemorySession` site (cheapest fix; preserves correctness on 32-bit). The other two casts are documented or unreachable — fine as-is.

### 8. `Compacted` projection edge cases should `tracing::warn!`

The spec says permissive handling of weird Compacted shapes is "best-effort, no error":

- `original_count = 0` → adds a system message representing nothing.
- `original_count > events_seen` → clamps to 0; all preceding messages dropped silently.

Both indicate producer bugs (or a corrupted log). Without telemetry, a downstream user can't distinguish "the projection is doing the right thing" from "my log is corrupt." A single-line `tracing::warn!(...)` at each edge case surfaces the anomaly without changing behavior.

## Smaller items

- **`(session_id, ts_nanos)` index missing.** The migration creates only the PK index. The spec justifies `ts_nanos`/`kind` denormalization with "for ad-hoc querying (e.g., all tool calls in the last hour)" — but without a secondary index, that query is a full table scan per session. Either add the index (`CREATE INDEX session_events_session_ts ON session_events(session_id, ts_nanos)`) or drop the denormalization rationale.
- **No `delete_session` / `truncate_session` API.** Out of scope but worth a non-goal line. Application-layer code currently has no clean way to expire old sessions from the SQLite store.
- **Per-commit scoping for release-plz.** The spec touches both `paigasus-helikon-core` (adds `ts` to `SessionEvent`, adds `SessionError::Backend`) and the new `paigasus-helikon-sessions-sqlite` crate. Per CLAUDE.md's release-plz rules, commits attribute version bumps per-crate via conventional-commit scope. The implementation PR needs at least two commit scopes: `feat(core): SMA-318 add timestamps to SessionEvent / Backend error variant` and `feat(sessions-sqlite): SMA-318 implement SqliteSession + MemorySession promotion`. Worth one line in §"Crate layout" to flag this for the implementer.
- **Facade `pub use` line not shown.** §"Crate layout" mentions the kebab→snake feature alias but doesn't show the `#[cfg(feature = "sessions-sqlite")] pub use paigasus_helikon_sessions_sqlite as sessions_sqlite;` line. Minor doc gap; same pattern as SMA-316 / SMA-317.
- **`Compacted.summary: String` flattens multimodal content.** Known trade-off (audit trail keeps the originals; projection sends summary text only). Fine.
- **Acceptance #3 (ADR compliance) has no compile-time enforcement.** Relies on grep audit + code review. Could pin with a `static_assertions`-style check that `dyn Session` has only the three trait methods, but that's overkill for an MVP backend. Accept the review-gate approach.
- **`#[non_exhaustive]` on `SessionEvent` enum-level only.** Downstream crates (including `paigasus-helikon-sessions-sqlite` tests) need to struct-init the variants to construct fixtures with pinned timestamps. Enum-level `#[non_exhaustive]` permits this; per-variant `#[non_exhaustive]` would not. The spec already keeps the enum-level form ✓ — but worth a one-liner in the rustdoc clarifying that variant construction is the intended downstream pattern (so a well-meaning contributor doesn't tighten this later).

## Verdict

This is a small, surgical ticket — the kind that's easy to land cleanly. The four pre-merge fixes are all minor:

- **#1 (`+ 'static` bound)** — one token.
- **#2 (in-memory pool config)** — one test-setup line plus a rustdoc comment.
- **#3 (projection-vs-translator interaction)** — accept and document, or pick a different `Item` variant.
- **#4 (`SessionError::backend` constructor)** — five lines.

Items 5–8 are pre-merge cleanups; the smaller list is land-as-implemented. Once items 1–4 are settled the spec is ready.

The architectural decisions (jiff over chrono, `std::sync::Mutex` over `tokio::sync::Mutex` for the in-memory backend, `i64` nanos with the 2262 cliff documented, denormalized `kind`/`ts_nanos` for query ergonomics, `BEGIN IMMEDIATE` + WAL for concurrent writers, PK-as-uniqueness-backstop) are all the right calls and well-justified in the spec.

## Sources

- [`docs/superpowers/specs/2026-05-27-sma-318-session-backends-design.md`](./2026-05-27-sma-318-session-backends-design.md)
- [Linear SMA-318](https://linear.app/smaschek/issue/SMA-318/memorysession-sqlitesession-backends)
- [Notion — Sessions](https://www.notion.so/355830e8fbaa81d79e15d62ac40954e8)
- [Notion — ADR-3 Session is an append-only event log](https://www.notion.so/355830e8fbaa81138ea6ed20f0d3d257)
- `crates/paigasus-helikon-core/src/session.rs`
- `crates/paigasus-helikon-core/tests/object_safety.rs`
- root `Cargo.toml`
