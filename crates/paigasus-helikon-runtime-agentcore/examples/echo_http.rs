//! Minimal AgentCore example: a dependency-free agent that echoes the concatenated
//! text of its input back as the run's final output. No model provider client, no
//! TLS stack — this is the crate's minimal-overhead example, and `docker/Dockerfile`
//! builds it by default (`ARG EXAMPLE=echo_http`).
//!
//! ```text
//! cargo run -p paigasus-helikon-runtime-agentcore --example echo_http
//!
//! curl -s localhost:8080/ping
//! curl -s -X POST localhost:8080/invocations \
//!     -H 'content-type: application/json' -H 'accept: application/json' \
//!     -d '{"prompt":"hi there"}'
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
    // `tracing_subscriber::fmt::init()`'s default filter suppresses INFO — including
    // `AgentCoreServer::serve`'s "ready in {ms}ms" cold-start log — unless `RUST_LOG`
    // is set. Default to `info` so the log this crate's docs and
    // `scripts/agentcore-image-check.sh` rely on is visible out of the box, while
    // still honoring an explicit `RUST_LOG` override.
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

    server.serve().await?;
    Ok(())
}
