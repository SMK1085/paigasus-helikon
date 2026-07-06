//! MCP-protocol AgentCore example: the same dependency-free echo agent as
//! `echo_http.rs`, served as a single MCP tool via `AgentCoreServer::serve_mcp`
//! (feature `mcp`, default on) instead of the HTTP-protocol contract.
//!
//! ```text
//! cargo run -p paigasus-helikon-runtime-agentcore --example mcp_server
//! ```
//!
//! The MCP endpoint is mounted at `http://localhost:8000/mcp` (streamable HTTP).

use std::sync::Arc;

use async_trait::async_trait;
use futures_util::stream::{self, BoxStream, StreamExt as _};
use paigasus_helikon_core::{
    Agent, AgentError, AgentEvent, AgentInput, ContentPart, Item, RunContext, TokenUsage,
};
use paigasus_helikon_runtime_agentcore::AgentCoreServer;

/// Echoes the concatenated text of every `Item::UserMessage` in the input back as
/// the run's sole assistant message.
struct EchoAgent;

#[async_trait]
impl Agent<()> for EchoAgent {
    fn name(&self) -> &str {
        "echo"
    }

    fn description(&self) -> &str {
        "Echoes the input back as the final output."
    }

    async fn run(
        &self,
        _ctx: RunContext<()>,
        input: AgentInput,
    ) -> Result<BoxStream<'static, AgentEvent>, AgentError> {
        let mut text = String::new();
        for item in &input.messages {
            if let Item::UserMessage { content } = item {
                for part in content {
                    if let ContentPart::Text { text: t } = part {
                        text.push_str(t);
                    }
                }
            }
        }

        let events = vec![
            AgentEvent::RunStarted {
                agent: self.name().to_owned(),
            },
            AgentEvent::MessageOutput {
                item: Item::AssistantMessage {
                    content: vec![ContentPart::Text { text }],
                    agent: Some(self.name().to_owned()),
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
    // See `echo_http.rs` for why this defaults to `info` rather than
    // `tracing_subscriber::fmt::init()`'s plain default (which would silently
    // swallow the "ready in {ms}ms" cold-start log).
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let server = AgentCoreServer::<()>::builder()
        .agent(Arc::new(EchoAgent))
        .with_default_context()
        .build()?;

    server.serve_mcp().await?;
    Ok(())
}
