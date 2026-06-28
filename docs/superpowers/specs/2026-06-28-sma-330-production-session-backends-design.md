# SMA-330 — Production session backends: Postgres, Redis, Compacting wrapper

- **Linear:** SMA-330 (`feature/sma-330-production-session-backends-postgres-redis-compacting`)
- **Status:** design — spec-challenged **twice** 2026-06-28 (both verdicts: approve-with-changes; round-2 confirmed the two-PR release plan correct and folded in the resume-seed BLOCKER + required-gate hardening). **GATE-1 decisions:** `sessions-it` is a **required** check; delivery is **two PRs** (Alternative B).
- **Date:** 2026-06-28
- **Related:** SMA-392 (wire `Session` persistence into the run lifecycle) — consumes these backends; out of scope here.

## 1. Problem & context

Today the SDK ships only two `Session` backends:

- `MemorySession` — in `paigasus-helikon-core`, a `Mutex<Vec<SessionEvent>>`; ephemeral, single-process.
- `SqliteSession` — `paigasus-helikon-sessions-sqlite`, single-node, file-backed.

Production deployments need **durable** storage (Postgres), **low-latency shared** storage (Redis), and a way to keep the projected context window **bounded** as conversations grow (compaction). This ticket adds all three, each implementing the existing `core::Session` trait, and — to literally satisfy the acceptance criterion *"the same conformance suite as Memory/SQLite"* — extracts that suite into a shared, reusable harness.

### Existing surface this builds on (verified, do not re-derive)

`core::Session` (`crates/paigasus-helikon-core/src/session.rs`):

```rust
#[async_trait]
pub trait Session: Send + Sync {
    async fn append(&self, events: &[SessionEvent]) -> Result<(), SessionError>;
    async fn events(&self, since: Option<SequenceId>) -> Result<Vec<SessionEvent>, SessionError>;
    async fn snapshot(&self) -> Result<ConversationSnapshot, SessionError>;
}
```

- `since` is an **exclusive** watermark; `SequenceId(s)` returns events at positions strictly `> s`. `SequenceId` is a positional `u64` (0-based); the trait does **not** return sequence ids to callers, so each backend's sequence numbering must align to the same 0-based contiguous positions Memory/SQLite use.
- `SessionEvent` already has the `Compacted { summary, original_count: u64, ts }` variant **and** `SessionEvent::compacted(summary, original_count)` constructor. **No core enum change is needed for compaction.**
- `project(events) -> ConversationSnapshot` already handles `Compacted`: a `Compacted { original_count: n }` marker **drops the `n` events immediately preceding it** (by message-contribution count, where `HandoffOccurred` contributes 0) and replaces them with one `Item::System { summary }` *at that position*; it truncates its `contributions` bookkeeping so a later `Compacted` indexes only over the events after the previous one. Verified against `crates/paigasus-helikon-core/tests/session_projection.rs` (`compaction_replaces_window_with_single_system_message`, `two_consecutive_compactions_chain`, `compaction_with_oversized_count_clamps_to_zero`).
- `SqliteSession::append` uses `BEGIN IMMEDIATE` + `SELECT COALESCE(MAX(sequence),-1)+1` to allocate a contiguous sequence safely under concurrent writers; `events` filters `sequence > ?` (default `-1`); a private `event_metadata(&SessionEvent) -> (&'static str, i64)` extracts the serde tag (`kind`) and a saturating-`i64` `ts_nanos` for the audit index, with a `_ => panic!` for unhandled `#[non_exhaustive]` variants.
- `core::Model` (`crates/paigasus-helikon-core/src/model.rs`): `async fn invoke(&self, ModelRequest, CancellationToken) -> Result<BoxStream<'static, Result<ModelEvent, ModelError>>, ModelError>`. `ModelRequest { messages: Vec<Item>, tools: Vec<ToolDef>, model_settings: ModelSettings }`. Stream yields `ModelEvent::{TokenDelta{text}, ReasoningDelta, ToolCallDelta, Usage{..}, Finish{reason}}`.
- No token-counting utility exists anywhere in the workspace.
- core's deps include `futures-util` (drive the model stream) and `tracing`; **no `tokio`** except as a dev-dependency.

## 2. Goals / non-goals

**Goals**

1. `PostgresSession`, `RedisSession`, `CompactingSession<S>` implementing `core::Session`.
2. A single shared conformance suite (append, read/watermark, projection, concurrent writers) run by Memory, SQLite, Postgres, and Redis.
3. `CompactingSession` reduces the projected token count below the threshold after compaction fires **when the produced summary is shorter than the threshold** (see the convergence note in §4.2).
4. Facade wiring, crate READMEs, mdBook Sessions page, CI, and release plumbing all updated in the same PR.

**Non-goals (explicitly deferred)**

- **Keep-recent-window compaction** (summarize an older prefix while keeping the last *K* turns verbatim). The current `project()` collapses events *preceding* the marker, so a running full-history summary is the only shape expressible without changing core. Window compaction is a future ticket touching `project()`, its tests, and the provider-translator caveat.
- Redis `events(since)` cursor optimization (O(n) full-stream read for now).
- CompactingSession snapshot-caching (the at-compaction O(total-events) replay is accepted; a cache is future work).
- Concurrent appends *through* a single `CompactingSession` (single-writer-per-session by design — §4.2).
- Redis stream trimming/retention (the contiguous-`seq` invariant assumes no trim — §7).
- Run-lifecycle wiring (SMA-392).

## 3. Crate roster changes

| Piece | Crate | Version / publish |
|---|---|---|
| `CompactingSession<S>`, `TokenCounter`, `HeuristicTokenCounter` | `paigasus-helikon-core` (additive) | core patch bump |
| `SessionEvent::{kind, ts, ts_nanos_saturating}` accessors | `paigasus-helikon-core` (additive) | (same bump) |
| `PostgresSession` | **new** `paigasus-helikon-sessions-postgres` | `0.1.0`, publishes |
| `RedisSession` | **new** `paigasus-helikon-sessions-redis` | `0.1.0`, publishes |
| Shared conformance suite | **new** `paigasus-helikon-sessions-testkit` | `0.0.0`, `publish=false`, never published |

Workspace inheritance is mandatory (only `name`, `description`, crate-specific bits per crate). Each new published crate copies the `[lints] workspace = true` opt-in block.

## 4. Core additions

### 4.1 `TokenCounter` + `HeuristicTokenCounter`

```rust
/// Estimates the token cost of a projected conversation, so a CompactingSession
/// can decide when to summarize. Pluggable so users can supply a model-accurate
/// tokenizer; the default is a cheap, deterministic heuristic.
pub trait TokenCounter: Send + Sync {
    fn count(&self, items: &[Item]) -> usize;
}

/// Default `TokenCounter`: ceil(total UTF-8 text bytes / 4) across every
/// `ContentPart::Text`/`Reasoning` in every item. Deterministic; no deps.
#[derive(Debug, Clone, Default)]
pub struct HeuristicTokenCounter;
```

- The heuristic returns `total_chars.div_ceil(4)`, where **`total_chars` is the count of Unicode scalar values (`str::chars().count()`), not UTF-8 bytes** — a fixed unit so implementers don't diverge on multibyte text (`div_ceil` is stable well before MSRV 1.94). **Exact enumeration** of what contributes (so the AC's exact-count assertion is reproducible): `ContentPart::Text.text`, `ContentPart::Reasoning.text`, `Item::ToolCall.name` + `Item::ToolCall.args` (compact JSON), `ContentPart::ToolUse.name` + `.args` (compact JSON), recursing into nested `ContentPart::ToolResult.content`. `ContentPart::Image`/`Audio` source parts contribute **0**.
- It **must** count `Item::System` content (the running summary), since CompactingSession's convergence depends on measuring the post-compaction snapshot.
- Non-text parts (image/audio sources) contribute 0 — we only bound text growth; image bytes are not in the projected text.
- Deterministic so the AC test can assert an exact below-threshold count.

### 4.2 `CompactingSession<S>`

```rust
pub struct CompactingSession<S: Session> {
    inner: S,
    model: Arc<dyn Model>,
    counter: Arc<dyn TokenCounter>,
    threshold: usize,           // builder rejects 0 (a 0 threshold would never compact)
    settings: ModelSettings,
    prompt: String,             // summarization instruction text
    cheap_estimate: AtomicUsize,// running char-count since last compaction; init usize::MAX
    compacting: AtomicBool,     // single-flight guard
}
```

Construction via a small builder (`CompactingSession::builder(inner, model).threshold(n)…build()`) with defaults: `HeuristicTokenCounter`, a built-in summarization prompt, `ModelSettings::default()`. The builder **rejects `threshold == 0`** (returns a build error / debug-panics) — `tokens <= 0` would never fire.

**Concurrency contract (important):** `CompactingSession` assumes a **single logical writer per session** — the normal runner usage where one run owns the session and appends serially. The inner backend remains fully durable and concurrency-safe (so the data is never lost), but the compaction *bookkeeping* (read-count-then-append-marker, §steps below) is **not** atomic against another `append` interleaving through the same wrapper: an event slipped in between the count and the marker write would make `original_count` stale and cause `project()` to drop un-summarized events. Therefore `CompactingSession` is **not** claimed to pass `run_concurrent_writers` and is documented as single-writer. The `AtomicBool` guards only against a *re-entrant/overlapping compaction*, not against concurrent user appends. (Closing the race fully would require serializing every append through an async lock, which would defeat the inner backend's concurrency for no benefit in the single-owner runner model — deferred.)

`Session` impl:

- `append(events)`:
  1. `inner.append(events).await?` (propagate inner errors — the durable write must not be masked).
  2. Add the new events' cheap char-estimate to `cheap_estimate`.
  3. `self.maybe_compact().await` — **best-effort**: any error (LLM unavailable, summary read failure) is logged at `warn!` and swallowed. The user's events are already persisted; compaction is an optimization, and failing `append` after a successful inner write would be surprising. `append` returns `Ok(())`.
- `events(since)` → `inner.events(since)`.
- `snapshot()` → `inner.snapshot()` (already projects, collapsing `Compacted`).

`maybe_compact()` (synchronous within `append`, so the AC is deterministic — no background race):

1. **Cheap perf gate:** if `cheap_estimate <= threshold * 4` (chars≈tokens·4), return without reading the log. `cheap_estimate` is **initialized to `usize::MAX`**, so the **first** `maybe_compact` after construction always fails this gate and runs the authoritative path — which seeds the estimate from the inner log's real size. **This is what makes resume correct:** a `CompactingSession` wrapping an already-populated durable backend (the primary Postgres/Redis use case) compacts its existing backlog on the first append, rather than silently treating the resumed session as empty. After seeding, the common-case append is O(new events) instead of O(total log); the authoritative path runs only when the cheap estimate suggests we're near the threshold. (Inherent cost: when it *does* run, `events(None)` + `project` is O(total events), because event-sourced projection must replay from the start. Acceptable for an MVP; a snapshot-cache optimization is noted as future work.)
2. **Single-flight:** `let guard = match self.compacting.compare_exchange(false, true, AcqRel, Acquire) { Ok(_) => Guard, Err(_) => return Ok(()) };` — the RAII `Guard` is constructed **only on the swap-won path** and resets the flag to `false` on drop (never resetting a flag we didn't set). No lock is held across the `await`.
3. Read `let evs = self.inner.events(None).await?;` → `let snap = project(&evs);` → `let tokens = self.counter.count(&snap.messages);`.
4. If `tokens <= self.threshold` → reset `cheap_estimate` to `tokens * 4` (re-sync the cheap estimate to reality) and return.
5. **Guard on *messages*, not events:** if `snap.messages.len() <= 1` → return. A lone running summary (or empty snapshot) has nothing useful to collapse; keying this on the projected message count (rather than the raw event count) avoids the `[Compacted, handoff]` ping-pong where two raw events project to a single `System`.
6. `live = evs.len() - last_compacted_index(&evs)` where `last_compacted_index` returns the index of the last `Compacted` event **inclusive** (returns `0` when there is none ⇒ `live = evs.len()`).
7. Build `ModelRequest { messages: <snap.messages> ++ [Item::UserMessage(prompt)], tools: vec![], model_settings: self.settings.clone() }` — the instruction is a **trailing `Item::UserMessage`** carrying `self.prompt` (default: *"Summarize the conversation so far into a concise summary, preserving key facts, decisions, and open questions."*). Drive `model.invoke(req, CancellationToken::new()).await?`, collecting `ModelEvent::TokenDelta { text }` into `summary` until `Finish`.
8. **Empty-summary guard:** if `summary` is empty/whitespace-only, log `warn!` and return without appending a marker (a `Compacted{summary:""}` would project to `[System("")]`, useless).
9. `self.inner.append(&[SessionEvent::compacted(summary, live as u64)]).await?`; reset `cheap_estimate` to the new summary's char-estimate.

**Correctness of `original_count = live`** (so the result is exactly `[System(summary)]`): when `project` reaches the new marker, its `contributions.len()` equals `live` (one entry per event since the previous `Compacted`, whose entries were truncated). `drop_from_idx = len - live = 0` ⇒ every prior message dropped ⇒ `[System(summary)]`; and `live == contributions.len()` (not `>`), so the "references more events than seen" `warn!` does **not** fire. The summarization instruction is **not** an event (it's only in the `ModelRequest`), so it does not affect the count. The dedicated test asserts the snapshot **equals** `[System(summary)]`; it does not assert on the absence of a log line (that property follows from the equality and is not separately observable without a tracing capture).

**Convergence & limits.** Compaction lowers the count below `threshold` only when the model's summary is itself shorter than `threshold`; the `messages.len() <= 1` guard deliberately refuses to re-compact a lone running summary, so an over-long summary is left as-is rather than ping-ponged (logged at `warn`). Two operational constraints follow and are **documented on the type**: (1) `threshold` must sit comfortably **below the summarization model's context window** (step 7 sends the *entire* projected history to the model, so a threshold at/above the model's limit makes the summarization call itself fail with a context-length error — swallowed best-effort — exactly when compaction is most needed); (2) the summarization model should reliably produce summaries materially shorter than `threshold`. The builder default `threshold` is chosen with headroom against common context windows.

### 4.3 `SessionEvent` accessors (de-dup)

```rust
impl SessionEvent {
    pub fn kind(&self) -> &'static str;            // serde tag: "user_message", "compacted", …
    pub fn ts(&self) -> Timestamp;                 // the variant's ts
    pub fn ts_nanos_saturating(&self) -> i64;      // i128→i64 saturating, for audit-index columns
}
```

`kind()`/`ts()` `match` all variants with **no `_ =>` arm**, so adding a future `#[non_exhaustive]` variant is a **single compile failure in core** (not three silent panics across backends). `SqliteSession::event_metadata` is refactored to `(ev.kind(), ev.ts_nanos_saturating())`; Postgres and Redis reuse the same accessors.

**Release consequence (drives §10):** the new accessors are consumed by the refactored sqlite crate (PR-1) and the new postgres/redis crates (PR-2). The **two-PR split** (GATE-1 decision) resolves the publish-verify-against-stale-registry-core trap *without* any manual version bumps: PR-1 ships core + sqlite together, and release-plz's release PR publishes them in **dependency order** (core first, then sqlite verifies against the just-published core); PR-2's new crates are authored *after* PR-1's core is on crates.io, so they verify against a registry core that already has the accessors. See §10.

## 5. Shared conformance harness (`-sessions-testkit`)

Unpublished crate (`publish = false`, `version = "0.0.0"`, `release = false` in `release-plz.toml`, **not** in the facade). Depends on `paigasus-helikon-core`.

Public API (all generic over a session factory that yields a **fresh, empty** session):

```rust
pub async fn run_append_read<F, Fut>(make: F) where F: Fn() -> Fut, Fut: Future<Output = Arc<dyn Session>>;
pub async fn run_watermark_exclusive<F, Fut>(make: F) …;   // SequenceId(2) ⇒ positions 3,4
pub async fn run_projection<F, Fut>(make: F) …;            // snapshot() == project(events())
pub async fn run_concurrent_writers<F, Fut>(make: F) …;    // clone the Arc, 16×10 appends, every event present once
pub async fn run_all<F, Fut>(make: F) …;                   // the four above
```

- `run_concurrent_writers` clones the returned `Arc<dyn Session>` across 16 tasks × 10 appends and asserts the read-back total and that every sent event is present exactly once — the existing sqlite invariant, lifted.
- **Factory contract:** `make` is `Fn() -> Fut` and is invoked **once per sub-test** inside `run_all`; each call must return a **fresh, empty** session. Backing resources (a `TempDir`+WAL pool for sqlite, a `PgPool` for postgres, a `ConnectionManager` for redis) are owned by the **caller's test scope** and captured by reference in the closure, so they outlive every `make()` call; the closure mints a **unique `session_id` per call** (a process-unique counter/uuid suffix) so runs against a shared CI server (postgres/redis) and a shared sqlite file never collide. Sqlite's concurrency sub-test specifically requires a **file-backed WAL pool** (an in-memory `max_connections=1` pool cannot model concurrent writers).
- testkit's own `tests/memory.rs` runs `run_all` against `MemorySession`, anchoring "**the same** suite Memory passes." Memory's factory returns a brand-new `MemorySession` each call (no id needed).
- **Migration under parallel tests (postgres):** `cargo test` runs the backend's `#[tokio::test]`s in parallel against one shared CI database, so the postgres harness must `migrate()` **once** before constructing factory sessions (or rely on sqlx's Postgres migrator advisory-lock, which serializes concurrent `migrate()` calls). Spec choice: migrate once at test-module setup; factory `make()` then only opens sessions with unique ids. (sqlite/redis have no shared-migration concern — sqlite migrates its own temp file; redis has no schema.)
- **Consumption:** each backend crate adds a **path-only, version-less** dev-dependency `paigasus-helikon-sessions-testkit = { path = "../paigasus-helikon-sessions-testkit" }` (SMA-326 pattern → omitted from published manifests, no version-pin/publish-cycle trap) and a `tests/conformance.rs` calling `run_all(make)`.
- **SQLite retrofit:** add `tests/conformance.rs` to the sqlite crate using the harness; keep its backend-specific tests (`persistence.rs`, `multi_session.rs`). The hand-rolled overlap in `roundtrip.rs`/`concurrent_writers.rs` may be slimmed to avoid duplication but is not required to be deleted.
- **doc-coverage / missing_docs:** `scripts/check-doc-coverage.sh` discovers **all** workspace members via `cargo metadata` and excludes only the CLI by name, and testkit opts into `[lints] workspace = true` (so `missing_docs` applies). testkit's public `run_*` fns are few — **document all of them with `///`** (the recommended path; satisfies both the required `doc-coverage` gate *and* the `docs` job's `missing_docs = warn` + `RUSTDOCFLAGS=-D warnings`). Note the `EXCLUDED_CRATES` route is **not sufficient alone**: it removes a crate from the doc-coverage aggregate only — to also pass the `docs` job, testkit would have to mirror the CLI exactly (`[lints.rust] missing_docs = "allow"`, dropping `[lints] workspace = true`). Just document the fns.

## 6. `PostgresSession` (`-sessions-postgres`)

API mirrors `SqliteSession`: `migrate(&PgPool)`, `open(PgPool, id)`, `open_without_migrate(PgPool, id)`, `session_id()`.

`migrations/0001_session_events.sql` (Postgres dialect):

```sql
CREATE TABLE IF NOT EXISTS session_events (
    session_id TEXT   NOT NULL,
    sequence   BIGINT NOT NULL,
    ts_nanos   BIGINT NOT NULL,
    kind       TEXT   NOT NULL,
    payload    JSONB  NOT NULL,
    PRIMARY KEY (session_id, sequence)
);
CREATE INDEX IF NOT EXISTS idx_session_events_session_ts
    ON session_events (session_id, ts_nanos);
```

(`PRIMARY KEY (session_id, sequence)` is the `(session_id, sequence)` index the ticket asks for; the second index is the timestamp index.)

`append` — empty input is a no-op early return (like sqlite). Otherwise, all statements run **inside one `sqlx` transaction bound to a single pooled connection** (`let mut tx = pool.begin().await?; … execute(&mut *tx).await?; tx.commit().await?;`) — **never** as separate `pool`-level `query()` calls, or the advisory lock (held on one connection) would not cover the `INSERT` (issued on another), silently breaking concurrency. The transaction takes a **per-session advisory lock**:

```text
let mut tx = pool.begin().await?;                                  // one connection for the whole txn
// per-session lock, auto-released at COMMIT; all on &mut *tx:
sqlx::query("SELECT pg_advisory_xact_lock(hashtext($1))").bind(&id)…
let next: i64 = "SELECT COALESCE(MAX(sequence), -1) + 1 FROM session_events WHERE session_id = $1"…
// one INSERT per event, sequence = next + offset, payload bound as JSONB:
"INSERT INTO session_events (session_id, sequence, ts_nanos, kind, payload) VALUES ($1,$2,$3,$4,$5)"…
tx.commit().await?;
```

Finer-grained than sqlite's whole-DB `BEGIN IMMEDIATE` — writers to *different* sessions usually don't contend. Caveat: `hashtext` is a **32-bit** hash, so two distinct `session_id`s can collide into the same advisory-lock key, causing occasional false cross-session contention. Correctness is preserved (the lock still serializes); only throughput is affected. Use `hashtextextended($1, 0)` (64-bit) if collisions prove material.

`events(since)`: `SELECT payload FROM session_events WHERE session_id = $1 AND sequence > $2 ORDER BY sequence`, watermark default `-1`, payload fetched as `sqlx::types::Json<SessionEvent>`.

`snapshot()`: `project(&self.events(None).await?)`.

**Query API:** use **runtime** `sqlx::query()`/`query_as()` exclusively, mirroring sqlite — **never** the compile-checked `sqlx::query!`/`query_as!` macros, which require `DATABASE_URL` or an offline cache **at compile time on every matrix platform** (including the serverless macOS/Windows jobs) and would break the build everywhere.

**Dependencies:** declared in the postgres crate's **own** Cargo.toml as `sqlx = { workspace = true, features = ["postgres", "<rustls-aws-lc-rs tls feature>"] }` (the workspace base pins `sqlite`+`macros`+`migrate`+`runtime-tokio`; cargo unions the extra `postgres`/tls features). The TLS feature **must** be the aws-lc-rs rustls variant (sqlx 0.8+ exposes `tls-rustls-aws-lc-rs`; verify the exact 0.9 name at impl time) — *not* a ring variant — to match the workspace `CryptoProvider` and avoid the dual-provider panic under the required `--all-features` test (the gcp_auth/aws-lc-rs memory). **Fallback if sqlx 0.9 ships no aws-lc-rs rustls feature** (unlikely — verify first): omit sqlx's TLS feature entirely and have `PostgresSession` accept a caller-built `PgPool`/`PgConnectOptions` (the same BYO-connection TLS story as redis), rather than introducing a ring or native-tls stack. Feature unification means the sqlite crate's sqlx also compiles the postgres driver under `--all-features`; benign.

## 7. `RedisSession` (`-sessions-redis`) — one Redis Stream per session

`redis` crate with tokio async (`ConnectionManager` — auto-reconnect, cheap clone, shareable). Constructors: `RedisSession::new(ConnectionManager, id)` (primary — the caller supplies a connection) and `RedisSession::connect(url, id)` (convenience, **plaintext**). Stream key `helikon:session:{id}:events`.

**TLS / CryptoProvider:** the crate's `redis` dependency enables **no rustls TLS feature** — `redis`'s rustls features are historically ring-backed, and enabling one would register a second `CryptoProvider` and reproduce the dual-provider panic under the required `--all-features` test (same failure mode as the sqlx/gcp_auth memory). Managed-Redis (TLS) users build their own TLS-configured `ConnectionManager` with the workspace's aws-lc-rs provider and pass it to `RedisSession::new`. Pin in `[workspace.dependencies]` as `redis = { version = "<latest>", default-features = false, features = ["tokio-comp", "connection-manager", "streams", "script"] }` (`connection-manager` is required for `ConnectionManager`; `tokio-comp` alone does not enable it). (Rationale for keeping TLS out of the crate's features: a crate feature is force-enabled by `--all-features`, so even an opt-in ring-backed TLS feature would trip the dual-provider panic in the required test job. The user-supplied-`ConnectionManager` path is therefore the *only* TLS story, regardless of whether `redis` ships an aws-lc-rs variant.)

Entry fields per event: `seq` (contiguous int), `kind`, `payload` (JSON), `ts`.

`append` empty input is a no-op early return (no EVALSHA round-trip).

`append` — atomic contiguous sequence via a cached Lua script (`redis::Script`, EVALSHA):

```lua
local n = redis.call('XLEN', KEYS[1])
for i = 0, (#ARGV / 3) - 1 do
  redis.call('XADD', KEYS[1], '*',
    'seq', n + i, 'kind', ARGV[i*3+1], 'payload', ARGV[i*3+2], 'ts', ARGV[i*3+3])
end
return n
```

Redis runs the whole script atomically (single-threaded), so `XLEN → XADD` cannot interleave across concurrent appends → `seq` is contiguous and unique (passes `run_concurrent_writers`).

`events(since)`: `XRANGE key - +`, parse each entry, deserialize `payload` → `SessionEvent`, filter `seq > since`, preserve order. O(n) read (documented; cursor optimization deferred).

`snapshot()`: `project(&self.events(None).await?)`.

**Retention caveat:** the `XLEN == max_seq + 1` invariant that makes `seq` contiguous holds **only if the stream is never trimmed**. The crate therefore does **not** call `XADD … MAXLEN`/`XTRIM`, so per-session memory grows with the log, and a Redis instance configured with a `maxmemory` + key-eviction policy that can evict the stream would both lose data **and** break the sequence invariant. Documented as an operational constraint (run the session keyspace with `noeviction`, or accept that compaction-via `CompactingSession` is the bound on growth). A trimming/retention strategy is future work.

Backend-specific tests (env-gated): reconnect-persistence (new `ConnectionManager`, same key, read back), multi-key isolation.

## 8. Facade wiring

- `crates/paigasus-helikon/Cargo.toml`: optional deps `paigasus-helikon-sessions-postgres`, `paigasus-helikon-sessions-redis`; features `sessions-postgres = ["dep:…-postgres"]`, `sessions-redis = ["dep:…-redis"]`.
- `crates/paigasus-helikon/src/lib.rs`: `#[cfg(feature = "sessions-postgres")] pub use … as sessions_postgres;` and redis equivalent, each with a `///` doc comment (missing_docs is `-D warnings` in the docs job).
- `CompactingSession`/`TokenCounter` live in core, reachable as `paigasus_helikon::core::CompactingSession` — no new facade feature.
- Root `Cargo.toml` `[workspace.dependencies]`: add the two published crates (path + `version = "0.1.0"`). testkit is referenced only by backend dev-deps via direct relative path, not via `[workspace.dependencies]`.
- **Third-party pins (mandatory per CLAUDE.md):** add `redis` to `[workspace.dependencies]` (features as in §7); the postgres `sqlx` driver/TLS feature is added on the postgres crate's own `sqlx` line (§6) but reuses the workspace `sqlx` pin. Members reference these via `dep.workspace = true`.
- **Supply-chain vetting (required `audit` + `deny` gates):** the new dep graph (`redis`, `sqlx-postgres`'s transitives — e.g. `md-5`, `whoami`, `stringprep`) must pass `cargo deny check` and `cargo audit`. Pre-run both during implementation; if a new license appears (e.g. an MIT/BSD transitive not yet allow-listed) add it to `deny.toml`'s `licenses.allow` via a `chore(deps)`-style change, and note any advisory exposure. Do this **before** opening the PR so the gates are green on first push.

## 9. CI

New job in `.github/workflows/ci.yml`, a **required** check (GATE-1 decision — it is the *only* gate that runs the Postgres/Redis concurrent-writers AC, mirroring the macOS/Seatbelt "only gate that exercises X ⇒ required" precedent). It is introduced in **PR-2** (alongside the postgres/redis crates) and added as a required context to `.github/rulesets/main-protection-checks.json`; it therefore reports on PR-2 itself (the workflow lives on PR-2's branch) and gates every PR thereafter. PR-1 (core/testkit/sqlite) does **not** include it — there are no postgres/redis crates to exercise yet, so PR-1's required set is the existing one.

**Job shape (required ⇒ must always report, but must not drag Docker onto every PR).** A required context is only satisfied by a job that *runs to success on the head commit of every gated PR*. A job that is `if:`-skipped reports as skipped and **blocks** a required context; and job-level `services:` start unconditionally whenever the job runs — so a naive `services:`-based job would spin up Postgres+Redis (and depend on Docker Hub) on *every* PR, including docs and bot PRs. The design therefore is a single **always-running** `sessions-it` job with an **in-job path filter** that starts containers only when relevant:

```yaml
sessions-it:                       # the required context name; runs on every PR (no job-level paths:)
  runs-on: ubuntu-latest
  steps:
    - checkout (pinned SHA)
    - id: filter  (dorny/paths-filter, pinned SHA)   # sessions = crates/*sessions-*/**, core/src/session.rs, this workflow, Cargo.lock
    - if steps.filter.outputs.sessions == 'false':  echo "no session changes"; exit 0   # fast green, NO Docker
    - if 'true':  rust-toolchain + rust-cache (pinned SHAs)
    - if 'true':  start postgres + redis via `docker run` (pinned by DIGEST, pulled from a GHCR mirror to
                  dodge Docker Hub anon rate limits), wait on health, set HELIKON_TEST_*_URL
    - if 'true':  cargo test -p …-postgres -p …-redis   (wrapped in a bounded retry, e.g. nick-fields/retry pinned SHA)
```

`HELIKON_TEST_POSTGRES_URL=postgres://postgres:postgres@localhost:5432/postgres`, `HELIKON_TEST_REDIS_URL=redis://localhost:6379`. Containers are pinned by **digest** (not just tag) and mirrored to GHCR; the `cargo test` is retry-wrapped so a transient pull/connection blip self-heals instead of blocking `main`. Non-session PRs hit the fast-green path and never touch Docker, so the required context reports in seconds without importing container flakiness.

- Action `uses:` pin to the **same commit SHAs** already in `ci.yml`.
- Everywhere else (`test` matrix `--workspace --all-features` on ubuntu/macos/windows × {stable, 1.94}) the Postgres/Redis tests **loud-skip** when the envs are unset: each is a real `#[tokio::test]` that, with no URL, `eprintln!("SKIP: …")` and returns `Ok` (forkd `tests/forkd_live.rs` pattern). They still **compile** on every platform (both clients are cross-platform pure-Rust).
- Image tags (`postgres:17`, `redis:7`) confirmed current at implementation time.
- **Lib-only build verification:** also run `cargo build -p paigasus-helikon-sessions-postgres -p paigasus-helikon-sessions-redis` (no dev-deps, no `--all-features`) somewhere in CI or as a pre-PR check — per the reqwest-feature-gating memory, dev-deps (testkit, tokio) and `--all-targets` can mask a missing **lib** feature that downstream consumers would hit.

**Resolved at GATE 1 — required.** `sessions-it` is a **required** context (the concurrent-writers AC for Postgres/Redis is exercised nowhere else; loud-skip everywhere else would let a Redis-Lua or advisory-lock regression merge on an ignorable signal). Rollout steps in PR-2:
- Add the bare job name `sessions-it` to `.github/rulesets/main-protection-checks.json`. **That file is a checked-in *mirror*, not auto-applied** — a maintainer with admin rights must apply the ruleset to the repo (`gh api`/settings) *after* PR-2's workflow exists. Sequence: merge PR-2 (so the `sessions-it` job is on `main`), then apply the ruleset.
- **Transition hazard:** once required, any *already-open* PR whose head predates the `sessions-it` job will show "Expected — waiting for status" and be **unmergeable until rebased** (the dropped-context failure mode in the team's memory; `strict_required_status_checks_policy:false` does not waive *reporting*). After applying the ruleset, refresh/rebase open PRs (incl. the release-plz bot PR) through the transition window, or briefly bypass for them.
- The always-run + in-job-path-filter shape above is what keeps the required context *reporting* on every PR while confining Docker to session PRs.

## 10. Delivery plan & release (two PRs)

GATE-1 decision: ship as **two sequential PRs** (Alternative B). This halves review surface and **eliminates the publish-verify-against-stale-core trap with no manual version bumps** — each PR uses release-plz's normal flow and relies on its dependency-ordered publish. New published crates are created at `version = "0.1.0"` with normal `publish` (the `providers-gemini`/SMA-449 shape), **not** in the release-plz stub list. `-sessions-testkit` is added to the stub list (`publish=false` + `release=false`).

### PR-1 — core + testkit + sqlite  (branch `feature/sma-330-sessions-core-compaction-testkit`)
Scope:
- core: `CompactingSession<S>`, `TokenCounter`, `HeuristicTokenCounter`, `SessionEvent::{kind,ts,ts_nanos_saturating}` (§4).
- new `paigasus-helikon-sessions-testkit` (`publish=false`): the conformance harness + `tests/memory.rs` (§5).
- `paigasus-helikon-sessions-sqlite`: refactor `event_metadata` onto the accessors + add `tests/conformance.rs` using testkit (§5).
- docs: mdBook Sessions page (compaction concept); sqlite/testkit READMEs.

Release: **no manual bumps.** release-plz's release PR bumps core (`feat`) and sqlite (its source changed) and cascades the facade (patch), then **publishes in dependency order — core first, then sqlite verifies against the just-published core** ✓. testkit never publishes; the merged PR-1 itself publishes nothing (versions are unchanged until the release PR merges). **PR-1 must merge *and* its release PR must publish core to crates.io before PR-2 begins.**

### PR-2 — postgres + redis backends  (branch `feature/sma-330-sessions-postgres-redis`, off `main` after PR-1's release)
Scope:
- new `paigasus-helikon-sessions-postgres` + `-sessions-redis` @ `0.1.0` (§6, §7), consuming the **already-published** core accessors ⇒ their first-publish `cargo publish --verify` builds against a registry core that has them ✓.
- facade: add `sessions-postgres`/`sessions-redis` optional deps + features + `///`-doc'd re-exports (§8).
- root `[workspace.dependencies]`: add `redis` + the two new crates; supply-chain vetting (§8).
- CI: add the **required** `sessions-it` job + the ruleset context (§9).
- docs: facade/root README roster + feature→module map; mdBook backends pages.

Release: **no manual bumps.** release-plz publishes postgres/redis (verify against published core ✓) and bumps the facade for its new features (cascade), in dependency order.

### Watch-points
- After each PR merges, watch the release-plz `chore: release` PR and the publish CI (memory: the bot PR's `cargo update` can pull a fresh advisory that reddens `audit`/`deny` on the bot PR only; fix with a `chore(deps)` pin and release-plz regenerates it clean).
- Confirm release-plz publishes **core before sqlite** in PR-1's release PR (topological order). If it ever doesn't, the fallback is a manual core bump in PR-1.
- **Linear auto-close:** both PRs reference SMA-330, so **PR-1's merge will auto-transition SMA-330 to Done** while PR-2 (the riskier half) is still open. After PR-1 merges, move SMA-330 back to **In Progress** (or open a sub-issue, e.g. SMA-330b, to carry PR-2) so the tracker reflects the outstanding backend work. PR-1's body should avoid a hard "Closes" keyword; only PR-2 closes the issue.
- **testkit & release-plz:** testkit is `publish = false` (Cargo) + `release = false` (release-plz) and is referenced only as a **path-only dev-dependency**, so it is never resolved against crates.io. Unlike the existing stubs it was never pre-published at `0.0.0`; confirm release-plz simply ignores a `release = false` member that is absent from the registry (expected — `release = false` removes it from the workspace release scan).
- Both feature branches must match the `feature/**` ruleset; the current branch is renamed to PR-1's name at the start of implementation.
- Bootstrap/release-plumbing edits (CI, `release-plz.toml`, ruleset) use `chore(...)`/`docs(...)` commit types, never `feat`/`fix`.

## 11. Testing strategy → AC mapping

| Acceptance criterion | Covered by |
|---|---|
| All three pass the same conformance suite (append, read, projection, concurrent writers) as Memory/SQLite | `-sessions-testkit::run_all` invoked by Memory (in testkit), SQLite (retrofit), Postgres, Redis. Postgres/Redis runs are env-gated and executed by the `sessions-it` CI job. |
| `CompactingSession` reduces input token count below the threshold after compaction fires | core test (**sequential appends only** — CompactingSession is single-writer per §4.2, so it does **not** run `run_concurrent_writers`): deterministic `TokenCounter` + fake `Model` returning a short summary + threshold `T`; append past `T`; assert `counter.count(snapshot().messages) < T`, and that `snapshot()` equals `[System(summary)]`. Plus: `Compacted` recorded with exact `original_count`; raw `events()` retains the full log; LLM-error path leaves the log untouched and `append` still `Ok`; empty-summary path appends no marker; lone-summary guard (projected `messages.len() <= 1` ⇒ no re-fire); **resume test** — wrap an *already-over-threshold* inner session (pre-seeded events) in a fresh `CompactingSession` and assert the **first** append compacts the backlog (guards the `cheap_estimate = usize::MAX` seeding, §4.2). |

All six existing CI gates stay green: `cargo fmt`, `clippy --workspace --all-features --all-targets -D warnings`, `test` matrix, `docs` (`RUSTDOCFLAGS=-D warnings`, so every new `pub` item needs `///`), `doc-coverage` (≥80%), `commits`/`pr-title`. The two **published** crates are added to the doc-coverage aggregator like other published crates; **testkit** (unpublished, but auto-discovered by the script) is handled per §5 (document its public fns, or add to `EXCLUDED_CRATES`). **PR-2 also adds `sessions-it` as a new required gate** (§9), so PR-2 and all later PRs must show it green.

**MSRV check:** verify the chosen `redis` + `sqlx`-postgres-tls graph does not raise the floor above `rust-version = 1.94`. If cargo demands higher, bump `[workspace.package].rust-version` to what it demands (per CLAUDE.md — raise the floor, don't downgrade the dep) and update the CI `1.94` matrix label.

## 12. Docs (same PR — mandatory)

- New `crates/paigasus-helikon-sessions-postgres/README.md`, `crates/paigasus-helikon-sessions-redis/README.md` (crates.io landing pages; drift-free `cargo add` install snippets). Minimal `…-testkit/README.md` noting it is internal/unpublished.
- Update facade `crates/paigasus-helikon/README.md` and root `README.md` crate roster + feature→module map (add `sessions-postgres`, `sessions-redis`; mention `CompactingSession` in core).
- Update the mdBook Sessions page(s) under `docs/book/src/` to document the three backends + compaction semantics (full-history running summary; provider-translator caveat). `mdbook build docs/book` must stay clean (`linkcheck` warning-policy = error).

## 13. Residual risks (post spec-challenge)

The spec-challenge (2026-06-28, verdict *approve-with-changes*) is folded into §§4–11 above. Remaining risks to watch during implementation:

1. **Release sequencing (§10)** — resolved by the **two-PR** split: no manual bumps, but PR-2 is **blocked on PR-1's core actually publishing to crates.io**. The one assumption to confirm is that release-plz publishes **core before sqlite** within PR-1's release PR (topological order); fallback is a manual core bump in PR-1.
2. **CompactingSession concurrency (§4.2)** — accepted as **single-writer per session**; the inner backend stays durable, but concurrent appends *through the wrapper* are unsupported (would corrupt `original_count`). Documented, not engineered around.
3. **CompactingSession per-append cost & resume (§4.2)** — the cheap `AtomicUsize` estimate (init `usize::MAX`, so the **first** post-construction append always runs the authoritative read and seeds correctly even on a resumed/pre-populated backend) gates the O(total-events) replay so the common-case append stays cheap; the at-compaction O(n) replay is inherent to event-sourced projection and accepted for the MVP (snapshot-cache deferred). Convergence requires the summary to be shorter than `threshold` and `threshold` below the model's context window (documented on the type).
4. **TLS / dual-CryptoProvider (§6, §7)** — sqlx must use the **aws-lc-rs** rustls variant; `redis` ships **no** rustls feature (TLS via user-supplied `ConnectionManager`). Both verified against the required `--all-features` test before PR.
5. **Redis contiguous-sequence-under-concurrency (§7)** — relies on Redis Lua atomicity **and** never trimming the stream; only the `sessions-it` job exercises it for real, which is why §9 flags the required-vs-signal decision for GATE 1.
6. **`original_count = live` exactness (§4.2)** — traced and confirmed by the spec-challenge; covered by the exact-`[System(summary)]` test.
7. **New-dependency exposure (§8)** — `redis` + sqlx-postgres transitives must clear `audit`/`deny` and possibly `deny.toml` license additions; pre-vetted before PR.
8. **MSRV (§11)** — verify the new graph stays at/under 1.94; bump the floor if cargo demands.
9. **`HeuristicTokenCounter` accuracy** — deliberately approximate (chars/4); acceptable because the trait is pluggable and the AC only needs *a* deterministic measure to drop below threshold.
10. **Required `sessions-it` rollout (§9)** — making it required imports a transition window (open PRs block until rebased after the ruleset is applied) and a Docker dependency; mitigated by the always-run + in-job-path-filter shape (non-session PRs report green without containers), digest-pinned GHCR-mirrored images, and a retry-wrapped test. The ruleset-apply is a manual admin step sequenced after PR-2 merges.
11. **Linear premature close (§10)** — PR-1's merge auto-closes SMA-330; re-open / sub-issue handling is a manual step.
```