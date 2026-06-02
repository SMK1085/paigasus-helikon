# SMA-323 Parity Examples + Dispatch Benchmark — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship four runnable personal-finance example agents (structured output, two provider-switching tool-using agents, token streaming) plus a Criterion tool-dispatch microbenchmark, all consuming only the public API — no `-core`/provider source changes.

**Architecture:** All artifacts live in the facade crate `crates/paigasus-helikon/`. Examples drive the agent loop via the bare `agent.run(ctx, input)` entrypoint (the core loop driver runs tools without a runtime crate). The benchmark measures `Tool::invoke` dispatch through a `dyn Tool` vtable using Criterion's async support to isolate dispatch from runtime entry. A manual `workflow_dispatch` CI job produces the authoritative Linux x86_64 number.

**Tech Stack:** Rust, `paigasus-helikon` facade (re-exports `core`, `openai`, `anthropic`, `tool`/`tools` macros, `runtime_tokio`), `tokio`, `criterion` 0.8 (`async_tokio`), `serde`/`schemars`.

**Spec:** [`docs/superpowers/specs/2026-06-01-sma-323-parity-examples-dispatch-bench-design.md`](../specs/2026-06-01-sma-323-parity-examples-dispatch-bench-design.md)

---

## Conventions for every task

- **Branch:** already on `feature/sma-323-side-by-side-parity-examples-dispatch-benchmark`.
- **Commit scopes** must match `.versionrc` `scopeRegex`. There is **no `examples` or `bench` scope** — use `facade` (these targets belong to the facade crate). Example/bench/doc commits use **non-`feat`** types (`docs`/`test`/`ci` → `increment: None`) so release-plz never bumps the facade for non-API work.
- **Never `git add -A`** — `.env` and `.claude/` are untracked-but-not-ignored. Stage explicit paths only.
- **Commits are signed** via a 1Password SSH key. If a commit fails with "failed to fill whole buffer", the vault is locked — stop and ask the user to unlock, then retry. Do not bypass signing.
- **"Tests" for examples = compilation + clippy.** Examples are integration/manual artifacts (the underlying loop and dispatch are already unit-tested in `core`); their gate is "compiles clean under the required features" plus a manual end-to-end run with real API keys (final task). The benchmark's gate is "compiles and `cargo bench` prints a number < 50 µs". This plan does not fabricate unit tests for example glue code.
- **Local toolchain note:** `criterion` 0.8's MSRV is 1.86. Building the bench (via `cargo bench` or `cargo clippy --all-targets`) needs a local **stable ≥ 1.86**. The `[[bench]] test = false` setting (Task 5) keeps the workspace MSRV-1.75 promise intact because the 1.75 CI job (`cargo test`) never compiles the bench; clippy `--all-targets` on stable does.

---

## File map

| Path | Action | Responsibility |
|---|---|---|
| `crates/paigasus-helikon/examples/leukemia_classifier.rs` | rename → `structured_output.rs` | structured-output demo, re-domained to finance |
| `crates/paigasus-helikon/examples/structured_output.rs` | create (via rename) | `output_type::<TransactionCategory>()` + `collect_typed` |
| `crates/paigasus-helikon/examples/budget_assistant_openai.rs` | create | tool-using agent on OpenAI |
| `crates/paigasus-helikon/examples/budget_assistant_anthropic.rs` | create | same agent on Anthropic (one-line diff) |
| `crates/paigasus-helikon/examples/streaming_console.rs` | create | token-by-token streaming to stdout |
| `crates/paigasus-helikon/benches/tool_dispatch.rs` | create | Criterion dispatch microbench |
| `crates/paigasus-helikon/Cargo.toml` | modify | rename example entry, add 3 examples + bench + criterion dev-dep |
| `Cargo.toml` (root) | modify | add `criterion` to `[workspace.dependencies]` |
| `BENCHMARKS.md` | create | bench methodology + recorded number |
| `.github/workflows/bench.yml` | create | manual `workflow_dispatch` ubuntu bench job |

---

## Task 1: Re-domain + rename the structured-output example

**Files:**
- Rename: `crates/paigasus-helikon/examples/leukemia_classifier.rs` → `crates/paigasus-helikon/examples/structured_output.rs`
- Modify: `crates/paigasus-helikon/Cargo.toml` (the `[[example]]` entry)

- [ ] **Step 1: Rename the file with git**

Run:
```bash
git mv crates/paigasus-helikon/examples/leukemia_classifier.rs \
       crates/paigasus-helikon/examples/structured_output.rs
```

- [ ] **Step 2: Replace the file contents (re-domain leukemia → transaction)**

Overwrite `crates/paigasus-helikon/examples/structured_output.rs` with:

```rust
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

use std::sync::Arc;

use paigasus_helikon::anthropic::AnthropicModel;
use paigasus_helikon::core::{
    Agent, AgentInput, CancellationToken, HookRegistry, LlmAgent, MemorySession, RunContext,
    RunResultStreaming, TracerHandle,
};

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
             single spending category and say whether it looks like a recurring charge.",
        )
        .output_type::<TransactionCategory>()
        .build();

    let ctx: RunContext<()> = RunContext::new(
        Arc::new(()),
        Arc::new(MemorySession::new()),
        HookRegistry::<()>::new(),
        TracerHandle::default(),
        CancellationToken::new(),
    );

    let input = AgentInput::from_user_text("NETFLIX.COM 866-579-7172 CA — $15.49");

    let stream = agent.run(ctx, input).await?;
    let result = RunResultStreaming::new(stream)
        .collect_typed::<TransactionCategory>()
        .await?;

    println!("{:#?}", result.final_output);
    Ok(())
}
```

- [ ] **Step 3: Rename the `[[example]]` entry in `crates/paigasus-helikon/Cargo.toml`**

Find:
```toml
[[example]]
name              = "leukemia_classifier"
required-features = ["anthropic"]
```
Replace with:
```toml
[[example]]
name              = "structured_output"
required-features = ["anthropic"]
```

- [ ] **Step 4: Verify it compiles**

Run:
```bash
cargo build -p paigasus-helikon --features anthropic --example structured_output
```
Expected: builds successfully (no output binary run; this is env-gated).

- [ ] **Step 5: Clippy-clean check**

Run:
```bash
cargo clippy -p paigasus-helikon --features anthropic --example structured_output -- -D warnings
```
Expected: no warnings.

- [ ] **Step 6: Commit**

```bash
git add crates/paigasus-helikon/examples/structured_output.rs \
        crates/paigasus-helikon/Cargo.toml
git commit -m "docs(facade): SMA-323 re-domain structured_output example to finance

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

## Task 2: `budget_assistant_openai.rs` — tool-using agent on OpenAI

**Files:**
- Create: `crates/paigasus-helikon/examples/budget_assistant_openai.rs`
- Modify: `crates/paigasus-helikon/Cargo.toml` (add `[[example]]`)

- [ ] **Step 1: Write the example**

Create `crates/paigasus-helikon/examples/budget_assistant_openai.rs`:

```rust
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
```

> Note for the implementer: the `#[tool]` macro emits fully-qualified `::paigasus_helikon_core::…` paths via `proc-macro-crate`, so the `Tool` trait is referenced without an explicit `use`. Do **not** add `use …::Tool;` — it would be an unused import and fail `-D warnings`. `ToolContext`/`ToolError` ARE used (in the fn signatures), so they stay imported.

- [ ] **Step 2: Add the `[[example]]` entry**

Append to `crates/paigasus-helikon/Cargo.toml` after the `structured_output` example entry:
```toml
[[example]]
name              = "budget_assistant_openai"
required-features = ["openai", "macros"]
```

- [ ] **Step 3: Verify it compiles**

Run:
```bash
cargo build -p paigasus-helikon --features openai,macros --example budget_assistant_openai
```
Expected: builds successfully.

- [ ] **Step 4: Clippy-clean check**

Run:
```bash
cargo clippy -p paigasus-helikon --features openai,macros --example budget_assistant_openai -- -D warnings
```
Expected: no warnings. (If a struct field trips `dead_code`, read it once like the `let _ = args.month;` line.)

- [ ] **Step 5: Commit**

```bash
git add crates/paigasus-helikon/examples/budget_assistant_openai.rs \
        crates/paigasus-helikon/Cargo.toml
git commit -m "docs(facade): SMA-323 add budget_assistant_openai example

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

## Task 3: `budget_assistant_anthropic.rs` — same agent, Anthropic

**Files:**
- Create: `crates/paigasus-helikon/examples/budget_assistant_anthropic.rs`
- Modify: `crates/paigasus-helikon/Cargo.toml` (add `[[example]]`)

- [ ] **Step 1: Write the example (identical to Task 2 except the import + model line + header)**

Create `crates/paigasus-helikon/examples/budget_assistant_anthropic.rs`:

```rust
//! Tool-using example (SMA-323): the SAME budgeting assistant as
//! `budget_assistant_openai.rs`, on Anthropic. The only logic-relevant
//! difference is the model-construction line — the provider-switching proof.
//!
//! ```text
//! ANTHROPIC_API_KEY=sk-… cargo run -p paigasus-helikon \
//!     --features anthropic,macros --example budget_assistant_anthropic
//! ```
//!
//! The model id (`claude-sonnet-4-6`) — swap it for any available model if
//! the API rejects it.

use std::sync::Arc;

use paigasus_helikon::anthropic::AnthropicModel;
use paigasus_helikon::core::{
    Agent, AgentInput, CancellationToken, HookRegistry, LlmAgent, MemorySession, RunContext,
    RunResultStreaming, ToolContext, ToolError, TracerHandle,
};
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
    let model = AnthropicModel::messages("claude-sonnet-4-6").build()?; // ⇐ only line that differs vs budget_assistant_openai.rs

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
```

- [ ] **Step 2: Add the `[[example]]` entry**

Append to `crates/paigasus-helikon/Cargo.toml`:
```toml
[[example]]
name              = "budget_assistant_anthropic"
required-features = ["anthropic", "macros"]
```

- [ ] **Step 3: Verify it compiles**

Run:
```bash
cargo build -p paigasus-helikon --features anthropic,macros --example budget_assistant_anthropic
```
Expected: builds successfully.

- [ ] **Step 4: Clippy-clean check**

Run:
```bash
cargo clippy -p paigasus-helikon --features anthropic,macros --example budget_assistant_anthropic -- -D warnings
```
Expected: no warnings.

- [ ] **Step 5: Confirm the one-line-diff claim**

Run:
```bash
diff crates/paigasus-helikon/examples/budget_assistant_openai.rs \
     crates/paigasus-helikon/examples/budget_assistant_anthropic.rs
```
Expected: only the header doc lines, the `use …::{OpenAiModel|AnthropicModel}` import, and the `let model = …` line differ. If anything else differs, reconcile so the agent/tool bodies are byte-identical.

- [ ] **Step 6: Commit**

```bash
git add crates/paigasus-helikon/examples/budget_assistant_anthropic.rs \
        crates/paigasus-helikon/Cargo.toml
git commit -m "docs(facade): SMA-323 add budget_assistant_anthropic example

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

## Task 4: `streaming_console.rs` — token-by-token to stdout

**Files:**
- Create: `crates/paigasus-helikon/examples/streaming_console.rs`
- Modify: `crates/paigasus-helikon/Cargo.toml` (add `[[example]]`)

- [ ] **Step 1: Write the example**

Create `crates/paigasus-helikon/examples/streaming_console.rs`:

```rust
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
```

- [ ] **Step 2: Add the `[[example]]` entry**

Append to `crates/paigasus-helikon/Cargo.toml`:
```toml
[[example]]
name              = "streaming_console"
required-features = ["openai"]
```

- [ ] **Step 3: Verify it compiles**

Run:
```bash
cargo build -p paigasus-helikon --features openai --example streaming_console
```
Expected: builds successfully.

- [ ] **Step 4: Clippy-clean check**

Run:
```bash
cargo clippy -p paigasus-helikon --features openai --example streaming_console -- -D warnings
```
Expected: no warnings.

- [ ] **Step 5: Commit**

```bash
git add crates/paigasus-helikon/examples/streaming_console.rs \
        crates/paigasus-helikon/Cargo.toml
git commit -m "docs(facade): SMA-323 add streaming_console example

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

## Task 5: `benches/tool_dispatch.rs` — Criterion dispatch microbench

> **⚠️ SUPERSEDED during implementation — the bench is dependency-free, NOT Criterion.**
> Criterion transitively pulls `clap_lex 1.1.0` (`edition = "2024"`), which Cargo 1.75
> cannot parse, breaking the workspace's Rust 1.75 MSRV at resolution time (a failure
> `[[bench]] test = false` cannot avoid). The shipped bench is a hand-rolled `fn main()`
> timing loop with **no new dependencies**: a single `rt.block_on` wraps warmup + measured
> loops (amortizing runtime entry, the same property `to_async` would give), timed with
> `std::time::Instant`, `assert!`ing `< 50 µs`. The Criterion-specific steps below
> (workspace dep, `async_tokio`, `to_async`, the MSRV-1.86/`test = false` rationale) do NOT
> apply. See the **updated spec, Deliverable 5** for the authoritative design. The
> remaining steps' *shape* (SumTool, registry lookup, build/clippy/fmt/commit) still holds.

**Files:**
- Modify: `Cargo.toml` (root — add `criterion` to `[workspace.dependencies]`)
- Modify: `crates/paigasus-helikon/Cargo.toml` (add `criterion` dev-dep + `[[bench]]`)
- Create: `crates/paigasus-helikon/benches/tool_dispatch.rs`

- [ ] **Step 1: Add `criterion` to root `[workspace.dependencies]`**

In the root `Cargo.toml`, under `[workspace.dependencies]` (keep the file's alignment style), add:
```toml
criterion = { version = "0.8", features = ["async_tokio"] }
```
(Keep criterion's default features — `cargo_bench_support` is required for plain `cargo bench` — and add `async_tokio` for `to_async`.)

- [ ] **Step 2: Add the criterion dev-dep + `[[bench]]` to the facade `Cargo.toml`**

In `crates/paigasus-helikon/Cargo.toml`, add to `[dev-dependencies]`:
```toml
criterion = { workspace = true }
```
Then add a `[[bench]]` target (after the `[[example]]` entries, before `[lints]`):
```toml
[[bench]]
name    = "tool_dispatch"
harness = false
# criterion 0.8 MSRV is 1.86 > workspace MSRV 1.75. `test = false` keeps the
# bench out of `cargo test` (so the `test (…, 1.75)` CI job never compiles it),
# while `cargo clippy --all-targets` (stable) and `cargo bench` still build it.
test    = false
```

- [ ] **Step 3: Write the benchmark**

Create `crates/paigasus-helikon/benches/tool_dispatch.rs`:

```rust
//! Criterion microbench (SMA-323): measure `Tool::invoke` dispatch overhead
//! — registry name-lookup + `dyn Tool` vtable call + JSON-output read.
//!
//! Uses Criterion's async support (`to_async`) rather than a per-iteration
//! `block_on`, so the number reflects the future's poll cost, not runtime
//! entry. Target: < 50 µs.
//!
//! Run: `cargo bench -p paigasus-helikon --bench tool_dispatch`

use std::hint::black_box;
use std::sync::Arc;

use async_trait::async_trait;
use criterion::{criterion_group, criterion_main, Criterion};
use serde_json::{json, Value};

use paigasus_helikon::core::{
    CancellationToken, Tool, ToolContext, ToolError, ToolOutput, TracerHandle,
};

/// A trivial tool: adds two amounts. The body is intentionally cheap so the
/// measurement is dominated by dispatch, not tool work.
struct SumTool {
    schema: Value,
}

#[async_trait]
impl Tool<()> for SumTool {
    fn name(&self) -> &str {
        "sum"
    }
    fn description(&self) -> &str {
        "Adds two amounts."
    }
    fn schema(&self) -> &Value {
        &self.schema
    }
    async fn invoke(&self, _ctx: &ToolContext<()>, args: Value) -> Result<ToolOutput, ToolError> {
        let a = args["a"].as_f64().unwrap_or(0.0);
        let b = args["b"].as_f64().unwrap_or(0.0);
        Ok(ToolOutput::new(json!({ "total": a + b })))
    }
}

fn bench_tool_dispatch(c: &mut Criterion) {
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("build current-thread runtime");

    // Heterogeneous registry, accessed by name — the realistic dispatch path.
    let registry: Vec<Arc<dyn Tool<()>>> =
        vec![Arc::new(SumTool { schema: json!({ "type": "object" }) })];
    let ctx = ToolContext::new(Arc::new(()), TracerHandle::default(), CancellationToken::new());
    let args = json!({ "a": 19.99, "b": 4.50 });

    c.bench_function("tool_dispatch", |b| {
        b.to_async(&rt).iter(|| async {
            let tool = registry
                .iter()
                .find(|t| t.name() == "sum")
                .expect("tool present");
            let out = tool.invoke(&ctx, args.clone()).await.expect("invoke ok");
            black_box(out.content);
        });
    });
}

criterion_group!(benches, bench_tool_dispatch);
criterion_main!(benches);
```

- [ ] **Step 4: Run the benchmark and confirm it builds + meets target**

Run:
```bash
cargo bench -p paigasus-helikon --bench tool_dispatch
```
Expected: Criterion prints a `tool_dispatch` line with a time well under 50 µs (sub-µs to low-µs). If the local stable toolchain is < 1.86, this fails to compile criterion — install/select stable ≥ 1.86 first (`rustup update stable`).

- [ ] **Step 5: Confirm the 1.75 test job won't touch the bench**

Run:
```bash
cargo +1.75 test -p paigasus-helikon --all-features -- --skip trybuild_ui 2>&1 | tail -5 || true
```
Expected: the bench is NOT among compiled targets (no `criterion` build). If `cargo +1.75` isn't installed, skip — the `test = false` setting guarantees exclusion regardless; this is just a local confirmation.

- [ ] **Step 6: Clippy the bench (stable, all-targets)**

Run:
```bash
cargo clippy -p paigasus-helikon --benches -- -D warnings
```
Expected: no warnings.

- [ ] **Step 7: Commit**

```bash
git add Cargo.toml Cargo.lock crates/paigasus-helikon/Cargo.toml \
        crates/paigasus-helikon/benches/tool_dispatch.rs
git commit -m "test(facade): SMA-323 add tool_dispatch criterion benchmark

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

## Task 6: `BENCHMARKS.md`

**Files:**
- Create: `BENCHMARKS.md` (repo root)

- [ ] **Step 1: Write the doc**

Create `BENCHMARKS.md`:

```markdown
# Benchmarks

## `tool_dispatch` — `Tool::invoke` dispatch overhead

**What it measures.** The hot path an agent takes to invoke a tool: name-lookup
in a `Vec<Arc<dyn Tool<Ctx>>>` registry, the `dyn Tool` vtable call to
`invoke`, awaiting the returned future, and reading the JSON `ToolOutput`.
It uses Criterion's async support (`Bencher::to_async`) so the figure reflects
the future's poll cost, not per-iteration runtime entry (`block_on`).

**What it does NOT measure.** Network/provider latency, model invocation, or the
full agent loop — only `Tool::invoke` dispatch.

**Target.** < 50 µs. Deliberately loose: the lookup + vtable call + JSON read
should cost on the order of sub-µs, so 50 µs is ~50× headroom — only a
pathological regression trips it. This is a guard, not a tracked SLO (there is
no stored Criterion baseline; see SMA-323 spec).

**Run it.**
```bash
cargo bench -p paigasus-helikon --bench tool_dispatch
```
Requires a stable toolchain ≥ 1.86 (criterion 0.8 MSRV); the bench is excluded
from `cargo test` via `[[bench]] test = false`.

## Results

Authoritative numbers are taken on **Linux x86_64** via the manual
`bench.yml` GitHub Actions job (`workflow_dispatch`). Local macOS/arm64 figures
are indicative only.

| Date | Platform | Runner | `tool_dispatch` | Target |
|---|---|---|---|---|
| _pending_ | Linux x86_64 | GitHub `ubuntu-latest` | _fill from bench.yml run_ | < 50 µs |
```

> Implementer: leave the results row as `_pending_` / `_fill from bench.yml run_`. The final task (Task 8) triggers the CI job and the user pastes the real number here.

- [ ] **Step 2: Commit**

```bash
git add BENCHMARKS.md
git commit -m "docs(repo): SMA-323 add BENCHMARKS.md for tool_dispatch

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

## Task 7: `.github/workflows/bench.yml` — manual ubuntu bench job

**Files:**
- Create: `.github/workflows/bench.yml`

- [ ] **Step 1: Confirm the action SHAs (reuse the repo's existing pins)**

The repo already pins these in `.github/workflows/ci.yml`. Reuse the SAME SHAs for consistency (do not introduce new pins):
```bash
grep -nE "uses: (actions/checkout|dtolnay/rust-toolchain|Swatinem/rust-cache)@" .github/workflows/ci.yml
```
Expected (as of this plan): `actions/checkout@de0fac2e4500dabe0009e67214ff5f5447ce83dd` (v6.0.2), `dtolnay/rust-toolchain@3c5f7ea28cd621ae0bf5283f0e981fb97b8a7af9` (master), `Swatinem/rust-cache@c19371144df3bb44fab255c43d04cbc2ab54d1c4` (v2.9.1). If ci.yml shows newer SHAs, use those instead.

- [ ] **Step 2: Write the workflow**

Create `.github/workflows/bench.yml` (substitute the SHAs from Step 1 if they changed):

```yaml
name: bench

on:
  workflow_dispatch:

permissions:
  contents: read

jobs:
  tool-dispatch:
    runs-on: ubuntu-latest
    steps:
      # actions/checkout v6.0.2
      - uses: actions/checkout@de0fac2e4500dabe0009e67214ff5f5447ce83dd
        with:
          persist-credentials: false
      # dtolnay/rust-toolchain master (no tagged releases)
      - uses: dtolnay/rust-toolchain@3c5f7ea28cd621ae0bf5283f0e981fb97b8a7af9
        with:
          toolchain: stable
      # Swatinem/rust-cache v2.9.1
      - uses: Swatinem/rust-cache@c19371144df3bb44fab255c43d04cbc2ab54d1c4
      - name: Run tool-dispatch benchmark
        run: cargo bench -p paigasus-helikon --bench tool_dispatch
```

- [ ] **Step 3: Lint the YAML locally (syntax sanity)**

Run:
```bash
python3 -c "import yaml,sys; yaml.safe_load(open('.github/workflows/bench.yml')); print('yaml ok')"
```
Expected: `yaml ok`.

- [ ] **Step 4: Commit**

```bash
git add .github/workflows/bench.yml
git commit -m "ci(workflows): SMA-323 add manual tool_dispatch bench job

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

## Task 8: Full local CI-gate sweep + manual acceptance handoff

**Files:** none (verification + handoff)

- [ ] **Step 1: Run the fast CI gates locally**

Run:
```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-features --all-targets -- -D warnings
```
Expected: both pass with no output/warnings. (`--all-targets` compiles the four examples and the bench; needs stable ≥ 1.86.)

- [ ] **Step 2: Run the test gate (compiles all examples under all features)**

Run:
```bash
cargo test --workspace --all-features
```
Expected: passes; the four examples compile as part of this (the bench is excluded by `test = false`).

- [ ] **Step 3: Run the docs gate**

Run:
```bash
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps
```
Expected: builds with no warnings.

- [ ] **Step 4: Push the branch**

Run:
```bash
git push -u origin feature/sma-323-side-by-side-parity-examples-dispatch-benchmark
```
(The pre-push hook runs fmt + clippy + convco; it needs stable ≥ 1.86 to build the bench. If the vault is locked and signing fails, ask the user to unlock.)

- [ ] **Step 5: Open the PR**

Run:
```bash
gh pr create \
  --title "docs(facade): SMA-323 add side-by-side parity examples + dispatch benchmark" \
  --body "Implements SMA-323: four personal-finance example agents (structured output, two provider-switching tool-using agents, token streaming) + a Criterion tool-dispatch benchmark. Spec: docs/superpowers/specs/2026-06-01-sma-323-parity-examples-dispatch-bench-design.md. Handoff-topology triage parity is deferred to SMA-324; RunContext ergonomics finding filed as SMA-403.

🤖 Generated with [Claude Code](https://claude.com/claude-code)"
```
> PR-title rules: the `docs(facade):` prefix is a valid Conventional Commit type+scope, and the subject after `SMA-323 ` starts lowercase (`add`). `docs` keeps release-plz from bumping the facade (no public-API change).

- [ ] **Step 6: Manual acceptance — run the examples end-to-end (USER)**

These need real keys and are the human acceptance step (CI only compiles them). Hand off to the user:
```bash
ANTHROPIC_API_KEY=… cargo run -p paigasus-helikon --features anthropic       --example structured_output
OPENAI_API_KEY=…    cargo run -p paigasus-helikon --features openai,macros    --example budget_assistant_openai
ANTHROPIC_API_KEY=… cargo run -p paigasus-helikon --features anthropic,macros --example budget_assistant_anthropic
OPENAI_API_KEY=…    cargo run -p paigasus-helikon --features openai           --example streaming_console
```
If a model id is rejected, swap it for a currently-available one (the header comments note this).

- [ ] **Step 7: Manual acceptance — capture the Linux x86_64 number (USER)**

After the branch/PR is up: trigger the bench job, read the number from the job log, and paste it into the `BENCHMARKS.md` results table (replacing the `_pending_` row), then commit `docs(repo): SMA-323 record tool_dispatch Linux x86_64 number`.
```bash
gh workflow run bench.yml --ref feature/sma-323-side-by-side-parity-examples-dispatch-benchmark
# then: gh run watch <run-id>   (or read the run log in the Actions tab)
```

---

## Self-review (completed against the spec)

- **Spec coverage:** structured_output (Task 1), budget_assistant_openai (Task 2), budget_assistant_anthropic (Task 3), streaming_console (Task 4), bench with `to_async` (Task 5), BENCHMARKS.md (Task 6), bench.yml (Task 7), Cargo wiring (Tasks 1–5), MSRV isolation via `test = false` (Task 5), honest-scope / deferrals reflected in PR body (Task 8). SMA-403 + SMA-324 referenced. ✓
- **Placeholder scan:** the only `_pending_` is the BENCHMARKS.md results cell, which legitimately awaits the CI number (Task 8 fills it). No TODO/TBD in steps. ✓
- **Type consistency:** `LlmAgent::builder::<()>()`, `ToolContext<()>`, `ToolError`, `RunResultStreaming::{new,collect,collect_typed}`, `AgentEvent::TokenDelta { text }`, `ToolOutput::new`, `OpenAiModel::chat(..).build()?`, `AnthropicModel::messages(..).build()?`, `tools![..]` → `.tools(..)` all match the verified `core`/provider/macro signatures. ✓
```
