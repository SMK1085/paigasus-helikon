# Structured Output & Builder

`LlmAgent` is constructed through a typestate builder. The same builder also
configures **structured output**: ask the agent to return a typed Rust value
instead of free text by deriving a schema for your output type and reading it
back off the result.

## The typestate builder

`LlmAgent::builder::<Ctx>()` returns an `LlmAgentBuilder`. Two setters are
**required** before `.build()` is in scope: `.name(…)` and `.model(…)`. The
builder tracks these with typestate markers (`NoName`/`HasName`,
`NoModel`/`HasModel`); `.build()` only exists on the
`HasName, HasModel` state, so forgetting either is a compile error, not a
runtime panic.

The setters:

| Method | Purpose |
| --- | --- |
| `.name(impl Into<String>)` | Agent name. Required; transitions to `HasName`. |
| `.model(m)` / `.shared_model(Arc<M>)` | The `Model` impl. Required; transitions to `HasModel`. |
| `.instructions(i)` / `.shared_instructions(Arc<…>)` | System-prompt renderer. |
| `.description(impl Into<String>)` | Human-readable description; used by handoff routing. |
| `.tool(t)` / `.shared_tool(Arc<…>)` / `.tools(iter)` | Tool registry (append vs. replace). |
| `.handoff(a)` / `.shared_handoff(Arc<…>)` / `.handoffs(iter)` | Handoff candidates. |
| `.hook(h)` / `.hooks(iter)` | Lifecycle hooks. |
| `.input_guardrail(g)` / `.output_guardrail(g)` (+ `shared_*` / plural) | Guardrails. |
| `.model_settings(ModelSettings)` | Per-call provider knobs. |
| `.max_turns(u32)` | Per-run turn budget. |
| `.output_type::<T>()` | Switch the output type to `T` (see below). |
| `.build()` | Finalize into `LlmAgent<Ctx, M, T>`. Only on `HasName, HasModel`. |

The `*` family follows a consistent convention: the singular form
(`.tool`, `.hook`, `.handoff`, …) **appends** and wraps an owned value in an
`Arc`; the `shared_*` form takes a pre-wrapped `Arc` without re-wrapping; the
plural form (`.tools`, `.hooks`, …) **replaces** the whole list from an
`IntoIterator`. See [Core Primitives](./core-primitives.md) for `Tool`,
`Handoff`, and `Hook`; [Tools](./tools.md) for the `tools![…]` macro that feeds
`.tools(…)`.

## Typed output: `output_type` + `collect_typed`

A default agent returns text: `RunResultStreaming::collect()` yields a
`RunResult<String>`. To get a typed value instead:

1. Define an output struct that derives `serde::Deserialize` and
   `schemars::JsonSchema`.
2. Call `.output_type::<T>()` on the builder. `T` is inferred into the
   `LlmAgent<Ctx, M, T>` type parameter, and the builder captures
   `T`'s JSON Schema.
3. Drain the run with `.collect_typed::<T>()` instead of `.collect()`.
   `result.final_output` is then a `T`.

`output_type` is repeatable and any-state — the last call wins, and it does not
disturb the `HasName`/`HasModel` markers.

```rust
use std::sync::Arc;

use paigasus_helikon::anthropic::AnthropicModel;
use paigasus_helikon::core::{
    Agent, AgentInput, CancellationToken, HookRegistry, LlmAgent, MemorySession,
    RunContext, RunResultStreaming, TracerHandle,
};

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct TransactionCategory {
    /// Spending category, e.g. "Groceries", "Dining", "Transport".
    category: String,
    /// 0.0–1.0 confidence in the category.
    confidence: f32,
    /// True if this looks like a recurring charge.
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
            "You are a personal-finance assistant. Categorize the transaction \
             into a single spending category, say whether it looks recurring, \
             and express your confidence as a number between 0.0 and 1.0.",
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

    // result.final_output is a TransactionCategory.
    println!("{:#?}", result.final_output);
    Ok(())
}
```

The doc-comments on each field flow into the schema (schemars maps them to
`description`), so they double as guidance to the model. This example is the
shipped `structured_output` example; run it with
`cargo run -p paigasus-helikon --features anthropic --example structured_output`.

## How it works under the hood

`.output_type::<T>()` stores an `OutputType` built by
`OutputType::from_schema::<T>()`. That carrier holds three things: the schema
name (derived from the schema `title`, defaulting to `"StructuredOutput"`),
`T`'s raw `schemars::Schema`, and a validator closure that proves a JSON value
deserializes back into the original `T`.

On a finalizing turn, the agent loop derives a `ResponseFormat::JsonSchema`
from the `OutputType` — the schema, its name, and `strict: true` — and sets it
as the request's `ModelSettings::response_format`. `ResponseFormat` is the
provider-neutral knob (with `Text` and `JsonObject` variants for the looser
cases); each provider maps it onto its native structured-output shape. The
model's terminal text is validated against the captured validator; if it does
not parse or does not match, the loop synthesizes a repair instruction and asks
the model again before surfacing failure.

`collect_typed::<T>()` then deserializes the terminal assistant text into `T`.
If a structured run fails validation, the error surfaces as
`AgentError::InvalidStructuredOutput` carrying the schema errors and the
offending text; calling `collect_typed` on a plain-text run (or any other parse
failure) surfaces `AgentError::Other`.

### Strict-mode schema normalization

OpenAI's strict structured-output mode imposes extra constraints on the schema:
every object needs `additionalProperties: false`, and every property must be
listed in `required` (optional fields use a nullable type rather than absence).
schemars output does not satisfy this by default, so the OpenAI provider runs
the schema through `paigasus_helikon::schema::strict` — the canonical
strict-mode normalizer, re-exported from
`paigasus_helikon_core::schema::strict`. Anthropic uses schemas as-is. You can
call the normalizer directly if you assemble a `ResponseFormat::JsonSchema` by
hand:

```rust
use paigasus_helikon::schema::strict;

let normalized = strict(&raw_schema); // raw_schema: &serde_json::Value
```

Note one current limitation: the normalizer does not traverse `$defs`/`$ref`.
schemars emits `$defs` for enums, recursive types, and types referenced more
than once, so schemas with those constructs are not fully rewritten — flat and
nested struct outputs are the well-supported shape today.

See [Model Providers](./model-providers.md) for how each provider maps
`ResponseFormat`, [The Agent Loop](./agent-loop.md) for the run lifecycle, and
the [API docs](../reference/api-docs.md) for the full `LlmAgentBuilder`,
`OutputType`, and `RunResultStreaming` signatures.
