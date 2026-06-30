//! Echo agent example — demonstrates how to mount an agent on [`AgentServer`] and serve it.
//!
//! This example defines a tiny [`EchoAgent`] that echoes the caller's input back as an
//! assistant message, then starts the server on `127.0.0.1:8080`.
//!
//! # Running
//!
//! ```text
//! cargo run -p paigasus-helikon-runtime-axum --example curl_server
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

use std::sync::Arc;

use async_trait::async_trait;
use futures_util::stream::{self, BoxStream, StreamExt as _};
use paigasus_helikon_core::{
    Agent, AgentError, AgentEvent, AgentInput, ContentPart, Item, RunContext, TokenUsage,
};
use paigasus_helikon_runtime_axum::AgentServer;

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

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let server = AgentServer::<()>::builder()
        .with_default_context()
        .agent(Arc::new(EchoAgent))
        .build()?;

    println!("Listening on http://127.0.0.1:8080");
    server.serve("127.0.0.1:8080").await?;
    Ok(())
}
