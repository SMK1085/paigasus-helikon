# paigasus-helikon-runtime-axum

Self-hosted HTTP/SSE/WebSocket server runtime for the [Paigasus Helikon](https://github.com/SMK1085/paigasus-helikon) AI SDK — a Rust SDK for building AI agents. Mounts one or more [`Agent`](https://docs.rs/paigasus-helikon-core/latest/paigasus_helikon_core/trait.Agent.html)s on an [axum](https://crates.io/crates/axum) router and serves them over REST (one-shot), Server-Sent Events, and WebSocket, with replayable run event logs.

## Install

```bash
cargo add paigasus-helikon-runtime-axum
```

Most users enable the `runtime-axum` feature on the [`paigasus-helikon`](https://crates.io/crates/paigasus-helikon) facade instead, which re-exports this crate as `paigasus_helikon::runtime_axum`:

```bash
cargo add paigasus-helikon --features runtime-axum
```

## Example

Define an agent, mount it, and serve:

```rust
use std::sync::Arc;
use async_trait::async_trait;
use futures_util::stream::{self, BoxStream, StreamExt as _};
use paigasus_helikon_core::{
    Agent, AgentError, AgentEvent, AgentInput, ContentPart, Item, RunContext, TokenUsage,
};
use paigasus_helikon_runtime_axum::AgentServer;

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

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    AgentServer::<()>::builder()
        .with_default_context()
        .agent(Arc::new(EchoAgent))
        .build()?
        .serve("0.0.0.0:8080")
        .await?;
    Ok(())
}
```

See the [`curl_server`](https://github.com/SMK1085/paigasus-helikon/blob/main/crates/paigasus-helikon-runtime-axum/examples/curl_server.rs) example for a runnable version with curl invocations.

## Routes

| Method | Path | Description |
|--------|------|-------------|
| `POST` | `/agents/{name}/runs` | One-shot run — blocks until complete, returns all events |
| `POST` | `/agents/{name}/runs?stream=sse` | SSE streaming run — one JSON event per `data:` line |
| `POST` | `/agents/{name}/runs?mode=async` | Async run — returns `202 Accepted` with a `run_id` immediately |
| `GET` | `/agents/{name}/runs/{id}/events` | Replay or stream events for a run (HTTP or WebSocket upgrade) |
| `GET` | `/agents` | List all mounted agents |
| `GET` | `/openapi.json` | OpenAPI 3.1 JSON spec (requires `openapi` feature, enabled by default) |

## Features

| Feature | Default | Description |
|---------|---------|-------------|
| `openapi` | yes | Generates and serves an OpenAPI 3.1 spec at `GET /openapi.json` via [utoipa](https://crates.io/crates/utoipa) |

## Links

- [API reference (docs.rs)](https://docs.rs/paigasus-helikon-runtime-axum)
- [Guide & concepts](https://smk1085.github.io/paigasus-helikon/) — see [Runtimes](https://smk1085.github.io/paigasus-helikon/concepts/runtimes.html)
- [Source & issues](https://github.com/SMK1085/paigasus-helikon)

## License

Licensed under either of [Apache-2.0](https://github.com/SMK1085/paigasus-helikon/blob/main/LICENSE-APACHE) or [MIT](https://github.com/SMK1085/paigasus-helikon/blob/main/LICENSE-MIT), at your option.
