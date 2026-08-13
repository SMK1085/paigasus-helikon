//! AgentCore **AG-UI** protocol mode: an AG-UI event stream over SSE at
//! `POST /invocations` plus a WebSocket at `GET /ws`, on port 8080. A dependency-free
//! echo agent — no model provider client, no TLS stack.
//!
//! AG-UI and the HTTP protocol are alternative `serverProtocol` settings for one
//! container and share port 8080, so run this *or* `echo_http`, never both.
//!
//! ```text
//! cargo run -p paigasus-helikon-runtime-agentcore --example agui_server --features ag-ui
//!
//! curl -s localhost:8080/ping
//!
//! # AG-UI's RunAgentInput body; the response is an SSE stream of AG-UI events
//! # (RUN_STARTED, TEXT_MESSAGE_START/CONTENT/END, RUN_FINISHED).
//! curl -N -X POST localhost:8080/invocations -H 'content-type: application/json' -d '{
//!   "threadId": "thread-123",
//!   "runId": "run-456",
//!   "messages": [{"id": "msg-1", "role": "user", "content": "Hello, agent!"}],
//!   "tools": [], "context": [], "state": {}, "forwardedProps": {}
//! }'
//! ```
//!
//! The same vocabulary is available over the WebSocket at `ws://localhost:8080/ws`,
//! where each inbound text frame is one `RunAgentInput`.

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
    // Default to `info` so `serve_agui`'s "ready in {ms}ms" cold-start log is visible
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

    server.serve_agui().await?;
    Ok(())
}
