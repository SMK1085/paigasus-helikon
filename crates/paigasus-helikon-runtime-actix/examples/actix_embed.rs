//! Echo agent example — demonstrates embedding an [`AgentServer`] inside an existing
//! actix-web service tree.
//!
//! This example defines a tiny [`EchoAgent`] that echoes the caller's input back as an
//! assistant message, builds an [`AgentServer`], and mounts its routes via
//! [`AgentServer::configure`] inside an `App` that *also* serves an unrelated
//! `GET /health` route of its own. This proves the Helikon routes coexist with a
//! host application's pre-existing routes rather than requiring exclusive ownership
//! of the `App`.
//!
//! # Running
//!
//! ```text
//! cargo run -p paigasus-helikon-runtime-actix --example actix_embed
//! ```
//!
//! # curl examples
//!
//! One-shot (blocks until the run completes, returns a JSON response):
//!
//! ```text
//! curl -H 'Content-Type: application/json' \
//!      -d '{"input":"hello"}' \
//!      http://localhost:8080/agents/echo/runs
//! ```
//!
//! Server-Sent Events stream (one JSON event per `data:` line):
//!
//! ```text
//! curl -N -H 'Content-Type: application/json' \
//!      -d '{"input":"hi"}' \
//!      'http://localhost:8080/agents/echo/runs?stream=sse'
//! ```
//!
//! List mounted agents:
//!
//! ```text
//! curl http://localhost:8080/agents
//! ```
//!
//! The host application's own route, served alongside the mounted agent routes:
//!
//! ```text
//! curl http://localhost:8080/health
//! ```

use std::sync::Arc;

use actix_web::{web, App, HttpResponse, HttpServer};
use async_trait::async_trait;
use futures_util::stream::{self, BoxStream, StreamExt as _};
use paigasus_helikon_core::{
    Agent, AgentError, AgentEvent, AgentInput, ContentPart, Item, RunContext, TokenUsage,
};
use paigasus_helikon_runtime_actix::AgentServer;

/// A minimal agent that echoes the caller's input back as an assistant message.
struct EchoAgent;

#[async_trait]
impl Agent<()> for EchoAgent {
    fn name(&self) -> &str {
        "echo"
    }

    fn description(&self) -> &str {
        "Echoes the caller's input back as an assistant message."
    }

    async fn run(
        &self,
        _ctx: RunContext<()>,
        input: AgentInput,
    ) -> Result<BoxStream<'static, AgentEvent>, AgentError> {
        // Extract the last user message's text, falling back to a fixed string.
        let text = input
            .messages
            .iter()
            .rev()
            .find_map(|item| match item {
                Item::UserMessage { content } => content.iter().find_map(|part| match part {
                    ContentPart::Text { text } => Some(text.clone()),
                    _ => None,
                }),
                _ => None,
            })
            .unwrap_or_else(|| "echo".to_owned());

        let events = vec![
            AgentEvent::MessageOutput {
                item: Item::AssistantMessage {
                    content: vec![ContentPart::Text { text }],
                    agent: None,
                },
            },
            AgentEvent::RunCompleted {
                usage: TokenUsage::default(),
            },
        ];
        Ok(stream::iter(events).boxed())
    }
}

#[actix_web::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Build the Helikon server, then obtain its `configure()` closure — this is the
    // embedding seam. The host app below owns the actix `App`/`HttpServer` and mounts
    // the Helikon routes alongside its own, rather than the Helikon runtime owning the
    // whole process.
    let server = AgentServer::<()>::builder()
        .with_default_context()
        .agent(Arc::new(EchoAgent))
        .build()?;
    let cfg = server.configure();

    println!("Listening on http://127.0.0.1:8080");
    HttpServer::new(move || {
        App::new()
            // The host application's own, unrelated route — untouched by Helikon.
            .route(
                "/health",
                web::get().to(|| async { HttpResponse::Ok().body("ok") }),
            )
            // Mounts `/agents`, `/agents/{name}/runs`, and friends alongside it.
            .configure(cfg.clone())
    })
    .bind(("127.0.0.1", 8080))?
    .run()
    .await?;
    Ok(())
}
