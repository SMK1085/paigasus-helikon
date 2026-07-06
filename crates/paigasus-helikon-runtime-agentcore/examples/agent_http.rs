//! Model-backed AgentCore example: an `LlmAgent` fronted by Anthropic's Messages
//! API, served via [`AgentCoreServer`]. This is the size/cold-start
//! acceptance-criteria image (`docker/Dockerfile`'s `FEATURES=example-anthropic`
//! build) — a real provider client (reqwest + rustls/aws-lc-rs) statically linked
//! under musl, as opposed to `echo_http`'s dependency-free agent.
//!
//! ```text
//! ANTHROPIC_API_KEY=sk-… cargo run -p paigasus-helikon-runtime-agentcore \
//!     --features example-anthropic --example agent_http
//! ```
//!
//! The model id (`claude-sonnet-4-6`) — swap it for any available model if the API
//! rejects it.

use std::sync::Arc;

use paigasus_helikon_core::LlmAgent;
use paigasus_helikon_providers_anthropic::AnthropicModel;
use paigasus_helikon_runtime_agentcore::AgentCoreServer;

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

    // Reads the API key from the ANTHROPIC_API_KEY environment variable.
    let model = AnthropicModel::messages("claude-sonnet-4-6").build()?;

    let agent = LlmAgent::builder::<()>()
        .name("agentcore-anthropic-demo")
        .model(model)
        .instructions("You are a helpful assistant running inside AWS Bedrock AgentCore.")
        .build();

    let server = AgentCoreServer::<()>::builder()
        .agent(Arc::new(agent))
        .with_default_context()
        .build()?;

    server.serve().await?;
    Ok(())
}
