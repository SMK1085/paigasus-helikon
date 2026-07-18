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
