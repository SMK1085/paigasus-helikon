//! Graph example (SMA-333): a diamond-shaped agent graph where `spending`
//! and `income` nodes analyze a household's finances in parallel and fan
//! into a `summary` sink that combines both into one report.
//!
//! ```text
//! OPENAI_API_KEY=sk-… cargo run -p paigasus-helikon \
//!     --features openai --example graph_report
//! ```

use paigasus_helikon::core::{
    Agent, AgentInput, GraphAgent, LlmAgent, RunContext, RunResultStreaming,
};
use paigasus_helikon::openai::OpenAiModel;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let spending = LlmAgent::builder::<()>()
        .name("spending")
        .description("Summarizes the household's monthly spending.")
        .model(OpenAiModel::chat("gpt-5-mini").build()?)
        .instructions("Summarize the spending side of the user's finances in one or two sentences.")
        .build();

    let income = LlmAgent::builder::<()>()
        .name("income")
        .description("Summarizes the household's monthly income.")
        .model(OpenAiModel::chat("gpt-5-mini").build()?)
        .instructions("Summarize the income side of the user's finances in one or two sentences.")
        .build();

    let summary = LlmAgent::builder::<()>()
        .name("summary")
        .description("Combines the spending and income summaries into one report.")
        .model(OpenAiModel::chat("gpt-5-mini").build()?)
        .instructions(
            "Combine the spending and income summaries you were given into one \
             concise financial report.",
        )
        .build();

    let graph = GraphAgent::builder()
        .name("finance_report")
        .description("Fans spending and income analysis into a combined summary.")
        .node("spending", spending)
        .node("income", income)
        .node("summary", summary)
        .edge("spending", "summary")
        .edge("income", "summary")
        .build()?;

    let ctx: RunContext<()> = RunContext::ephemeral(());
    let input = AgentInput::from_user_text(
        "I earn $6,000/month and spend $4,500/month, mostly on rent and groceries.",
    );

    // Single sink ("summary"): the graph's final_output carries its text verbatim.
    let stream = graph.run(ctx, input).await?;
    let result = RunResultStreaming::new(stream).collect().await?;

    println!("{}", result.final_output);
    Ok(())
}
