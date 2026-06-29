# SMA-331 — `paigasus-helikon-runtime-axum`: REST/SSE/WebSocket server

**Status:** Design approved (brainstorming gate) — 2026-06-29
**Ticket:** [SMA-331](https://linear.app/smaschek/issue/SMA-331)
**Branch:** `feature/sma-331-paigasus-helikon-runtime-axum-restssewebsocket-server`
**Crate:** `paigasus-helikon-runtime-axum` (stub `0.0.0` → ascends to `0.1.0`)

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
| Retention/eviction | **Hybrid TTL + LRU cap** (default 5 min TTL, 1024 runs) |
| Session mapping | **`SessionProvider` trait**, default in-memory ephemeral sessions |
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
  → [optional AuthLayer middleware]                 (401/403 on reject)
  → handler resolves agent by :name                 (404 if unknown)
  → ContextProvider builds Arc<Ctx> from req parts   (+ SessionProvider via X-Session-Id)
  → RunRegistry.create() → run_id + RunHandle (append-only EventLog)
  → spawn writer: Runner::run_streamed(agent, ctx, input, cfg) → drain stream into EventLog
  → response mode:
       plain   → await terminal, aggregate → 200 RunResult JSON (+ X-Run-Id header)
       sse     → subscribe EventLog cursor → SSE frames until terminal
       async   → 202 { run_id }
  GET :id/events (WS) → lookup → replay EventLog from seq 0 + live tail → close on terminal
```

### Dependency boundary

- Depends on `paigasus-helikon-core` (the `Agent`, `Runner`, `AgentEvent`, `Session`,
  `RunContext`, `AgentInput`, `RunResult`, `RunConfig` types) and
  `paigasus-helikon-runtime-tokio` (the default `TokioRunner`).
- **No `paigasus-helikon-core` API changes are required.** Every type the server needs is
  already public and published. This keeps the ascend clean: **no same-PR core bump.**

---

## 3. Event distribution — the core mechanism

To satisfy *replay-from-seq-0 + live tail + backpressure* with one code path, the design uses
an **append-only `EventLog` per run + a `tokio::sync::Notify`** rather than a raw broadcast
channel.

- The `Runner::run_streamed` output (`BoxStream<'static, AgentEvent>`) is drained by **one
  writer task** that appends each event to a `Vec<AgentEvent>` (behind a lock) and calls
  `notify_waiters()`.
- Each subscriber (SSE/WS) holds a **cursor** (index into the log). It reads all events
  `>= cursor`, emits them, advances the cursor, then `await`s the `Notify` for more. Terminal
  event (`RunCompleted`/`RunFailed`) closes the subscriber.
- **Properties:** no event loss (a slow client reads at its own pace from the persisted log),
  no broadcast "lagged" errors, and **replay is the same code path as live** — a brand-new
  subscriber simply starts at cursor 0.

**Alternatives rejected:** raw `tokio::sync::broadcast` (lossy under lag, cannot replay
history); per-subscriber `mpsc` fanned by the writer (writer must track subscribers and still
cannot replay).

### `EventLog` shape (illustrative)

```rust
struct EventLog {
    inner: std::sync::Mutex<EventLogInner>,
    notify: tokio::sync::Notify,
}
struct EventLogInner { events: Vec<AgentEvent>, terminal: bool }
```

- `append(event)` — push, set `terminal` if the event is `RunCompleted`/`RunFailed`, then
  `notify_waiters()`.
- `read_from(cursor) -> (Vec<AgentEvent>, bool /*terminal*/)` — snapshot tail + terminal flag.
- A subscriber loop: `read_from(cursor)`; emit; if terminal, stop; else `notify.notified().await`.

> Note on `Notify`: subscribers register `notified()` **before** re-reading the log to avoid
> the lost-wakeup race (await-future created prior to the snapshot read).

---

## 4. Run registry & retention

`RunRegistry` maps `run_id → Arc<RunHandle>`.

```rust
struct RunHandle {
    agent_name: String,
    log: Arc<EventLog>,
    cancel: tokio_util::sync::CancellationToken,
    terminal_at: std::sync::Mutex<Option<Instant>>, // stamped when the writer sees terminal
}
```

- **Live runs are never evicted.** When the writer drains a terminal event it stamps
  `terminal_at`.
- A background **sweeper task** (interval ~30 s) evicts runs whose `terminal_at + TTL` has
  elapsed, then enforces the LRU count cap (drop oldest-terminal beyond `max_retained_runs`).
- `run_id` = UUID v4.
- Builder knobs: `.run_retention(Duration)` (default **5 min**), `.max_retained_runs(usize)`
  (default **1024**).
- Backing store: `std::sync::RwLock<HashMap<Uuid, Arc<RunHandle>>>` plus a `VecDeque<Uuid>`
  recording terminal order for LRU. **No `dashmap` dependency.**
- The sweeper task is spawned by `serve()` and by `router()`; it holds a `Weak` reference to
  the registry and exits when the server is dropped.

---

## 5. Public API surface

```rust
pub struct AgentServer<Ctx> { /* … */ }

impl<Ctx: Send + Sync + 'static> AgentServer<Ctx> {
    pub fn builder() -> AgentServerBuilder<Ctx>;
    pub fn router(&self) -> axum::Router;                 // mount into a larger app
    pub async fn serve(self, addr: impl Into<std::net::SocketAddr>) -> Result<(), ServerError>;
}

pub struct AgentServerBuilder<Ctx> { /* … */ }
impl<Ctx: Send + Sync + 'static> AgentServerBuilder<Ctx> {
    pub fn agent(self, agent: Arc<dyn Agent<Ctx>>) -> Self;        // dup name → build() error
    pub fn runner(self, runner: Arc<dyn Runner<Ctx>>) -> Self;     // default Arc::new(TokioRunner)
    pub fn session_provider(self, p: impl SessionProvider<Ctx> + 'static) -> Self;
    pub fn context_provider(self, p: impl ContextProvider<Ctx> + 'static) -> Self;
    pub fn auth(self, a: impl AuthLayer + 'static) -> Self;        // default: none
    pub fn run_config(self, cfg: RunConfig) -> Self;
    pub fn run_retention(self, ttl: Duration) -> Self;
    pub fn max_retained_runs(self, n: usize) -> Self;
    pub fn build(self) -> Result<AgentServer<Ctx>, ServerError>;
}

#[async_trait] pub trait SessionProvider<Ctx>: Send + Sync {
    async fn session(&self, id: Option<&str>) -> Result<Arc<dyn Session>, ServerError>;
}
#[async_trait] pub trait ContextProvider<Ctx>: Send + Sync {
    async fn context(&self, parts: &http::request::Parts) -> Result<Arc<Ctx>, ServerError>;
}
#[async_trait] pub trait AuthLayer: Send + Sync {
    async fn authenticate(&self, parts: &mut http::request::Parts) -> Result<(), AuthRejection>;
}
```

### Default providers

- `InMemorySessionProvider` — backs sessions with core's in-memory `Session`, keyed by id in a
  `RwLock<HashMap<String, Arc<dyn Session>>>`; a request with no `X-Session-Id` (i.e. `id ==
  None`) gets a fresh anonymous in-memory session per request.
- `DefaultContextProvider<Ctx: Default>` — yields `Arc::new(Ctx::default())`. Provided via a
  blanket so `Ctx = ()` (and any `Default` context) needs zero wiring; a non-`Default` `Ctx`
  must supply a `context_provider`, else `build()` errors.

### Routes

| Method | Path | Behaviour |
|---|---|---|
| `POST` | `/agents/:name/runs` | One-shot: block, `200` aggregated `RunResult` JSON, `X-Run-Id` header |
| `POST` | `/agents/:name/runs?stream=sse` | SSE: `text/event-stream`, live events from seq 0 until terminal |
| `POST` | `/agents/:name/runs?mode=async` | `202 { "run_id": "…" }`, run executes in background |
| `GET` | `/agents/:name/runs/:id/events` | **WebSocket**: replay log from seq 0 + live tail, close on terminal |
| `GET` | `/agents` | `[{ "name", "description" }, …]` for mounted agents (runtime) |
| `GET` | `/openapi.json` | utoipa spec + injected agent list (behind `openapi` feature, default on) |

### Request body

```jsonc
// canonical
{ "messages": [ { "type": "user_message", "content": [ { "type": "text", "text": "hi" } ] } ] }
// convenience shorthand (deserialized into a single user-text message)
{ "input": "hi" }
```

Both are accepted (untagged enum / custom deserialize). The `input` shorthand keeps the curl
AC a one-liner: `curl -d '{"input":"hello"}' .../agents/echo/runs`.

---

## 6. Error & status model

- **Transport / lookup / auth** map to real HTTP status: unknown agent `404`, malformed body
  `400`, auth reject `401`/`403` (per `AuthRejection`), internal `500`.
- **Agent-level `RunFailed` is data, not an HTTP error.** For SSE/WS it is the terminal event;
  for plain `POST` it returns `200` with the aggregated body carrying the terminal outcome.
  One-shot and streaming stay consistent — the failure is the same `AgentEvent` in all three
  transports.
- **Runner-level `RunError`** (timeout, cancellation, session load failure) → plain `POST`
  `500`, except timeout → `504`. Streams emit a final synthetic error frame, then close.
- **Client disconnect** (SSE/WS dropped) → the run's `CancellationToken` is cancelled so the
  agent stops; the run is still retained for later replay (within retention).
- `ServerError` and `AuthRejection` are `thiserror` enums marked `#[non_exhaustive]`.

### Aggregated one-shot body

The plain-`POST` body serializes the run outcome: the ordered `AgentEvent` list (or the
semantic items + final `RunCompleted { usage }` / `RunFailed { error }`). Exact JSON shape is
fixed in the plan; it must round-trip the same events the SSE/WS transports deliver.

---

## 7. Module layout

```
src/
  lib.rs        // crate-level docs (#![doc]), re-exports, feature notes
  server.rs     // AgentServer<Ctx>, AgentServerBuilder<Ctx>, router(), serve()
  registry.rs   // RunRegistry, RunHandle, sweeper task, retention/LRU
  event_log.rs  // EventLog, cursor read, Notify-based subscription
  handlers/
    mod.rs
    runs.rs     // POST one-shot / sse / async
    events.rs   // GET WebSocket :id/events
    agents.rs   // GET /agents
    openapi.rs  // GET /openapi.json   (cfg(feature = "openapi"))
  session.rs    // SessionProvider, InMemorySessionProvider
  context.rs    // ContextProvider, DefaultContextProvider
  auth.rs       // AuthLayer, AuthRejection
  error.rs      // ServerError
```

---

## 8. Testing strategy

All tests are **deterministic and offline** (no LLM/network), safe on the
`{ubuntu, macos, windows} × {stable, 1.94}` matrix. No `FORKD`/env gating.

- **Unit**
  - `EventLog`: append/cursor/replay; the notify-before-read ordering; terminal flag.
  - `RunRegistry`: TTL eviction and LRU cap (drive the sweep manually / inject the clock; do
    not rely on wall-clock sleeps).
  - SSE serialization for representative `AgentEvent` variants (`event:` tag = type, `data:` =
    JSON).
- **Integration** (`tests/`): a **deterministic fake agent** that emits a scripted
  `AgentEvent` sequence (implements `Agent<Ctx>` directly, no model), the router bound on an
  ephemeral port, driven by a `reqwest` client:
  - **AC1** — plain `POST` over HTTP returns the aggregated result.
  - **AC2** — the SSE event sequence equals the fake agent's local stream, event-for-event.
  - WS replay-after-completion + live-tail.
  - `?mode=async` → `202` then replay by id.
  - `404` unknown agent; auth reject (`401`); session affinity (two requests, same
    `X-Session-Id`, second sees the first's history).
- **Example** — `examples/curl_server.rs` (or a doc snippet) backs the curl AC narrative.

---

## 9. Dependencies

- `paigasus-helikon-core` (workspace), `paigasus-helikon-runtime-tokio` (workspace).
- `axum` (workspace pin) with **extra features declared per-crate**: `json`, `query`, `ws`
  (plus the workspace's `http1`, `tokio`). Declaring features on top of `axum = { workspace =
  true }` does not disturb the shared pin or the `mcp` crate's build.
- `tokio`, `tokio-util` (CancellationToken), `futures-util`, `async-trait`, `serde`,
  `serde_json`, `tracing`, `thiserror`, `http` — all already in `[workspace.dependencies]`.
- **New `[workspace.dependencies]` entries:** `uuid` (`features = ["v4"]`); `utoipa`
  (behind the `openapi` feature). Versions pinned to current latest at implementation time.

### Features

```toml
[features]
default = ["openapi"]
openapi = ["dep:utoipa"]
```

`openapi` on by default so the spec endpoint "just works"; `default-features = false` drops the
utoipa cost for minimal consumers.

---

## 10. Release engineering (stub-ascend ritual)

Per `CLAUDE.md`. In **one** PR on the feature branch:

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

**No core bump** — no new core API is introduced.

### Documentation (same PR)

- `crates/paigasus-helikon-runtime-axum/README.md` — promote from stub to real (title, install
  via `cargo add`, runnable example, links, license).
- Facade `crates/paigasus-helikon/README.md` + root `README.md` — update the crate roster /
  feature→module map (stub → published).
- mdBook `docs/book/src/*.md` — the runtime page covering the axum server (was a stub).
- This crate's CHANGELOG.

---

## 11. Scope boundaries (YAGNI)

**In scope:** the routes in §5, the registry/replay model, `SessionProvider` /
`ContextProvider` / `AuthLayer` traits with defaults, utoipa OpenAPI behind a feature, the
deterministic test suite, the docs + ascend ritual.

**Explicitly out of scope** (future tickets): durable/persistent run history (registry is
in-memory only), horizontal scale-out / shared registry across nodes (the `X-Session-Id`
header is the documented sticky-routing hook, not a built-in coordinator), a built-in auth
implementation (JWT/OIDC — users wire their own via `AuthLayer`), bundled Swagger UI, per-agent
dynamic OpenAPI paths, and the actix sibling (SMA-343).
