# SMA-316 — OpenAI provider design

**Status:** Design approved 2026-05-26 — implementation pending.
**Linear:** [SMA-316](https://linear.app/smaschek/issue/SMA-316/openai-provider-chat-completions-responses-streaming-tools-structured)
**Branch:** `feature/sma-316-openai-provider-chat-completions-responses-streaming-tools`

## Purpose

First concrete `Model` implementation for the Paigasus Helikon SDK. Wraps `async-openai` and translates between Paigasus carrier types (`ModelRequest`, `ModelEvent`, `Item`, `Tool`) and the OpenAI wire protocol. Supports both the Chat Completions and Responses endpoints behind one `OpenAiModel` type.

Unblocks SMA-317 (Anthropic provider) by landing the cross-provider `ModelSettings` fields and the `ModelEvent::Usage` variant.

## Architectural decisions

### Wire layer — wrap `async-openai`

Per ticket. We sit on async-openai **0.40** for its typed request/response structs, HTTP client, and Chat Completions SSE handling. For Responses-API streaming events that async-openai does not yet model, we layer a thin SSE parser on top of its raw HTTP client. This decision can be revisited if upstream cadence becomes a problem; the translation layer is the natural insulator.

**TLS feature graph verified.** `cargo tree -e features` on `async-openai = "0.40"` with default features resolves to a rustls-only graph (`aws-lc-rs`) with no `native-tls` or `openssl`. The implementer must re-verify on any future async-openai version bump — if upstream regresses, supply-chain checks (`audit`, `deny`) may flip on transitive advisories.

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

OpenAI exposes no machine-readable capability manifest (`GET /v1/models` returns only `{id, object, created, owned_by}`). We maintain `KNOWN_MODELS: &[(&str, ModelCapabilities)]` inside the crate. Unknown ids fall through to conservative defaults (`streaming: true, tools: true`, everything else including `parallel_tool_calls: false`). `OpenAiModelBuilder::with_capabilities(...)` lets callers override for models the table hasn't catalogued.

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
    /// OpenAI Responses-API server-side state token. **Caller-managed:**
    /// when set, callers MUST trim [`ModelRequest::messages`] to only the
    /// items added since the response identified by this id. The provider
    /// passes `messages` through as-is — it does not filter. Integration
    /// with [`crate::LlmAgent`]'s automatic conversation accumulation is
    /// out of scope for SMA-316; see follow-up ticket. Ignored by
    /// non-OpenAI-Responses providers.
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
├── builder.rs              # OpenAiModelBuilder, BuildError
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

Public surface re-exported through `lib.rs`: `OpenAiModel`, `OpenAiModelBuilder`, `BuildError`.

## Public API

```rust
impl OpenAiModel {
    pub fn chat(model_id: impl Into<String>) -> OpenAiModelBuilder;
    pub fn responses(model_id: impl Into<String>) -> OpenAiModelBuilder;
}

impl OpenAiModelBuilder {
    /// Set the API key explicitly. If unset, `build()` reads `OPENAI_API_KEY`
    /// from the process environment.
    pub fn api_key(mut self, key: impl Into<String>) -> Self;
    /// Use a pre-minted bearer token (Azure AD, custom proxy). Mutually
    /// exclusive with `api_key`; the last-set value wins.
    pub fn bearer(mut self, token: impl Into<String>) -> Self;
    pub fn base_url(mut self, url: impl Into<String>) -> Self;
    pub fn organization(mut self, org: impl Into<String>) -> Self;
    pub fn project(mut self, project: impl Into<String>) -> Self;
    pub fn http_client(mut self, client: reqwest::Client) -> Self;
    pub fn with_capabilities(mut self, caps: ModelCapabilities) -> Self;
    pub fn build(self) -> Result<OpenAiModel, BuildError>;
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

The earlier draft of this spec exposed an `AuthMethod` enum; it was dropped because `Env` is just the default, leaving only two distinguishable cases (api-key vs bearer) which are clearer as two builder methods.

## Wire translation

### Messages — `Vec<Item>` → OpenAI input

**Chat Completions:**

| `Item` variant                            | Maps to                                                                                  |
| ----------------------------------------- | ---------------------------------------------------------------------------------------- |
| `System { content }`                      | `{role: "system", content: <text>}`                                                      |
| `UserMessage { content }` (text/media)    | `{role: "user", content: <text-or-multimodal-parts>}`                                    |
| `UserMessage { content }` containing `ContentPart::ToolResult` (Anthropic-nested shape) | Each nested `ToolResult` is **hoisted** to a top-level `{role: "tool", tool_call_id, content}` message; any remaining text/media parts emit as a separate `{role: "user", content: ...}` |
| `AssistantMessage { content, agent }`     | `{role: "assistant", content, tool_calls: [...]}` — `ContentPart::ToolUse` blocks hoisted to sibling `tool_calls`; `agent` attribution is dropped (OpenAI has no slot for it); `content: null` when only tool calls and no text; **`ContentPart::Image`/`Audio` inside an `AssistantMessage` are dropped with `tracing::warn!`** (OpenAI Chat assistant role accepts only string-or-null content, no multimodal parts) |
| `ToolCall { call_id, name, args }`        | **Standalone `ToolCall`s** (no preceding `AssistantMessage` carrier in this turn, e.g. when the model emitted only tool calls with no text) are gathered into a **synthesized** `{role: "assistant", content: null, tool_calls: [...]}` message. When a preceding `AssistantMessage` exists in the same turn, fold instead. (This is the common case — `LlmAgent::build_items` does not synthesize a carrier when text+reasoning are empty.) |
| `ToolResult { call_id, content }`         | `{role: "tool", tool_call_id, content: <text>}`                                          |
| `ContentPart::Reasoning { .. }`           | Dropped (OpenAI does not accept reasoning input on Chat)                                 |
| `MediaSource::Base64 { mime_type, data }` | Rendered as `data:<mime_type>;base64,<data>` inside `image_url.url` / `input_audio` shape |

**Responses API:** equivalent mapping into `input: Vec<InputItem>` (`{type, role, content}`, `{type: "function_call", call_id, name, arguments}`, `{type: "function_call_output", call_id, output}`). The standalone-ToolCall synthesis rule applies identically. **`previous_response_id` is caller-managed:** when set, the backend passes `input` through as the caller built it — no filtering, no trimming. Per the rustdoc on `ModelSettings::previous_response_id`, the caller is responsible for ensuring `messages` contains only items added since the previous response.

### Tools — `ToolDef::schema` → OpenAI strict tool

`to_strict_schema(value: &Value) -> Value`:

1. Set `additionalProperties: false` on every nested object.
2. Add every key in each level's `properties` to that level's `required`.
3. Return rewritten schema.

Schemas with features OpenAI rejects (`pattern` outside the allowlist, unsupported `format`, discriminated `oneOf`) surface as `ModelError::Other(anyhow!(...))` at request time. We translate, we do not pre-validate. The `#[tool]` proc-macro (SMA-315) already emits `schemars`-friendly schemas that round-trip cleanly.

**Verified for `Option<T>`:** schemars 1.x emits `Option<T>` as `"type": ["T", "null"]` natively (confirmed empirically — not `oneOf`/`anyOf`). Combined with the rewriter's `required`-forcing pass, this produces exactly OpenAI's nullable-required pattern. No collapse pass is needed for the proc-macro path. The `tools_strict_schema.rs` suite includes an `Option<String>` snapshot test to lock this behavior in against future schemars regressions.

Hand-authored schemas using Draft-7-style `oneOf: [X, {type: "null"}]` patterns are not rewritten by us — they pass through and may produce a strict-mode rejection from OpenAI as `ModelError::Other`. If a user files an issue, we can add a collapse pass then (deferred per YAGNI).

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
- `previous_response_id` → `previous_response_id` (Responses only). Set with no filtering applied to `messages`; the caller-managed contract on `ModelSettings::previous_response_id` is load-bearing. Ignored with `tracing::debug!` on Chat.

## Streaming

### Chat Completions

`stream_options: { include_usage: true }` is set on streaming requests only (the option is invalid on non-streaming bodies). Non-streaming Chat returns usage on the response root.

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
| `response.refusal.delta`                           | `TokenDelta { text }` (the refusal is the model's response text; consumer sees it as content) |
| `response.output_item.added` (function_call)       | (internal — register call_id, mark name un-emitted)             |
| `response.function_call.arguments.delta`           | `ToolCallDelta { call_id, name: Some on first / None after, args_delta }` |
| `response.completed`                               | `Usage` (from `event.response.usage`) then `Finish { reason }` derived from `event.response.status` + `incomplete_details` (see below) |
| `response.incomplete`                              | Map per `incomplete_details.reason` to a `Finish` event:<br>• `"max_output_tokens"` → `Finish { reason: Length }`<br>• `"content_filter"` → `Finish { reason: ContentFilter }`<br>• other / unknown → `Finish { reason: Other(reason) }`<br>This matches Chat's symmetric treatment of `finish_reason: length` / `content_filter` and never produces a `ModelError` for in-band terminations. |
| `response.failed`                                  | terminate stream with `ModelError` (mapped from `event.error`)  |
| `response.error`                                   | terminate stream with `ModelError::Transport(err.message)`      |
| `response.refusal.done`, `*.done` variants, `response.created`, `response.in_progress`, `response.content_part.*`, `response.output_item.done` | dropped with `tracing::debug!(target = "paigasus::openai::responses", event = …)` — deltas already conveyed the content |

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
        streaming: true, tools: true, parallel_tool_calls: false,
        structured_output: false, server_managed_state: false,
        reasoning: false, vision: false, audio: false,
    }
}
```

`parallel_tool_calls` defaults to `false` for unknown ids because most OpenAI-compatible servers (vLLM, LiteLLM, Ollama-via-OpenAI-shim, llama.cpp's server) do not support parallel tool calls. The agent loop expecting multiple tool calls per response and getting only one is a worse failure mode than the inverse. `streaming: true` and `tools: true` stay on as defaults because their absence produces obvious errors rather than subtle misbehavior.

**Backend-dependent capability masking.** `server_managed_state` is computed as `lookup(model_id).server_managed_state AND backend == Responses` in `OpenAiModel::build()` — so `chat("o3")` (legal but weird) does not claim server-managed state. `reasoning` follows the same pattern (`AND backend == Responses` for the o-series). If the table grows further backend-dependent capabilities, the masking logic generalizes to a `backend_masked(caps, backend)` helper in `capabilities.rs`. `with_capabilities()` always wins over both lookup and masking.

**Table verification.** Ids above are illustrative. The implementer MUST cross-check each entry against OpenAI's published model docs at implementation time — capability claims that diverge from official docs are bugs, and `gpt-5` / `o-series` coverage is unstable as of the spec's authoring date (2026-05-26). The capability table is the single artifact most likely to drift from reality; treat updates to it as low-ceremony chore-PRs.

## Testing strategy

### Unit tests (in-crate)

- `translate/tools.rs` — `to_strict_schema` snapshots via `insta::assert_json_snapshot!` (key-order normalized; protects against schemars BTreeMap/IndexMap shuffling across patch bumps). Covers nested objects, arrays-of-objects, partial-`required`, `additionalProperties: true` override, **`Option<T>` round-trip** (locks in schemars 1.x's `type: ["T","null"]` emission).
- `translate/request.rs` — `Item` → OpenAI messages: assistant with hoisted `ContentPart::ToolUse`, **`UserMessage` containing nested `ContentPart::ToolResult` (Anthropic shape)**, tool-call/tool-result pairs, vision parts, system messages, reasoning-content drop, **standalone-`ToolCall` synthesis into assistant carrier**, **multimodal-on-assistant drop**.
- `error.rs` — table-driven mapping for representative `OpenAIError` shapes.
- `capabilities.rs` — unknown id falls through to defaults; override wins; Responses-backend constraint on `server_managed_state` and `reasoning`; **`parallel_tool_calls: false` in conservative defaults**.

### Wire integration tests (`tests/`, wiremock)

- `chat_wire.rs` — non-streaming Chat happy path, tool-call response, 429 with `Retry-After`, content-filter response, context-length error, **synthesized assistant carrier for standalone-`ToolCall` items**.
- `responses_wire.rs` — non-streaming Responses, `previous_response_id` round-trip (verifies caller-managed pass-through with no filtering), structured output with `json_schema` strict, tool-call response.
- `chat_streaming.rs` — hand-authored SSE fixtures under `tests/fixtures/chat_*.txt`. Plain text deltas + finish, parallel tool calls interleaved by `index`, mid-stream rate-limit, usage on final chunk. JSON content asserted via `assert_json_snapshot!`. **Header comment notes the limitation:** wiremock serves fixture bytes in one buffer; these tests prove byte-level correctness, not resilience to slow chunk delivery.
- `responses_streaming.rs` — `tests/fixtures/responses_*.txt`. Text delta + completed, reasoning summary deltas, function-call argument deltas, **`response.incomplete` with `max_output_tokens` → `Finish { Length }`**, **`response.incomplete` with `content_filter` → `Finish { ContentFilter }`**, **`response.refusal.delta` → `TokenDelta`**, `response.failed` → `ModelError`.
- `tools_strict_schema.rs` — large `assert_json_snapshot!` suite for schema translation.

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
async-openai = "0.40"   # default features = ["rustls"] — verified rustls-only graph, no native-tls
wiremock     = "0.6"    # dev-only
```

No explicit `reqwest` pin: async-openai owns reqwest internally and only exposes it through its `rustls` feature flag (verified via `cargo tree -e features` against 0.40.2). If a future async-openai bump regresses this — e.g. adds a non-default feature that pulls native-tls — the implementer must restore an explicit `default-features = false` + curated feature list, and re-document the verification.

MSRV stays at 1.75. async-openai 0.40 declares `rust-version = "1.75"`, matching. If a future bump raises MSRV, we bump `[workspace.package].rust-version` per the CLAUDE.md rule, not downgrade the dep.

## Out of scope

YAGNI for SMA-316:

- Azure-AD-specific token-refresh authentication flow. Azure works today via `base_url` + the `bearer(token)` builder for a pre-minted token; auto-refreshing AAD credentials are a follow-up if asked.
- Request-level tracing instrumentation beyond what async-openai emits. Spans live on `RunContext::Tracer`.
- Provider-level retry. Per ADR-10, retries are a `RunConfig::retry_policy` concern.
- Fine-tuning, embeddings, image-gen, audio-gen, files API. Out of `Model`-trait scope.
- `Tool::output_schema()` translation. OpenAI does not accept return-payload schemas.
- **`LlmAgent::run` integration with `previous_response_id`.** The agent loop accumulates the full conversation and re-sends it each turn. For `previous_response_id` to be useful through `LlmAgent`, the loop would need response-id tracking and per-turn message trimming — non-trivial. SMA-316 ships the field as caller-managed (direct `Model::invoke` callers only); automated `LlmAgent` integration is a separate follow-up ticket.
- **Hand-authored `oneOf: [_, {type: "null"}]` collapse in the strict-schema rewriter.** The proc-macro path emits schemars 1.x output which uses `type: ["_", "null"]` natively. Defer until a user files an issue.

## Known open questions

- **Provider-specific knobs on shared `ModelSettings`.** `previous_response_id` is OpenAI-Responses-specific and lives on the shared `ModelSettings` struct for SMA-316. If SMA-317 (Anthropic) introduces its own provider-specific knobs (e.g. extended-thinking budget, cache control), revisit before landing them: introduce a typed `ProviderExtensions` enum or `extensions: HashMap<String, serde_json::Value>` rather than accreting more single-provider fields onto the shared struct. One field is below the threshold to abstract; two is the threshold to act.

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
