# Axum Server Runtime

`paigasus-helikon-runtime-axum` mounts one or more [`Agent`](https://docs.rs/paigasus-helikon-core/latest/paigasus_helikon_core/trait.Agent.html)s on an [axum](https://docs.rs/axum) router and exposes them over HTTP, Server-Sent Events (SSE), and WebSocket. It is the self-hosted alternative to `paigasus-helikon-runtime-tokio`'s in-process runner — suitable when you need a network-accessible agent server with replayable runs.

Enable it via the `runtime-axum` facade feature:

```sh
cargo add paigasus-helikon --features openai,runtime-axum
```

Or depend on the crate directly:

```sh
cargo add paigasus-helikon-runtime-axum
```

## Quick start

```ignore
use std::sync::Arc;
use paigasus_helikon::runtime_axum::AgentServer;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let server = AgentServer::<()>::builder()
        .with_default_context()     // Ctx = () satisfies Default
        .agent(Arc::new(my_agent))
        .build()?;

    server.serve("0.0.0.0:8080").await?;
    Ok(())
}
```

`AgentServer::builder()` returns an [`AgentServerBuilder`](https://docs.rs/paigasus-helikon-runtime-axum/latest/paigasus_helikon_runtime_axum/struct.AgentServerBuilder.html) that lets you chain configuration. Once built, call `.serve(addr)` to bind and start serving, or `.router()` to embed the axum `Router` inside a larger application.

## HTTP endpoints

The server exposes four routes (six endpoint modes) under a flat prefix (no configurable base path):

| Method | Path | Purpose |
| --- | --- | --- |
| `POST` | `/agents/{name}/runs` | Start a run — one-shot (default), SSE (`?stream=sse`), or async (`?mode=async`) |
| `GET` | `/agents/{name}/runs/{id}/events` | Replay a run's event log over WebSocket |
| `GET` | `/agents` | List all registered agents and their descriptions |
| `GET` | `/openapi.json` | OpenAPI 3.1 schema (requires the `openapi` feature, enabled by default) |

### Response shapes for `POST /agents/{name}/runs`

The `?stream=` and `?mode=` query parameters select the response transport:

| Query | Status | Body |
| --- | --- | --- |
| *(none)* | `200 OK` | `RunResponse` JSON — full event list + final output, after run completes |
| `?stream=sse` | `200 OK` | `text/event-stream` — each `AgentEvent` as an SSE frame, streamed live |
| `?mode=async` | `202 Accepted` | `AsyncAccepted` JSON — `{ "run_id": "…" }` returned immediately |

All responses include an `X-Run-Id` response header carrying the UUID of the run.

### Request body

`POST /agents/{name}/runs` accepts JSON in either of two shapes:

```json
{ "input": "What is my dining budget this month?" }
```

or an explicit multi-turn message list:

```json
{ "messages": [ { "type": "user_message", "content": [{ "type": "text", "text": "…" }] } ] }
```

### Session affinity

Callers pass `X-Session-Id: <opaque-string>` to pin a run to a named session, but the header alone no longer resolves one. Every lookup is keyed on a `SessionKey` — the pair of that header value and the `Principal` your `AuthLayer` established for the request (see below) — so two callers can no longer collide on a guessed or shared id (CWE-639). The default `InMemorySessionProvider` maps a `SessionKey` to a shared `MemorySession`; two requests with an equal key share history and are serialised (the second waits until the first run completes) to avoid race conditions on the shared session state.

By default — whenever `.auth(...)` is configured on the builder — a request that carries `X-Session-Id` but for which no `Principal` was established is refused with `403 Forbidden`, because it would otherwise land in a namespace shared by every principal-less caller. `.allow_unbound_sessions()` opts out of the check for a single-tenant or shared-API-key deployment; `.require_principal(true)` opts in explicitly for an embedded deployment where a host application authenticates and no `AuthLayer` is configured on this builder.

Requests without `X-Session-Id` receive a fresh anonymous session that is never stored, regardless of principal.

Principal scoping extends to the WebSocket events endpoint too: a run's event stream is readable only by the principal that started it. Another principal who reaches the same run id — a leaked link, a guessed UUID, anything short of owning the run — receives a plain `404`, deliberately indistinguishable from a run that never existed, so the endpoint cannot be used to confirm which run ids exist. There is no administrative override; not even an operator can subscribe to a run they did not start.

## Replayable runs

Every run — regardless of the transport used to start it — drains into an in-memory `EventLog`. The key properties:

- **One-shot mode** subscribes to the log and blocks until the run is terminal.
- **SSE mode** subscribes and streams events as they arrive; a client reconnect to `GET /agents/{name}/runs/{id}/events` (WebSocket) replays already-emitted events before tailing live ones.
- **Async mode** returns `202` immediately and the run continues in a background task. The log survives connection close.
- **Cancellation**: one-shot and SSE responses hold a `CancellationToken` drop-guard so a client disconnect cancels the run. The async mode deliberately does not, so the run outlives the connection.
- **Stream error frames**: if a run ends without a real terminal event — e.g. its runner fails to start after the run is registered, or its stream ends early — the SSE and WebSocket transports emit a final synthetic `run_failed` event before closing, so a streaming client always observes a terminal frame. (One-shot mode instead returns HTTP `500` on a start error, or `200` with a partial result when a started run ends without a terminal event.)

Completed runs are retained for a configurable period and count:

| Builder method | Default | Effect |
| --- | --- | --- |
| `.run_retention(Duration)` | 5 minutes | How long completed runs stay in the registry |
| `.max_retained_runs(usize)` | 1 024 | Cap on retained completed runs (oldest evicted first) |
| `.max_sessions(usize)` | 4 096 | Cap on tracked named sessions (oldest evicted first) |
| `.max_in_flight(usize)` | 1 024 | Cap on simultaneously in-flight (non-terminal) runs; further run creation is rejected with `503` + `Retry-After` once reached |
| `.max_run_duration(Duration)` | 1 hour | Wall-clock lifetime after which a still-live run is cancelled and marked terminal by the sweeper, reclaiming its in-flight slot |

A background sweeper task prunes expired entries and reclaims runs over `max_run_duration`. It starts on `.serve()`, `.serve_with_listener()`, and — since the embed path must reclaim too — `.router()` as well (actix's `.configure()` starts it the same way). On axum, `.router()`'s spawn needs an ambient Tokio runtime; if none is available (e.g. an embedding host still assembling its router before ever starting one) the spawn is skipped with a `tracing::warn!`, and a later call made from within a runtime spawns it then.

## Provider traits

Three traits are the extension points for operator customisation:

### `SessionProvider`

```ignore
#[async_trait]
pub trait SessionProvider: Send + Sync {
    async fn session(&self, key: SessionKey<'_>) -> Result<Arc<dyn Session>, ServerError>;
}
```

Maps a `SessionKey` — the pair of the authenticated `Principal` (if any) and the caller-supplied `X-Session-Id` (if any) — to a `Session`. The built-in `InMemorySessionProvider` is the default; swap it for a `PostgresSession` or `RedisSession` backend via `.session_provider(Arc::new(...))` on the builder.

**Key on `storage_key()`, not `id` alone.** `SessionKey::storage_key()` returns a collision-free single-string encoding of the compound key, for a backend that needs one string to key on — as `Option<String>`, where `None` means the request is anonymous and MUST NOT be stored. Folding that `None` into a default string (e.g. `.unwrap_or_default()`) puts every anonymous caller on one shared row, reopening a cross-caller leak. Reading `key.id` alone preserves the pre-0.2 behaviour *and* the CWE-639 vulnerability it fixed: a custom provider that keys only on the caller-supplied id lets any admitted caller who learns or guesses another caller's id read and append to that conversation.

### `ContextProvider<Ctx>`

```ignore
#[async_trait]
pub trait ContextProvider<Ctx>: Send + Sync {
    async fn build(
        &self,
        parts: &axum::http::request::Parts,
        session: Arc<dyn Session>,
        cancel: CancellationToken,
    ) -> Result<RunContext<Ctx>, ServerError>;
}
```

Builds the per-request `RunContext`. Implement this to inject request-scoped data into `Ctx` — for example, JWT-parsed tenant identity — and to tighten the permission posture for network clients (see the security note below). When `Ctx: Default`, use the convenience shortcut `.with_default_context()` on the builder instead of supplying a custom implementation.

### `AuthLayer`

```ignore
#[async_trait]
pub trait AuthLayer: Send + Sync {
    async fn authenticate(&self, parts: &mut axum::http::request::Parts) -> Result<(), AuthRejection>;
}
```

Called before every request. Return `Ok(())` to allow; return `Err(AuthRejection { status, message })` to reject. On success, you may insert an identity value into `parts.extensions` — the `ContextProvider` receives the same `parts`, creating the auth→context bridge. When `.auth(...)` is not called on the builder, all requests are admitted without authentication.

One extension type is not opaque to the server: insert `Principal(user_id)` to name the authenticated caller, and the server scopes every session that caller reaches to that name (see [Session affinity](#session-affinity) above). Implementing `AuthLayer` names `axum::http::request::Parts`, but this crate does not re-export `axum` — add it as a direct dependency and keep its minor version aligned with this crate's.

### Security note

The `DefaultContextProvider` leaves all `RunContext` settings at their core defaults. For production deployments:

- Implement `ContextProvider` and call `.with_permission_mode(PermissionMode::Deny)` to prevent agents from escalating tool permissions at runtime.
- Supply a custom `ApprovalHandler` that enforces your tenant's access-control list.
- Attach a `HookRegistry` for telemetry and policy enforcement.

## The `openapi` feature

The `openapi` feature (enabled by default) activates the `GET /openapi.json` endpoint, which serves an OpenAPI 3.1 schema generated with [utoipa](https://docs.rs/utoipa). Disable it if you do not need the schema endpoint:

```sh
cargo add paigasus-helikon-runtime-axum --no-default-features
```

## Embedding in a larger router

`.router()` returns a plain axum `Router` without binding any socket. Use this to nest the agent endpoints under a prefix or combine them with your own routes:

```ignore
let app = axum::Router::new()
    .nest("/api/v1", server.router())
    .route("/healthz", axum::routing::get(|| async { "ok" }));

axum::serve(listener, app).await?;
```

## actix-web variant

`paigasus-helikon-runtime-actix` (feature `runtime-actix`) is the same server, ported to [actix-web](https://docs.rs/actix-web) instead of axum — for when you're embedding into an existing actix-web service rather than owning the process's `axum::Router`. It is **API-identical** to `paigasus-helikon-runtime-axum`: the same `AgentServer` / `AgentServerBuilder`, the same routes, the same DTOs, and byte-identical JSON/SSE wire formats (verified by a shared conformance suite that runs both servers side by side). Only the handful of deltas below — all forced by the framework, not by design choice — differ.

1. **Mount seam.** Instead of `.router() -> axum::Router`, call `.configure()`, which returns an `App::configure` closure (`impl Fn(&mut actix_web::web::ServiceConfig) + Send + Clone + 'static`) that mounts the agent routes on an existing actix `App`.
2. **Listener type.** `.serve_with_listener(listener)` takes a **`std::net::TcpListener`** (set non-blocking internally), not axum's `tokio::net::TcpListener`. `.serve(addr)` keeps the same signature as the axum runtime.
3. **Entrypoint attribute.** The standalone `.serve()`/`.serve_with_listener()` path requires an `actix-rt` `System`, so binaries use `#[actix_web::main]` rather than `#[tokio::main]`.
4. **Custom `AuthLayer` / `ContextProvider` are framework-coupled.** Because the request type *is* the framework, a hand-rolled implementation takes `&actix_web::HttpRequest` rather than `&axum::http::request::Parts` — the trait names, method names, and auth→context identity hand-off are otherwise the same.
5. **Client-disconnect behavior differs for one-shot runs.** SSE cancels the run on a client disconnect, matching the axum runtime. A **one-shot** run does not: actix does not drop the buffered handler future when the client goes away, so the run is driven to completion regardless of disconnect — unlike the axum runtime, where both one-shot and SSE cancel on disconnect.

Embedding via `configure()` inside a host `App`:

```rust,ignore
use actix_web::{App, HttpServer};
use paigasus_helikon_runtime_actix::AgentServer;

let server = AgentServer::<()>::builder()
    .with_default_context()
    .agent(Arc::new(my_agent))
    .build()?;
let cfg = server.configure(); // Fn(&mut ServiceConfig) + Send + Clone + 'static

HttpServer::new(move || {
    App::new()
        .configure(cfg.clone()) // mounts /agents, /agents/{name}/runs, …
})
.bind(("127.0.0.1", 8080))?
.run()
.await?;
```

See the [crate README](https://docs.rs/paigasus-helikon-runtime-actix) for a full walkthrough, and `examples/actix_embed.rs` in the crate for a runnable version mounted alongside an unrelated host route.

## API reference

Full per-item documentation: [`paigasus_helikon_runtime_axum`](https://docs.rs/paigasus-helikon-runtime-axum) ([`paigasus_helikon_runtime_actix`](https://docs.rs/paigasus-helikon-runtime-actix) for the actix-web variant).

Facade re-export: enable the `runtime-axum` feature on `paigasus-helikon` and import via `paigasus_helikon::runtime_axum::*` (or `runtime-actix` / `paigasus_helikon::runtime_actix::*`).
