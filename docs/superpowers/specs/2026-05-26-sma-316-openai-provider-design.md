# SMA-316 — OpenAI provider design

**Status:** Design approved 2026-05-26 — implementation pending.
**Linear:** [SMA-316](https://linear.app/smaschek/issue/SMA-316/openai-provider-chat-completions-responses-streaming-tools-structured)
**Branch:** `feature/sma-316-openai-provider-chat-completions-responses-streaming-tools`

## Purpose

First concrete `Model` implementation for the Paigasus Helikon SDK. Wraps `async-openai` and translates between Paigasus carrier types (`ModelRequest`, `ModelEvent`, `Item`, `Tool`) and the OpenAI wire protocol. Supports both the Chat Completions and Responses endpoints behind one `OpenAiModel` type.

Unblocks SMA-317 (Anthropic provider) by landing the cross-provider `ModelSettings` fields and the `ModelEvent::Usage` variant.

## Architectural decisions

### Wire layer — wrap `async-openai`

Per ticket. We sit on async-openai 0.27 for its typed request/response structs, HTTP client, and Chat Completions SSE handling. For Responses-API streaming events that async-openai does not yet model, we layer a thin SSE parser on top of its raw HTTP client. This decision can be revisited if upstream cadence becomes a problem; the translation layer is the natural insulator.

### Type shape — single `OpenAiModel` with internal `Backend` enum

The ticket spec writes `OpenAiModel::chat(...)` and `OpenAiModel::responses(...)`, so the public type is decided. Internally an enum:

```rust
enum Backend {
    Chat { client: async_openai::Client<_> },
    Responses { client: async_openai::Client<_> },
}
```

One `impl Model for OpenAiModel` matches and dispatches. Auth, base URL, capability lookup, and error mapping are shared across both backends.

### Settings scope split — SMA-316 lands what OpenAI needs

`paigasus-helikon-core::ModelSettings` gains the five fields OpenAI uses. SMA-317 (Anthropic) reshapes these later if Anthropic's shape demands it; reshape cost is one provider's call sites, which is acceptable.

### `ModelEvent::Usage` as a separate variant

Rather than extending `Finish` with `usage: Option<TokenUsage>`, we add a new `ModelEvent::Usage` variant. This composes with Anthropic's incremental-usage streaming (SMA-317) without ordering surprises, and keeps `Finish` minimal.

Ordering contract on `Model::invoke`'s stream: `Usage` events MAY appear anywhere; `Finish` is always the terminal event. Consumers that want a final total should retain the last `Usage` seen.

### Capabilities — hardcoded table with builder override

OpenAI exposes no machine-readable capability manifest (`GET /v1/models` returns only `{id, object, created, owned_by}`). We maintain `KNOWN_MODELS: &[(&str, ModelCapabilities)]` inside the crate. Unknown ids fall through to conservative defaults (`streaming: true, tools: true, parallel_tool_calls: true`, everything else `false`). `OpenAiModelBuilder::with_capabilities(...)` lets callers override for models the table hasn't catalogued.

## Cross-crate changes — `paigasus-helikon-core`

```rust
// crates/paigasus-helikon-core/src/model.rs

#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct ModelSettings {
    pub temperature: Option<f32>,
    pub top_p: Option<f32>,
    pub max_output_tokens: Option<u32>,
    pub tool_choice: Option<ToolChoice>,
    pub response_format: Option<ResponseFormat>,
    pub previous_response_id: Option<String>,
}

#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum ToolChoice {
    Auto,
    Required,
    None,
    Tool { name: String },
}

#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum ResponseFormat {
    Text,
    JsonObject,
    JsonSchema {
        name: String,
        schema: serde_json::Value,
        strict: bool,
    },
}

#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum ModelEvent {
    TokenDelta { text: String },
    ReasoningDelta { text: String },
    ToolCallDelta { call_id: String, name: Option<String>, args_delta: String },
    Usage {
        input_tokens: u32,
        output_tokens: u32,
        cached_input_tokens: Option<u32>,
        reasoning_tokens: Option<u32>,
    },
    Finish { reason: FinishReason },
}
```

Doc-comment on `Model::invoke` documents the `Usage` ordering contract.

**Deliberately out of scope:** `seed`, `frequency_penalty`/`presence_penalty`, `logprobs`, `stop` sequences. Easy to add later if a user asks; not load-bearing for the agent loop.

## Crate layout — `paigasus-helikon-providers-openai`

```
crates/paigasus-helikon-providers-openai/src/
├── lib.rs                  # re-exports
├── model.rs                # OpenAiModel + impl Model
├── builder.rs              # OpenAiModelBuilder, AuthMethod, BuildError
├── capabilities.rs         # KNOWN_MODELS table, lookup, conservative_defaults
├── error.rs                # async-openai::Error -> ModelError mapping
├── backend/
│   ├── mod.rs              # Backend enum, dispatch
│   ├── chat.rs             # ChatTranslator, Chat Completions translation
│   └── responses.rs        # ResponsesTranslator, Responses translation
└── translate/
    ├── mod.rs              # shared helpers
    ├── request.rs          # Vec<Item> -> OpenAI messages
    ├── tools.rs            # ToolDef -> strict tool schema
    └── response_format.rs  # ResponseFormat -> OpenAI response_format

tests/
├── chat_wire.rs                # wiremock non-streaming Chat
├── chat_streaming.rs           # wiremock SSE Chat fixtures
├── responses_wire.rs           # wiremock non-streaming Responses
├── responses_streaming.rs      # wiremock SSE Responses fixtures
├── tools_strict_schema.rs      # insta snapshots
├── live.rs                     # OPENAI_API_KEY-gated, #[ignore]
└── fixtures/
    ├── chat_*.txt              # raw SSE bytes
    └── responses_*.txt
```

Public surface re-exported through `lib.rs`: `OpenAiModel`, `OpenAiModelBuilder`, `AuthMethod`, `BuildError`.

## Public API

```rust
impl OpenAiModel {
    pub fn chat(model_id: impl Into<String>) -> OpenAiModelBuilder;
    pub fn responses(model_id: impl Into<String>) -> OpenAiModelBuilder;
}

impl OpenAiModelBuilder {
    pub fn api_key(mut self, key: impl Into<String>) -> Self;
    pub fn base_url(mut self, url: impl Into<String>) -> Self;
    pub fn organization(mut self, org: impl Into<String>) -> Self;
    pub fn project(mut self, project: impl Into<String>) -> Self;
    pub fn http_client(mut self, client: reqwest::Client) -> Self;
    pub fn with_capabilities(mut self, caps: ModelCapabilities) -> Self;
    pub fn build(self) -> Result<OpenAiModel, BuildError>;
}

#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum AuthMethod {
    Env,                  // read OPENAI_API_KEY at build()
    ApiKey(String),
    Bearer(String),       // Azure AD, custom proxy
}

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum BuildError {
    #[error("OPENAI_API_KEY not set in environment")]
    MissingApiKey,
    #[error("invalid base URL: {0}")]
    InvalidBaseUrl(String),
}
```

`OpenAiModel::chat("gpt-4o").build()?` reads `OPENAI_API_KEY` from env, targets `https://api.openai.com/v1`, looks up caps in `KNOWN_MODELS`. `base_url` override is what makes LiteLLM, vLLM, Azure-via-proxy, and OpenAI-compatible local servers work.

## Wire translation

### Messages — `Vec<Item>` → OpenAI input

**Chat Completions:**

| `Item` variant                            | Maps to                                                                                  |
| ----------------------------------------- | ---------------------------------------------------------------------------------------- |
| `System { content }`                      | `{role: "system", content: <text>}`                                                      |
| `UserMessage { content }`                 | `{role: "user", content: <text-or-multimodal-parts>}`                                    |
| `AssistantMessage { content, agent }`     | `{role: "assistant", content, tool_calls: [...]}` — `ContentPart::ToolUse` blocks hoisted to sibling `tool_calls`; `agent` attribution is dropped (OpenAI has no slot for it); `content: null` when only tool calls and no text |
| `ToolCall { call_id, name, args }`        | Folded into preceding `AssistantMessage`'s `tool_calls`                                  |
| `ToolResult { call_id, content }`         | `{role: "tool", tool_call_id, content: <text>}`                                          |
| `ContentPart::Reasoning { .. }`           | Dropped (OpenAI does not accept reasoning input on Chat)                                 |

**Responses API:** equivalent mapping into `input: Vec<InputItem>` (`{type, role, content}`, `{type: "function_call", call_id, name, arguments}`, `{type: "function_call_output", call_id, output}`). When `previous_response_id` is set, prior turns are omitted — server holds state.

### Tools — `ToolDef::schema` → OpenAI strict tool

`to_strict_schema(value: &Value) -> Value`:

1. Set `additionalProperties: false` on every nested object.
2. Add every key in each level's `properties` to that level's `required`.
3. Return rewritten schema.

Schemas with features OpenAI rejects (`pattern` outside the allowlist, unsupported `format`, discriminated `oneOf`) surface as `ModelError::Other(anyhow!(...))` at request time. We translate, we do not pre-validate. The `#[tool]` proc-macro (SMA-315) already emits `schemars`-friendly schemas that round-trip cleanly.

`Tool::output_schema()` is ignored — OpenAI does not accept a return-payload schema.

### Response format

| `ResponseFormat`                          | OpenAI                                                                  |
| ----------------------------------------- | ----------------------------------------------------------------------- |
| `Text`                                    | omit `response_format`                                                  |
| `JsonObject`                              | `{type: "json_object"}`                                                 |
| `JsonSchema { name, schema, strict }`     | `{type: "json_schema", json_schema: {name, schema, strict}}` — schema run through `to_strict_schema` when `strict` |

### Settings passthrough

- `temperature`, `top_p` — passthrough.
- `max_output_tokens` → `max_tokens` (Chat) or `max_output_tokens` (Responses).
- `tool_choice` → `auto | required | none | {type: "function", function: {name}}`.
- `previous_response_id` → `previous_response_id` (Responses); ignored with `tracing::debug!` on Chat.

## Streaming

### Chat Completions

`stream_options: { include_usage: true }` is always set so we receive a final usage chunk.

`ChatTranslator` accumulator state:

```rust
struct ChatTranslator {
    tool_calls: HashMap<u32, ToolCallAccumulator>,
}
struct ToolCallAccumulator { call_id: String, name_seen: bool }
```

Per-chunk mapping:

- `delta.content` non-empty → `TokenDelta { text }`.
- `delta.tool_calls[i]` first delta (has `id`, `function.name`) → insert accumulator, emit `ToolCallDelta { call_id, name: Some, args_delta: function.arguments.unwrap_or_default() }`.
- `delta.tool_calls[i]` subsequent delta (no `id`, indexed by `index`) → look up accumulator, emit `ToolCallDelta { call_id, name: None, args_delta }`.
- `chunk.usage` (final chunk) → `Usage { ... }`.
- `finish_reason: Some(reason)` → translate (`stop → Stop`, `length → Length`, `tool_calls → ToolCalls`, `content_filter → ContentFilter`, else `Other(s)`), emit `Finish { reason }`.

Per-chunk output is a small `Vec<ModelEvent>`, flattened via `futures::stream::iter`. Usage precedes Finish when both are present on the same chunk.

### Responses API

| Server event                                       | `ModelEvent`                                                    |
| -------------------------------------------------- | --------------------------------------------------------------- |
| `response.output_text.delta`                       | `TokenDelta { text }`                                           |
| `response.reasoning_summary_text.delta`            | `ReasoningDelta { text }`                                       |
| `response.output_item.added` (function_call)       | (internal — register call_id, mark name un-emitted)             |
| `response.function_call.arguments.delta`           | `ToolCallDelta { call_id, name: Some on first / None after, args_delta }` |
| `response.completed`                               | `Usage` (from `event.response.usage`) then `Finish { reason }`  |
| `response.failed`, `response.incomplete`           | terminate stream with `ModelError`                              |

`ResponsesTranslator` owns a `HashMap<String, bool>` (call_id → name_emitted) to gate the `name: Some/None` decision.

### Cancellation

`Model::invoke` receives a `CancellationToken`. The stream `select!`s between upstream chunks and `cancel.cancelled()`. On cancel, the response body is dropped (closing the underlying TCP/TLS connection) and our stream ends without emitting `Finish` — the runner already handles this case per ADR-10.

## Error mapping

```rust
fn map_openai_error(e: async_openai::error::OpenAIError) -> ModelError {
    use async_openai::error::OpenAIError as E;
    match e {
        E::ApiError(api) => match (api.code.as_deref(), api.r#type.as_deref(), status_of(&api)) {
            (_, _, Some(401)) | (_, _, Some(403)) =>
                ModelError::Refused { reason: api.message },
            (_, _, Some(429)) =>
                ModelError::RateLimited { retry_after_ms: parse_retry_after(&api) },
            (Some("context_length_exceeded"), _, _) =>
                ModelError::ContextLengthExceeded,
            (_, Some("content_filter"), _) =>
                ModelError::Refused { reason: api.message },
            (_, _, Some(s)) if (500..=599).contains(&s) =>
                ModelError::Unavailable,
            _ => ModelError::Other(anyhow!("openai api error: {}", api.message)),
        },
        E::Reqwest(re) => ModelError::Transport(re.to_string()),
        E::JSONDeserialize(je) =>
            ModelError::Other(anyhow!("malformed openai response: {}", je)),
        E::StreamError(s) => ModelError::Transport(s),
        other => ModelError::Other(anyhow!(other.to_string())),
    }
}
```

Two deliberate calls:

- **401/403 → `Refused`** rather than a new `AuthFailed` variant. `Refused` is non-retryable per ADR-10, which is the right semantic for bad credentials. The reason string carries the detail.
- **Generic 5xx → `Unavailable`**. Also non-retryable at the runner level; application-layer retry policies (`RunConfig::retry_policy`) handle revival.

`status_of` and `parse_retry_after` are small helpers — the only code coupled to async-openai's `ApiError` shape, which makes future upstream-version churn local.

## Capabilities table

```rust
const KNOWN_MODELS: &[(&str, ModelCapabilities)] = &[
    // Chat Completions family
    ("gpt-4o",        /* streaming, tools, parallel, structured, vision */),
    ("gpt-4o-mini",   /* streaming, tools, parallel, structured, vision */),
    ("gpt-4.1",       /* streaming, tools, parallel, structured, vision */),
    ("gpt-4.1-mini",  /* streaming, tools, parallel, structured, vision */),
    ("gpt-3.5-turbo", /* streaming, tools, parallel */),
    // Responses family (reasoning models)
    ("o1",            /* + reasoning, + server_managed_state */),
    ("o1-mini",       /* + reasoning, + server_managed_state */),
    ("o3",            /* + reasoning, + server_managed_state */),
    ("o3-mini",       /* + reasoning, + server_managed_state */),
    ("gpt-5",         /* + reasoning, + vision, + server_managed_state */),
];

fn lookup(model_id: &str) -> ModelCapabilities { /* table or conservative_defaults */ }

fn conservative_defaults() -> ModelCapabilities {
    ModelCapabilities {
        streaming: true, tools: true, parallel_tool_calls: true,
        structured_output: false, server_managed_state: false,
        reasoning: false, vision: false, audio: false,
    }
}
```

`server_managed_state` is set to `lookup(model_id).server_managed_state AND backend == Responses` in `OpenAiModel::build()` — so `chat("o3")` (legal but weird) does not claim server-managed state. `with_capabilities()` always wins.

Ids above are illustrative — cross-checked against OpenAI's docs during implementation.

## Testing strategy

### Unit tests (in-crate)

- `translate/tools.rs` — `to_strict_schema` snapshots via `insta`. Covers nested objects, arrays-of-objects, partial-`required`, `additionalProperties: true` override.
- `translate/request.rs` — `Item` → OpenAI messages: assistant with hoisted `ContentPart::ToolUse`, tool-call/tool-result pairs, vision parts, system messages, reasoning-content drop.
- `error.rs` — table-driven mapping for representative `OpenAIError` shapes.
- `capabilities.rs` — unknown id falls through to defaults; override wins; Responses-backend constraint on `server_managed_state`.

### Wire integration tests (`tests/`, wiremock)

- `chat_wire.rs` — non-streaming Chat happy path, tool-call response, 429 with `Retry-After`, content-filter response, context-length error.
- `responses_wire.rs` — non-streaming Responses, `previous_response_id` round-trip, structured output with `json_schema` strict, tool-call response.
- `chat_streaming.rs` — hand-authored SSE fixtures under `tests/fixtures/chat_*.txt`. Plain text deltas + finish, parallel tool calls interleaved by `index`, mid-stream rate-limit, usage on final chunk.
- `responses_streaming.rs` — `tests/fixtures/responses_*.txt`. Text delta + completed, reasoning summary deltas, function-call argument deltas, failed/incomplete terminal events.
- `tools_strict_schema.rs` — large `insta` snapshot suite for schema translation.

Fixtures are raw SSE bytes (`data: {...}\n\n`, `data: [DONE]\n\n`), `include_str!`-loaded. Hand-authored — they double as wire-format documentation.

### Live integration tests (`tests/live.rs`)

- Every test `#[ignore]` + guarded by `if std::env::var("OPENAI_API_KEY").is_err() { return; }`.
- Five tests: chat smoke, responses smoke, tool-call round-trip, structured-output round-trip, streaming round-trip.
- Skipped in CI. Documented in `CONTRIBUTING.md`: set `OPENAI_API_KEY` and `cargo test -p paigasus-helikon-providers-openai -- --ignored`.

## Facade wiring

```toml
# crates/paigasus-helikon/Cargo.toml
[features]
providers-openai = ["dep:paigasus-helikon-providers-openai"]

[dependencies]
paigasus-helikon-providers-openai = { workspace = true, optional = true }
```

```rust
// crates/paigasus-helikon/src/lib.rs
/// OpenAI provider — [`paigasus-helikon-providers-openai`].
#[cfg(feature = "providers-openai")]
pub use paigasus_helikon_providers_openai as providers_openai;
```

Kebab-case feature, snake_case alias, doc-comment on the `pub use` so `-D warnings` on the docs job passes (CLAUDE.md non-obvious pattern).

## Dependencies

Added to `[workspace.dependencies]`:

```toml
async-openai = "0.27"
wiremock     = "0.6"   # dev-only
reqwest      = { version = "0.12", default-features = false, features = ["rustls-tls", "json", "stream"] }
```

MSRV stays at 1.75. If async-openai or wiremock has raised their MSRV by implementation time, we bump `[workspace.package].rust-version` per the CLAUDE.md rule, not downgrade the dep.

## Out of scope

YAGNI for SMA-316:

- Azure-AD-specific authentication flow. Azure works today via `base_url` + `AuthMethod::Bearer`; deeper integration is a follow-up if asked.
- Request-level tracing instrumentation beyond what async-openai emits. Spans live on `RunContext::Tracer`.
- Provider-level retry. Per ADR-10, retries are a `RunConfig::retry_policy` concern.
- Fine-tuning, embeddings, image-gen, audio-gen, files API. Out of `Model`-trait scope.
- `Tool::output_schema()` translation. OpenAI does not accept return-payload schemas.

## Acceptance criteria (ticket-restated)

- Integration tests against the OpenAI API, gated by env var, skipped in CI. ✓ (`tests/live.rs`)
- Mock-server tests verify wire format round-trip. ✓ (wiremock suite)
- `OpenAiModel` implements `Model`. ✓
- Chat Completions + Responses both supported via `chat()` / `responses()`. ✓
- Streaming parses SSE deltas into `TokenDelta`, `ToolCallDelta`, `ReasoningDelta`, `Finish` (plus the new `Usage`). ✓
- Strict tool schemas translated from `Tool::schema()`. ✓
- Structured output via `response_format` with `json_schema` strict. ✓
- `previous_response_id` plumbing. ✓
- `ModelCapabilities` accurate per known model id. ✓
- `OPENAI_API_KEY` env or builder param; customizable base URL. ✓
