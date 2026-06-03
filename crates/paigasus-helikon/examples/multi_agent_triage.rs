//! Multi-agent handoff example (SMA-324): a personal-finance triage agent that
//! routes the conversation to a budgeting specialist or an investing
//! specialist via `Handoff`.
//!
//! ```text
//! OPENAI_API_KEY=sk-… cargo run -p paigasus-helikon \
//!     --features openai --example multi_agent_triage
//! ```

use std::sync::Arc;

use paigasus_helikon::core::{
    Agent, AgentInput, CancellationToken, Handoff, HookRegistry, LlmAgent, MemorySession,
    RunContext, RunResultStreaming, TracerHandle,
};
use paigasus_helikon::openai::OpenAiModel;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let budgeting = LlmAgent::builder::<()>()
        .name("budgeting specialist")
        .description("Answers questions about monthly budgets and cutting spending.")
        .model(OpenAiModel::chat("gpt-5-mini").build()?)
        .instructions("You are a budgeting specialist. Give concrete, friendly advice.")
        .build();

    let investing = LlmAgent::builder::<()>()
        .name("investing specialist")
        .description("Answers questions about investing, portfolios, and retirement.")
        .model(OpenAiModel::chat("gpt-5-mini").build()?)
        .instructions("You are an investing specialist. Give concrete, prudent advice.")
        .build();

    let triage = LlmAgent::builder::<()>()
        .name("triage")
        .model(OpenAiModel::chat("gpt-5-mini").build()?)
        .instructions(
            "Classify the user's personal-finance question and transfer to the right \
             specialist. Do not answer yourself — always hand off.",
        )
        .handoffs([Handoff::to(budgeting), Handoff::to(investing)])
        .build();

    let ctx: RunContext<()> = RunContext::new(
        Arc::new(()),
        Arc::new(MemorySession::new()),
        HookRegistry::<()>::new(),
        TracerHandle::default(),
        CancellationToken::new(),
    );

    let input = AgentInput::from_user_text("How should I start investing $5,000?");

    // With handoffs the terminal agent is dynamic, so consume as a string
    // (see the spec's post-handoff output-type contract).
    let stream = triage.run(ctx, input).await?;
    let result = RunResultStreaming::new(stream).collect().await?;

    println!("{}", result.final_output);
    Ok(())
}
