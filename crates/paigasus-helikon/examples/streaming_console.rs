//! Streaming example (SMA-323): print the assistant's tokens to stdout as
//! they arrive. Provider-agnostic; uses OpenAI here.
//!
//! ```text
//! OPENAI_API_KEY=sk-… cargo run -p paigasus-helikon \
//!     --features openai --example streaming_console
//! ```
//!
//! The model id (`gpt-5`) — swap it for any available model if the API
//! rejects it.

use std::io::Write;
use std::sync::Arc;

use futures_util::StreamExt;
use paigasus_helikon::core::{
    Agent, AgentEvent, AgentInput, CancellationToken, HookRegistry, LlmAgent, MemorySession,
    RunContext, TracerHandle,
};
use paigasus_helikon::openai::OpenAiModel;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let model = OpenAiModel::chat("gpt-5").build()?;

    let agent = LlmAgent::builder::<()>()
        .name("subscription-coach")
        .model(model)
        .instructions("You are a personal-finance assistant. Answer concisely.")
        .build();

    let ctx: RunContext<()> = RunContext::new(
        Arc::new(()),
        Arc::new(MemorySession::new()),
        HookRegistry::<()>::new(),
        TracerHandle::default(),
        CancellationToken::new(),
    );

    let input =
        AgentInput::from_user_text("Give me three quick tips to trim my monthly subscriptions.");

    let mut stream = agent.run(ctx, input).await?;
    let mut stdout = std::io::stdout();
    while let Some(event) = stream.next().await {
        match event {
            AgentEvent::TokenDelta { text } => {
                print!("{text}");
                stdout.flush()?;
            }
            // Surface an in-run failure (bad key, rejected model, …) instead of
            // exiting silently; fatal errors arrive as a stream event, not the
            // outer Result.
            AgentEvent::RunFailed { error } => anyhow::bail!("run failed: {error}"),
            _ => {}
        }
    }
    println!();
    Ok(())
}
