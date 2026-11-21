# Model Providers

Every LLM in Helikon sits behind one trait: `Model`, from `paigasus-helikon-core`.
There are no per-provider traits. Capability differences (streaming, tool calling,
structured output, vision, prompt caching, …) are surfaced through a flag struct,
`ModelCapabilities`, rather than split interfaces. Five adapters ship today —
`OpenAiModel`, `AnthropicModel`, `BedrockModel`, `GeminiModel`, and
`LiteLlmModel` — and switching between them is a single line.

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
  `ToolCallDelta`, `ProviderData` (opaque data that one translator owns, for
  example a Gemini thought signature; see
  [Core Primitives](./core-primitives.md)), `Usage { input_tokens, output_tokens, cached_input_tokens,
  reasoning_tokens }`, and the terminal `Finish { reason }` (a `FinishReason`).
  **Contract clarification:** implementations MUST emit `Finish` at
  end-of-stream when a stop reason was observed, and MUST NOT emit it on
  truncation with no stop reason observed, on cancellation, or after a
  mid-stream error.
  Implementations that cannot honor cancellation MUST still terminate the
  stream when the `CancellationToken` fires, ending it without emitting
  `Finish`. Both rules restate `Model::invoke`'s existing public contract
  here, where third-party implementors will look for them.
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

## The shipped adapters

Five provider adapters ship today. All are published on crates.io and reached
through the facade behind a feature flag. Each exposes a `Model` implementation
plus a builder.

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

OpenAI does not use provider tool types. It sends the generic schema for every tool. See [Provider tool types](./tools.md#provider-tool-types).

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

Anthropic uses provider tool types. If a tool declares an `anthropic` type, the adapter sends the typed entry, for example `{"type": "memory_20250818", "name": "memory"}`, with no description and no input schema. To turn this off, call `native_tool_types(false)` on the builder. The adapter then sends the generic schema. See [Provider tool types](./tools.md#provider-tool-types).

### Bedrock — `paigasus-helikon-providers-bedrock`

Reached as `paigasus_helikon::bedrock::BedrockModel` behind the `bedrock`
feature. Covers the **Converse streaming API** — tool use and multi-turn
conversations across Anthropic Claude, Amazon Nova, Amazon Titan, Meta Llama,
Mistral, and Cohere model families hosted on AWS Bedrock. Structured output
uses forced-tool synthesis on Anthropic Claude, Amazon Nova, and Mistral. It
uses native output on Claude Sonnet 5.5 and Opus 5.5. Titan, Llama, and Cohere
degrade to plain text responses.

> **Disambiguation:** this is the Bedrock *Converse model provider*. It is
> distinct from `paigasus-helikon-runtime-agentcore`, which is the Bedrock
> *AgentCore runtime* host — a separate crate implementing the AgentCore
> container contract; see [Runtimes](./runtimes.md).

#### Construction

The simplest path loads AWS configuration from the standard credential chain:

```rust,ignore
use paigasus_helikon::bedrock::BedrockModel;

let model = BedrockModel::from_env("anthropic.claude-3-5-sonnet-20241022-v2:0").await?;
```

`from_env` is `async` because it may fetch IMDS or SSO tokens. The credential
chain is **lazy** — auth failures surface at `invoke()` time, not here.

For explicit configuration, use the synchronous builder:

```rust,ignore
use aws_config::BehaviorVersion;
use paigasus_helikon::bedrock::BedrockModel;

let sdk_cfg = aws_config::defaults(BehaviorVersion::v2026_11_17())
    .region(aws_config::Region::new("us-east-1"))
    .load()
    .await;

let model = BedrockModel::converse("amazon.nova-pro-v1:0")
    .sdk_config(&sdk_cfg)
    .build()?;
```

`BedrockModel::converse(id)` returns a `BedrockModelBuilder`; `build()` yields a
`Result<BedrockModel, BuildError>`. You can also inject a pre-constructed
`aws_sdk_bedrockruntime::Client` via `.client(c)` for full control.

#### Family-gated structured output

The `bedrock` provider detects the model family from the Bedrock model ID. For
families that support Bedrock's forced-tool-choice — `Anthropic`, `AmazonNova`,
and `Mistral` — `ResponseFormat::JsonSchema` is synthesized as a hidden tool
call: the model is forced to invoke an internal reserved tool whose input schema
is the user-supplied JSON Schema, and the result is returned as a structured
JSON response. For other families (`AmazonTitan`, `Llama`, `Cohere`, `Unknown`)
a `JsonSchema` format degrades silently to a text response.

Claude Sonnet 5.5 and Opus 5.5 reject a forced tool choice. For these two
models, the provider sends the `JsonSchema` as native output
(`outputConfig.textFormat`). Another Anthropic model can also reject the forced
choice. The provider then sends one native request, which is a different request.
After that, the `BedrockModel` handle and its clones use native output. Only a
`ValidationException` with one of the two known rejection texts, from a request
that sent a forced choice, changes the handle. A request that fails locally does
not reach Bedrock and does not change the handle. For example, `JsonSchema`
together with `ToolChoice::Tool` fails locally on a forced-choice handle.

The provider rewrites the schema first. It adds `additionalProperties: false`,
changes `oneOf` to `anyOf`, and removes keywords that Bedrock does not accept,
such as numeric limits. It does not add `additionalProperties` to a node in an
`allOf` composition or to any node below an `allOf` member. It also does not add
it to an object node next to `oneOf` or `anyOf`. The
branches of such a node stay open too. Bedrock rejects maps, `patternProperties`,
`propertyNames`, tuples, the schemas `true` and `{}`, and recursive types.
Bedrock can also reject an open object in an `allOf` composition. Closing each
member does not help, because each closed member rejects the properties of the
other members. Put the properties into one closed object instead.

Bedrock does not enforce the removed limits. The agent loop does not evaluate
the JSON schema. It checks that the result deserializes into the output type. A
limit that only the schema states is not checked, so add your own validation for
it. The change from `oneOf` to `anyOf` also accepts a value that matches more
than one branch. If your code needs exactly one branch to match, validate the
result against the original schema.

`JsonObject` degrades to text on this path. The provider omits `ToolChoice::Required` and `ToolChoice::Tool` with a
warning. `ToolChoice::Tool` with an undefined name is still an error. For the full rules, see the crate README.

Family detection reads the first segment of the model ID. If the first segment
is not a provider name, detection reads the second segment. This skips a
geographic prefix. The known prefixes are `us.`, `eu.`, `apac.`, `jp.`, `au.`,
`ca.`, `in.` and `global.`. A future prefix works too. For example,
`global.anthropic.claude-sonnet-4-5-20250929-v1:0` is identified as `Anthropic`.

For an ARN, detection uses the resource ID, which is the text after the last
`/`. An opaque ARN, such as an application inference profile ARN, gives
`Unknown`.

Reasoning content surfaced by the model (e.g. extended-thinking responses from
Claude) is delivered as `ModelEvent::ReasoningDelta` in the stream — no extra
configuration is required on this provider.

#### Tool-input schema rewriter

For tool input schemas, Bedrock's Converse API validator rejects JSON Schemas
containing `$ref`/`$defs`, `oneOf`/`anyOf`/`allOf`, or meta-keywords such as
`$schema`, `format`, and `examples`. The crate exposes `rewrite_tool_schema(schema, Ruleset::Strict)`,
which runs automatically on every tool spec before the request is sent:

- `$ref` references are inlined recursively (cycles and excessive depth are
  terminated with a permissive `{"type": "object"}`).
- `oneOf`/`anyOf`/`allOf` combinators (e.g. serde-generated tagged enums) are
  collapsed into a flat `object` whose `properties` union the variants' fields
  and whose shared tag key becomes an `enum` of variant names.
- Unsupported meta-keywords are stripped.

This means schemars-derived schemas for tagged enums and generic structs pass
Bedrock's validator without any manual schema adjustment.

#### Credentials and TLS

The provider uses the standard AWS credential chain via `aws-config`
(environment variables, `~/.aws/credentials`, SSO, IMDS, ECS task role, …) and
the `aws-lc-rs` TLS backend — the same crypto provider the workspace already
uses via `reqwest`/`async-openai`. Do not enable the AWS SDK `ring` feature
alongside this crate; a second `CryptoProvider` panics.

> Model ids are illustrative — swap them for any model your account can reach
> if the provider rejects the id.

Bedrock does not use provider tool types. It sends the generic schema for every tool. See [Provider tool types](./tools.md#provider-tool-types).

### Gemini — `paigasus-helikon-providers-gemini`

Reached as `paigasus_helikon::gemini::GeminiModel` behind the `gemini` feature.
Covers the **Gemini streaming API** — tool use, structured output, and
multi-turn conversations against Google's Gemini model family. Structured output
is **native** (`responseSchema`) — the provider does not use forced-tool synthesis.

> **Note:** Gemini rejects requests that combine structured output with active
> function calling. If your `ModelRequest` sets a `ResponseFormat::JsonObject`
> or `ResponseFormat::JsonSchema` while `tools` is non-empty **or** `tool_choice`
> is anything other than `None`, the provider returns a conflict error before
> sending.

#### Transports

Two transports are available:

**Developer API** — authenticates with an API key from the environment:

```rust
use paigasus_helikon::gemini::GeminiModel;

// Reads GEMINI_API_KEY or GOOGLE_API_KEY.
let model = GeminiModel::from_env("gemini-2.5-flash")?;
```

Or explicitly via the builder:

```rust
use paigasus_helikon::gemini::GeminiModel;

let model = GeminiModel::developer("gemini-2.5-flash")
    .api_key("your-api-key")
    .build()?;
```

**Vertex AI** — authenticates with a bearer token or `TokenProvider`:

```rust,ignore
use paigasus_helikon::gemini::GeminiModel;

// vertex-adc feature: reads GOOGLE_CLOUD_PROJECT + GOOGLE_CLOUD_LOCATION (default: "global").
// ADC credentials are discovered from the gcloud credential chain.
let model = GeminiModel::vertex_from_env("gemini-2.5-flash").await?;
```

Or with an explicit static bearer token:

```rust
use paigasus_helikon::gemini::GeminiModel;

let model = GeminiModel::vertex("gemini-2.5-flash", "my-project", "us-central1")
    .bearer_token("ya29.your-token")
    .build()?;
```

Enable the `vertex-adc` feature for Application Default Credentials via `gcp_auth`:

```toml
[dependencies]
paigasus-helikon-providers-gemini = { version = "0.2", features = ["vertex-adc"] }
```

#### JSON-Schema sanitizer

A JSON-Schema sanitizer runs automatically on every schema passed to
`responseSchema`. It inlines `$ref` references, converts `[T, "null"]` type
arrays to `nullable: true`, replaces `const` with single-item `enum`, renames
`oneOf` to `anyOf`, strips unsupported keywords (`$schema`,
`additionalProperties`, `unevaluatedProperties`, `patternProperties`,
`examples`, `default`, `$defs`/`definitions`), and drops `format` values Gemini
doesn't recognize for their type (keeping e.g. `enum`/`date-time`). Schemars-
derived schemas for typical Rust structs and enums pass without manual adjustment.

#### Limitations

Remote-URL images, audio parts, and non-text tool-result parts are silently
dropped during request translation. Reasoning streaming (`ModelEvent::ReasoningDelta`)
is not yet emitted by this provider.

Tool loops on Gemini 3 work. Gemini 3 returns a `thoughtSignature` with a
tool call and rejects the next request if the signature is missing. The
provider keeps each signature as `ContentPart::ProviderData` (key
`PROVIDER_DATA_KEY`, value `"gemini"`) and sends it back on its own call. It
also restores the call order that Gemini returned. For a call of the current
turn that has no valid signature, it sends the documented dummy value
`skip_thought_signature_validator`. A signature from another model id or
another API counts as not valid. The dummy is sent only for Gemini 3 and later
ids. Gemini 1.x and 2.x requests keep their earlier shape and get no dummy.
For Gemini 3 and later, a request that ends with a model turn gets a `user` text turn "Continue." appended, because Gemini rejects such a request.
These limits apply:

- Vertex signatures are not live-tested.
- The dummy value is live-tested only on `gemini-3.8-flash` (Developer API).
  It is not tested on Gemini 2.x (these ids return 404 for new users) or on
  Vertex.
- Text-level signatures are not sent back.

Gemini bills signatures as input tokens, so provider data adds to the prompt
size.

> Model ids are illustrative — swap them for any model your account can reach.

Gemini does not use provider tool types. It sends the generic schema for every tool. See [Provider tool types](./tools.md#provider-tool-types).

### LiteLLM — `paigasus-helikon-providers-litellm`

Reached as `paigasus_helikon::litellm::LiteLlmModel` behind the `litellm`
feature. Talks to a [LiteLLM](https://docs.litellm.ai) proxy over its
OpenAI-compatible Chat Completions endpoint, adding LiteLLM's own router and
observability fields (fallbacks, retries, tags, metadata).

#### When to use this instead of the OpenAI provider

`OpenAiModel::base_url()` can also point at a LiteLLM proxy, and is the better
choice when the proxy simply fronts OpenAI models. Reach for `litellm` when you
need LiteLLM's router fallbacks and retries, spend/trace metadata, reasoning
streaming from non-OpenAI backends, or arbitrary operator-chosen model aliases.

#### `base_url` is required

There is no default — an unset base URL is a build error. It may also come
from `LITELLM_API_BASE` (or `LITELLM_PROXY_API_BASE`).

```rust,ignore
use paigasus_helikon::litellm::LiteLlmModel;

let model = LiteLlmModel::chat("claude-sonnet-4") // your proxy's alias
    .base_url("http://litellm:4000")
    .build()?;
```

#### Capabilities are your declaration

A LiteLLM alias (`prod-fast`, `team-a/gpt`) carries no information the SDK can
act on, so the model defaults to a conservative `streaming + tools` capability
set. Declare what the backend actually supports with `.with_capabilities()`:

```rust,ignore
use paigasus_helikon::core::ModelCapabilities;
use paigasus_helikon::litellm::LiteLlmModel;

let model = LiteLlmModel::chat("prod-fast")
    .base_url("http://litellm:4000")
    .with_capabilities(
        ModelCapabilities::empty()
            .with_streaming()
            .with_tools()
            .with_vision(),
    )
    .build()?;
```

`streaming` is always forced on — this provider has no non-streaming path.

> Model ids are illustrative — swap them for whatever alias your proxy operator
> configured.

LiteLLM does not use provider tool types. It sends the generic schema for every tool. See [Provider tool types](./tools.md#provider-tool-types).

## Switching providers is one line

Because all adapters implement the same `Model` trait, the *only* code that
changes between providers is the construction line. Everything downstream — the
agent, its tools, the run context, the streamed result — is identical. Compare
the two budgeting-assistant examples (`budget_assistant_openai.rs` and
`budget_assistant_anthropic.rs`); the agent, the `#[tool]` functions, and the
run are byte-for-byte the same, and the diff is one line:

```rust,ignore
// OpenAI
let model = OpenAiModel::chat("gpt-5-mini").build()?;

// Anthropic — same agent, same tools, same run
let model = AnthropicModel::messages("claude-sonnet-4-6").build()?;

// Bedrock (async construction — loads the AWS credential chain)
let model = BedrockModel::from_env("anthropic.claude-3-5-sonnet-20241022-v2:0").await?;

// Gemini Developer API
let model = GeminiModel::from_env("gemini-2.5-flash")?;

// LiteLLM proxy — alias is whatever the proxy operator configured
let model = LiteLlmModel::chat("claude-sonnet-4")
    .base_url("http://litellm:4000")
    .build()?;
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
paigasus-helikon = { version = "0.8", features = ["openai", "macros"] }
# or, for Anthropic:
# paigasus-helikon = { version = "0.8", features = ["anthropic", "macros"] }
# or, for Bedrock:
# paigasus-helikon = { version = "0.8", features = ["bedrock", "macros"] }
# or, for Gemini:
# paigasus-helikon = { version = "0.8", features = ["gemini", "macros"] }
# or, for LiteLLM:
# paigasus-helikon = { version = "0.8", features = ["litellm", "macros"] }
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
