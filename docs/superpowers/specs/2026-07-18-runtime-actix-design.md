# Design — `paigasus-helikon-runtime-actix` (SMA-343)

- **Ticket:** [SMA-343](https://linear.app/smaschek/issue/SMA-343) — *paigasus-helikon-runtime-actix: actix-web REST/SSE/WebSocket server*
- **Companion to:** [SMA-331](https://linear.app/smaschek/issue/SMA-331) (`paigasus-helikon-runtime-axum`)
- **Status:** Approved (design) + hardened after adversarial spec-challenge, 2026-07-18
- **Branch:** `feature/sma-343-paigasus-helikon-runtime-actix-actix-web-restssewebsocket`

## 1. Motivation

The wider Paigasus platform standardizes on **actix-web**. Helikon today ships only an
axum HTTP runtime (`paigasus-helikon-runtime-axum`), so embedding an agent into an
existing actix-web service forces a framework switch. This ticket adds a first-party
actix runtime that exposes the **same public surface** as the axum runtime, so moving a
Helikon agent server from axum to actix (or dropping one into an existing actix app)
changes the dependency line, the mount call, and the small set of unavoidable
framework deltas enumerated in §5.1 — nothing more.

- axum stays the documented default for greenfield Helikon users.
- actix is the path for "drop this into an existing actix-web service."

## 2. Goals / non-goals

**Goals**

- A new crate `paigasus-helikon-runtime-actix` whose `AgentServer` builder + DTOs +
  trait *names* match the axum runtime.
- Endpoints identical to the axum runtime (REST one-shot, `?stream=sse`, `?mode=async`,
  WebSocket events, `GET /agents`, `GET /openapi.json`).
- SSE and WebSocket streaming (WebSocket via `actix-ws`, **not** `actix-web-actors`).
- Session affinity via `X-Session-Id`.
- OpenAPI spec generation via `utoipa`.
- Optional auth-middleware trait (no built-in implementation).
- A shared conformance test proving wire-format parity between the axum and actix
  servers.
- `cargo build --features runtime-actix` from the facade works in isolation, with **no
  axum dependency leakage**, enforced by CI.
- An `examples/actix_embed.rs` demonstrating embedding into an existing actix-web app.

**Non-goals**

- **No shared `paigasus-helikon-runtime-http-core` crate.** The ticket explicitly defers
  the "extract a shared HTTP core" decision until both runtimes exist and the real
  overlap is visible. This design deliberately *duplicates* the framework-agnostic
  internals rather than pre-abstracting them. (See §12.)
- No new transports, no auth implementations, no changes to `paigasus-helikon-core`.

## 3. Key design decisions

Approved at GATE 1 (2026-07-18); decisions 5–6 added after the adversarial spec-challenge.

1. **Mount seam = `configure()`.** The actix analog of axum's `router() -> axum::Router`
   is a method returning an `impl Fn(&mut ServiceConfig) + Send + Clone + 'static` that
   the host passes to `App::configure(...)`. Routes mount at root. **Internally**,
   `configure()` registers the routes under an empty-prefix `web::scope("")` so that the
   optional auth middleware can be `.wrap()`-ed onto them — `ServiceConfig` itself has no
   `.wrap()`. The *public* seam is still a single `configure()` method (not a
   host-visible `Scope`); the inner `scope("")` is an implementation detail that mounts at
   root and is invisible to the caller.
2. **OpenAPI = mirror axum's manual handler.** Reuse the same hand-built `utoipa`
   `ApiDoc` + runtime agent-list augmentation, served by a plain `web::Json` handler. Near-
   free `/openapi.json` parity, one fewer dependency (no `utoipa-actix-web`), and it
   sidesteps the generic-over-`Ctx` handler/proc-macro conflict. `utoipa`'s `axum_extras`
   feature (from the shared workspace pin) only toggles `utoipa-gen` codegen and pulls no
   `axum` crate.
3. **Conformance suite home = a new non-published workspace member at
   `tests/runtime-http-conformance/`.** `publish = false` + release-plz `release = false`,
   following the `paigasus-helikon-sessions-testkit` precedent. It boots *both* an axum
   and an actix `AgentServer` and asserts wire-format equivalence (§9).
4. **Byte-parity scope:** byte-identical for all pure-JSON handler responses and each
   streamed JSON payload; decoded (order-insensitive) parity for `GET /agents`; structural
   parity for `/openapi.json`. SSE frame bytes: see decision 5. (Full table in §9.)
5. **SSE = hand-rolled `.streaming()` (recommended; GATE-1 confirmable).** Instead of the
   ticket's suggested `actix-web-lab::sse`, emit SSE with
   `HttpResponse::Ok().content_type("text/event-stream").streaming(byte_stream)`, framing
   each event exactly as axum's `to_sse_event` does. Rationale: (a) `actix-web-lab` is an
   explicitly experimental pre-1.0 staging crate — a poor foundation for a
   stability-guaranteed published crate; (b) hand-rolling lets us match axum's SSE framing
   **byte-for-byte**, promoting SSE into the byte-parity bucket; (c) it drops a dependency.
   *Alternative (ticket-literal):* use `actix-web-lab::sse` and keep SSE at structural
   parity. **This is a flagged GATE-1 decision.**
6. **Writer tasks + sweeper run on a shared multi-thread tokio runtime handle, not on
   per-worker `actix-rt`.** `AppStateInner` stores a `tokio::runtime::Handle` (a shared
   multi-thread runtime owned by the server); `spawn_writer` and the registry sweeper use
   `handle.spawn(...)`. This decouples a run's lifecycle from the actix worker that
   accepted it (an `actix-rt` worker is a per-worker current-thread runtime that can be
   recycled), matching axum's shared-runtime semantics and keeping detached
   (`?mode=async`) runs alive across worker churn. The sweeper is spawned exactly once
   (guarded by the existing `OnceCell`) at server construction / first `configure()` so
   the **embedded** path (AC #5) also evicts completed runs.

## 4. Crate shape — port map

`crates/paigasus-helikon-runtime-actix/` mirrors the axum module layout. Each module is
tagged by how much of the axum source it reuses:

| Module | Reuse | Notes |
|---|---|---|
| `dto.rs` | **verbatim** | Pure serde types. Identical source ⇒ identical JSON bytes ⇒ wire parity by construction. |
| `event_log.rs` | **verbatim** (±imports) | `EventLog`, `RunHandle`, `is_terminal`, `synthetic_terminal_frame`. tokio `Notify`-based, runtime-agnostic (works across actix workers). |
| `registry.rs` | **verbatim** (±imports) | `RunRegistry`, `create`, `get`, `sweep`, `note_terminal`; `spawn_sweeper` now takes/uses the shared tokio `Handle` (decision 6). |
| `session.rs` | **verbatim** (±imports) | `SessionProvider`, `InMemorySessionProvider`, `SessionLocks`. |
| `error.rs` | **mostly verbatim** | `ServerError`, `AuthRejection`, `ErrorBody`, status map unchanged. Replace axum `IntoResponse` with actix `ResponseError` (same `{"error": …}` body + same status codes). `AuthRejection.status` becomes `actix_web::http::StatusCode` (`http` 0.2); `StatusCode` `Display` is identical across `http` 0.2/1.x, so error-body bytes still match axum. |
| `context.rs` | **adapted** | `ContextProvider`, `DefaultContextProvider`; request param `Parts` → `&HttpRequest` (§6). |
| `auth.rs` | **adapted** | `AuthLayer`; request param `&mut Parts` → `&HttpRequest` (§6). |
| `server.rs` | **new** | `AgentServer` / `AgentServerBuilder`; owns the shared tokio runtime; `serve()`/`serve_with_listener()` + `configure()`. |
| `handlers/agents.rs` | **new** | `GET /agents`. |
| `handlers/runs.rs` | **new** | `POST /agents/{name}/runs` (one-shot / sse / async); manual payload read (§7). |
| `handlers/events.rs` | **new** | `GET /agents/{name}/runs/{id}/events` (WebSocket via `actix-ws`). |
| `handlers/openapi.rs` | **near-verbatim** | Same `ApiDoc` + augmentation; served via `web::Json`. Feature-gated behind `openapi` (default). |
| `middleware.rs` (new, small) | **new** | Hand-rolled auth `Transform` wrapping the inner root scope (§6). |
| `lib.rs` | **new** | Same `pub use` set as the axum crate. |

> The verbatim reuse is a deliberate consequence of the "no shared crate" constraint and
> is what makes byte-for-byte JSON parity *structural* rather than aspirational: the
> JSON-bearing code is literally the same, and the conformance test guards against drift.

## 5. Public API

Re-exported from `lib.rs`, matching the axum crate name-for-name:

```
pub use error::{AuthRejection, ServerError};
pub use session::{InMemorySessionProvider, SessionProvider};
pub use context::{ContextProvider, DefaultContextProvider};
pub use auth::AuthLayer;
pub use dto::{AgentInfo, AsyncAccepted, RunRequest, RunResponse, RunStatus};
pub use server::{AgentServer, AgentServerBuilder};
```

`AgentServerBuilder` methods are identical to axum: `agent`, `runner`,
`session_provider`, `context_provider`, `with_default_context` (when `Ctx: Default`),
`auth`, `run_config`, `run_retention`, `max_retained_runs`, `max_sessions`, `build`.

### 5.1 Unavoidable framework deltas (the *complete* list)

These are the only public-surface / usage differences from the axum crate. All are
inherent to the framework and are documented in the crate docs + book:

1. **Mount seam:** `configure() -> impl Fn(&mut ServiceConfig) + Send + Clone + 'static`
   replaces `router() -> axum::Router`. (The full `Send + 'static` bound is required
   because `HttpServer::new`'s app factory is `Fn() -> App + Send + Clone + 'static`; the
   captured `Arc<AppStateInner<Ctx>>` satisfies it given `Ctx: Send + Sync + 'static`.)
2. **Listener type:** `serve_with_listener(listener: std::net::TcpListener)` — actix's
   `HttpServer::listen` takes a **`std::net::TcpListener`** (set non-blocking), not axum's
   `tokio::net::TcpListener`. `serve(addr)` keeps its signature.
3. **Runtime attribute:** awaiting the standalone `serve()` requires an `actix-rt`
   `System`, so the entrypoint uses `#[actix_web::main]` rather than `#[tokio::main]`. A
   custom `AuthLayer` or `ContextProvider` must be re-implemented against `HttpRequest`
   (its request type is the framework). These are the "swap is not literally one line"
   caveats.

### 5.2 Usage

```rust,ignore
// Standalone:
#[actix_web::main]                                 // NOT #[tokio::main]
async fn main() -> std::io::Result<()> {
    AgentServer::<()>::builder()
        .with_default_context()
        .agent(Arc::new(EchoAgent))
        .build()?
        .serve("127.0.0.1:8080")
        .await
}

// Embedded in an existing actix-web App:
let cfg = server.configure();                      // Fn(&mut ServiceConfig)+Send+Clone+'static
HttpServer::new(move || {
    App::new()
        .service(existing_routes())
        .configure(cfg.clone())                    // mounts /agents, /openapi.json, … at root
})
.bind(addr)?
.run()
.await?;
```

## 6. Auth & Context adaptation

Both traits keep their names and semantics; only the request-metadata type changes,
because the request type *is* the framework.

```rust,ignore
#[async_trait]
pub trait AuthLayer: Send + Sync {
    async fn authenticate(&self, req: &HttpRequest) -> Result<(), AuthRejection>;
}

#[async_trait]
pub trait ContextProvider<Ctx>: Send + Sync where Ctx: Send + Sync + 'static {
    async fn build(
        &self,
        req: &HttpRequest,
        session: Arc<dyn Session>,
        cancel: CancellationToken,
    ) -> Result<RunContext<Ctx>, ServerError>;
}
```

- **Identity hand-off preserved (verified).** actix's middleware `ServiceRequest` and the
  handler's `HttpRequest` share **one** `RefCell<Extensions>`, and
  `HttpRequest::extensions_mut(&self)` uses interior mutability. So an `AuthLayer` inserts
  identity via `req.extensions_mut().insert(..)` through a shared `&HttpRequest`, and
  `ContextProvider::build` reads it back via `req.extensions()` — same auth→context bridge
  as axum, with no `&mut` and no drop between middleware and handler.
- **RefCell caveat (documented):** because extensions are `RefCell`-backed, an impl must
  **drop the `RefMut` from `extensions_mut()` before `.await`**-ing, and must not read
  `extensions()` while a mutable borrow is live — otherwise a runtime `already borrowed`
  panic, not a compile error. Documented in the trait docs.
- **Uniform gating.** When an `AuthLayer` is configured it runs before *every* route
  (parity with axum's router-level gate), implemented as a hand-rolled actix `Transform`
  (`middleware.rs`) wrapped on the inner root `web::scope("")` that `configure()`
  registers (decision 1). When no `AuthLayer` is set, the scope is registered without the
  wrap. Hand-rolling the `Transform` avoids taking `actix-web-lab` as a dependency for
  middleware.

## 7. Endpoints & transports

Identical set and semantics to the axum runtime:

| Method | Path | Behaviour |
|---|---|---|
| `GET` | `/agents` | JSON array of `AgentInfo` (HashMap order — unspecified). |
| `POST` | `/agents/{name}/runs` | One-shot: block to terminal, return aggregated `RunResponse` JSON + `X-Run-Id`. Cancels the run if the client disconnects. |
| `POST` | `/agents/{name}/runs?stream=sse` | SSE: replay-from-0 then live-tail; one event per frame; synthetic terminal `run_failed` on a terminal-less close; `Content-Type: text/event-stream`. Cancels the run on disconnect. |
| `POST` | `/agents/{name}/runs?mode=async` | Detach; `202 Accepted` + `{run_id}` immediately. Run is **not** cancelled on disconnect (detached). |
| `GET` | `/agents/{name}/runs/{id}/events` | WebSocket (`actix-ws`): **400** on a non-UUID id; **404-before-upgrade** on unknown/agent-mismatched run; else replay-from-0, live-tail to terminal; read-only observer (disconnect does **not** cancel the run); synthetic terminal frame on terminal-less close. |
| `GET` | `/openapi.json` | Static `ApiDoc` + sorted runtime agent list (feature `openapi`, default on). |

**Shared execution model (reused unchanged):** every run spawns one **writer task**
(via the shared tokio `Handle`, decision 6) that drives the agent through the `Runner`
and drains events into the run's `EventLog`; response handlers merely *subscribe*. This
is what makes runs replayable across all four transports and lets `?mode=async` return
early. Per-session serialization lock (owned guard moved into the writer task), the
`X-Run-Id` header, `X-Session-Id` affinity, and `?stream`/`?mode` validation (invalid or
conflicting selectors → 400) all carry over verbatim in behaviour.

**Implementation notes forced by actix (must be specified, not assumed):**

- **Body limits & content-type — do NOT use default extractors.** actix's `web::Json`/
  `web::Bytes` default to a **256 KiB** cap and emit actix's own error body. To preserve
  axum's 2 MiB cap and the `{"error": …}` body + tolerant content-type check
  (`handlers/runs.rs:215-237`), the handler reads `web::Payload` manually with an explicit
  2 MiB limit (or configures `PayloadConfig`) and runs the same content-type check. This
  also keeps all body-error responses on the `ServerError` path (relevant to §9 parity).
- **WebSocket loop.** Use `actix_ws::handle(&req, body)` → `(response, session,
  msg_stream)`, then drive the replay/live-tail loop with **`actix_rt::spawn`** (a local
  spawn on the worker — `actix_ws::Session`/`MessageStream` are not assumed `Send`), and
  return `response`. The loop's `EventLog` subscription is a tokio `Notify` await, which is
  runtime-agnostic and works on the worker's runtime. `tokio::select!` over the
  subscription + inbound frames as in axum.
- **Disconnect → cancel.** One-shot and SSE hold a `CancellationToken` `DropGuard` so a
  client disconnect cancels the run; `?mode=async` deliberately holds none. **The spike
  (below) must verify actix drops the response future promptly on disconnect** so the
  `DropGuard` actually fires (actix's disconnect semantics differ from hyper's).

**Concurrency spike — plan task 1 (expanded).** Before porting the rest, a spike proves,
on a **multi-worker** `HttpServer`: (a) one end-to-end streamed run; (b) a WebSocket
`events` subscription to a run whose writer was created on a *different* worker (exercises
the shared `EventLog` `Notify` across workers); (c) two same-`X-Session-Id` requests
landing on different workers serialize via the shared `SessionLocks`; (d) a client
disconnect on a one-shot/SSE run fires the cancel `DropGuard`. This de-risks decision 6
and the cross-worker paths the single-run happy path would miss.

## 8. OpenAPI (decision 2)

Reuse the axum crate's approach verbatim in spirit: non-generic `#[utoipa::path]`
documentation stubs, `#[derive(utoipa::OpenApi)] struct ApiDoc`, and a handler serving
`ApiDoc::openapi()` augmented with a sorted "Mounted agents" section in
`info.description`, via `web::Json`. `utoipa` via the workspace pin (`workspace = true`);
no `utoipa-actix-web`. AC #4 (no axum leakage) verified by the CI gate in §14.

## 9. Wire-format parity & conformance suite (decisions 3 & 4)

**Home:** a new **non-published workspace member** `tests/runtime-http-conformance/`
(added to `[workspace] members` alongside `crates/*`), `version = "0.0.0"`,
`publish = false`, with a matching `release = false` block in `release-plz.toml`.
Structure:

- `fixtures/` — shared scripted-agent definitions, request bodies, and golden expected
  outputs (JSON).
- `src/lib.rs` — a transport-agnostic, client-side verifier: a documented public
  `async fn check(base_url: &str)` that exercises every endpoint against a running server
  and asserts each response against the goldens; plus a shared `scripted_agents()`
  builder. **All public items are documented** (the crate is in the doc-coverage
  denominator — `scripts/check-doc-coverage.sh` excludes only the CLI — so it must either
  hit 80% doc coverage like `sessions-testkit`, or be added to `EXCLUDED_CRATES`; we
  document it, matching the testkit precedent).
- `tests/parity.rs` — boots an axum `AgentServer` **and** an actix `AgentServer` with
  identical agents on ephemeral ports, runs `check()` against each, and additionally
  asserts **axum-bytes == actix-bytes** for the JSON bodies (and SSE payloads).

**Booting both servers in one test process (BLOCKER fix — not symmetric).** The axum
server boots as today: `tokio::spawn(server.serve_with_listener(tokio_listener))` inside
`#[tokio::test]`. The actix server **cannot** be booted the same way — `HttpServer::run()`
requires an `actix_rt::System`, absent in a tokio test runtime. Instead the harness spawns
a **dedicated OS thread** that creates `actix_rt::System::new()` and `block_on`s the actix
server bound to a pre-selected `std::net::TcpListener`, and signals readiness back to the
tokio test over a `std::sync::mpsc` / oneshot channel. The tokio test then drives its
`reqwest`/`tokio-tungstenite` client against both base URLs.

It takes both runtime crates as **dev-dependencies**. A dev-dependency on axum does not
violate AC #4 — dev-deps are stripped from the published crate and absent from
`cargo build --features runtime-actix`. `cargo test --workspace` runs `parity.rs` in CI.

**Parity scope (revised):**

| Response | Parity level | Why |
|---|---|---|
| One-shot `RunResponse`, `202 AsyncAccepted`, error bodies (`ServerError`-originated) | **byte-identical** | Same serde DTOs / error type. |
| Each SSE `data:` payload JSON; each WS text frame | **byte-identical** | Same `serde_json::to_string(&AgentEvent)`. |
| SSE frame *framing* (field order/whitespace, `event:` tag) | **byte-identical** *if decision 5 = hand-rolled*; else structural | Hand-rolled `.streaming()` matches axum's `to_sse_event` bytes; `actix-web-lab::sse` may differ. |
| `Content-Type` (JSON `application/json`; SSE `text/event-stream`) + `X-Run-Id` value format + HTTP status codes | **asserted equal** | AC #1 curl SSE consumer needs `text/event-stream`. |
| `GET /agents` (`AgentInfo[]`) | **decoded set equality** (order-insensitive) | axum builds from `state.agents.values()` — HashMap order is nondeterministic across processes/runs (`handlers/agents.rs:11-23`), so byte-parity would be flaky. Compare as a set, or pin the byte fixture to a single agent. |
| `/openapi.json` | **structural** (valid 3.1 + 3 paths + `AgentInfo`/`AsyncAccepted`/`RunStatus` schemas) | Documented via stubs; not a byte contract. |

Fixtures must avoid inputs that trip **framework-level** rejections (oversized body,
malformed `Path`/`Query`) — those bypass `ServerError` and produce framework-default
bodies that legitimately differ between axum and actix; the byte-parity claim is scoped to
`ServerError`-originated errors.

## 10. Facade & workspace wiring

- **`[workspace.dependencies]`** (root `Cargo.toml`): add
  `paigasus-helikon-runtime-actix = { path = "crates/paigasus-helikon-runtime-actix", version = "0.1.0", default-features = false }`
  and third-party pins (latest compatible, resolved + `cargo tree`/`deny`-verified at
  implementation time):
  - `actix-web = { version = "4", default-features = false, features = ["macros"] }` —
    **no** `openssl`/`rustls`/`compress-*`/`http2`, to keep the workspace's
    rustls/aws-lc-rs TLS posture intact and avoid a second TLS/crypto stack (MEMORY:
    dual-CryptoProvider panic class). Confirm this minimal set still supports SSE + JSON +
    `actix-ws`; add only the feature(s) `actix-ws` demands.
  - `actix-ws = "0.3"` (or latest).
  - `actix-web-lab` — **only if** decision 5 chooses the ticket-literal SSE path;
    otherwise omitted entirely.
- **Facade `Cargo.toml`:** add optional dep
  `paigasus-helikon-runtime-actix = { workspace = true, optional = true, features = ["openapi"] }`
  and feature `runtime-actix = ["dep:paigasus-helikon-runtime-actix"]` (kebab-case). The
  `default-features = false` on the workspace pin + explicit `features = ["openapi"]` here
  mirrors the axum wiring exactly.
- **Facade `src/lib.rs`:**
  `#[cfg(feature = "runtime-actix")] pub use paigasus_helikon_runtime_actix as runtime_actix;`
  with a `///` doc comment (snake_case alias — the kebab/snake pairing rule).
- **`members`:** `crates/*` auto-includes the new crate; explicitly add
  `"tests/runtime-http-conformance"`.

## 11. Release plumbing

- New crate ships at **`0.1.0`** and is first-published by release-plz on merge. The name
  `paigasus-helikon-runtime-actix` is **confirmed available on crates.io** (preflight
  2026-07-18: HTTP 404). It is a brand-new crate, **not** a pre-claimed `0.0.0` stub, so it
  does **not** follow the stub-ascend `publish=false`→remove ritual — it simply publishes
  for the first time.
- It uses only **existing** `paigasus-helikon-core` API, so **no same-PR core bump** is
  required (avoids the cargo-verify-against-stale-registry-core trap).
- Facade cascade: release-plz performs the facade's dependent bump itself when it adds the
  new optional dep during the release PR, so the facade cascades normally. **Verify on the
  release-plz PR after merge**; if the cascade is suppressed, add a facade patch bump per
  the CLAUDE.md cascade caveat.
- Conformance crate publishes nothing (`publish = false` + `release = false`).
- Commit types on release/CI plumbing files stay `chore(...)`.

## 12. Deferred: shared HTTP core

The ticket's open question — whether to extract a shared
`paigasus-helikon-runtime-http-core` to dedupe route handling + wire format — stays
**deferred by design**. This spec's verbatim duplication of `dto/event_log/registry/
session/error` is the intended v0 state; the duplication is small, mechanical, and now
*measured* (both crates exist), which is exactly the precondition the ticket set for
revisiting the abstraction. A follow-up ticket can evaluate it. **Do not pre-abstract in
this PR.**

## 13. Documentation (same PR)

- `docs/book/src/concepts/runtimes.md`: add an actix row to the runtimes table.
- `docs/book/src/concepts/axum-server.md` (or a short sibling note): document the actix
  variant — API-identical, when to choose it, and the §5.1 deltas (`configure()` vs
  `router()`, `std` listener, `#[actix_web::main]`, custom-`AuthLayer`/`ContextProvider`
  framework-coupling). `mdbook build` must stay clean.
- New `crates/paigasus-helikon-runtime-actix/README.md` (crates.io landing page).
- Facade `crates/paigasus-helikon/README.md` + root `README.md`: crate roster + feature →
  module map gain the `runtime-actix` entry.
- Crate-level rustdoc mirroring the axum crate's.

## 14. Testing plan & CI

- **Ported unit/integration tests** (actix equivalents): `runs.rs` (one-shot / sse /
  async / selector-validation / unknown-agent / synthetic-terminal / 2 MiB cap /
  content-type), `ws.rs` (400-bad-UUID / 404-before-upgrade / replay+tail / terminal-less
  synthetic / read-only observer), `auth.rs` (gate + identity hand-off + RefCell-safe
  insertion), `openapi.rs` (paths + schemas + agent augmentation), `server.rs` (builder
  errors: duplicate agent, missing context provider, zero max_sessions), `concurrency.rs`
  (same-`X-Session-Id` serialization).
- **Conformance parity test** (`tests/runtime-http-conformance/tests/parity.rs`): axum vs
  actix per §9, with the dedicated-`System`-thread harness.
- **Example** `crates/paigasus-helikon-runtime-actix/examples/actix_embed.rs`: an
  `EchoAgent` mounted via `configure()` inside a host `App` that also serves an unrelated
  route (`#[actix_web::main]`, direct `actix-web` dev-dep), with curl invocations in the
  module docs. Satisfies AC #1 and AC #5.
- **CI gates (new/changed):**
  - Extend the `build-no-default-features` job to also run
    `cargo build -p paigasus-helikon-runtime-actix --no-default-features` (actix has
    `default = ["openapi"]` + a feature-gated module — the exact SMA-452 regression class).
  - Add a scripted, **required** no-axum-leakage check, e.g.
    `! cargo tree -p paigasus-helikon --features runtime-actix -e no-dev | grep -qx 'axum'`
    (fails if axum appears in the non-dev graph), enforcing AC #4.
  - Update `.github/rulesets/main-protection-checks.json` if either check should be a
    required context.
  - Existing gates unchanged: `fmt`, `clippy --all-features --all-targets`,
    `test --workspace --all-features`, `doc -D warnings`, `doc-coverage`.

## 15. Acceptance-criteria mapping

| Ticket AC | Satisfied by |
|---|---|
| Simple agent runs via `curl` against actix | `examples/actix_embed.rs` + one-shot handler (§7) |
| SSE emits the same `AgentEvent`s as axum, via a shared suite in `tests/runtime-http-conformance/` | §9 conformance crate + hand-rolled SSE handler |
| WebSocket handshake + stream without `actix-web-actors` | `actix-ws` events handler (§7) |
| `cargo build --features runtime-actix` works in isolation, no axum leakage | §8 (utoipa no-axum), §10 wiring, §14 scripted required gate |
| `examples/actix_embed.rs` embeds in an existing actix-web tree | §14 example via `configure()` (§5.2) |

## 16. Risks

| Risk | Mitigation |
|---|---|
| Writer tasks / sweeper pinned to a recyclable per-worker `actix-rt` runtime | Decision 6: shared tokio `Handle` in `AppStateInner`; sweeper spawned once from construction/`configure()`. Multi-worker spike (§7). |
| Booting axum + actix in one test process (actix needs its own `System`) | Dedicated-thread `actix_rt::System` harness + readiness channel (§9). |
| `GET /agents` byte-parity flaky (HashMap order) | Decoded set-equality, not byte comparison (§9). |
| Default `web::Json`/`web::Bytes` silently cap at 256 KiB + wrong error body | Manual `web::Payload` read with 2 MiB cap + custom content-type check (§7). |
| Raw SSE-byte divergence | Hand-rolled `.streaming()` matches axum bytes (decision 5); parity table falls back to structural if `actix-web-lab` is chosen. |
| actix drops response future on disconnect differently than hyper (cancel semantics) | Spike verifies the `DropGuard` fires on disconnect (§7). |
| `actix-web` default features pulling openssl / a second TLS stack | `default-features = false` + minimal feature set; `cargo tree`/`deny`/`audit` verified (§10). |
| Facade version cascade suppressed | Inspect the release-plz PR post-merge; add a facade patch bump if needed (§11). |
| Conformance crate counted in doc-coverage denominator | Document its public items (testkit precedent) or add to `EXCLUDED_CRATES` (§9). |

## 17. Challenge changelog (adversarial spec-challenge, 2026-07-18)

Folded in (all justified): dedicated-`System`-thread conformance harness; auth via inner
`scope("").wrap()` (ServiceConfig has no `wrap()`); `GET /agents` → set-equality (HashMap
order); shared tokio `Handle` for writer/sweeper + sweeper-in-embed-path; `std::net::TcpListener`
signature; `#[actix_web::main]` requirement; manual 2 MiB payload read (not default
extractors); full `Send + Clone + 'static` bound on `configure()`; hand-rolled SSE via
`.streaming()` (drops experimental `actix-web-lab`, enables SSE byte-parity) — **flagged
GATE-1 decision**; `build-no-default-features` + scripted no-axum-leakage as CI gates;
Content-Type/Cache-Control assertions; conformance-crate doc-coverage handling;
`examples/actix_embed.rs` crate-relative path; WS 400-bad-UUID + cancel-on-disconnect-vs-
detach; error-body byte-parity scoped to `ServerError`; RefCell drop-before-await note;
`actix_ws` + `actix_rt::spawn` WS loop; crates.io name preflight (available). Confirmed
strengths preserved: runtime-agnostic tokio-primitive internals, the auth→context
extensions bridge, `StatusCode`-`Display` parity across the `http`-version skew, and
deferring the shared HTTP core.
