# Design — `paigasus-helikon-runtime-actix` (SMA-343)

- **Ticket:** [SMA-343](https://linear.app/smaschek/issue/SMA-343) — *paigasus-helikon-runtime-actix: actix-web REST/SSE/WebSocket server*
- **Companion to:** [SMA-331](https://linear.app/smaschek/issue/SMA-331) (`paigasus-helikon-runtime-axum`)
- **Status:** Approved (design), 2026-07-18
- **Branch:** `feature/sma-343-paigasus-helikon-runtime-actix-actix-web-restssewebsocket`

## 1. Motivation

The wider Paigasus platform standardizes on **actix-web**. Helikon today ships only an
axum HTTP runtime (`paigasus-helikon-runtime-axum`), so embedding an agent into an
existing actix-web service forces a framework switch. This ticket adds a first-party
actix runtime that exposes the **same public surface** as the axum runtime, so moving a
Helikon agent server from axum to actix (or dropping one into an existing actix app)
changes essentially only the dependency line and the mount call.

- axum stays the documented default for greenfield Helikon users.
- actix is the path for "drop this into an existing actix-web service."

## 2. Goals / non-goals

**Goals**

- A new crate `paigasus-helikon-runtime-actix` whose `AgentServer` builder + DTOs +
  trait *names* match the axum runtime.
- Endpoints identical to the axum runtime (REST one-shot, `?stream=sse`, `?mode=async`,
  WebSocket events, `GET /agents`, `GET /openapi.json`).
- SSE via `actix-web-lab::sse`; WebSocket via `actix-ws` (**not** `actix-web-actors`).
- Session affinity via `X-Session-Id`.
- OpenAPI spec generation via `utoipa`.
- Optional auth-middleware trait (no built-in implementation).
- A shared conformance test proving wire-format parity between the axum and actix
  servers.
- `cargo build --features runtime-actix` from the facade works in isolation, with **no
  axum dependency leakage**.
- An `examples/actix_embed.rs` demonstrating embedding into an existing actix-web app.

**Non-goals**

- **No shared `paigasus-helikon-runtime-http-core` crate.** The ticket explicitly defers
  the "extract a shared HTTP core" decision until both runtimes exist and the real
  overlap is visible. This design deliberately *duplicates* the framework-agnostic
  internals rather than pre-abstracting them. (See §12.)
- No new transports, no auth implementations, no changes to `paigasus-helikon-core`.

## 3. Key design decisions (approved 2026-07-18)

1. **Mount seam = `configure()`.** The actix analog of axum's `router() -> axum::Router`
   is a method returning an `impl Fn(&mut actix_web::web::ServiceConfig) + Clone` that the
   host passes to `App::configure(...)`. Routes mount at root, mirroring axum's
   root-mounted router. (Chosen over a path-prefixed `Scope`.)
2. **OpenAPI = mirror axum's manual handler.** Reuse the same hand-built `utoipa`
   `ApiDoc` + runtime agent-list augmentation, served by a plain `web::Json` handler.
   This gives near-free `/openapi.json` parity with axum, drops the `utoipa-actix-web`
   dependency, and sidesteps the generic-over-`Ctx` handler/proc-macro conflict that the
   axum crate documented. (`utoipa-actix-web`'s auto-collection buys little here since our
   handlers are generic and paths are documented via non-generic stubs.)
3. **Conformance suite home = a new non-published workspace member at
   `tests/runtime-http-conformance/`.** It boots *both* an axum and an actix
   `AgentServer` and asserts byte-for-byte equivalence. `publish = false` +
   release-plz `release = false`, following the `paigasus-helikon-sessions-testkit`
   precedent.
4. **Byte-parity scope:** byte-identical for all pure-JSON responses and each streamed
   JSON payload; structural (decoded) parity for SSE frame whitespace and for
   `/openapi.json`. (See §9.)

## 4. Crate shape — port map

`crates/paigasus-helikon-runtime-actix/` mirrors the axum module layout. Each module is
tagged by how much of the axum source it reuses:

| Module | Reuse | Notes |
|---|---|---|
| `dto.rs` | **verbatim** | Pure serde types (`RunRequest`, `RunResponse`, `RunStatus`, `AsyncAccepted`, `AgentInfo`). Identical source ⇒ identical JSON bytes ⇒ wire parity by construction. |
| `event_log.rs` | **verbatim** (±imports) | `EventLog`, `RunHandle`, `is_terminal`, `synthetic_terminal_frame`. Framework-agnostic (tokio/uuid/core). |
| `registry.rs` | **verbatim** (±imports) | `RunRegistry`, `create`, `get`, sweeper, `note_terminal`. |
| `session.rs` | **verbatim** (±imports) | `SessionProvider`, `InMemorySessionProvider`, `SessionLocks`. |
| `error.rs` | **mostly verbatim** | `ServerError`, `AuthRejection`, `ErrorBody`, status map unchanged. Replace axum `IntoResponse` with actix `ResponseError` (same `{"error": …}` body + same status codes). `AuthRejection.status` becomes `actix_web::http::StatusCode`. |
| `context.rs` | **adapted** | `ContextProvider`, `DefaultContextProvider`; request param `Parts` → `&HttpRequest` (§6). |
| `auth.rs` | **adapted** | `AuthLayer`; request param `&mut Parts` → `&HttpRequest` (extensions via interior mutability) (§6). |
| `server.rs` | **new** | `AgentServer` / `AgentServerBuilder`; `serve()`/`serve_with_listener()` + `configure()`. |
| `handlers/agents.rs` | **new** | `GET /agents`. |
| `handlers/runs.rs` | **new** | `POST /agents/{name}/runs` (one-shot / sse / async). |
| `handlers/events.rs` | **new** | `GET /agents/{name}/runs/{id}/events` (WebSocket). |
| `handlers/openapi.rs` | **near-verbatim** | Same `ApiDoc` + augmentation; served via `web::Json`. Feature-gated behind `openapi` (default). |
| `lib.rs` | **new** | Same `pub use` set as the axum crate. |

> The verbatim reuse is a deliberate consequence of the "no shared crate" constraint. It
> is the mechanism that makes byte-for-byte parity *structural* rather than aspirational:
> the JSON-bearing code is literally the same, and the conformance test guards against
> future drift.

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

**Framework-specific deltas** (the only public-surface differences from axum, all
unavoidable):

- `configure() -> impl Fn(&mut ServiceConfig) + Clone` replaces `router() -> axum::Router`.
- `serve(addr)` / `serve_with_listener(listener)` are implemented on `HttpServer` instead
  of `axum::serve`, but keep the same signatures.
- The request-metadata parameter type in `AuthLayer` and `ContextProvider` (see §6).

### `AgentServer` usage

```rust,ignore
// Standalone (identical to the axum crate):
AgentServer::<()>::builder()
    .with_default_context()
    .agent(Arc::new(EchoAgent))
    .build()?
    .serve("127.0.0.1:8080")
    .await?;

// Embedded in an existing actix-web App:
let cfg = server.configure();                 // impl Fn(&mut ServiceConfig) + Clone
HttpServer::new(move || {
    App::new()
        .service(existing_routes())
        .configure(cfg.clone())               // mounts /agents, /openapi.json, …
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

- **Identity hand-off preserved.** actix request extensions use interior mutability, so an
  `AuthLayer` inserts identity via `req.extensions_mut().insert(..)` through a shared
  `&HttpRequest`; `ContextProvider::build` reads it back via `req.extensions()`. Same
  auth→context bridge as axum, no `&mut` needed.
- **Uniform gating.** When an `AuthLayer` is configured it runs before *every* route
  (parity with axum's router-level middleware), implemented as a small actix middleware
  (e.g. `actix-web-lab::middleware::from_fn` or a hand-rolled `Transform`).
- **DefaultContextProvider** builds `RunContext::ephemeral(Ctx::default()).with_session(..).with_cancel(..)`,
  identical to axum.

**Documented scope caveat (goes in the crate docs + book):** "swap only the dependency
line" holds for consumers using the DTOs, the builder, and the default providers. A
consumer who implements a **custom `AuthLayer` or `ContextProvider`** must re-implement it
against `HttpRequest` — those two impls are inherently framework-coupled.

## 7. Endpoints & transports

Identical set and semantics to the axum runtime:

| Method | Path | Behaviour |
|---|---|---|
| `GET` | `/agents` | JSON array of `AgentInfo`. |
| `POST` | `/agents/{name}/runs` | One-shot: block to terminal, return aggregated `RunResponse` JSON + `X-Run-Id`. |
| `POST` | `/agents/{name}/runs?stream=sse` | SSE: replay-from-0 then live-tail; one event per frame; synthetic terminal `run_failed` on a terminal-less close. `actix-web-lab::sse`. |
| `POST` | `/agents/{name}/runs?mode=async` | Detach; `202 Accepted` + `{run_id}` immediately. |
| `GET` | `/agents/{name}/runs/{id}/events` | WebSocket (`actix-ws`): 404-before-upgrade; replay-from-0, live-tail to terminal; read-only (disconnect does not cancel); synthetic terminal frame on terminal-less close. |
| `GET` | `/openapi.json` | Static `ApiDoc` + runtime agent list (feature `openapi`, default on). |

**Shared execution model (reused unchanged):** every run spawns one **writer task** that
drives the agent through the `Runner` and drains events into the run's `EventLog`;
response handlers merely *subscribe*. This is what makes runs replayable across all four
transports and lets `?mode=async` return early. Per-session serialization lock (owned
guard moved into the writer task), 2 MiB body cap, JSON content-type check, `X-Run-Id`
header, and `?stream`/`?mode` validation (invalid or conflicting selectors → 400) all
carry over verbatim in behaviour.

**Concurrency risk (de-risked first in the plan):** actix workers run on `actix-rt`
(per-worker current-thread tokio). The reused `tokio::spawn` writer tasks + registry
sweeper must behave correctly under it. **Plan task 1 is a spike** that boots a scripted
agent on actix and proves one end-to-end streamed run before the rest of the port
proceeds.

## 8. OpenAPI (decision 2 — mirror axum)

Reuse the axum crate's approach verbatim in spirit: non-generic `#[utoipa::path]`
documentation stubs, a `#[derive(utoipa::OpenApi)] struct ApiDoc`, and a handler that
serves `ApiDoc::openapi()` augmented with a sorted "Mounted agents" section in
`info.description`. The only actix-specific change is serving it via `web::Json` instead
of axum's `Json`. `utoipa` is pulled via the workspace pin (`workspace = true`); its
`axum_extras` feature only toggles `utoipa-gen` codegen and pulls **no** `axum` crate, so
AC #4 (no axum leakage) holds. No `utoipa-actix-web` dependency.

## 9. Wire-format parity & conformance suite (decisions 3 & 4)

**Home:** a new **non-published workspace member** `tests/runtime-http-conformance/`
(added to `[workspace] members`), `version = "0.0.0"`, `publish = false`, with a
matching `release = false` block in `release-plz.toml`. Structure:

- `fixtures/` — shared scripted-agent definitions, request bodies, and golden expected
  outputs (JSON).
- `src/lib.rs` — a transport-agnostic, client-side verifier: `async fn check(base_url)`
  that exercises every endpoint against a running server and asserts each response
  against the goldens; plus a shared `scripted_agents()` builder.
- `tests/parity.rs` — boots an axum `AgentServer` **and** an actix `AgentServer` with
  identical agents on ephemeral ports, runs `check()` against each, and additionally
  asserts **axum-bytes == actix-bytes** for the JSON bodies and SSE `data:` payloads.

It takes both runtime crates as **dev-dependencies**. A dev-dependency on axum does not
violate AC #4 — dev-deps are stripped from the published crate and absent from
`cargo build --features runtime-actix`. `cargo test --workspace` runs `parity.rs` in CI.
The crate opts out of `missing_docs` locally (internal test harness) or documents its one
public `check` fn.

**Parity scope:**

- **Byte-identical:** one-shot `RunResponse`, `202 AsyncAccepted`, `GET /agents`
  (`AgentInfo[]`), error bodies (`{"error": …}`); each SSE `data:` payload's JSON; each
  WebSocket text frame; HTTP status codes; the `X-Run-Id` header value format.
- **Structural (decoded) parity, not raw bytes:** SSE frame framing/whitespace (axum's
  `axum::response::sse` vs `actix-web-lab::sse` may differ in field spacing) — assert the
  decoded event sequence + `event:` type tag; and `/openapi.json` — assert valid OpenAPI
  3.1 containing the three documented paths + the `AgentInfo`/`AsyncAccepted`/`RunStatus`
  schemas.

## 10. Facade & workspace wiring

- **`[workspace.dependencies]`** (root `Cargo.toml`): add
  `paigasus-helikon-runtime-actix = { path = "crates/paigasus-helikon-runtime-actix", version = "0.1.0", default-features = false }`
  and third-party pins `actix-web`, `actix-ws`, `actix-web-lab` (latest compatible,
  resolved at implementation time; `actix-web` `default-features = false` + only the
  features we need to avoid leakage).
- **Facade `Cargo.toml`:** add optional dep
  `paigasus-helikon-runtime-actix = { workspace = true, optional = true, features = ["openapi"] }`
  and feature `runtime-actix = ["dep:paigasus-helikon-runtime-actix"]` (kebab-case).
- **Facade `src/lib.rs`:**
  `#[cfg(feature = "runtime-actix")] pub use paigasus_helikon_runtime_actix as runtime_actix;`
  with a `///` doc comment (snake_case alias — the kebab/snake pairing rule).
- **`members`:** `crates/*` already auto-includes the new crate; explicitly add
  `"tests/runtime-http-conformance"`.

## 11. Release plumbing

- New crate ships at **`0.1.0`** and is first-published by release-plz on merge. It uses
  only **existing** `paigasus-helikon-core` API, so **no same-PR core bump** is required
  (avoids the cargo-verify-against-stale-registry-core trap).
- Because release-plz performs the facade's dependent bump itself (the new sibling starts
  at its target `0.1.0` and release-plz adds the optional dep during the release PR), the
  facade cascades normally. If the sibling version is pre-set in this PR in a way that
  suppresses the cascade, add a facade patch bump — decided at PR time by inspecting the
  release-plz PR (per the CLAUDE.md cascade caveat).
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
  variant — API-identical, when to choose it, the `configure()`-vs-`router()` delta, and
  the custom-`AuthLayer`/`ContextProvider` framework-coupling caveat. `mdbook build` must
  stay clean.
- New `crates/paigasus-helikon-runtime-actix/README.md` (crates.io landing page).
- Facade `crates/paigasus-helikon/README.md` + root `README.md`: crate roster + feature →
  module map gain the `runtime-actix` entry.
- Crate-level rustdoc mirroring the axum crate's.

## 14. Testing plan

- **Ported unit/integration tests** (actix equivalents): `runs.rs` (one-shot / sse /
  async / selector-validation / unknown-agent / synthetic-terminal), `ws.rs`
  (404-before-upgrade / replay+tail / terminal-less synthetic), `auth.rs` (gate +
  identity hand-off), `openapi.rs` (paths + schemas + agent augmentation), `server.rs`
  (builder errors: duplicate agent, missing context provider, zero max_sessions),
  `concurrency.rs` (same-`X-Session-Id` serialization).
- **Conformance parity test** (`tests/runtime-http-conformance/tests/parity.rs`): axum vs
  actix byte/structural parity per §9.
- **Example** `examples/actix_embed.rs`: an `EchoAgent` mounted via `configure()` inside a
  host `App` that also serves an unrelated route, with curl invocations in the module
  docs. Satisfies AC #1 and AC #5.
- **CI gates:** the change must pass `fmt`, `clippy --all-features --all-targets`,
  `test --workspace --all-features`, `doc -D warnings`, doc-coverage, and — notably —
  `build-no-default-features` (SMA-452) and a facade-isolation build
  `cargo build -p paigasus-helikon --features runtime-actix` proving no axum leakage
  (`cargo tree` must show no `axum` under the actix feature).

## 15. Acceptance-criteria mapping

| Ticket AC | Satisfied by |
|---|---|
| Simple agent runs via `curl` against actix | `examples/actix_embed.rs` + one-shot handler (§7) |
| SSE emits the same `AgentEvent`s as axum, via a shared suite in `tests/runtime-http-conformance/` | §9 conformance crate + `actix-web-lab::sse` handler |
| WebSocket handshake + stream without `actix-web-actors` | `actix-ws` events handler (§7) |
| `cargo build --features runtime-actix` works in isolation, no axum leakage | §8 (utoipa no-axum), §10 wiring, §14 isolation gate |
| `examples/actix_embed.rs` embeds in an existing actix-web tree | §14 example via `configure()` (§5) |

## 16. Risks

| Risk | Mitigation |
|---|---|
| `tokio::spawn` writer tasks / sweeper under `actix-rt` current-thread workers | Plan task 1 spike proves an end-to-end streamed run before porting the rest. |
| Raw SSE-byte divergence between axum-sse and actix-web-lab-sse | Parity scoped to decoded payload + `event:` tag (§9), not raw framing. |
| actix-web pulling unexpected transitive deps (TLS/openssl) that bloat or conflict | `actix-web` with `default-features = false`, minimal feature set; verify `cargo tree` + audit/deny stay green. |
| Facade version cascade suppressed | Inspect the release-plz PR post-merge; add a facade patch bump if needed (§11). |
