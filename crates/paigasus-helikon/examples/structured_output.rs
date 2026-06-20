//! Structured-output example (SMA-323): a personal-finance assistant that
//! categorizes a transaction into a typed struct via `output_type` +
//! `collect_typed`.
//!
//! ```text
//! ANTHROPIC_API_KEY=sk-… cargo run -p paigasus-helikon \
//!     --features anthropic --example structured_output
//! ```
//!
//! The model id (`claude-sonnet-4-6`) is current as of writing; swap it for
//! any available model if the API rejects it.

use paigasus_helikon::anthropic::AnthropicModel;
use paigasus_helikon::core::{Agent, AgentInput, LlmAgent, RunContext, RunResultStreaming};

#[allow(dead_code)]
#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct TransactionCategory {
    /// Spending category, e.g. "Groceries", "Dining", "Transport", "Entertainment".
    category: String,
    /// 0.0–1.0 confidence in the category.
    confidence: f32,
    /// True if this looks like a recurring charge (subscription, utility, rent).
    recurring: bool,
    /// One-sentence justification.
    reasoning: String,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let model = AnthropicModel::messages("claude-sonnet-4-6").build()?;

    let agent = LlmAgent::builder::<()>()
        .name("transaction-categorizer")
        .model(model)
        .instructions(
            "You are a personal-finance assistant. Categorize the transaction into a \
             single spending category, say whether it looks like a recurring charge, \
             and express your confidence as a number between 0.0 and 1.0.",
        )
        .output_type::<TransactionCategory>()
        .build();

    let ctx: RunContext<()> = RunContext::ephemeral(());

    let input = AgentInput::from_user_text("NETFLIX.COM 866-579-7172 CA — $15.49");

    let stream = agent.run(ctx, input).await?;
    let result = RunResultStreaming::new(stream)
        .collect_typed::<TransactionCategory>()
        .await?;

    println!("{:#?}", result.final_output);
    Ok(())
}
