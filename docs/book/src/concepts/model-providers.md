# Model Providers

Every LLM in Helikon sits behind one trait: `Model`, from `paigasus-helikon-core`.
There are no per-provider traits. Capability differences (streaming, tool calling,
structured output, vision, prompt caching, …) are surfaced through a flag struct,
`ModelCapabilities`, rather than split interfaces. Two adapters ship today —
`OpenAiModel` and `AnthropicModel` — and switching between them is a single line.

## The `Model` trait

```rust
use async_trait::async_trait;
use futures_core::stream::BoxStream;
use paigasus_helikon::core::{
    CancellationToken, Model, ModelCapabilities, ModelError, ModelEvent, ModelRequest,
};

#[async_trait]
pub trait Model: Send + Sync {
    async fn invoke(
        &self,
        request: ModelRequest,
        cancel: CancellationToken,
    ) -> Result<BoxStream<'static, Result<ModelEvent, ModelError>>, ModelError>;

    fn capabilities(&self) -> ModelCapabilities;

    fn provider(&self) -> &str; // GenAI `gen_ai.provider.name`, e.g. "openai"
    fn model(&self) -> &str;    // GenAI `gen_ai.request.model`, e.g. "gpt-4o"
}
```

`invoke` and `capabilities` are the only methods an implementor must define.
`provider` and `model` are provided methods with defaults (`"unknown"` and `""`
respectively); the shipped adapters override them so traces carry the real
provider and model id.

`invoke` takes a `ModelRequest` and returns a stream of `ModelEvent`s. The
agent loop drives this trait; you rarely call `invoke` yourself — you hand a
`Model` to an `LlmAgent` and let the loop run it.

### Carrier types

These live in `paigasus_helikon::core` (re-exported from
`paigasus-helikon-core`) and cross the model boundary:

- `ModelRequest` — the request envelope: accumulated `messages`, the `tools`
  the model may call this turn, and a `ModelSettings` of provider-tuning knobs.
- `ModelSettings` — `temperature`, `top_p`, `max_output_tokens`, a `tool_choice`
  (`ToolChoice`), a `response_format` (`ResponseFormat`), and OpenAI Responses'
  `previous_response_id`.
- `ResponseFormat` — `Text`, `JsonObject`, or `JsonSchema { name, schema, strict }`.
  Used to request structured output; see [Structured Output](./structured-output-builder.md).
- `ModelEvent` — the streaming union: `TokenDelta`, `ReasoningDelta`,
  `ToolCallDelta`, `Usage { input_tokens, output_tokens, cached_input_tokens,
  reasoning_tokens }`, and the terminal `Finish { reason }` (a `FinishReason`).
- `ModelCapabilities` — the per-instance capability flags below.
- `ModelError` — `Unavailable`, `RateLimited`, `ContextLengthExceeded`,
  `Refused`, `Transport`, `Other`. The loop does **not** auto-retry on these.

`ModelRequest`, `ModelSettings`, `ModelEvent`, `ModelCapabilities`,
`ResponseFormat`, `ToolChoice`, `FinishReason`, and `ModelError` are all
`#[non_exhaustive]`, so new fields and variants are additive.

### `ModelCapabilities`

A `Copy` flag struct, stable per `Model` instance, that tells the loop what the
provider can do: `streaming`, `tools`, `parallel_tool_calls`,
`structured_output`, `server_managed_state`, `reasoning`, `vision`, `audio`,
`prompt_caching`. Construct from `empty()` (or `default()`) with chained const
builders:

```rust
use paigasus_helikon::core::ModelCapabilities;

let caps = ModelCapabilities::empty()
    .with_streaming()
    .with_tools()
    .with_structured_output();
```

## The two shipped adapters

Both crates are published on crates.io and reached through the facade behind a
feature flag. Each exposes a `Model` implementation plus a builder.

### OpenAI — `paigasus-helikon-providers-openai`

Reached as `paigasus_helikon::openai::OpenAiModel` behind the `openai` feature
(alias `providers-openai`). Covers the Chat Completions and Responses APIs. The
builder reads `OPENAI_API_KEY` from the environment.

```rust
use paigasus_helikon::openai::OpenAiModel;

let model = OpenAiModel::chat("gpt-5-mini").build()?;
```

`OpenAiModel::chat(id)` returns an `OpenAiModelBuilder`; `build()` yields a
`Result<OpenAiModel, BuildError>`.

### Anthropic — `paigasus-helikon-providers-anthropic`

Reached as `paigasus_helikon::anthropic::AnthropicModel` behind the `anthropic`
feature. Covers the Messages API. The builder reads `ANTHROPIC_API_KEY` from the
environment, and the crate also exports the Anthropic-specific settings types
`CacheStrategy` and `ExtendedThinking`.

```rust
use paigasus_helikon::anthropic::AnthropicModel;

let model = AnthropicModel::messages("claude-sonnet-4-6").build()?;
```

`AnthropicModel::messages(id)` returns an `AnthropicModelBuilder`; `build()`
yields a `Result<AnthropicModel, BuildError>`.

> Model ids (`gpt-5-mini`, `claude-sonnet-4-6`) are illustrative — swap them for
> any model your account can reach if the provider rejects the id.

## Switching providers is one line

Because both adapters implement the same `Model` trait, the *only* code that
changes between providers is the construction line. Everything downstream — the
agent, its tools, the run context, the streamed result — is identical. Compare
the two budgeting-assistant examples (`budget_assistant_openai.rs` and
`budget_assistant_anthropic.rs`); the agent, the `#[tool]` functions, and the
run are byte-for-byte the same, and the diff is one line:

```rust
// OpenAI
let model = OpenAiModel::chat("gpt-5-mini").build()?;

// Anthropic — same agent, same tools, same run
let model = AnthropicModel::messages("claude-sonnet-4-6").build()?;
```

```rust
let agent = LlmAgent::builder::<()>()
    .name("budget-assistant")
    .model(model) // ← the only thing that differs is how `model` was built
    .instructions("You are a budgeting assistant. ...")
    .tools(tools![lookup_spending, budget_status])
    .build();
```

`.model(model)` accepts anything that implements `Model`, so your own adapter
slots in the same way — implement `invoke` and `capabilities` on a type and the
loop will drive it.

## Enabling the providers

```toml
[dependencies]
paigasus-helikon = { version = "0.3", features = ["openai", "macros"] }
# or, for Anthropic:
# paigasus-helikon = { version = "0.3", features = ["anthropic", "macros"] }
```

Feature names are kebab-case (`openai`, `anthropic`); the `pub use` aliases are
snake_case (`openai`, `anthropic`, `providers_openai`). See
[Workspace Layout](../getting-started/workspace-layout.md) for the full feature
map and [Crates](../reference/crates.md) for the published crate list.

## Retrying transient errors

Provider calls can fail transiently — rate limits (`RateLimited`), `503`s
(`Unavailable`), or dropped connections (`Transport`). Per ADR-10 the agent loop
never auto-retries; retry is an opt-in composition-layer concern.

`paigasus-helikon-runtime-tokio` provides a `RetryingModel<M>` decorator: it
wraps any `Model` and retries those transient variants with exponential backoff
and jitter. It is configured by **wrapping the model** (not via `RunConfig`),
and is disabled unless you wrap.

```rust,ignore
use std::time::Duration;
use paigasus_helikon_runtime_tokio::{RetryPolicy, RetryingModel};

let policy = RetryPolicy::new()
    .max_attempts(4)
    .base_delay(Duration::from_millis(250));
let resilient = RetryingModel::new(model, policy);
```

Retry covers *connection establishment*: a retryable error that arrives before
any content has streamed is retried; once tokens or tool-call deltas have been
emitted, a later error is surfaced rather than retried (output can't be
un-emitted). `RateLimited { retry_after_ms }` waits at least the provider's
hint, and backoff sleeps abort promptly on run cancellation.
