# Axum Server Runtime

`paigasus-helikon-runtime-axum` mounts one or more [`Agent`](https://docs.rs/paigasus-helikon-core/latest/paigasus_helikon_core/trait.Agent.html)s on an [axum](https://docs.rs/axum) router and exposes them over HTTP, Server-Sent Events (SSE), and WebSocket. It is the self-hosted alternative to `paigasus-helikon-runtime-tokio`'s in-process runner — suitable when you need a network-accessible agent server with replayable runs.

Enable it via the `runtime-axum` facade feature:

```toml
[dependencies]
paigasus-helikon = { version = "0.1", features = ["openai", "runtime-axum"] }
```

Or depend on the crate directly:

```toml
[dependencies]
paigasus-helikon-runtime-axum = "0.1"
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

The server exposes six endpoints under a flat prefix (no configurable base path):

| Method | Path | Purpose |
| --- | --- | --- |
| `POST` | `/agents/{name}/runs` | Start a run — one-shot (default), SSE (`?stream=sse`), or async (`?mode=async`) |
| `GET` | `/agents/{name}/runs/{id}/events` | Replay a run's event log over WebSocket |
| `GET` | `/agents` | List all registered agents and their descriptions |
| `GET` | `/openapi.json` | OpenAPI 3.0 schema (requires the `openapi` feature, enabled by default) |

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

Callers pass `X-Session-Id: <opaque-string>` to pin a run to a named session. The default `InMemorySessionProvider` maps that header to a shared `MemorySession`; two requests with the same `X-Session-Id` share history and are serialised (the second waits until the first run completes) to avoid race conditions on the shared session state.

Requests without `X-Session-Id` receive a fresh anonymous session that is never stored.

## Replayable runs

Every run — regardless of the transport used to start it — drains into an in-memory `EventLog`. The key properties:

- **One-shot mode** subscribes to the log and blocks until the run is terminal.
- **SSE mode** subscribes and streams events as they arrive; a client reconnect to `GET /agents/{name}/runs/{id}/events` (WebSocket) replays already-emitted events before tailing live ones.
- **Async mode** returns `202` immediately and the run continues in a background task. The log survives connection close.
- **Cancellation**: one-shot and SSE responses hold a `CancellationToken` drop-guard so a client disconnect cancels the run. The async mode deliberately does not, so the run outlives the connection.

Completed runs are retained for a configurable period and count:

| Builder method | Default | Effect |
| --- | --- | --- |
| `.run_retention(Duration)` | 5 minutes | How long completed runs stay in the registry |
| `.max_retained_runs(usize)` | 1 024 | Cap on retained completed runs (oldest evicted first) |
| `.max_sessions(usize)` | 4 096 | Cap on tracked named sessions (oldest evicted first) |

A background sweeper task is started by `.serve()` / `.serve_with_listener()` to prune expired entries.

## Provider traits

Three traits are the extension points for operator customisation:

### `SessionProvider`

```ignore
#[async_trait]
pub trait SessionProvider: Send + Sync {
    async fn session(&self, id: Option<&str>) -> Result<Arc<dyn Session>, ServerError>;
}
```

Maps the `X-Session-Id` header value to a `Session`. The built-in `InMemorySessionProvider` is the default; swap it for a `PostgresSession` or `RedisSession` backend via `.session_provider(Arc::new(...))` on the builder.

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

### Security note

The `DefaultContextProvider` leaves all `RunContext` settings at their core defaults. For production deployments:

- Implement `ContextProvider` and call `.with_permission_mode(PermissionMode::Deny)` to prevent agents from escalating tool permissions at runtime.
- Supply a custom `ApprovalHandler` that enforces your tenant's access-control list.
- Attach a `HookRegistry` for telemetry and policy enforcement.

## The `openapi` feature

The `openapi` feature (enabled by default) activates the `GET /openapi.json` endpoint, which serves an OpenAPI 3.0 schema generated with [utoipa](https://docs.rs/utoipa). Disable it if you do not need the schema endpoint:

```toml
paigasus-helikon-runtime-axum = { version = "0.1", default-features = false }
```

## Embedding in a larger router

`.router()` returns a plain axum `Router` without binding any socket. Use this to nest the agent endpoints under a prefix or combine them with your own routes:

```ignore
let app = axum::Router::new()
    .nest("/api/v1", server.router())
    .route("/healthz", axum::routing::get(|| async { "ok" }));

axum::serve(listener, app).await?;
```

## API reference

Full per-item documentation: [`paigasus_helikon_runtime_axum`](https://docs.rs/paigasus-helikon-runtime-axum).

Facade re-export: enable the `runtime-axum` feature on `paigasus-helikon` and import via `paigasus_helikon::runtime_axum::*`.
