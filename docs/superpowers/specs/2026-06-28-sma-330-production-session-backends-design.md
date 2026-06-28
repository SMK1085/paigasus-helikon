# SMA-330 — Production session backends: Postgres, Redis, Compacting wrapper

- **Linear:** SMA-330 (`feature/sma-330-production-session-backends-postgres-redis-compacting`)
- **Status:** design
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
3. `CompactingSession` provably reduces the projected token count below the threshold after compaction fires.
4. Facade wiring, crate READMEs, mdBook Sessions page, CI, and release plumbing all updated in the same PR.

**Non-goals (explicitly deferred)**

- **Keep-recent-window compaction** (summarize an older prefix while keeping the last *K* turns verbatim). The current `project()` collapses events *preceding* the marker, so a running full-history summary is the only shape expressible without changing core. Window compaction is a future ticket touching `project()`, its tests, and the provider-translator caveat.
- Redis `events(since)` cursor optimization (O(n) full-stream read for now).
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

- The heuristic walks each `Item`'s textual content (`ContentPart::Text`, `Reasoning`; tool-call args counted as their JSON string length) and returns `total_chars.div_ceil(4)`. Non-text parts (image/audio sources) contribute a small fixed constant or 0 — **decision: 0** (we only bound text growth; image bytes are not in the projected text).
- Deterministic so the AC test can assert an exact below-threshold count.

### 4.2 `CompactingSession<S>`

```rust
pub struct CompactingSession<S: Session> {
    inner: S,
    model: Arc<dyn Model>,
    counter: Arc<dyn TokenCounter>,
    threshold: usize,
    settings: ModelSettings,
    prompt: String,             // summarization instruction text
    compacting: AtomicBool,     // single-flight guard
}
```

Construction via a small builder (`CompactingSession::builder(inner, model).threshold(n)…build()`) with defaults: `HeuristicTokenCounter`, a built-in summarization prompt, `ModelSettings::default()`.

`Session` impl:

- `append(events)`:
  1. `inner.append(events).await?` (propagate inner errors — the durable write must not be masked).
  2. `self.maybe_compact().await` — **best-effort**: any error (LLM unavailable, summary read failure) is logged at `warn!` and swallowed. The user's events are already persisted; compaction is an optimization, and failing `append` after a successful inner write would be surprising. `append` returns `Ok(())`.
- `events(since)` → `inner.events(since)`.
- `snapshot()` → `inner.snapshot()` (already projects, collapsing `Compacted`).

`maybe_compact()` (synchronous within `append`, so the AC is deterministic — no background race):

1. **Single-flight:** `if self.compacting.swap(true, AcqRel) { return Ok(()) }`. Cleared with `store(false, Release)` on every exit path (RAII guard struct). No lock is held across the `await`; a concurrent append that finds the flag set simply skips its own compaction (the in-flight one will lower the count; if new events keep it high, the next append re-checks → converges).
2. Read `let evs = self.inner.events(None).await?;` → `let snap = project(&evs);` → `let tokens = self.counter.count(&snap.messages);`.
3. If `tokens <= self.threshold` → return.
4. `live = evs.len() - last_compacted_index(&evs)` where `last_compacted_index` returns the index of the last `Compacted` event **inclusive** (`0` ⇒ none ⇒ `live = evs.len()`). **Guard:** if `live < 2`, return (a lone running summary can't be usefully shrunk; prevents re-summarization ping-pong).
5. Build `ModelRequest { messages: snap.messages-plus-instruction, tools: vec![], model_settings: self.settings.clone() }`; drive `model.invoke(req, CancellationToken::new()).await?`, collecting `ModelEvent::TokenDelta { text }` into `summary` until `Finish`.
6. `self.inner.append(&[SessionEvent::compacted(summary, live as u64)]).await?`.

**Correctness of `original_count = live`** (so the result is exactly `[System(summary)]` and `project` logs no warning): when `project` reaches the new marker, its `contributions.len()` equals `live` (one entry per event since the previous `Compacted`, whose entries were truncated). `drop_from_idx = len - live = 0` ⇒ every prior message dropped ⇒ `[System(summary)]`; and `live == contributions.len()` (not `>`), so the "references more events than seen" `warn!` does **not** fire. The summarization instruction is **not** an event (it's only in the `ModelRequest`), so it does not affect the count.

### 4.3 `SessionEvent` accessors (de-dup)

```rust
impl SessionEvent {
    pub fn kind(&self) -> &'static str;            // serde tag: "user_message", "compacted", …
    pub fn ts(&self) -> Timestamp;                 // the variant's ts
    pub fn ts_nanos_saturating(&self) -> i64;      // i128→i64 saturating, for audit-index columns
}
```

`kind()`/`ts()` `match` all variants with **no `_ =>` arm**, so adding a future `#[non_exhaustive]` variant is a **single compile failure in core** (not three silent panics across backends). `SqliteSession::event_metadata` is refactored to `(ev.kind(), ev.ts_nanos_saturating())`; Postgres and Redis reuse the same accessors.

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
- testkit's own `tests/memory.rs` runs `run_all` against `MemorySession`, anchoring "**the same** suite Memory passes."
- **Consumption:** each backend crate adds a **path-only, version-less** dev-dependency `paigasus-helikon-sessions-testkit = { path = "../paigasus-helikon-sessions-testkit" }` (SMA-326 pattern → omitted from published manifests, no version-pin/publish-cycle trap) and a `tests/conformance.rs` calling `run_all(make)`.
- **SQLite retrofit:** add `tests/conformance.rs` to the sqlite crate using the harness; keep its backend-specific tests (`persistence.rs`, `multi_session.rs`). The hand-rolled overlap in `roundtrip.rs`/`concurrent_writers.rs` may be slimmed to avoid duplication but is not required to be deleted.

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

`append` — contiguous sequence under concurrent writers via a **per-session advisory lock**:

```sql
BEGIN;
SELECT pg_advisory_xact_lock(hashtext($1));               -- $1 = session_id; auto-released at COMMIT
SELECT COALESCE(MAX(sequence), -1) + 1 FROM session_events WHERE session_id = $1;
INSERT INTO session_events (session_id, sequence, ts_nanos, kind, payload)
  VALUES ($1, $2, $3, $4, $5);                            -- one row per event; payload bound as JSONB
COMMIT;
```

Finer-grained than sqlite's whole-DB `BEGIN IMMEDIATE` — writers to *different* sessions never contend.

`events(since)`: `SELECT payload FROM session_events WHERE session_id = $1 AND sequence > $2 ORDER BY sequence`, watermark default `-1`, payload fetched as `sqlx::types::Json<SessionEvent>`.

`snapshot()`: `project(&self.events(None).await?)`.

**Dependencies:** `sqlx = { workspace = true, features = ["postgres", "<rustls-aws-lc-rs tls feature>"] }`. The TLS feature **must** be the aws-lc-rs rustls variant (not ring), to match the workspace `CryptoProvider` and avoid the dual-provider panic (the gcp_auth/aws-lc-rs memory). Exact sqlx-0.9 feature name verified at implementation time. Adding `postgres` to the (workspace-unified) sqlx build is benign for the sqlite crate.

## 7. `RedisSession` (`-sessions-redis`) — one Redis Stream per session

`redis` crate with tokio async (`ConnectionManager` — auto-reconnect, cheap clone, shareable) and the rustls-aws-lc TLS feature. Constructors: `RedisSession::connect(url, id)` (build a `ConnectionManager`) and `RedisSession::new(ConnectionManager, id)`. Stream key `helikon:session:{id}:events`.

Entry fields per event: `seq` (contiguous int), `kind`, `payload` (JSON), `ts`.

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

Backend-specific tests (env-gated): reconnect-persistence (new `ConnectionManager`, same key, read back), multi-key isolation.

## 8. Facade wiring

- `crates/paigasus-helikon/Cargo.toml`: optional deps `paigasus-helikon-sessions-postgres`, `paigasus-helikon-sessions-redis`; features `sessions-postgres = ["dep:…-postgres"]`, `sessions-redis = ["dep:…-redis"]`.
- `crates/paigasus-helikon/src/lib.rs`: `#[cfg(feature = "sessions-postgres")] pub use … as sessions_postgres;` and redis equivalent, each with a `///` doc comment (missing_docs is `-D warnings` in the docs job).
- `CompactingSession`/`TokenCounter` live in core, reachable as `paigasus_helikon::core::CompactingSession` — no new facade feature.
- Root `Cargo.toml` `[workspace.dependencies]`: add the two published crates (path + `version = "0.1.0"`). testkit is referenced only by backend dev-deps via direct relative path, not via `[workspace.dependencies]`.

## 9. CI

New job in `.github/workflows/ci.yml`, **non-required signal** (not added to `.github/rulesets/main-protection-checks.json`):

```yaml
sessions-it:
  runs-on: ubuntu-latest
  services:
    postgres: { image: postgres:17, env: { POSTGRES_PASSWORD: postgres }, ports: ["5432:5432"], options: --health-cmd pg_isready … }
    redis:    { image: redis:7, ports: ["6379:6379"], options: --health-cmd "redis-cli ping" … }
  env:
    HELIKON_TEST_POSTGRES_URL: postgres://postgres:postgres@localhost:5432/postgres
    HELIKON_TEST_REDIS_URL:    redis://localhost:6379
  steps: [checkout (pinned SHA), rust-toolchain stable (pinned SHA), rust-cache (pinned SHA),
          cargo test -p paigasus-helikon-sessions-postgres -p paigasus-helikon-sessions-redis]
```

- Action `uses:` pin to the **same commit SHAs** already in `ci.yml`.
- Everywhere else (`test` matrix `--workspace --all-features` on ubuntu/macos/windows × {stable, 1.94}) the Postgres/Redis tests **loud-skip** when the envs are unset: each is a real `#[tokio::test]` that, with no URL, `eprintln!("SKIP: …")` and returns `Ok` (forkd `tests/forkd_live.rs` pattern). They still **compile** on every platform (both clients are cross-platform pure-Rust).
- Image tags (`postgres:17`, `redis:7`) confirmed current at implementation time.

## 10. Release & versioning choreography

New crates follow the `providers-gemini` (SMA-449) precedent: created at `version = "0.1.0"` with normal `publish`, **not** added to the release-plz stub list. testkit is added to the stub list (`publish=false` + `release=false`).

Because core gains public API in the same PR and the new published crates depend on core, plan the **conservative same-PR bump** to dodge the `cargo publish --verify`-against-stale-registry-core trap (CLAUDE.md "Caveat" / SMA-321 / SMA-346):

1. Bump `paigasus-helikon-core` (patch) + its `[workspace.dependencies]` pin + CHANGELOG.
2. Bump the `paigasus-helikon` facade (patch) + its self-pin + CHANGELOG (manual core bump otherwise defeats release-plz's facade cascade).
3. New `-sessions-postgres` / `-sessions-redis` at `0.1.0`; add to `[workspace.dependencies]`.

**Verify-before-finalizing (planning task):** inspect how SMA-449 actually shipped `providers-gemini` (git log + the merged release/publish). If release-plz already publishes a brand-new crate and cascades the facade automatically without a stale-core verify failure, **drop the manual core/facade bumps** and let the normal flow run. After merge, watch the release-plz `chore: release` PR's CI (memory: cargo-update can redden `audit`/`deny` on the bot PR only).

Bootstrap/release-plumbing edits use `chore(...)`/`docs(...)` commit types, never `feat`/`fix`.

## 11. Testing strategy → AC mapping

| Acceptance criterion | Covered by |
|---|---|
| All three pass the same conformance suite (append, read, projection, concurrent writers) as Memory/SQLite | `-sessions-testkit::run_all` invoked by Memory (in testkit), SQLite (retrofit), Postgres, Redis. Postgres/Redis runs are env-gated and executed by the `sessions-it` CI job. |
| `CompactingSession` reduces input token count below the threshold after compaction fires | core test: deterministic `TokenCounter` + fake `Model` returning a short summary + threshold `T`; append past `T`; assert `counter.count(snapshot().messages) < T`. Plus: `Compacted` recorded with exact `original_count`; raw `events()` retains the full log; LLM-error path leaves the log untouched and `append` still `Ok`; lone-summary (`live < 2`) guard. |

All six existing CI gates stay green: `cargo fmt`, `clippy --workspace --all-features --all-targets -D warnings`, `test` matrix, `docs` (`RUSTDOCFLAGS=-D warnings`, so every new `pub` item needs `///`), `doc-coverage` (≥80%), `commits`/`pr-title`. New crates are added to the doc-coverage aggregator like other published crates.

## 12. Docs (same PR — mandatory)

- New `crates/paigasus-helikon-sessions-postgres/README.md`, `crates/paigasus-helikon-sessions-redis/README.md` (crates.io landing pages; drift-free `cargo add` install snippets). Minimal `…-testkit/README.md` noting it is internal/unpublished.
- Update facade `crates/paigasus-helikon/README.md` and root `README.md` crate roster + feature→module map (add `sessions-postgres`, `sessions-redis`; mention `CompactingSession` in core).
- Update the mdBook Sessions page(s) under `docs/book/src/` to document the three backends + compaction semantics (full-history running summary; provider-translator caveat). `mdbook build docs/book` must stay clean (`linkcheck` warning-policy = error).

## 13. Risks / things the challenge should attack

1. **Release choreography (§10)** — most error-prone; the manual-bump-vs-let-release-plz call hinges on the SMA-449 precedent, which must be verified, not assumed.
2. **`original_count = live` exactness (§4.2)** — depends on the precise `project()` contribution-truncation behavior; covered by a dedicated test asserting the snapshot is exactly `[System(summary)]` with no warning.
3. **Redis contiguous-sequence-under-concurrency (§7)** — relies on Redis Lua atomicity; the conformance concurrency test must run against a real server (the `sessions-it` job), since loud-skip would otherwise hide a regression.
4. **`HeuristicTokenCounter` accuracy** — deliberately approximate; acceptable because the trait is pluggable and the AC only requires *a* deterministic measure to drop below threshold.
5. **sqlx TLS feature name / dual-CryptoProvider** — must be the aws-lc-rs rustls variant; wrong choice reintroduces the `--all-features` panic.
6. **`AtomicBool` single-flight vs. dropped compaction** — a compaction skipped due to the guard relies on a later append to re-fire; document that unbounded growth is bounded only by append frequency, acceptable for this ticket.
```