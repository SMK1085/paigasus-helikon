# `paigasus-helikon-runtime-actix` Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship `paigasus-helikon-runtime-actix` — an actix-web REST/SSE/WebSocket agent-server runtime with the same public surface as `paigasus-helikon-runtime-axum`, plus a shared conformance suite proving wire-format parity.

**Architecture:** The framework-agnostic internals (`dto`, `event_log`, `registry`, `session`) are copied verbatim from the axum crate; a thin actix adapter layer (`server`, `handlers/*`, `middleware`, adapted `auth`/`context`/`error`) reimplements the transport. Writer tasks and the run-registry sweeper run on a shared multi-thread tokio runtime owned by the server, decoupling runs from actix's per-worker `actix-rt` runtimes. A non-published `tests/runtime-http-conformance` workspace member boots both an axum and an actix server and asserts byte/structural parity.

**Tech Stack:** Rust (edition 2021, MSRV 1.94), actix-web 4 (`default-features = false`), actix-ws, tokio (multi-thread), utoipa (openapi feature), serde/serde_json, uuid, async-trait, thiserror.

**Reference spec:** `docs/superpowers/specs/2026-07-18-runtime-actix-design.md` (read it first).
**Reference implementation to mirror:** `crates/paigasus-helikon-runtime-axum/` (the source of every verbatim-ported file).

## Global Constraints

- **Workspace inheritance is mandatory.** Each crate `Cargo.toml` sets only `name`, `description`, per-crate `version`, and crate-specific `[dependencies]`/`[features]`. Everything else (`edition`, `rust-version`, `authors`, `license`, `repository`, `homepage`, `keywords`, `categories`) is `*.workspace = true`.
- **MSRV = 1.94**, license `Apache-2.0 OR MIT` (inherited). `#![forbid(unsafe_code)]` at crate root.
- **`missing_docs` lint:** the new library crate opts in with `[lints] workspace = true`; every `pub` item needs a `///` doc comment. The facade re-export needs a `///` doc comment.
- **Feature naming:** kebab-case in `[features]` (`runtime-actix`), snake-case in the facade `pub use` alias (`runtime_actix`). Keep the pair in sync across facade `Cargo.toml` and `src/lib.rs`.
- **No axum dependency leakage** under `--features runtime-actix` (AC #4). Enforced by a scripted CI gate (Task 15).
- **Third-party version pins live in root `[workspace.dependencies]`;** members reference via `dep.workspace = true`. Internal crate paths are workspace deps too.
- **`actix-web` uses `default-features = false`** + a minimal feature set — **no** openssl/rustls/compress/http2 — to preserve the workspace's rustls/aws-lc-rs TLS posture.
- **Byte-parity scope (§9 of spec):** byte-identical for JSON handler bodies + streamed JSON payloads + hand-rolled SSE frames; decoded set-equality for `GET /agents`; structural for `/openapi.json`.
- **Commit prefix:** `<type>(<scope>): SMA-343 <lowercase subject>` (e.g. `feat(runtime-actix): SMA-343 add …`). Run `cargo fmt --all` + `cargo clippy --workspace --all-features --all-targets -- -D warnings` before every commit (pre-push hook enforces the first two CI gates). Commits are signed via the 1Password SSH key.
- **Do NOT pre-abstract a shared `runtime-http-core` crate** (ticket open question, deferred).
- **Docs & READMEs update in this same PR** (book runtimes page, crate README, facade README, root README) — see Task 16.

---

## File structure

New crate `crates/paigasus-helikon-runtime-actix/`:

| File | Responsibility | Origin |
|---|---|---|
| `Cargo.toml` | crate manifest, deps, `openapi` feature | new |
| `src/lib.rs` | module tree + `pub use` re-exports | new (mirror axum) |
| `src/dto.rs` | wire DTOs | **verbatim** copy |
| `src/event_log.rs` | replayable event log | **verbatim** copy (±spawn handle) |
| `src/registry.rs` | run registry + sweeper | **verbatim** copy (sweeper takes tokio `Handle`) |
| `src/session.rs` | session provider + locks | **verbatim** copy |
| `src/error.rs` | `ServerError`/`AuthRejection` + actix `ResponseError` | adapted |
| `src/context.rs` | `ContextProvider` (`&HttpRequest`) | adapted |
| `src/auth.rs` | `AuthLayer` trait (`&HttpRequest`) | adapted |
| `src/middleware.rs` | auth `Transform` | new |
| `src/server.rs` | `AgentServer`/builder, shared runtime, `configure()`/`serve()` | new |
| `src/handlers/mod.rs` | handler module tree | new |
| `src/handlers/agents.rs` | `GET /agents` | new |
| `src/handlers/runs.rs` | `POST …/runs` (oneshot/sse/async) | new |
| `src/handlers/events.rs` | `GET …/events` (WebSocket) | new |
| `src/handlers/openapi.rs` | `GET /openapi.json` | near-verbatim |
| `tests/*.rs`, `tests/support/mod.rs` | integration tests | new (mirror axum) |
| `examples/actix_embed.rs` | embedding demo | new |
| `README.md`, `CHANGELOG.md` | crate docs | new |

New workspace member `tests/runtime-http-conformance/`:

| File | Responsibility |
|---|---|
| `Cargo.toml` | non-published (`publish=false`, `version="0.0.0"`); dev-deps on both runtimes |
| `src/lib.rs` | `pub async fn check(base_url)` + `scripted_agents()` + goldens |
| `fixtures/*.json` | shared request/response fixtures |
| `tests/parity.rs` | boots axum + actix, asserts parity |

Modified: root `Cargo.toml` (`members`, `[workspace.dependencies]`), `crates/paigasus-helikon/Cargo.toml` + `src/lib.rs` (facade), `release-plz.toml`, `.github/workflows/ci.yml`, `.github/rulesets/main-protection-checks.json`, docs/READMEs.

---

## Task 1: Crate scaffold + workspace wiring (compiles empty, no axum leak)

**Files:**
- Create: `crates/paigasus-helikon-runtime-actix/Cargo.toml`
- Create: `crates/paigasus-helikon-runtime-actix/src/lib.rs`
- Modify: `Cargo.toml` (root — `[workspace.dependencies]`)

**Interfaces:**
- Produces: the crate `paigasus-helikon-runtime-actix` (empty lib), workspace dep pins `actix-web`, `actix-ws`, and the internal path dep.

- [ ] **Step 1: Add third-party pins to root `[workspace.dependencies]`** (resolve exact latest-compatible versions at this point; verify each with `cargo tree`/`deny`):

```toml
actix-web = { version = "4", default-features = false, features = ["macros"] }
actix-ws  = "0.3"
```

Add the internal path dep (in the internal block):

```toml
paigasus-helikon-runtime-actix = { path = "crates/paigasus-helikon-runtime-actix", version = "0.1.0", default-features = false }
```

- [ ] **Step 2: Write the crate `Cargo.toml`:**

```toml
[package]
name        = "paigasus-helikon-runtime-actix"
description = "Self-hosted actix-web HTTP/SSE/WebSocket server runtime for Paigasus Helikon agents"
version                = "0.1.0"
edition.workspace      = true
rust-version.workspace = true
authors.workspace      = true
license.workspace      = true
repository.workspace   = true
homepage.workspace     = true
keywords.workspace     = true
categories.workspace   = true

[dependencies]
paigasus-helikon-core          = { workspace = true }
paigasus-helikon-runtime-tokio = { workspace = true }
actix-web    = { workspace = true }
actix-ws     = { workspace = true }
tokio        = { workspace = true }
tokio-util   = { workspace = true }
futures-util = { workspace = true }
async-trait  = { workspace = true }
serde        = { workspace = true }
serde_json   = { workspace = true }
thiserror    = { workspace = true }
tracing      = { workspace = true }
uuid         = { workspace = true }
utoipa       = { workspace = true, optional = true }

[dev-dependencies]
tokio             = { workspace = true }
reqwest           = { workspace = true, features = ["json", "rustls"] }
tokio-tungstenite = { workspace = true }

[features]
default = ["openapi"]
openapi = ["dep:utoipa"]

[lints]
workspace = true
```

- [ ] **Step 3: Write a minimal `src/lib.rs`:**

```rust
//! Self-hosted actix-web server runtime for Paigasus Helikon agents.
//!
//! Mounts one or more [`Agent`](paigasus_helikon_core::Agent)s on an [`actix_web`] app and
//! serves them over REST (one-shot), Server-Sent Events, and WebSocket, with replayable runs.
//! Public-surface-compatible with `paigasus-helikon-runtime-axum`.
//!
//! See the crate `README.md` for a runnable example.
#![forbid(unsafe_code)]
```

- [ ] **Step 4: Build the empty crate and assert no axum leakage:**

Run:
```bash
cargo build -p paigasus-helikon-runtime-actix
cargo tree -p paigasus-helikon-runtime-actix -e no-dev | grep -qE '(^|[^-])axum v' && echo "LEAK" || echo "no axum"
```
Expected: builds clean; prints `no axum`.

- [ ] **Step 5: Commit:**

```bash
git add crates/paigasus-helikon-runtime-actix/Cargo.toml crates/paigasus-helikon-runtime-actix/src/lib.rs Cargo.toml Cargo.lock
git commit -m "feat(runtime-actix): SMA-343 scaffold actix runtime crate"
```

---

## Task 2: Port framework-agnostic internals verbatim (`dto`, `event_log`, `registry`)

These three modules are runtime-agnostic (tokio primitives + core types only). Copy each from the axum crate **unchanged** except: (a) crate-internal `use crate::…` paths stay the same; (b) `registry::spawn_sweeper` is changed in Task 5 to accept a tokio `Handle` — for now copy it verbatim (it uses `tokio::spawn`, which Task 5 replaces).

> **Sequencing note (discovered during execution):** `session.rs` imports `crate::error::ServerError`, so it moved to Task 3 (which creates `error.rs` first). Task 2 ships only the three modules that compile standalone.

**Files:**
- Create (copy): `crates/paigasus-helikon-runtime-actix/src/dto.rs` ← `…-axum/src/dto.rs`
- Create (copy): `crates/paigasus-helikon-runtime-actix/src/event_log.rs` ← axum
- Create (copy): `crates/paigasus-helikon-runtime-actix/src/registry.rs` ← axum

**Interfaces:**
- Produces (unchanged from axum): `dto::{AgentInfo, AsyncAccepted, RunRequest, RunResponse, RunStatus}`; `event_log::{EventLog, RunHandle, is_terminal}`; `registry::{RunRegistry, RunHandle re-export}`. (`session` moves to Task 3 — it imports `crate::error`.)

- [ ] **Step 1: Copy the three files verbatim** from `crates/paigasus-helikon-runtime-axum/src/{dto,event_log,registry}.rs` to the actix crate's `src/`. Do not edit logic. The `#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]` attrs in `dto.rs` carry over as-is.

- [ ] **Step 2: Declare the modules in `src/lib.rs`** (add below the crate doc):

```rust
mod event_log;
mod registry;

mod dto;
pub use dto::{AgentInfo, AsyncAccepted, RunRequest, RunResponse, RunStatus};
```

- [ ] **Step 3: Run the copied unit tests** (each file carries its own `#[cfg(test)] mod tests`):

Run: `cargo test -p paigasus-helikon-runtime-actix --lib dto:: event_log:: registry::`
Expected: PASS (same tests that pass in the axum crate).

- [ ] **Step 4: fmt + build/test + commit** (dead_code warnings on not-yet-wired `event_log`/`registry` items are expected until later tasks use them — do NOT gate on `clippy -D warnings` here; the full clippy gate runs at Task 17):

```bash
cargo fmt --all
cargo build -p paigasus-helikon-runtime-actix
cargo test -p paigasus-helikon-runtime-actix --lib
git add crates/paigasus-helikon-runtime-actix/src/dto.rs crates/paigasus-helikon-runtime-actix/src/event_log.rs crates/paigasus-helikon-runtime-actix/src/registry.rs crates/paigasus-helikon-runtime-actix/src/lib.rs
git commit -m "feat(runtime-actix): SMA-343 port framework-agnostic dto/event_log/registry"
```

---

## Task 3: Adapt `error.rs` (actix `ResponseError`) + port `session.rs`

`ServerError`, `AuthRejection`, `ErrorBody`, the status map, and the `AuthRejection` `Display`/`Error` impls are **identical** to axum. Only the response-conversion trait changes: axum's `IntoResponse` → actix's `ResponseError`, and `StatusCode` comes from `actix_web::http`. `session.rs` is a verbatim port (moved here from Task 2 because it imports `crate::error::ServerError`).

**Files:**
- Create: `crates/paigasus-helikon-runtime-actix/src/error.rs`
- Create (copy): `crates/paigasus-helikon-runtime-actix/src/session.rs` ← axum (verbatim)

**Interfaces:**
- Produces: `error::{ServerError, AuthRejection, ErrorBody}` (`ServerError: actix_web::ResponseError`; `AuthRejection.status: actix_web::http::StatusCode`); `session::{SessionProvider, InMemorySessionProvider, SessionLocks}` (verbatim from axum).

- [ ] **Step 1: Copy `error.rs` from axum**, then replace the imports + the `IntoResponse` impl. Keep the enum, `ErrorBody`, `AuthRejection`, the `Display`/`Error` impls, and the exact status mapping (including the auth-status clamp) unchanged. New top + new impl:

```rust
use actix_web::{http::StatusCode, HttpResponse, ResponseError};
use serde::Serialize;
use thiserror::Error;
// … ErrorBody, AuthRejection, ServerError enum, Display/Error impls: copied verbatim …

impl ResponseError for ServerError {
    fn status_code(&self) -> StatusCode {
        match self {
            ServerError::UnknownAgent(_) => StatusCode::NOT_FOUND,
            ServerError::BadRequest(_) => StatusCode::BAD_REQUEST,
            ServerError::Unauthorized(rej) => match rej.status {
                StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => rej.status,
                _ => StatusCode::UNAUTHORIZED,
            },
            ServerError::RunStart(_) => StatusCode::INTERNAL_SERVER_ERROR,
            ServerError::Unavailable(_) => StatusCode::SERVICE_UNAVAILABLE,
            ServerError::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    fn error_response(&self) -> HttpResponse {
        HttpResponse::build(self.status_code()).json(ErrorBody { error: self.to_string() })
    }
}
```

- [ ] **Step 2: Port the `error.rs` unit tests** (`status_mapping`, `unauthorized_status_is_clamped`) to assert on `ResponseError::status_code()` instead of `.into_response().status()`:

```rust
#[test]
fn status_mapping() {
    use actix_web::http::StatusCode;
    assert_eq!(ServerError::UnknownAgent("x".into()).status_code(), StatusCode::NOT_FOUND);
    assert_eq!(ServerError::BadRequest("x".into()).status_code(), StatusCode::BAD_REQUEST);
    assert_eq!(ServerError::RunStart("x".into()).status_code(), StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(ServerError::Unavailable("x".into()).status_code(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(ServerError::Internal("x".into()).status_code(), StatusCode::INTERNAL_SERVER_ERROR);
}
// + unauthorized_status_is_clamped mirrored onto status_code()
```

- [ ] **Step 3: Copy `session.rs` verbatim** from `…-axum/src/session.rs` (it imports `crate::error::ServerError`, now available). Then **declare + export in `lib.rs`:**

```rust
mod error;
pub use error::{AuthRejection, ServerError};
mod session;
pub use session::{InMemorySessionProvider, SessionProvider};
```

- [ ] **Step 4: Run + verify:** `cargo test -p paigasus-helikon-runtime-actix --lib error:: session::` → PASS (session's copied tests come along; `dead_code` on not-yet-wired items is expected).

- [ ] **Step 5: fmt + build/test + commit** (do not gate on `clippy -D warnings` yet): `feat(runtime-actix): SMA-343 adapt ServerError to actix ResponseError + port session`.

---

## Task 4: Adapt `context.rs` + `auth.rs` (`&HttpRequest`)

Same traits/semantics as axum; request param `axum::http::request::Parts` → `&actix_web::HttpRequest`. `DefaultContextProvider` body is unchanged (it ignores the request).

**Files:**
- Create: `crates/paigasus-helikon-runtime-actix/src/context.rs`
- Create: `crates/paigasus-helikon-runtime-actix/src/auth.rs`

**Interfaces:**
- Produces: `context::{ContextProvider<Ctx>, DefaultContextProvider}` with `build(&self, req: &HttpRequest, session: Arc<dyn Session>, cancel: CancellationToken) -> Result<RunContext<Ctx>, ServerError>`; `auth::AuthLayer` with `authenticate(&self, req: &HttpRequest) -> Result<(), AuthRejection>`.

- [ ] **Step 1: Write `auth.rs`** — copy axum's doc comments (they describe the auth→context bridge; update the `parts.extensions` wording to `req.extensions_mut()` and add the RefCell drop-before-`await` note). Trait:

```rust
use actix_web::HttpRequest;
use async_trait::async_trait;
use crate::error::AuthRejection;

/// Middleware hook called before every route when configured. Insert identity via
/// `req.extensions_mut()`; **drop the returned `RefMut` before any `.await`** (actix
/// request extensions are `RefCell`-backed — holding a borrow across a yield panics).
#[async_trait]
pub trait AuthLayer: Send + Sync {
    /// Return `Ok(())` to admit the request (optionally inserting identity into
    /// `req.extensions_mut()`), or `Err(AuthRejection)` to reject it.
    async fn authenticate(&self, req: &HttpRequest) -> Result<(), AuthRejection>;
}
```

Port the unit tests to build a test `HttpRequest` via `actix_web::test::TestRequest::default().insert_header(("authorization", v)).to_http_request()` and assert identity round-trips through `req.extensions()`.

- [ ] **Step 2: Write `context.rs`** — copy axum's doc comments; swap the signature:

```rust
use std::sync::Arc;
use actix_web::HttpRequest;
use async_trait::async_trait;
use paigasus_helikon_core::{RunContext, Session};
use tokio_util::sync::CancellationToken;
use crate::error::ServerError;

#[async_trait]
pub trait ContextProvider<Ctx>: Send + Sync where Ctx: Send + Sync + 'static {
    async fn build(&self, req: &HttpRequest, session: Arc<dyn Session>, cancel: CancellationToken)
        -> Result<RunContext<Ctx>, ServerError>;
}

/// Zero-config provider for `Ctx: Default`.
pub struct DefaultContextProvider;

#[async_trait]
impl<Ctx> ContextProvider<Ctx> for DefaultContextProvider where Ctx: Default + Send + Sync + 'static {
    async fn build(&self, _req: &HttpRequest, session: Arc<dyn Session>, cancel: CancellationToken)
        -> Result<RunContext<Ctx>, ServerError> {
        Ok(RunContext::ephemeral(Ctx::default()).with_session(session).with_cancel(cancel))
    }
}
```

Port the `default_provider_builds_context_for_unit_ctx` test using `TestRequest::default().to_http_request()`.

- [ ] **Step 3: Declare + export in `lib.rs`:**

```rust
mod context;
pub use context::{ContextProvider, DefaultContextProvider};
mod auth;
pub use auth::AuthLayer;
```

- [ ] **Step 4: Run:** `cargo test -p paigasus-helikon-runtime-actix --lib auth:: context::` → PASS.

- [ ] **Step 5: fmt/clippy/commit** `feat(runtime-actix): SMA-343 adapt AuthLayer/ContextProvider to HttpRequest`.

---

## Task 5: `server.rs` + `runtime.rs` — AppState, builder, shared runtime, `configure()`/`serve()`

The builder is identical to axum. Writer tasks + the registry sweeper run on a **process-wide** multi-thread tokio runtime (`runtime.rs`), NOT on actix's per-worker `actix-rt` and NOT on a per-server `Runtime`. `configure()` registers routes under an inner `web::scope("")` (so Task 10's auth can `.wrap()` it) and spawns the (idempotent) sweeper; `serve()`/`serve_with_listener()` drive `HttpServer`.

> **CRITICAL runtime note (why not a per-server `Runtime`):** `tokio::runtime::Runtime::new()` **panics** ("cannot create a runtime from within a runtime") whenever it runs inside an existing runtime — which is ALWAYS the case here: `build()`/`serve()` are called from `#[actix_web::main]`, `#[tokio::test]`, or the conformance harness's `System` thread. So the shared runtime is created **lazily on a dedicated OS thread** and exposed as a `'static` `Handle` (`runtime::shared_handle()`). This avoids the nested-runtime panic, avoids the drop-in-async panic (a `'static` runtime is never dropped), and is shared by all servers in the process. `AppStateInner` holds **no** runtime.

**Files:**
- Create: `crates/paigasus-helikon-runtime-actix/src/runtime.rs`
- Create: `crates/paigasus-helikon-runtime-actix/src/server.rs`
- Modify: `crates/paigasus-helikon-runtime-actix/src/registry.rs` (idempotent `spawn_sweeper(&Handle)`)
- Modify: `crates/paigasus-helikon-runtime-actix/src/lib.rs` (`mod runtime; mod server; pub use server::{AgentServer, AgentServerBuilder};`)

**Interfaces:**
- Consumes: everything from Tasks 2–4.
- Produces: `server::{AgentServer<Ctx>, AgentServerBuilder<Ctx>}`; `pub(crate) struct AppState<Ctx>` (Deref to `AppStateInner<Ctx>`) with fields `registry, runner, agents, sessions, context, auth, run_config, locks` (SAME as axum — **no** `rt` field); `runtime::shared_handle() -> tokio::runtime::Handle`. `AgentServer::configure(&self) -> impl Fn(&mut ServiceConfig) + Send + Clone + 'static`; `serve(self, addr)`, `serve_with_listener(self, std::net::TcpListener)`.

- [ ] **Step 1: Write `runtime.rs`** — the process-wide executor:

```rust
//! Process-wide executor for detached run work. actix runs each worker on its
//! own single-threaded `actix-rt` runtime (recyclable if a worker dies), so run
//! writer tasks and the registry sweeper run on ONE process-wide multi-thread
//! tokio runtime instead. It is created lazily on a dedicated OS thread — a
//! fresh thread avoids the "cannot create a runtime from within a runtime" panic
//! that firing `Runtime::new()` inside `#[actix_web::main]`/`#[tokio::test]`
//! would cause — and held `'static`, so it is never dropped in an async context.
use std::sync::OnceLock;
use tokio::runtime::{Builder, Handle};

static SHARED: OnceLock<Handle> = OnceLock::new();

/// Handle to the process-wide runtime that executes run writer tasks and the
/// registry sweeper. Lazily initialised on first use.
pub(crate) fn shared_handle() -> Handle {
    SHARED
        .get_or_init(|| {
            let (tx, rx) = std::sync::mpsc::channel();
            std::thread::Builder::new()
                .name("helikon-actix-rt".to_owned())
                .spawn(move || {
                    let rt = Builder::new_multi_thread().enable_all().build()
                        .expect("build shared runtime");
                    tx.send(rt.handle().clone()).expect("send runtime handle");
                    rt.block_on(std::future::pending::<()>()); // keep alive for process lifetime
                })
                .expect("spawn shared runtime thread");
            rx.recv().expect("receive runtime handle")
        })
        .clone()
}
```

- [ ] **Step 2: Make `registry.rs`'s sweeper idempotent + `Handle`-driven.** `configure()` runs once PER actix worker, so `spawn_sweeper` must spawn exactly ONE sweeper no matter how many times it's called. Add a `sweeper: std::sync::Once` field to `RunRegistry` (init `Once::new()` in `new()`), and:

```rust
// registry.rs
pub fn spawn_sweeper(self: &Arc<Self>, handle: &tokio::runtime::Handle) {
    let registry = Arc::clone(self);
    let handle = handle.clone();
    self.sweeper.call_once(move || {
        handle.spawn(async move { registry.sweep_loop().await });
    });
}
```
(Rename the existing loop body to `sweep_loop` if needed; keep TTL + `max_runs` eviction logic unchanged. The existing registry unit tests are unaffected.)

- [ ] **Step 3: Write `AppStateInner`/`AppState`** — copy axum's `AppStateInner`/`AppState` (Deref + Clone) VERBATIM (same fields, **no** `rt`). All fields' types are `Send + Sync` (the `Arc<dyn ContextProvider>`/`Arc<dyn AuthLayer>` trait objects are `Send + Sync` despite their `?Send` futures), so `AppState<Ctx>: Send + Sync + Clone + 'static` — required for `web::Data` and the `configure()` closure bound.

- [ ] **Step 4: Write `AgentServerBuilder`** — copy axum's builder **verbatim** (all setter methods, `dup_error` handling, defaults, `with_default_context`). `build()` is the SAME as axum except it does **not** create any runtime and constructs `AppStateInner` with the same field set (no `rt`). Do NOT call `Runtime::new()` anywhere.

- [ ] **Step 5: Write `AgentServer` with `configure()` + serve methods:**

```rust
impl<Ctx: Send + Sync + 'static> AgentServer<Ctx> {
    pub fn builder() -> AgentServerBuilder<Ctx> { AgentServerBuilder::new() }

    /// Returns a closure that mounts the agent routes on an actix `App` at root.
    ///
    /// **Incremental build note:** `configure()` may only route to handlers that
    /// EXIST. Task 5 ships an EMPTY scope (state + sweeper, no routes) so it
    /// compiles with no `handlers` module present. Each later handler task APPENDS
    /// its route here and adds its module to `handlers/mod.rs`: Task 6 adds
    /// `GET /agents`, Task 7 adds `POST /agents/{name}/runs`, Task 9 adds
    /// `GET /agents/{name}/runs/{id}/events`, Task 11 adds the feature-gated
    /// `/openapi.json`. Do NOT reference a handler before its task.
    pub fn configure(&self) -> impl Fn(&mut ServiceConfig) + Send + Clone + 'static {
        let state = self.state.clone();
        move |cfg: &mut ServiceConfig| {
            // Idempotent: spawns exactly one sweeper across all workers, on the
            // process-wide runtime. Runs on the embed path too (host calls
            // configure()); a built-but-never-served server never calls this, so
            // it leaks no sweeper.
            state.registry.spawn_sweeper(&crate::runtime::shared_handle());
            let scope = web::scope("").app_data(Data::new(state.clone()));
            // Task 6/7/9/11 append .route(...) here; Task 10 wraps `scope` with the
            // auth Transform when state.auth.is_some().
            cfg.service(scope);
        }
    }

    pub async fn serve(self, addr: impl std::net::ToSocketAddrs) -> Result<(), ServerError> {
        let listener = std::net::TcpListener::bind(addr)
            .map_err(|e| ServerError::Internal(e.to_string()))?;
        self.serve_with_listener(listener).await
    }

    pub async fn serve_with_listener(self, listener: std::net::TcpListener) -> Result<(), ServerError> {
        listener.set_nonblocking(true).map_err(|e| ServerError::Internal(e.to_string()))?;
        let cfg = self.configure();
        HttpServer::new(move || App::new().configure(cfg.clone()))
            .listen(listener).map_err(|e| ServerError::Internal(e.to_string()))?
            .run().await.map_err(|e| ServerError::Internal(e.to_string()))
    }
}
```
(`use actix_web::{web::{self, Data, ServiceConfig}, App, HttpServer};`. Note: `serve()` only needs to COMPILE in this task — it isn't run until Task 6's harness, which drives it inside an `actix_rt::System`.)

- [ ] **Step 6: Add builder unit tests** in `server.rs` (`#[cfg(test)]`, no HTTP): duplicate agent name → `Err(ServerError::BadRequest)`; no context provider → `Err(ServerError::Internal)`; `max_sessions(0)` with the default session store → `Err(ServerError::BadRequest)`; happy path `AgentServer::<()>::builder().with_default_context().agent(..).build()` → `Ok`. Use plain `#[test]` (build() spawns nothing and creates no runtime, so no async runtime is needed; do NOT wrap these in `#[tokio::test]`). No `handlers` module is created in this task.

- [ ] **Step 7: Build + test + commit:** `cargo build -p paigasus-helikon-runtime-actix` and `cargo test -p paigasus-helikon-runtime-actix --lib` → clean/PASS. (`dead_code` on not-yet-wired items expected; do not gate on `clippy -D warnings`.) Commit `feat(runtime-actix): SMA-343 add AgentServer builder, process-wide runtime, configure()/serve()`.

---

## Task 6: `handlers/agents.rs` — `GET /agents`

**Files:**
- Create: `crates/paigasus-helikon-runtime-actix/src/handlers/mod.rs` + `src/handlers/agents.rs`
- Modify: `crates/paigasus-helikon-runtime-actix/src/lib.rs` (add `mod handlers;`) + `src/server.rs` (append the `/agents` route to `configure()`)
- Create: `crates/paigasus-helikon-runtime-actix/tests/support/mod.rs`
- Create: `crates/paigasus-helikon-runtime-actix/tests/server.rs`

**Interfaces:**
- Consumes: `AppState` (from Task 5), `dto::AgentInfo`.
- Produces: `handlers::agents::list::<Ctx>(state: Data<AppState<Ctx>>) -> Json<Vec<AgentInfo>>`; test helper `spawn_echo_server() -> String` (base URL) using a dedicated actix `System` thread (see Step 1). This is the first task to create the `handlers` module and to add a route to `configure()`.

- [ ] **Step 1: Write `tests/support/mod.rs`** — port axum's `ScriptedAgent`, `echo_script`, `FailingRunner`, `PartialThenEndRunner`, `OrderingAgent`, `SignallingHangingAgent` **verbatim** (they use core types only). Replace `spawn_echo_server` with the actix `System`-thread pattern returning a base URL:

```rust
pub fn spawn_actix_server<Ctx>(server: AgentServer<Ctx>) -> String
where Ctx: Send + Sync + 'static {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    std::thread::spawn(move || {
        actix_web::rt::System::new().block_on(async move {
            server.serve_with_listener(listener).await.expect("serve");
        });
    });
    // brief readiness wait or retry-connect loop
    std::thread::sleep(std::time::Duration::from_millis(150));
    format!("http://{addr}")
}
pub fn spawn_echo_server() -> String {
    let server = AgentServer::<()>::builder().with_default_context()
        .agent(Arc::new(ScriptedAgent { name: "echo".into(), events: echo_script() }))
        .build().expect("builds");
    spawn_actix_server(server)
}
```

- [ ] **Step 2: Write the `GET /agents` failing test** in `tests/server.rs`:

```rust
mod support;
#[tokio::test]
async fn lists_mounted_agents() {
    let base = support::spawn_echo_server();
    let v: serde_json::Value = reqwest::get(format!("{base}/agents")).await.unwrap().json().await.unwrap();
    assert_eq!(v.as_array().unwrap().len(), 1);
    assert_eq!(v[0]["name"], "echo");
}
```

- [ ] **Step 3: Run → FAIL** (handler `unimplemented!`/missing). `cargo test -p paigasus-helikon-runtime-actix --test server`.

- [ ] **Step 4: Implement `handlers/agents.rs` and wire the route:**

```rust
// src/handlers/agents.rs
use actix_web::web::{Data, Json};
use crate::{dto::AgentInfo, server::AppState};

/// `GET /agents` — list all mounted agents (HashMap order, unspecified).
pub(crate) async fn list<Ctx: Send + Sync + 'static>(state: Data<AppState<Ctx>>) -> Json<Vec<AgentInfo>> {
    Json(state.agents.values().map(|a| AgentInfo { name: a.name().to_owned(), description: a.description().to_owned() }).collect())
}
```

Then wire it up: create `src/handlers/mod.rs` with `pub(crate) mod agents;`; add `mod handlers;` to `src/lib.rs`; and in `src/server.rs` append the route to `configure()`'s scope so it reads
`web::scope("").app_data(Data::new(state.clone())).route("/agents", web::get().to(handlers::agents::list::<Ctx>))`
(add `use crate::handlers;` if the path isn't already in scope).

- [ ] **Step 5: Run → PASS.** (Builder-error unit tests live in Task 5's `server.rs`; this task's `tests/server.rs` covers the `/agents` HTTP behaviour + the test harness.)

- [ ] **Step 6: fmt/clippy/commit** `feat(runtime-actix): SMA-343 GET /agents + test harness`.

---

## Task 7: `handlers/runs.rs` one-shot + the multi-worker de-risking spike

This is the **go/no-go checkpoint for decision 6.** Implement the shared writer task + one-shot response, then prove it on a multi-worker server incl. disconnect-cancel and same-session serialization. If these fail, STOP and revisit the shared-runtime approach before continuing.

**Files:**
- Create: `crates/paigasus-helikon-runtime-actix/src/handlers/runs.rs`
- Create: `crates/paigasus-helikon-runtime-actix/tests/runs.rs`
- Create: `crates/paigasus-helikon-runtime-actix/tests/concurrency.rs`

**Interfaces:**
- Consumes: `AppState`, `RunRegistry`, `EventLog`, `dto::{RunRequest, RunResponse}`, `SessionLocks`.
- Produces: `handlers::runs::create_run::<Ctx>(state, path, query, req, body: web::Payload) -> Result<HttpResponse, ServerError>`; internal `spawn_writer` (on `state.rt.handle()`), `read_run_request(&HttpRequest, web::Payload) -> Result<RunRequest, ServerError>` with the 2 MiB cap.

- [ ] **Step 1: Write the failing one-shot test** in `tests/runs.rs` (mirror axum's `oneshot_returns_aggregated_result`), asserting `x-run-id` header + `{"status":"completed","output":"echo"}`.

- [ ] **Step 2: Run → FAIL.**

- [ ] **Step 3: Implement `runs.rs`.** Port axum's structure — `RunQuery` (+`validate`), `TerminalGuard`, `spawn_writer`, `oneshot_response` — changing: (a) manual body read via `web::Payload` with `MAX_BODY_BYTES = 2*1024*1024` and the tolerant content-type check (return `ServerError::BadRequest` on oversize/non-JSON); (b) `spawn_writer` spawns the writer future via `crate::runtime::shared_handle().spawn(...)` (the process-wide runtime from Task 5) — NOT `tokio::spawn` (which would pin the writer to the actix worker) and NOT `state.rt` (removed). The writer future is `Send` (holds only `Arc<dyn Runner>`, `Arc<dyn Agent>`, `RunContext<Ctx>`, `AgentInput`, `Arc<RunHandle>`, `OwnedMutexGuard`, all `Send`), so `handle.spawn` accepts it. Build the `RunContext` via `state.context.build(&req, …).await?` in the handler FIRST (its future is `!Send`, fine on the worker), then move the resulting `Send` `RunContext` into the spawned writer; (c) session id from `req.headers().get("x-session-id")`; (d) one-shot holds `handle.cancel.clone().drop_guard()` while awaiting `handle.log.subscribe(0).collect()`, then builds `HttpResponse::Ok().insert_header(("x-run-id", run_id.to_string())).json(RunResponse::from_events(run_id, events))`; start-error → `ServerError::RunStart`.

```rust
const MAX_BODY_BYTES: usize = 2 * 1024 * 1024;

async fn read_run_request(req: &HttpRequest, mut body: web::Payload) -> Result<RunRequest, ServerError> {
    if let Some(ct) = req.headers().get(actix_web::http::header::CONTENT_TYPE).and_then(|v| v.to_str().ok()) {
        let mime = ct.split(';').next().unwrap_or("").trim();
        let is_json = mime == "application/json" || (mime.starts_with("application/") && mime.ends_with("+json"));
        if !is_json { return Err(ServerError::BadRequest(format!("unsupported content type `{mime}`; expected application/json"))); }
    }
    let mut bytes = actix_web::web::BytesMut::new();
    while let Some(chunk) = futures_util::StreamExt::next(&mut body).await {
        let chunk = chunk.map_err(|e| ServerError::BadRequest(format!("failed to read request body: {e}")))?;
        if bytes.len() + chunk.len() > MAX_BODY_BYTES { return Err(ServerError::BadRequest("request body too large".into())); }
        bytes.extend_from_slice(&chunk);
    }
    serde_json::from_slice::<RunRequest>(&bytes).map_err(|e| ServerError::BadRequest(format!("invalid run request body: {e}")))
}
```

- [ ] **Step 4: Run one-shot test → PASS.** Add tests mirroring axum: `unknown_agent_404`, `invalid_mode_selector_is_400`, `invalid_stream_selector_is_400`, `conflicting_async_and_sse_is_400`.

- [ ] **Step 5: Write the SPIKE tests** in `tests/concurrency.rs`:
  - **Multi-worker one-shot:** build the server with `.workers(2)` (via a variant of `spawn_actix_server` that sets `HttpServer::workers(2)`), fire 4 concurrent one-shot echo runs, assert all return `completed`.
  - **Same-session serialization:** port axum's `concurrent_same_session_serialize` using `OrderingAgent` — two concurrent requests with the same `X-Session-Id` on a 2-worker server must produce ticks `[START,END,START,END]`.
  - **Disconnect cancels one-shot:** port axum's hanging-agent disconnect test — drop the client mid-run, assert the run's cancel token fired (the `SignallingHangingAgent` + injected session provider pattern).

- [ ] **Step 6: Run the spike → PASS.** If any fail, STOP: the shared-runtime/cross-worker model needs rework before proceeding (see Task 5 Step 3 note). Otherwise commit `feat(runtime-actix): SMA-343 one-shot run handler + multi-worker spike`.

---

## Task 8: `handlers/runs.rs` — SSE (`?stream=sse`) + async (`?mode=async`)

**Files:**
- Modify: `crates/paigasus-helikon-runtime-actix/src/handlers/runs.rs`
- Modify: `crates/paigasus-helikon-runtime-actix/tests/runs.rs`

**Interfaces:**
- Produces: `sse_response(run_id, &RunHandle) -> HttpResponse` (hand-rolled `text/event-stream` `.streaming(...)`, byte-matching axum's `to_sse_event`), `async_response(run_id) -> HttpResponse` (202 + `AsyncAccepted`). `create_run` dispatches on `query.is_async()` / `query.is_sse()`.

- [ ] **Step 1: Write failing SSE + async tests** mirroring axum's `sse_stream_matches_local_events`, `async_mode_returns_202`, `sse_emits_synthetic_run_failed_on_start_error`, `sse_emits_synthetic_run_failed_after_terminalless_stream`. Reuse a `parse_sse` helper in `support` ported verbatim.

- [ ] **Step 2: Run → FAIL.**

- [ ] **Step 3: Implement hand-rolled SSE.** Frame each event exactly as axum: `event: <type-tag>\ndata: <event-json>\n\n` (tag = the event's serde `type` discriminant; no tag ⇒ omit the `event:` line). Build a `futures_util::stream` of `Result<web::Bytes, ServerError>` from `handle.log.subscribe(0)`, appending the synthetic terminal frame via `handle.synthetic_terminal_frame(saw_terminal)` on stream end, and hold the cancel `DropGuard` in the stream state (client disconnect cancels). Respond:

```rust
fn sse_response(run_id: Uuid, handle: &Arc<RunHandle>) -> HttpResponse {
    let byte_stream = /* unfold over handle.log.subscribe(0) + DropGuard + synthetic terminal, yielding web::Bytes of the SSE frame */;
    HttpResponse::Ok()
        .insert_header(("x-run-id", run_id.to_string()))
        .insert_header((actix_web::http::header::CONTENT_TYPE, "text/event-stream"))
        .insert_header((actix_web::http::header::CACHE_CONTROL, "no-cache"))
        .streaming(byte_stream)
}

fn sse_frame(ev: &AgentEvent) -> web::Bytes {
    let value = serde_json::to_value(ev).expect("AgentEvent serializes");
    let json = serde_json::to_string(&value).expect("serializes");
    let mut s = String::new();
    if let Some(tag) = value.get("type").and_then(|t| t.as_str()) { s.push_str("event: "); s.push_str(tag); s.push('\n'); }
    s.push_str("data: "); s.push_str(&json); s.push_str("\n\n");
    web::Bytes::from(s)
}
```
> Match axum's exact byte layout: verify against `axum::response::sse::Event` output during the conformance task (Task 13) and adjust the `event:`/`data:` spacing/newlines to be byte-identical.

Implement `async_response`: `HttpResponse::Accepted().json(AsyncAccepted { run_id: run_id.to_string() })`.

- [ ] **Step 4: Run → PASS.** fmt/clippy/commit `feat(runtime-actix): SMA-343 SSE (hand-rolled) + async run transports`.

---

## Task 9: `handlers/events.rs` — WebSocket via `actix-ws`

**Files:**
- Create: `crates/paigasus-helikon-runtime-actix/src/handlers/events.rs`
- Create: `crates/paigasus-helikon-runtime-actix/tests/ws.rs`

**Interfaces:**
- Consumes: `AppState`, `RunRegistry`, `EventLog::subscribe`.
- Produces: `handlers::events::events::<Ctx>(state, path, req: HttpRequest, body: web::Payload) -> Result<HttpResponse, ServerError>`.

- [ ] **Step 1: Write failing WS tests** (`tests/ws.rs`) mirroring axum: 400-on-bad-UUID (HTTP 400, no upgrade); 404-before-upgrade (unknown/agent-mismatch → HTTP 404, no upgrade); replay+tail delivers the echo events then closes; terminal-less run gets a synthetic `run_failed` frame; read-only (client stays subscribed, run not cancelled). Use `tokio_tungstenite::connect_async` and first `create_async_run` (ported helper) to get a run id.

- [ ] **Step 2: Run → FAIL.**

- [ ] **Step 3: Implement `events.rs`:**

```rust
pub(crate) async fn events<Ctx: Send + Sync + 'static>(
    state: Data<AppState<Ctx>>, path: web::Path<(String, String)>, req: HttpRequest, body: web::Payload,
) -> Result<HttpResponse, ServerError> {
    let (name, id) = path.into_inner();
    let run_id = Uuid::parse_str(&id).map_err(|_| ServerError::BadRequest(format!("invalid run id: {id}")))?;
    let handle = state.registry.get(run_id).filter(|h| h.agent_name == name)
        .ok_or_else(|| ServerError::UnknownAgent(format!("{name}/{id}")))?;
    let (response, mut session, mut msg_stream) = actix_ws::handle(&req, body)
        .map_err(|e| ServerError::Internal(e.to_string()))?;
    actix_web::rt::spawn(async move {
        let mut sub = handle.log.subscribe(0);
        let mut saw_terminal = false;
        loop {
            tokio::select! {
                ev = futures_util::StreamExt::next(&mut sub) => match ev {
                    Some(ev) => { if is_terminal(&ev) { saw_terminal = true; }
                        let Ok(text) = serde_json::to_string(&ev) else { break };
                        if session.text(text).await.is_err() { break } }
                    None => { if let Some(frame) = handle.synthetic_terminal_frame(saw_terminal) {
                            if let Ok(text) = serde_json::to_string(&frame) { let _ = session.text(text).await; } }
                        let _ = session.close(None).await; break }
                },
                msg = futures_util::StreamExt::next(&mut msg_stream) => match msg {
                    None | Some(Err(_)) | Some(Ok(actix_ws::Message::Close(_))) => break,
                    Some(Ok(_)) => {}
                }
            }
        }
    });
    Ok(response)
}
```
(`use actix_web::{web::{self, Data}, HttpRequest, HttpResponse}; use crate::event_log::is_terminal; use uuid::Uuid;`)

- [ ] **Step 4: Run → PASS.** Add the WS **cross-worker** spike assertion to `tests/concurrency.rs`: on a 2-worker server, create an async run then WS-subscribe (the subscription may land on a different worker than the writer) and assert the full event sequence arrives.

- [ ] **Step 5: fmt/clippy/commit** `feat(runtime-actix): SMA-343 WebSocket events via actix-ws`.

---

## Task 10: Auth middleware + wire into `configure()`

**Files:**
- Create: `crates/paigasus-helikon-runtime-actix/src/middleware.rs`
- Modify: `crates/paigasus-helikon-runtime-actix/src/server.rs` (wrap the inner scope when auth set)
- Create: `crates/paigasus-helikon-runtime-actix/tests/auth.rs`

**Interfaces:**
- Produces: `middleware::AuthMiddleware` (`Transform`) holding `Arc<dyn AuthLayer>`; on each `ServiceRequest`, calls `authenticate(req.request())` and short-circuits with `ServerError::Unauthorized(rej).error_response()` on `Err`, else forwards. Identity inserted by the auth impl into the shared request extensions survives to the handler.

- [ ] **Step 1: Write failing auth tests** (`tests/auth.rs`): a `MockAuthLayer` (reject when no `authorization` header → 401; else insert identity). Assert: no header → every route 401 (incl. `/agents`, `/openapi.json`); with header → 200 and identity reaches a custom `ContextProvider` (assert via an agent that echoes something derived from identity, or via a provider that errors unless identity present).

- [ ] **Step 2: Run → FAIL.**

- [ ] **Step 3: Implement `middleware.rs`** — a standard actix `Transform`/`Service` pair. In `call`, run `self.auth.authenticate(req.request()).await`; on `Err(rej)`, return `req.into_response(ServerError::Unauthorized(rej).error_response())`; on `Ok`, `self.service.call(req).await`. Use `actix_web::body::EitherBody` for the two response-body types.

- [ ] **Step 4: Wire into `server.rs` `configure()`** — when `state.auth.is_some()`, `scope = scope.wrap(AuthMiddleware::new(auth.clone()))` before `cfg.service(scope)`.

- [ ] **Step 5: Run → PASS.** fmt/clippy/commit `feat(runtime-actix): SMA-343 optional auth middleware gating all routes`.

---

## Task 11: `handlers/openapi.rs` — `GET /openapi.json` (feature `openapi`)

**Files:**
- Create: `crates/paigasus-helikon-runtime-actix/src/handlers/openapi.rs`
- Create: `crates/paigasus-helikon-runtime-actix/tests/openapi.rs`

**Interfaces:**
- Produces: `handlers::openapi::openapi_json::<Ctx>(state) -> Json<utoipa::openapi::OpenApi>` behind `#[cfg(feature = "openapi")]`.

- [ ] **Step 1: Write failing test** (`tests/openapi.rs`, mirror axum's `openapi.rs` test): `GET /openapi.json` returns 200; JSON contains `paths["/agents"]`, `paths["/agents/{name}/runs"]`, `paths["/agents/{name}/runs/{id}/events"]`; `info.description` contains the mounted agent name.

- [ ] **Step 2: Run → FAIL.**

- [ ] **Step 3: Copy axum's `handlers/openapi.rs` near-verbatim** — the `#[utoipa::path]` doc stubs, `ApiDoc` derive, and the agent-list augmentation are identical. Change only the handler return to actix `web::Json` and the `State` extractor to `Data<AppState<Ctx>>`. Keep `#![cfg(feature = "openapi")]` at module top and the sorted agent lines.

- [ ] **Step 4: Add `handlers/mod.rs` gate** for the module: `#[cfg(feature = "openapi")] pub(crate) mod openapi;`

- [ ] **Step 5: Run → PASS.** Also `cargo build -p paigasus-helikon-runtime-actix --no-default-features` → clean (openapi module + route fully gated out). fmt/clippy/commit `feat(runtime-actix): SMA-343 OpenAPI /openapi.json handler`.

---

## Task 12: Facade wiring

**Files:**
- Modify: `crates/paigasus-helikon/Cargo.toml`
- Modify: `crates/paigasus-helikon/src/lib.rs`

**Interfaces:**
- Produces: facade feature `runtime-actix` + `pub use paigasus_helikon_runtime_actix as runtime_actix`.

- [ ] **Step 1: Add optional dep + feature** to `crates/paigasus-helikon/Cargo.toml`:

```toml
paigasus-helikon-runtime-actix = { workspace = true, optional = true, features = ["openapi"] }
# in [features]:
runtime-actix = ["dep:paigasus-helikon-runtime-actix"]
```

- [ ] **Step 2: Add the re-export** to `crates/paigasus-helikon/src/lib.rs` (with a `///` doc comment, next to the other runtime re-exports):

```rust
/// Self-hosted actix-web runtime. Enabled via the `runtime-actix` feature.
#[cfg(feature = "runtime-actix")]
pub use paigasus_helikon_runtime_actix as runtime_actix;
```

- [ ] **Step 3: Facade isolation build + no-axum-leak assertion:**

```bash
cargo build -p paigasus-helikon --features runtime-actix
cargo tree -p paigasus-helikon --features runtime-actix -e no-dev | grep -qE '(^|[^-])axum v' && echo LEAK || echo ok
```
Expected: builds clean; prints `ok`.

- [ ] **Step 4: Commit** `feat(facade): SMA-343 wire runtime-actix feature + re-export`.

---

## Task 13: Conformance suite (`tests/runtime-http-conformance`)

**Files:**
- Create: `tests/runtime-http-conformance/Cargo.toml`
- Create: `tests/runtime-http-conformance/src/lib.rs`
- Create: `tests/runtime-http-conformance/tests/parity.rs`
- Create: `tests/runtime-http-conformance/fixtures/*.json`
- Modify: `Cargo.toml` (root `members`), `release-plz.toml`

**Interfaces:**
- Produces: `pub async fn check(base_url: &str)` (documented) + `pub fn scripted_agents() -> Vec<Arc<dyn Agent<()>>>`.

- [ ] **Step 1: Add the member + release exclusion.** Root `Cargo.toml`: `members = ["crates/*", "tests/runtime-http-conformance"]`. `release-plz.toml`: append a `[[package]] name = "paigasus-helikon-runtime-http-conformance" \n publish = false \n release = false` block.

- [ ] **Step 2: Write `Cargo.toml`:**

```toml
[package]
name        = "paigasus-helikon-runtime-http-conformance"
description = "Internal: shared HTTP wire-format conformance suite for Helikon runtimes."
version     = "0.0.0"
publish     = false
edition.workspace = true
# … other .workspace = true inherits …

[dependencies]
paigasus-helikon-core = { workspace = true }
async-trait           = { workspace = true }
futures-util          = { workspace = true }
serde_json            = { workspace = true }

[dev-dependencies]
paigasus-helikon-runtime-axum  = { workspace = true }
paigasus-helikon-runtime-actix = { workspace = true }
paigasus-helikon-runtime-tokio = { workspace = true }
tokio             = { workspace = true, features = ["macros", "rt-multi-thread"] }
reqwest           = { workspace = true, features = ["json", "rustls"] }
tokio-tungstenite = { workspace = true }
actix-web         = { workspace = true }

[lints]
workspace = true
```

- [ ] **Step 3: Write `src/lib.rs`** — a documented `scripted_agents()` returning the shared agent set, and a documented `check(base_url)` that exercises `GET /agents` (assert as decoded set), one-shot (assert byte-equal to golden), SSE (assert decoded events + `Content-Type: text/event-stream`), async 202, and WS (decoded events). Load goldens from `fixtures/` via `include_str!`.

- [ ] **Step 4: Write `tests/parity.rs`** — boot axum via `tokio::spawn(axum_server.serve_with_listener(tokio_listener))`; boot actix via a dedicated `std::thread` running `actix_web::rt::System::new().block_on(actix_server.serve_with_listener(std_listener))` with a readiness wait; run `check()` against both; then assert **axum-bytes == actix-bytes** for the one-shot body and each SSE `data:` payload (fetch both, compare). Mark `#[tokio::test]`.

```rust
// sketch
#[tokio::test]
async fn axum_and_actix_are_wire_compatible() {
    let axum_base = boot_axum().await;   // tokio::spawn
    let actix_base = boot_actix();       // dedicated System thread
    conformance::check(&axum_base).await;
    conformance::check(&actix_base).await;
    let a = reqwest::Client::new().post(format!("{axum_base}/agents/echo/runs")).header("content-type","application/json").body(r#"{"input":"hi"}"#).send().await.unwrap();
    let b = reqwest::Client::new().post(format!("{actix_base}/agents/echo/runs")).header("content-type","application/json").body(r#"{"input":"hi"}"#).send().await.unwrap();
    // normalize run_id (nondeterministic) before byte-compare, then assert equal
}
```
> `run_id`/`x-run-id` are per-run UUIDs — normalize (replace with a fixed token) before the byte-compare, or assert structural equality on those fields and byte-equality on the rest.

- [ ] **Step 5: Run:** `cargo test -p paigasus-helikon-runtime-http-conformance` → PASS. This is where SSE byte-framing gets validated against axum — adjust `sse_frame` (Task 8) if bytes differ.

- [ ] **Step 6: doc-coverage check** for the new crate: `cargo test -p paigasus-helikon-runtime-http-conformance --doc` and confirm all `pub` items documented. fmt/clippy/commit `test(runtime-actix): SMA-343 axum/actix wire-format conformance suite`.

---

## Task 14: Example `actix_embed.rs`

**Files:**
- Create: `crates/paigasus-helikon-runtime-actix/examples/actix_embed.rs`
- Modify: `crates/paigasus-helikon-runtime-actix/Cargo.toml` (example needs `actix-web` in dev scope — already a dep; the example may need the `macros` feature for `#[actix_web::main]`, already enabled).

- [ ] **Step 1: Write the example** — an `EchoAgent` (port from axum's `curl_server.rs`), mounted via `configure()` inside an `App` that also serves an unrelated `GET /health` route, under `#[actix_web::main]`. Module docs carry the curl invocations (one-shot, SSE, list) as in axum's example.

- [ ] **Step 2: Build the example:** `cargo build -p paigasus-helikon-runtime-actix --example actix_embed` → clean.

- [ ] **Step 3: Manual smoke (optional, records AC #1):** run it, `curl -d '{"input":"hi"}' -H 'content-type: application/json' localhost:8080/agents/echo/runs` → `{"status":"completed","output":"hi",…}`; `curl localhost:8080/health` → the unrelated route still works.

- [ ] **Step 4: Commit** `docs(runtime-actix): SMA-343 add actix_embed example`.

---

## Task 15: CI gates (no-default-features + no-axum-leak)

**Files:**
- Modify: `.github/workflows/ci.yml`
- Modify: `.github/rulesets/main-protection-checks.json` (if a new required context is added)

- [ ] **Step 1: Extend the `build-no-default-features` job** to also run `cargo build -p paigasus-helikon-runtime-actix --no-default-features`.

- [ ] **Step 2: Add a no-axum-leakage step** (in an existing job or a small new one), e.g.:

```yaml
- name: no axum under runtime-actix
  run: |
    if cargo tree -p paigasus-helikon --features runtime-actix -e no-dev | grep -qE '(^|[^-])axum v'; then
      echo "axum leaked into runtime-actix"; exit 1; fi
```

- [ ] **Step 3:** If the leak check is a distinct job, add its bare job name to `.github/rulesets/main-protection-checks.json` required contexts. Keep any `uses:` actions pinned to commit SHAs of the latest stable release.

- [ ] **Step 4: Commit** `chore(workflows): SMA-343 gate actix no-default-features + no-axum-leak`.

---

## Task 16: Docs (book + READMEs)

**Files:**
- Modify: `docs/book/src/concepts/runtimes.md`
- Modify: `docs/book/src/concepts/axum-server.md` (add an "actix variant" section)
- Create: `crates/paigasus-helikon-runtime-actix/README.md`
- Modify: `crates/paigasus-helikon/README.md`, `README.md` (root)

- [ ] **Step 1: `runtimes.md`** — add a `paigasus-helikon-runtime-actix` / `runtime-actix` row to the runtimes table ("Self-hosted actix-web HTTP/SSE/WebSocket agent server — when you're embedding into an existing actix-web service").

- [ ] **Step 2: `axum-server.md`** — add a short "actix-web variant" subsection: API-identical, the §5.1 deltas (`configure()` vs `router()`, `std` listener, `#[actix_web::main]`, custom-`AuthLayer`/`ContextProvider` coupling), and a `configure()` embed snippet.

- [ ] **Step 3: crate `README.md`** — adapt the axum README (install, `configure()`/`serve()` example under `#[actix_web::main]`, routes table, features table, links). Fence any network/`.serve()` example as ```` ```ignore ```` if it would run under doctests (the facade README is the only include_str!'d one, but keep crate examples honest).

- [ ] **Step 4: facade + root README** — add `runtime-actix → runtime_actix` to the feature→module map and the crate roster.

- [ ] **Step 5: `mdbook build docs/book`** → clean (linkcheck `warning-policy = "error"`). Commit `docs(runtime-actix): SMA-343 document actix runtime in book + READMEs`.

---

## Task 17: Full CI-gate dry run + release sanity

- [ ] **Step 1: Run the full local CI matrix** (from CLAUDE.md):

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-features --all-targets -- -D warnings
cargo test --workspace --all-features
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps
DOC_COVERAGE_THRESHOLD=80 NIGHTLY_CHANNEL=nightly-2026-05-01 bash scripts/check-doc-coverage.sh
cargo build -p paigasus-helikon-runtime-actix --no-default-features
cargo build -p paigasus-helikon --features runtime-actix
```
Expected: all green; no axum under the actix feature.

- [ ] **Step 2: Release sanity** — confirm the new crate is at `0.1.0`, `publish` not disabled; the conformance crate is `publish=false` + `release=false` in `release-plz.toml`; facade + workspace dep pins consistent. (release-plz publishes the new crate on merge; no stub ritual — the name is confirmed free.)

- [ ] **Step 3: Final commit if any fixups** `chore(runtime-actix): SMA-343 satisfy full CI gate`.

---

## Self-review — spec coverage

| Spec section | Task |
|---|---|
| §3.1 `configure()` mount seam | 5, 10 |
| §3.2 OpenAPI manual handler | 11 |
| §3.3 conformance crate | 13 |
| §3.4 byte-parity scope | 13 |
| §3.5 hand-rolled SSE | 8, 13 |
| §3.6 shared runtime + sweeper-in-embed | 5, 7 |
| §4 port map (verbatim/adapted/new) | 2, 3, 4, 5–11 |
| §5.1 framework deltas (listener, `#[actix_web::main]`) | 5, 14, 16 |
| §6 auth/context adaptation + RefCell note | 4, 10 |
| §7 endpoints/transports + payload cap + WS + disconnect | 6, 7, 8, 9 |
| §8 OpenAPI no-axum | 11, 12, 15 |
| §9 conformance harness (dedicated System thread) | 13 |
| §10 facade + workspace wiring + minimal actix features | 1, 12 |
| §11 release plumbing | 1, 13, 17 |
| §13 docs | 16 |
| §14 CI gates | 11, 12, 15, 17 |
| §16 risks (multi-worker spike) | 7, 9 |

All spec requirements map to a task. No pre-abstraction (§12) — internals are duplicated, not shared.
