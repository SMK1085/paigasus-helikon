# paigasus-helikon-runtime-actix

Self-hosted [actix-web](https://crates.io/crates/actix-web) HTTP/SSE/WebSocket server runtime for the [Paigasus Helikon](https://github.com/SMK1085/paigasus-helikon) AI SDK — a Rust SDK for building AI agents. API-identical to [`paigasus-helikon-runtime-axum`](https://crates.io/crates/paigasus-helikon-runtime-axum): mounts one or more [`Agent`](https://docs.rs/paigasus-helikon-core/latest/paigasus_helikon_core/trait.Agent.html)s and serves them over REST (one-shot), Server-Sent Events, and WebSocket, with replayable run event logs. Use this crate instead of the axum runtime when you're embedding into an existing actix-web service.

## Install

```bash
cargo add paigasus-helikon-runtime-actix
```

Most users enable the `runtime-actix` feature on the [`paigasus-helikon`](https://crates.io/crates/paigasus-helikon) facade instead, which re-exports this crate as `paigasus_helikon::runtime_actix`:

```bash
cargo add paigasus-helikon --features runtime-actix
```

## Example

Define an agent, mount it via `configure()`, and serve with `#[actix_web::main]`:

```rust,no_run
use std::sync::Arc;
use actix_web::{App, HttpServer};
use async_trait::async_trait;
use futures_util::stream::{self, BoxStream, StreamExt as _};
use paigasus_helikon_core::{
    Agent, AgentError, AgentEvent, AgentInput, ContentPart, Item, RunContext, TokenUsage,
};
use paigasus_helikon_runtime_actix::AgentServer;

struct EchoAgent;

#[async_trait]
impl Agent<()> for EchoAgent {
    fn name(&self) -> &str { "echo" }
    fn description(&self) -> &str { "Echoes the input back." }

    async fn run(
        &self,
        _ctx: RunContext<()>,
        _input: AgentInput,
    ) -> Result<BoxStream<'static, AgentEvent>, AgentError> {
        Ok(stream::iter(vec![
            AgentEvent::MessageOutput {
                item: Item::AssistantMessage {
                    content: vec![ContentPart::Text { text: "echo".to_owned() }],
                    agent: None,
                },
            },
            AgentEvent::RunCompleted { usage: TokenUsage::default() },
        ]).boxed())
    }
}

#[actix_web::main]                                     // NOT #[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let server = AgentServer::<()>::builder()
        .with_default_context()
        .agent(Arc::new(EchoAgent))
        .build()?;
    let cfg = server.configure();                      // Fn(&mut ServiceConfig)+Send+Clone+'static

    HttpServer::new(move || {
        App::new()
            .configure(cfg.clone())                    // mounts /agents, /openapi.json, …
    })
    .bind(("127.0.0.1", 8080))?
    .run()
    .await?;
    Ok(())
}
```

See the [`actix_embed`](https://github.com/SMK1085/paigasus-helikon/blob/main/crates/paigasus-helikon-runtime-actix/examples/actix_embed.rs) example for a runnable version that mounts the agent routes alongside an unrelated host route (`GET /health`), with curl invocations in the module docs.

## Routes

| Method | Path | Description |
|--------|------|-------------|
| `POST` | `/agents/{name}/runs` | One-shot run — blocks until complete, returns all events |
| `POST` | `/agents/{name}/runs?stream=sse` | SSE streaming run — one JSON event per `data:` line |
| `POST` | `/agents/{name}/runs?mode=async` | Async run — returns `202 Accepted` with a `run_id` immediately |
| `GET` | `/agents/{name}/runs/{id}/events` | Subscribe to a run's events over **WebSocket** — replays from the start, then live-tails until terminal |
| `GET` | `/agents` | List all mounted agents |
| `GET` | `/openapi.json` | OpenAPI 3.1 JSON spec (requires `openapi` feature, enabled by default) |

On a start error — or any run that ends without a terminal event — the streaming transports (SSE and WebSocket) emit a final synthetic `run_failed` event before closing, so a streaming client always sees a terminal frame. One-shot runs instead return `500` on a start error, or `200` with a partial result when a started run ends without a terminal event.

## Differences from `paigasus-helikon-runtime-axum`

This crate is a port, not a reimplementation — the wire format, DTOs, and behavior are identical, guarded by a shared conformance suite that runs both servers side by side. Only these deltas are forced by the actix-web framework itself:

- **Mount seam:** `.configure()` (an `App::configure` closure) instead of `.router() -> axum::Router`.
- **Listener type:** `.serve_with_listener(listener)` takes a `std::net::TcpListener`, not axum's `tokio::net::TcpListener`.
- **Entrypoint attribute:** `#[actix_web::main]`, not `#[tokio::main]`.
- **`AuthLayer` / `ContextProvider`:** a custom implementation takes `&actix_web::HttpRequest` rather than `&axum::http::request::Parts` — the request type is the framework surface.
- **Client-disconnect behavior for one-shot runs:** SSE cancels the run on disconnect (same as axum), but a one-shot run runs to completion — actix does not drop the buffered handler future on disconnect, unlike axum.

See the [Axum Server Runtime](https://smk1085.github.io/paigasus-helikon/concepts/axum-server.html#actix-web-variant) book chapter for the full writeup.

## Security: sessions are scoped to the authenticated principal

Session affinity still comes from the `X-Session-Id` request header, which the
caller chooses — but the header alone no longer resolves a session. Every
lookup is keyed on a `SessionKey`, the pair of that header value and the
`Principal` your `AuthLayer` established for the request, so two callers can
no longer collide on a guessed or shared id (CWE-639).

An `AuthLayer` establishes the principal by inserting a `Principal(String)`
into the request extensions from `authenticate()`:

```ignore
use actix_web::HttpMessage; // brings `extensions_mut()` into scope

req.extensions_mut().insert(Principal(user_id));
```

By default (whenever `.auth(...)` is configured), a request that carries
`X-Session-Id` with no `Principal` attached is refused with `403 Forbidden` —
a named session with no principal would otherwise land in a namespace shared
by every principal-less caller. Two builder methods adjust this:

- `.allow_unbound_sessions()` — permit `X-Session-Id` from callers with no
  established principal, for a single-tenant service or a shared-API-key
  deployment that genuinely wants one shared session namespace. This
  suppresses the 403 and nothing else: a caller that *does* carry a
  `Principal` is still isolated to it.
- `.require_principal(true)` — turn the check on explicitly for an embedded
  deployment (`AgentServer::configure()` mounted into a host `App`), where no
  `AuthLayer` is configured on this builder because the host application
  authenticates upstream. Insert `Principal` into the request extensions
  yourself before the request reaches this server.

A run's WebSocket event stream (`GET /agents/{name}/runs/{id}/events`) is
readable only by the principal that started it; any other principal —
including an operator with no special override — gets `404 Not Found`, the
same response a nonexistent run id gets, so the endpoint cannot be used as an
existence oracle for harvested run ids.

**Mixed authenticated/anonymous traffic.** A run started with no `Principal`
is stored with `principal: None` and stays readable by any other anonymous
caller; `require_principal` does not close this, because it only fires when
`X-Session-Id` is present. It also means a caller who starts a run
anonymously and later subscribes to its WebSocket *with* credentials gets
`404` for their own run — `Some("alice")` and `None` are different owners.
Deployments that mix authenticated and anonymous traffic should authenticate
consistently across a run's lifetime.

**The `actix-web` dependency seam.** `AuthLayer::authenticate` takes
`&actix_web::HttpRequest`, but this crate does not re-export `actix-web`, so
implementing `AuthLayer` forces the dependency directly. Calling `.serve(addr)`
on its own does too — not because `serve` itself names an `actix_web` type
(it doesn't), but because driving it to completion needs an actix-rt `System`,
which in practice means wrapping `main` in `#[actix_web::main]` (see
"Entrypoint attribute" above). Either way, add `actix-web` as a direct
dependency and keep its **major** version aligned with this crate's —
actix-web is a 4.x crate, so the major, not the minor, is the breaking
component.

## Migrating to 0.2

- `SessionProvider::session` now takes a `SessionKey<'_>` instead of
  `Option<&str>`. Use `key.storage_key()` for a single-string backend key
  (Postgres, Redis, a filesystem path); it returns `Option<String>`, `None`
  meaning the request is anonymous and MUST NOT be stored — folding that
  `None` into a default string (e.g. `.unwrap_or_default()`) puts every
  anonymous caller on one shared row, reopening a cross-caller leak. **Reading
  `key.id` alone preserves the old behaviour *and* the CWE-639 vulnerability**
  — it drops the principal component the key exists to add.
- An `AuthLayer` used together with `X-Session-Id` must now insert a
  `Principal`, or the server must be built with `.allow_unbound_sessions()`;
  otherwise sessioned requests are refused with `403`.
- Embedded deployments where a host application authenticates (no `AuthLayer`
  configured on this builder) should insert `Principal` themselves and call
  `.require_principal(true)`.
- Every `5xx` response body is now a fixed, non-diagnostic string; the
  underlying detail is logged via `tracing` at `error` level instead.
- In-flight runs are capped at 1 024 by default (`.max_in_flight(usize)`),
  rejecting further run creation with `503` + `Retry-After` once reached; a
  run still live after 1 hour (`.max_run_duration(Duration)`) is cancelled and
  its slot reclaimed.

## Features

| Feature | Default | Description |
|---------|---------|-------------|
| `openapi` | yes | Generates and serves an OpenAPI 3.1 spec at `GET /openapi.json` via [utoipa](https://crates.io/crates/utoipa) |

## Links

- [API reference (docs.rs)](https://docs.rs/paigasus-helikon-runtime-actix)
- [Guide & concepts](https://smk1085.github.io/paigasus-helikon/) — see [Axum Server Runtime § actix-web variant](https://smk1085.github.io/paigasus-helikon/concepts/axum-server.html#actix-web-variant)
- [Source & issues](https://github.com/SMK1085/paigasus-helikon)

## License

Licensed under either of [Apache-2.0](https://github.com/SMK1085/paigasus-helikon/blob/main/LICENSE-APACHE) or [MIT](https://github.com/SMK1085/paigasus-helikon/blob/main/LICENSE-MIT), at your option.
