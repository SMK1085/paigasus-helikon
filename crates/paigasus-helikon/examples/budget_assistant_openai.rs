//! Tool-using example (SMA-323): a budgeting assistant that calls tools to
//! look up the user's spending and budget, then advises in plain text.
//!
//! Exercises the tool-calling loop end-to-end. Its sibling
//! `budget_assistant_anthropic.rs` is identical except the model line — that
//! one-line diff is the provider-switching proof.
//!
//! ```text
//! OPENAI_API_KEY=sk-… cargo run -p paigasus-helikon \
//!     --features openai,macros --example budget_assistant_openai
//! ```
//!
//! The model id (`gpt-5-mini`) — swap it for any available model if the API
//! rejects it.

use std::sync::Arc;

use paigasus_helikon::core::{
    Agent, AgentInput, CancellationToken, HookRegistry, LlmAgent, MemorySession, RunContext,
    RunResultStreaming, ToolContext, ToolError, TracerHandle,
};
use paigasus_helikon::openai::OpenAiModel;
use paigasus_helikon::{tool, tools};

#[derive(serde::Deserialize, schemars::JsonSchema)]
struct LookupSpendingArgs {
    /// Spending category, e.g. "Dining".
    category: String,
    /// Month in YYYY-MM form.
    month: String,
}

#[derive(serde::Serialize, schemars::JsonSchema)]
struct LookupSpendingOut {
    /// Total spent in the category this month, in dollars.
    total: f64,
    /// Number of transactions.
    count: u32,
}

/// Look up the user's total spending and transaction count for a category in a month.
#[tool]
async fn lookup_spending(
    _ctx: &ToolContext<()>,
    args: LookupSpendingArgs,
) -> Result<LookupSpendingOut, ToolError> {
    // Read `month` so the field is not flagged unused; the canned ledger ignores it.
    let _ = args.month;
    let out = match args.category.to_lowercase().as_str() {
        "dining" => LookupSpendingOut { total: 312.40, count: 18 },
        "groceries" => LookupSpendingOut { total: 540.10, count: 9 },
        _ => LookupSpendingOut { total: 0.0, count: 0 },
    };
    Ok(out)
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
struct BudgetStatusArgs {
    /// Spending category, e.g. "Dining".
    category: String,
}

#[derive(serde::Serialize, schemars::JsonSchema)]
struct BudgetStatusOut {
    /// Monthly budget for the category, in dollars.
    budget: f64,
    /// Amount spent so far, in dollars.
    spent: f64,
    /// Remaining budget (negative = over budget).
    remaining: f64,
}

/// Look up the user's monthly budget, amount spent, and remaining balance for a category.
#[tool]
async fn budget_status(
    _ctx: &ToolContext<()>,
    args: BudgetStatusArgs,
) -> Result<BudgetStatusOut, ToolError> {
    let out = match args.category.to_lowercase().as_str() {
        "dining" => BudgetStatusOut { budget: 250.0, spent: 312.40, remaining: -62.40 },
        "groceries" => BudgetStatusOut { budget: 600.0, spent: 540.10, remaining: 59.90 },
        _ => BudgetStatusOut { budget: 0.0, spent: 0.0, remaining: 0.0 },
    };
    Ok(out)
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let model = OpenAiModel::chat("gpt-5-mini").build()?; // ⇐ only line that differs vs budget_assistant_anthropic.rs

    let agent = LlmAgent::builder::<()>()
        .name("budget-assistant")
        .model(model)
        .instructions(
            "You are a budgeting assistant. Use the tools to look up the user's spending \
             and budget for the relevant category, then tell them whether they are on \
             track and suggest one concrete action.",
        )
        .tools(tools![lookup_spending, budget_status])
        .build();

    let ctx: RunContext<()> = RunContext::new(
        Arc::new(()),
        Arc::new(MemorySession::new()),
        HookRegistry::<()>::new(),
        TracerHandle::default(),
        CancellationToken::new(),
    );

    let input = AgentInput::from_user_text("How am I doing on my dining budget this month?");

    let stream = agent.run(ctx, input).await?;
    let result = RunResultStreaming::new(stream).collect().await?;

    println!("{}", result.final_output);
    Ok(())
}
