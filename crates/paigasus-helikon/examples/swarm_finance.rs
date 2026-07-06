//! Swarm example (SMA-333): a personal-finance support swarm with a triage
//! member plus budgeting and investing specialists, wired with full-mesh
//! handoffs via `SwarmAgent`. The swarm converges on a winner — the first
//! member that answers instead of handing off — within the configured
//! handoff budget.
//!
//! ```text
//! OPENAI_API_KEY=sk-… cargo run -p paigasus-helikon \
//!     --features openai --example swarm_finance
//! ```

use paigasus_helikon::core::{
    Agent, AgentInput, LlmAgent, RunContext, RunResultStreaming, SwarmAgent,
};
use paigasus_helikon::openai::OpenAiModel;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let triage = LlmAgent::builder::<()>()
        .name("triage")
        .description("Routes personal-finance questions to the right specialist.")
        .model(OpenAiModel::chat("gpt-5-mini").build()?)
        .instructions(
            "Route to the right specialist via transfer; answer yourself only for \
             trivial questions.",
        )
        .build();

    let budgeting = LlmAgent::builder::<()>()
        .name("budgeting")
        .description("Answers questions about monthly budgets and cutting spending.")
        .model(OpenAiModel::chat("gpt-5-mini").build()?)
        .instructions("You are a budgeting specialist. Give concrete, friendly advice.")
        .build();

    let investing = LlmAgent::builder::<()>()
        .name("investing")
        .description("Answers questions about investing, portfolios, and retirement.")
        .model(OpenAiModel::chat("gpt-5-mini").build()?)
        .instructions("You are an investing specialist. Give concrete, prudent advice.")
        .build();

    let swarm = SwarmAgent::builder()
        .name("support_swarm")
        .description("A personal-finance support swarm covering budgeting and investing.")
        .member(triage)
        .member(budgeting)
        .member(investing)
        .entry("triage")
        .max_handoffs(6)
        .build()?;

    let ctx: RunContext<()> = RunContext::ephemeral(());
    let input = AgentInput::from_user_text("How should I start investing $5,000?");

    let stream = swarm.run(ctx, input).await?;
    let result = RunResultStreaming::new(stream).collect().await?;

    println!("{}", result.final_output);
    Ok(())
}
