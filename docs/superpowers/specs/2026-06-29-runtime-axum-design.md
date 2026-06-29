# SMA-331 — `paigasus-helikon-runtime-axum`: REST/SSE/WebSocket server

**Status:** Design approved (brainstorming gate); revised after adversarial review — 2026-06-29
**Ticket:** [SMA-331](https://linear.app/smaschek/issue/SMA-331)
**Branch:** `feature/sma-331-paigasus-helikon-runtime-axum-restssewebsocket-server`
**Crate:** `paigasus-helikon-runtime-axum` (stub `0.0.0` → ascends to `0.1.0`)

> Revised against the spec-challenger findings (see §12 for the changelog). The append-only
> `EventLog`/replay model, running every run through the core `Runner` trait, the test plan,
> and the release ritual all survived review; the error/status model, per-request
> `RunContext` construction, retention bounding, concurrency, and several API ergonomics were
> reworked.

---

## 1. Problem & goal

Helikon agents run in-process via the pluggable `Runner<Ctx>` boundary (`runtime-tokio`).
There is no way to expose an agent as a network service. This crate — one of the four
remaining `0.0.0` stubs — ascends to a real implementation: a self-hosted HTTP server that
mounts one or more agents and serves them over REST, SSE, and WebSocket.

### Acceptance criteria (from the ticket)

1. A simple agent runs via `curl` against the server.
2. The SSE stream contains the same `AgentEvent`s a local run would.

### Decisions taken at brainstorming (2026-06-29)

| Decision | Choice |
|---|---|
| Run model | **Async-run + replay (max scope)** — every run is a tracked, replayable entity |
| Plain `POST` response | **Block & return the aggregated result** (`+ X-Run-Id`); `?mode=async` opts into `202` |
| Retention/eviction | **Hybrid TTL + count cap** (default 5 min TTL, 1024 runs) |
| Session mapping | **`SessionProvider` trait**, default bounded in-memory ephemeral sessions |
| OpenAPI | **`openapi` feature (default on)**: static utoipa spec + runtime agent listing |

---

## 2. Architecture overview

A **library** crate. It exposes an `AgentServer<Ctx>` builder that mounts
`Arc<dyn Agent<Ctx>>`s and produces an `axum::Router` (or binds and serves directly). Agents
execute through the core **`Runner<Ctx>` trait**, defaulting to `TokioRunner`
(`runtime-tokio`). Every run becomes a tracked, replayable entry in an in-memory **run
registry**.

```
HTTP request
  → [optional AuthLayer middleware]                 (401/403 on reject; may insert identity into Parts.extensions)
  → handler resolves agent by {name}                (404 if unknown)
  → SessionProvider.session(X-Session-Id) → Arc<dyn Session>
  → acquire per-session lock (intra-session run serialization)
  → ContextProvider.build(parts, session, cancel) → RunContext<Ctx>
  → RunRegistry.create() → run_id + RunHandle (bounded EventLog + error slot + incremental aggregate)
  → spawn writer task:
        match Runner::run_streamed(agent, ctx, input, cfg):
          Ok(streaming) → drain streaming.events (BoxStream<'static, AgentEvent>) into EventLog,
                          folding each event into the incremental aggregate
          Err(run_error) → record run_error in the handle's error slot,
                          synthesize a terminal so awaiters/subscribers unblock
  → response mode:
       plain   → await terminal → 200 aggregated JSON (+ X-Run-Id)  | start-error → 500/503
       sse     → subscribe EventLog cursor → SSE frames until terminal
       async   → 202 { run_id }   (detached: client disconnect does NOT cancel)
  GET {name}/runs/{id}/events (WS) → lookup by id → replay log + live tail → close on terminal
```

axum 0.8 path syntax uses `{param}` (not `:param`).

### Dependency boundary

- Depends on `paigasus-helikon-core` (the `Agent`, `Runner`, `AgentEvent`, `Session`,
  `MemorySession`, `RunContext`, `AgentInput`, `RunResultStreaming`, `RunError`, `RunConfig`,
  `TokenUsage`, `Item` types) and `paigasus-helikon-runtime-tokio` (the default `TokioRunner`).
- **No `paigasus-helikon-core` API changes are required.** Every type the server needs is
  already public and (long-standing) published — `RunResultStreaming.events` is a
  `BoxStream<'static, AgentEvent>` (`runner.rs:248`) the writer task can own and drain. This
  keeps the ascend clean: **no same-PR core bump.** (The plan verifies the published
  `paigasus-helikon-core` exports each used item before relying on this.)

---

## 3. Event distribution — the core mechanism

To satisfy *replay + live tail + non-blocking writes* with one code path, the design uses an
**append-only, bounded `EventLog` per run + a `tokio::sync::Notify`** rather than a raw
broadcast channel.

- The `Runner::run_streamed` output (`RunResultStreaming.events`, a `BoxStream<'static,
  AgentEvent>`) is drained by **one writer task** that appends each event to the run's
  `EventLog` and calls `notify_waiters()`. The writer never blocks on slow clients (it does
  not stall the agent) — so this is **decoupled, non-blocking buffering, not backpressure.**
- Each subscriber (SSE/WS) holds a **cursor**. It reads all retained events `>= cursor`, emits
  them, advances the cursor, then `await`s the `Notify` for more. Terminal
  (`RunCompleted`/`RunFailed`, or a synthesized start-error terminal) closes the subscriber.
- **Replay == live via one code path** — a brand-new subscriber simply starts at the earliest
  retained cursor.

### Bounded retention (fixes the unbounded-memory risk)

A naive "retain every event forever" log is a memory-exhaustion risk: high-volume
`TokenDelta`/`ReasoningDelta` events (`agent.rs`) × 1024 retained runs is effectively
unbounded. Therefore:

- The `EventLog` is **capped per run** (configurable; a generous default sized for typical
  runs, e.g. on the order of 10k events or a byte budget — exact policy fixed in the plan).
  The cap targets the high-volume raw deltas.
- A **live subscriber that keeps up always receives the full-fidelity stream** (it reads each
  event as it is appended, before any eviction) — so **AC2 holds**: an SSE consumer connected
  from run start sees exactly the same `AgentEvent`s a local run emits.
- Replay of a run that *exceeded* the cap begins from the **earliest retained** event, not
  necessarily seq 0; the log records whether its head was truncated.
- **One-shot aggregation is computed incrementally** by the writer (folding each event into a
  running aggregate) so the final aggregated body is independent of the replay cap — even a
  run whose head was truncated returns a correct terminal result.

### `EventLog` / `RunHandle` shape (illustrative)

```rust
struct EventLog {
    inner: std::sync::Mutex<EventLogInner>,
    notify: tokio::sync::Notify,
}
struct EventLogInner {
    events: std::collections::VecDeque<AgentEvent>, // bounded ring
    first_seq: u64,                                 // earliest retained seq (0 unless truncated)
    truncated_head: bool,
    terminal: bool,
}
```

- `append(event)` — push (evicting head if over cap, advancing `first_seq`/`truncated_head`),
  fold into the incremental aggregate, set `terminal` on a terminal event, then
  `notify_waiters()`.
- `read_from(cursor) -> (first_seq, Vec<AgentEvent>, terminal)`.
- Subscriber loop, in this order to avoid the lost-wakeup race: **register the
  `notify.notified()` future and `enable()` it *before* re-reading the log**, then
  `read_from(cursor)`, emit, stop if terminal, else `await` the pinned future. The exact
  pin/`enable()` pattern is pinned down in the plan.

---

## 4. Run registry & retention

`RunRegistry` maps `run_id → Arc<RunHandle>`.

```rust
struct RunHandle {
    agent_name: String,
    log: Arc<EventLog>,
    cancel: tokio_util::sync::CancellationToken,
    start_error: std::sync::Mutex<Option<RunError>>, // set by the writer's Err arm
    terminal_at: std::sync::Mutex<Option<Instant>>,  // stamped when terminal is observed
}
```

- **Live runs are never evicted.** When the writer drains a terminal event (or synthesizes a
  start-error terminal) it stamps `terminal_at`.
- A background **sweeper task** (interval ~30 s) evicts runs whose `terminal_at + TTL` has
  elapsed, then enforces the count cap.
- `run_id` = UUID v4.
- Builder knobs: `.run_retention(Duration)` (default **5 min**), `.max_retained_runs(usize)`
  (default **1024**).
- Eviction order is **FIFO-by-completion** — the oldest-*completed* run is evicted first
  (recency is *not* bumped on replay access; an actively-replayed old run keeps streaming via
  the `Arc<RunHandle>` its handler holds, but is no longer discoverable for new lookups).
  (Described as "FIFO-by-completion," not "LRU," to avoid implying access-recency tracking.)
- Backing store: `std::sync::RwLock<HashMap<Uuid, Arc<RunHandle>>>` plus a `VecDeque<Uuid>`
  recording completion order. **No `dashmap` dependency.**

### Sweeper lifetime & registry ownership (fixes the `router()` side-effect hazards)

- **`router()` is pure** — it spawns nothing (a sync method must not `tokio::spawn`, which
  panics outside a runtime, and must not double-spawn when called twice).
- The **router's handler state owns a strong `Arc<RunRegistry>`**, so the registry lives as
  long as the router/handlers (no premature-drop 404s when the originating `AgentServer` is
  dropped).
- The **sweeper is spawned lazily on first request** (guarded by a `OnceCell`/`Once`), which
  guarantees a runtime context, and holds a **`Weak<RunRegistry>`** so it exits once the last
  strong reference (the router state) drops. `serve()` may spawn it eagerly since it is already
  async.

---

## 5. Public API surface

```rust
pub struct AgentServer<Ctx> { /* … */ }

impl<Ctx: Send + Sync + 'static> AgentServer<Ctx> {
    pub fn builder() -> AgentServerBuilder<Ctx>;
    pub fn router(&self) -> axum::Router;                     // pure; mount into a larger app
    pub async fn serve(self, addr: impl tokio::net::ToSocketAddrs) -> Result<(), ServerError>;
    pub async fn serve_with_listener(self, l: tokio::net::TcpListener) -> Result<(), ServerError>;
}

pub struct AgentServerBuilder<Ctx> { /* … */ }
impl<Ctx: Send + Sync + 'static> AgentServerBuilder<Ctx> {
    pub fn agent(self, agent: Arc<dyn Agent<Ctx>>) -> Self;        // dup name → build() error
    pub fn runner(self, runner: Arc<dyn Runner<Ctx>>) -> Self;     // default Arc::new(TokioRunner)
    pub fn session_provider(self, p: impl SessionProvider + 'static) -> Self;
    pub fn context_provider(self, p: impl ContextProvider<Ctx> + 'static) -> Self;
    pub fn auth(self, a: impl AuthLayer + 'static) -> Self;        // default: none
    pub fn run_config(self, cfg: RunConfig) -> Self;
    pub fn run_retention(self, ttl: Duration) -> Self;
    pub fn max_retained_runs(self, n: usize) -> Self;
    pub fn max_sessions(self, n: usize) -> Self;                   // default-provider session cap
    pub fn build(self) -> Result<AgentServer<Ctx>, ServerError>;   // Err if no context provider configured
}

// Zero-config entry for Default contexts (mirrors mcp's `with_default_ctx`; compile-time, not runtime):
impl<Ctx: Default + Send + Sync + 'static> AgentServerBuilder<Ctx> {
    pub fn with_default_context(self) -> Self;                     // installs DefaultContextProvider
}
```

### Providers (reworked so `RunContext` is fully constructible)

`RunContext::new` needs five inputs (`user_ctx, session, hooks, tracer, cancel`,
`context.rs:106`) plus optional permission/guard posture. The server owns **session
resolution** and the **run's cancel token**; the `ContextProvider` assembles everything else
and is the single seam for the security posture:

```rust
#[async_trait] pub trait SessionProvider: Send + Sync {
    /// Resolve the X-Session-Id header (None when absent) to a session.
    async fn session(&self, id: Option<&str>) -> Result<Arc<dyn Session>, ServerError>;
}
#[async_trait] pub trait ContextProvider<Ctx>: Send + Sync {
    /// Build the full RunContext for one run. The server injects the resolved session and the
    /// run's cancellation token; the provider supplies user_ctx, hooks, tracer, and any
    /// permission_mode / approval_handler / guard / deny configuration.
    async fn build(
        &self,
        parts: &axum::http::request::Parts,
        session: Arc<dyn Session>,
        cancel: tokio_util::sync::CancellationToken,
    ) -> Result<RunContext<Ctx>, ServerError>;
}
#[async_trait] pub trait AuthLayer: Send + Sync {
    /// Authenticate the request. On success, may insert an identity into `parts.extensions`
    /// for the ContextProvider to read. On failure, the run is rejected with the AuthRejection.
    async fn authenticate(&self, parts: &mut axum::http::request::Parts) -> Result<(), AuthRejection>;
}
```

**Auth → context handoff:** `AuthLayer` takes `&mut Parts` and inserts an identity into
`parts.extensions`; `ContextProvider::build` takes `&Parts` and reads it. This is the
documented mechanism by which authentication influences `Ctx`.

**Security posture:** because the operator's `ContextProvider` builds the `RunContext`, it
controls `with_permission_mode` / `with_permission_policy` / `with_approval_handler` /
guard/deny/redaction (`context.rs:353-439`). The default provider leaves core's defaults
(approval defaults to deny — safe for a network service). This is called out so operators of a
network-exposed, tool-calling agent know where to set policy.

### Default providers

- `InMemorySessionProvider` — backs sessions with **`core::MemorySession`** keyed by id, in a
  **bounded** `RwLock<HashMap<String, Arc<dyn Session>>>` (count-capped via `.max_sessions`,
  default e.g. 4096, FIFO/clock eviction). A request with no `X-Session-Id` (`id == None`)
  gets a **fresh anonymous `MemorySession` per request** (never inserted into the map, so no
  growth). Bounding fixes the distinct-`X-Session-Id` memory-exhaustion DoS; operators using a
  real backend (sqlite/postgres/redis) supply their own provider and own its lifecycle.
- `DefaultContextProvider<Ctx: Default>` — installed only via `with_default_context()` (a
  `Ctx: Default`-bounded entry point). It yields
  `RunContext::ephemeral(Ctx::default()).with_session(session).with_cancel(cancel)`. **No
  blanket impl, no specialization** — a non-`Default` `Ctx` must call `.context_provider(...)`
  (omitting it is a `build()` error; the zero-config path is `Default`-gated at compile time).

### Intra-session concurrency (fixes the same-session race)

The runner does snapshot → run → append, a read-modify-write on the session. Two concurrent
requests with the same `X-Session-Id` would both snapshot, run blind, and append — corrupting
conversation order. The server therefore **serializes runs per session id**: a keyed map of
`Arc<tokio::sync::Mutex<()>>` (one per active session id); a run holds its session's lock for
its duration, so same-session requests queue. This trades intra-session concurrency for
conversation-ordering integrity (the correct trade for a chat session) and is documented and
tested. Distinct sessions run concurrently. (`?mode=async` runs also take the lock for their
duration.)

### Routes

| Method | Path | Behaviour |
|---|---|---|
| `POST` | `/agents/{name}/runs` | One-shot: block, `200` aggregated JSON, `X-Run-Id` header |
| `POST` | `/agents/{name}/runs?stream=sse` | SSE: `text/event-stream`, live events until terminal |
| `POST` | `/agents/{name}/runs?mode=async` | `202 { "run_id": "…" }`, run executes detached |
| `GET` | `/agents/{name}/runs/{id}/events` | **WebSocket**: replay log + live tail, close on terminal |
| `GET` | `/agents` | `[{ "name", "description" }, …]` for mounted agents (runtime) |
| `GET` | `/openapi.json` | utoipa spec + injected agent list (behind `openapi` feature, default on) |

WebSocket specifics: the `{name}` segment is kept for REST symmetry but the run is looked up
by `{id}`; an unknown/evicted `{id}`, or an `{id}` whose run belongs to a different `{name}`,
returns **HTTP 404 before the upgrade** (non-101 response, no `on_upgrade`). The handler polls
the inbound half to observe client close; inbound client→server frames are otherwise ignored.

### Request body

Because core's `AgentInput` is not `Deserialize` (`agent.rs:88` derives only
`Debug, Clone, Default`), the server defines its own request DTO with a **custom
`Deserialize`** (clean `400`s, no untagged-enum "did not match any variant" noise):

```jsonc
// canonical
{ "messages": [ { "type": "user_message", "content": [ { "type": "text", "text": "hi" } ] } ] }
// convenience shorthand (deserialized into a single user-text message)
{ "input": "hi" }
```

`Item` *is* serde-ready (`item.rs:18`), so `messages` maps to `Vec<Item>`; the DTO converts to
`AgentInput`. The curl AC must set the JSON content type:

```
curl -H 'Content-Type: application/json' -d '{"input":"hello"}' http://localhost:8080/agents/echo/runs
```

---

## 6. Error & status model (reworked)

The model is split by **where** the failure occurs, because `run_streamed` surfaces failures
two different ways (`runtime-tokio/src/lib.rs`): timeout/cancel become **synthetic in-stream
`AgentEvent::RunFailed`** events (and `run_streamed` returns `Ok`), while session-load /
agent-start failures return the **outer `Err(RunError)`**.

- **Pre-run, transport/lookup/auth** → real HTTP status: unknown agent `404`, malformed body
  `400`, auth reject `401`/`403` (per `AuthRejection`), other internal `500`.
- **Start failure — `run_streamed` returns `Err(RunError)`** (e.g. session backend down): the
  writer records it in `RunHandle.start_error` **and synthesizes a terminal** (so plain-POST
  awaiters and stream subscribers unblock — *this fixes the permanent-hang bug*). Plain POST
  maps it to **`500`** (`503` if the variant denotes an unavailable dependency); streams emit a
  final synthetic error frame, then close.
- **Run reached an in-stream terminal** (`RunCompleted` *or* `RunFailed`, including
  timeout/cancel which arrive as synthetic `RunFailed`): **plain POST returns `200`** with the
  aggregated body carrying the terminal outcome — *the failure is data*, identical across
  one-shot/SSE/WS. (Deliberate simplification: we do **not** special-case timeout→`504`,
  because timeout is delivered as an in-stream `RunFailed`, not an outer error; surfacing it as
  `200`-with-failure keeps all three transports consistent. The terminal `RunFailed` carries
  the reason string for clients that care.)

### Cancellation scoping (fixes the disconnect-vs-replay/async conflict)

- The run's `CancellationToken` is linked to the **creating client only for non-async
  (plain/SSE) requests**. If that owning client disconnects, the token is cancelled and the
  agent stops.
- **`?mode=async` runs are detached** — client disconnect never cancels them; they run to
  completion (or their own `RunConfig` timeout).
- **WS subscribers are observers** — they attach to an existing run; their disconnect never
  cancels the run. A run with a plain/SSE owner plus WS observers is cancelled only by the
  owner's disconnect.
- A disconnect-cancelled run is **retained for replay** (it carries a terminal
  `RunFailed { "…cancelled…" }`); replaying it shows the partial+cancelled outcome —
  consistent with "cancellation is data."

`ServerError` and `AuthRejection` are `thiserror` enums marked `#[non_exhaustive]`.

### Aggregated one-shot body

Built from **serde-ready** core types (`AgentEvent`, `Item`, `TokenUsage`) via the writer's
incremental aggregate — **not** by serializing `RunResult` (which is not `Serialize`,
`runner.rs:208`). The plan defines the exact DTO (with its own `///` docs); it must round-trip
the same events the SSE/WS transports deliver.

---

## 7. Module layout

```
src/
  lib.rs        // crate-level docs (#![doc]), re-exports, feature notes
  server.rs     // AgentServer<Ctx>, AgentServerBuilder<Ctx>, router(), serve()
  registry.rs   // RunRegistry, RunHandle, sweeper task, retention/FIFO eviction
  event_log.rs  // EventLog (bounded ring + Notify), cursor read, incremental aggregate
  handlers/
    mod.rs
    runs.rs     // POST one-shot / sse / async
    events.rs   // GET WebSocket {id}/events
    agents.rs   // GET /agents
    openapi.rs  // GET /openapi.json   (cfg(feature = "openapi"))
  session.rs    // SessionProvider, InMemorySessionProvider (bounded), per-session lock map
  context.rs    // ContextProvider, DefaultContextProvider
  auth.rs       // AuthLayer, AuthRejection
  dto.rs        // request/response DTOs (custom Deserialize/Serialize over AgentInput/aggregate)
  error.rs      // ServerError
```

---

## 8. Testing & quality

All tests are **deterministic and offline** (no LLM/network), safe on the
`{ubuntu, macos, windows} × {stable, 1.94}` matrix. No `FORKD`/env gating.

- **Unit**
  - `EventLog`: append/cursor/replay; bounded-ring head truncation + `first_seq`; the
    notify-before-read ordering; terminal flag; incremental aggregate correctness.
  - `RunRegistry`: TTL eviction and count cap (drive the sweep manually / inject the clock;
    no wall-clock sleeps).
  - SSE serialization for representative `AgentEvent` variants (`event:` tag = type, `data:` =
    JSON).
  - Request DTO: `{input}` and `{messages}` both deserialize; malformed body → clean `400`.
- **Integration** (`tests/`): a **deterministic fake agent** (implements `Agent<Ctx>`, emits a
  scripted `AgentEvent` sequence, no model), router bound on an ephemeral port via
  `serve_with_listener`, driven by a `reqwest` client:
  - **AC1** — plain `POST` (with `Content-Type: application/json`) returns the aggregated result.
  - **AC2** — the SSE event sequence equals the fake agent's local stream, event-for-event.
  - WS replay-after-completion + live-tail; WS `404` for unknown/evicted id.
  - `?mode=async` → `202` then replay by id; async run survives creator disconnect.
  - `404` unknown agent; auth reject (`401`); start-error → `500` (no hang).
  - Session affinity: two **sequential** requests, same `X-Session-Id`, second sees the
    first's history.
  - **Intra-session serialization:** two **concurrent** same-`X-Session-Id` requests do not
    interleave/corrupt order.
- **Example** — `examples/curl_server.rs` (or a doc snippet) backs the curl AC narrative.
- **Doc gate** — the crate opts into `[lints] workspace = true`, so under `-D warnings` every
  public item needs a `///`: the ~12 builder methods, `SessionProvider`/`ContextProvider`/
  `AuthLayer` (+ their methods), `InMemorySessionProvider`/`DefaultContextProvider`,
  `AgentServer`/`router`/`serve`/`serve_with_listener`, the DTOs, and `ServerError`/
  `AuthRejection` (+ every variant). Budget for this and the **80% doc-coverage** CI gate.

---

## 9. Dependencies

- `paigasus-helikon-core` (workspace), `paigasus-helikon-runtime-tokio` (workspace).
- `axum` (workspace pin) with **extra features declared per-crate**: `json`, `query`, `ws`
  (on top of the workspace's `http1`, `tokio`). Declaring features on `axum = { workspace =
  true, features = [...] }` does not disturb the shared pin or the `mcp` crate's build.
- HTTP types come from the **`axum::http`** re-export — `http` is **not** a separate workspace
  dependency, so no new dep and no false claim.
- `tokio`, `tokio-util` (CancellationToken), `futures-util`, `async-trait`, `serde`,
  `serde_json`, `tracing`, `thiserror` — already in `[workspace.dependencies]`.
- **New `[workspace.dependencies]` entries:** `uuid` (`features = ["v4"]`, MIT/Apache —
  permissive); `utoipa` (behind the `openapi` feature). Versions pinned to current latest at
  implementation time.
- **Supply-chain gate:** the new deps must clear `deny.toml`'s license/advisory allowlist on
  the required `audit`/`deny` checks. `utoipa` pulls proc-macro-heavy transitive deps — the
  plan verifies the graph against `deny.toml` early (cf. the `jiff`→`proc-macro-error2`
  advisory episode); the `openapi` feature being opt-out-able is the fallback if a transitive
  advisory appears.
- **DTOs:** `RunResult`/`AgentInput` are not serde-ready, so the plan budgets hand-rolled
  request/response DTOs (built over the serde-ready `AgentEvent`/`Item`/`TokenUsage`), each
  with its own `///` docs.

### Features

```toml
[features]
default = ["openapi"]
openapi = ["dep:utoipa"]
```

`openapi` on by default so the spec endpoint "just works"; `default-features = false` drops the
utoipa cost (and its transitive graph) for minimal consumers.

---

## 10. Release engineering (stub-ascend ritual)

Per `CLAUDE.md`, verified against the repo (`release-plz.toml` has the
`paigasus-helikon-runtime-axum` `release = false` block; facade self-pin and workspace pin
patterns confirmed). In **one** PR on the feature branch:

1. Bump `crates/paigasus-helikon-runtime-axum/Cargo.toml` `version = "0.0.0"` → `"0.1.0"`.
2. Remove `publish = false` from that `Cargo.toml`.
3. Remove the `paigasus-helikon-runtime-axum` `release = false` block from `release-plz.toml`.
4. Bump the `[workspace.dependencies] paigasus-helikon-runtime-axum` path-pin to `version =
   "0.1.0"` so the facade requests the real version.
5. **Bump the facade `paigasus-helikon`** (patch: `version` + its `[workspace.dependencies]`
   self-pin + CHANGELOG). *Rationale:* a same-PR manual sibling bump defeats release-plz's
   `dependencies_update` cascade (the SMA-346 trap), so the facade would otherwise stay pinned
   to `runtime-axum ^0.0.0` and the new surface would be unreachable through the facade's
   `runtime-axum` feature.

**No core bump** — no new core API is introduced (verify the published core exports the used
items; they are long-standing, not added here).

### Documentation (same PR)

- `crates/paigasus-helikon-runtime-axum/README.md` — promote from stub to real (title, install
  via `cargo add`, runnable example, links, license).
- Facade `crates/paigasus-helikon/README.md` + root `README.md` — update the crate roster /
  feature→module map (stub → published).
- mdBook `docs/book/src/*.md` — the runtime page covering the axum server (was a stub).
- This crate's CHANGELOG.

---

## 11. Scope boundaries (YAGNI)

**In scope:** the routes in §5, the registry/replay model with bounded retention,
`SessionProvider` / `ContextProvider` / `AuthLayer` traits with defaults, intra-session run
serialization, utoipa OpenAPI behind a feature, the deterministic test suite, the docs +
ascend ritual.

**Explicitly out of scope** (future tickets): durable/persistent run history (registry is
in-memory only), horizontal scale-out / shared registry / cross-node session coordination (the
`X-Session-Id` header is the documented single-node sticky-routing hook; intra-session
serialization is single-node only), a built-in auth implementation (JWT/OIDC — users wire
their own via `AuthLayer`), bundled Swagger UI, per-agent dynamic OpenAPI paths, and the actix
sibling (SMA-343).

---

## 12. Adversarial review changelog (2026-06-29)

Spec-challenger verdict: **NEEDS REWORK** (core architecture sound; error model, RunContext
wiring, and several ergonomics needed fixing). Folded in:

- **Route syntax** → axum 0.8 `{param}` everywhere (was `:param`, which panics at `Router`
  build). *(BLOCKER)*
- **Permanent-hang / status model** → writer records outer `RunError` in a `start_error` slot
  and synthesizes a terminal so awaiters/subscribers unblock; status model split into
  start-error (`500`/`503`) vs in-stream-terminal (`200`, failure-as-data); dropped the
  unreachable timeout→`504` special case. *(BLOCKER)*
- **Per-request `RunContext`** → `ContextProvider::build` returns a fully-built
  `RunContext<Ctx>` (server injects session + cancel; provider owns hooks/tracer + the
  permission/approval/guard security posture). *(MAJOR)*
- **Default-context ergonomics** → compile-time `Ctx: Default`-gated `with_default_context()`
  (mirrors `mcp`'s `with_default_ctx`); dropped the non-expressible "blanket + runtime error."
  *(MAJOR)*
- **Session-map DoS** → default `InMemorySessionProvider` is count-bounded (`.max_sessions`);
  anonymous requests get a non-inserted ephemeral session. *(MAJOR)*
- **Unbounded EventLog** → bounded per-run ring + incremental aggregation; reframed
  "backpressure" as "decoupled non-blocking buffering"; documented head-truncation + that
  keeping-up live subscribers retain full fidelity (AC2 safe). *(MAJOR)*
- **Same-session race** → per-session run serialization via a keyed async-mutex map; concurrent
  same-session test added. *(MAJOR)*
- **`router()` side effects** → `router()` made pure; sweeper spawned lazily on first request;
  router state holds a strong `Arc<RunRegistry>`, sweeper a `Weak`. *(MAJOR)*
- **Cancel-on-disconnect** → scoped to the non-async creating client; async detached; WS
  observers never cancel; cancelled runs retained as data. *(MAJOR)*
- **`http` dep** → use the `axum::http` re-export (no workspace dep); corrected §9. *(MINOR)*
- **Serde gaps** → custom request/response DTOs over serde-ready core types; documented. *(MINOR)*
- **curl AC** → requires `-H 'Content-Type: application/json'`. *(MINOR)*
- **`serve` signature** → `impl ToSocketAddrs` + `serve_with_listener` for ephemeral-port
  tests. *(MINOR)*
- **"LRU" wording** → "FIFO-by-completion" (no access-recency tracking). *(MINOR)*
- **WS details** → 404-before-upgrade on unknown/mismatched id; poll inbound for close. *(MINOR)*
- **Notify pattern** → pin/`enable()` the `notified()` future before re-reading. *(MINOR)*
- **Doc gate** → §8 budgets `///` for the full public surface + the 80% coverage gate. *(MINOR)*
- **Auth→context** → identity passed via `parts.extensions`. *(QUESTION)*
- **Supply chain** → §9 verifies `utoipa`/`uuid` against `deny.toml` early. *(QUESTION)*

Nothing was rejected as unjustified — every finding was actionable and folded in.
