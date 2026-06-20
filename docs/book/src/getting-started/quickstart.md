# Quickstart

A complete, copy-pasteable agent: a personal-finance **budgeting assistant** that calls two tools to look up the user's spending and budget, then advises in plain text. It exercises the tool-calling loop end-to-end against OpenAI.

## 1. Add the dependency

```toml
[dependencies]
paigasus-helikon = { version = "0.3", features = ["openai", "macros"] }
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
anyhow = "1"
serde = { version = "1", features = ["derive"] }
schemars = "1"
```

The `openai` feature pulls in `paigasus_helikon::openai::OpenAiModel`; the `macros` feature pulls in the `#[tool]` attribute and the `tools!` macro. `paigasus_helikon::core` is always available.

## 2. Set your API key

The OpenAI adapter reads the key from the environment.

```bash
export OPENAI_API_KEY=sk-...
```

## 3. Write `main.rs`

Each `#[tool]` function takes a `&ToolContext<()>` and a single `Deserialize + JsonSchema` argument struct, and returns `Result<T, ToolError>` where `T` is `Serialize + JsonSchema`. The doc comment on the function becomes the tool description the model sees; the doc comments on the argument fields become the JSON-Schema field descriptions.

```rust
use paigasus_helikon::core::{
    Agent, AgentInput, LlmAgent, RunContext,
    RunResultStreaming, ToolContext, ToolError,
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
        "dining" => LookupSpendingOut {
            total: 312.40,
            count: 18,
        },
        "groceries" => LookupSpendingOut {
            total: 540.10,
            count: 9,
        },
        _ => LookupSpendingOut {
            total: 0.0,
            count: 0,
        },
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
        "dining" => BudgetStatusOut {
            budget: 250.0,
            spent: 312.40,
            remaining: -62.40,
        },
        "groceries" => BudgetStatusOut {
            budget: 600.0,
            spent: 540.10,
            remaining: 59.90,
        },
        _ => BudgetStatusOut {
            budget: 0.0,
            spent: 0.0,
            remaining: 0.0,
        },
    };
    Ok(out)
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let model = OpenAiModel::chat("gpt-5-mini").build()?; // ← provider-specific line

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

    let ctx: RunContext<()> = RunContext::ephemeral(());

    let input = AgentInput::from_user_text("How am I doing on my dining budget this month?");

    let stream = agent.run(ctx, input).await?;
    let result = RunResultStreaming::new(stream).collect().await?;

    println!("{}", result.final_output);
    Ok(())
}
```

What each piece does:

- `OpenAiModel::chat("gpt-5-mini").build()?` constructs the model adapter. The id is an example — swap it for any available model if the API rejects it.
- `LlmAgent::builder::<()>()` opens the agent builder; the `()` type parameter is the per-run context state (here, the unit type — no shared state). `.name`, `.model`, `.instructions`, and `.tools(tools![...])` configure it; `.build()` finalizes.
- `RunContext::ephemeral(())` is the one-liner for the all-defaults case: an in-memory `MemorySession`, an empty `HookRegistry`, a default `TracerHandle`, and a fresh `CancellationToken`. Use consuming setters (`.with_session(...)`, `.with_hooks(...)`, `.with_tracer(...)`, `.with_cancel(...)`) to override individual parts.
- `AgentInput::from_user_text(...)` builds the initial user turn.
- `agent.run(ctx, input).await?` returns an event stream; `RunResultStreaming::new(stream).collect().await?` drains it to a final result. `result.final_output` is the agent's plain-text answer.

## 4. Run it

```bash
OPENAI_API_KEY=sk-... cargo run --features openai,macros
```

The same code ships as a runnable example in the workspace:

```bash
OPENAI_API_KEY=sk-... cargo run -p paigasus-helikon \
    --features openai,macros --example budget_assistant_openai
```

## Switching providers

The provider lives entirely in the one model-construction line. To run the identical agent against Anthropic, enable the `anthropic` feature and swap:

```rust
use paigasus_helikon::anthropic::AnthropicModel;

let model = AnthropicModel::messages("claude-sonnet-4-6").build()?;
```

Everything downstream — the `#[tool]` functions, the builder, `RunContext`, the stream collection — is unchanged.

## Next steps

- [The agent loop](../concepts/agent-loop.md) — how `run` drives model calls, tool dispatch, and turn assembly.
- [Tools](../concepts/tools.md) — the `#[tool]` attribute, the `tools!` macro, and `ToolContext` in depth.
