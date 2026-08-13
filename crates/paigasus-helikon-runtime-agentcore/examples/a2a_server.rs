//! AgentCore **A2A** protocol mode: JSON-RPC 2.0 on port 9000 with agent-card
//! discovery. A dependency-free echo agent — no model provider client, no TLS stack.
//!
//! ```text
//! cargo run -p paigasus-helikon-runtime-agentcore --example a2a_server --features a2a
//!
//! # Discovery: the agent card AWS and A2A clients fetch first.
//! curl -s localhost:9000/.well-known/agent-card.json | jq .
//!
//! curl -s localhost:9000/ping
//!
//! # Buffered: run to completion and return the finished task.
//! curl -s -X POST localhost:9000/ -H 'content-type: application/json' -d '{
//!   "jsonrpc": "2.0",
//!   "id": "req-001",
//!   "method": "message/send",
//!   "params": {"message": {
//!     "role": "user",
//!     "parts": [{"kind": "text", "text": "hi there"}],
//!     "messageId": "unique-message-id"
//!   }}
//! }' | jq .
//!
//! # Streaming: the same run as Server-Sent Events.
//! curl -N -X POST localhost:9000/ -H 'content-type: application/json' -d '{
//!   "jsonrpc": "2.0", "id": "req-002", "method": "message/stream",
//!   "params": {"message": {"role": "user",
//!     "parts": [{"kind": "text", "text": "stream me"}], "messageId": "m2"}}
//! }'
//!
//! # Fetch a task afterwards by the id `message/send` returned.
//! curl -s -X POST localhost:9000/ -H 'content-type: application/json' \
//!   -d '{"jsonrpc":"2.0","id":3,"method":"tasks/get","params":{"id":"<task-id>"}}' | jq .
//! ```

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
    // Default to `info` so `serve_a2a`'s "ready in {ms}ms" cold-start log is visible
    // without setting `RUST_LOG`, while still honoring an explicit override.
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

    server.serve_a2a().await?;
    Ok(())
}
