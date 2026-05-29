//! Structured-output example (SMA-320): a classifier that returns a typed
//! struct directly. Run with an Anthropic key:
//!
//! ```text
//! ANTHROPIC_API_KEY=sk-… cargo run -p paigasus-helikon \
//!     --features anthropic --example leukemia_classifier
//! ```

use std::sync::Arc;

use paigasus_helikon::anthropic::AnthropicModel;
use paigasus_helikon::core::{
    Agent, AgentInput, CancellationToken, HookRegistry, LlmAgent, MemorySession, RunContext,
    RunResultStreaming, TracerHandle,
};

#[allow(dead_code)]
#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct LeukemiaSubtypeAnalysis {
    /// e.g. "AML", "ALL", "CLL", "CML".
    subtype: String,
    /// 0–100.
    confidence: u32,
    /// One-sentence rationale.
    rationale: String,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let model = AnthropicModel::messages("claude-sonnet-4-6").build()?;

    let agent = LlmAgent::builder::<()>()
        .name("leukemia-classifier")
        .model(model)
        .instructions("You are a hematopathology assistant. Classify the leukemia subtype.")
        .output_type::<LeukemiaSubtypeAnalysis>()
        .build();

    let ctx: RunContext<()> = RunContext::new(
        Arc::new(()),
        Arc::new(MemorySession::new()),
        HookRegistry::<()>::new(),
        TracerHandle::default(),
        CancellationToken::new(),
    );

    let input =
        AgentInput::from_user_text("Flow: CD13+ CD33+ CD34+ MPO+. Blasts 80%. Auer rods present.");

    let stream = agent.run(ctx, input).await?;
    let result = RunResultStreaming::new(stream)
        .collect_typed::<LeukemiaSubtypeAnalysis>()
        .await?;

    println!("{:#?}", result.final_output);
    Ok(())
}
