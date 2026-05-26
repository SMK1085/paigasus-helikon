# SMA-317 — Anthropic provider design

**Status:** Design approved 2026-05-26 — implementation pending.
**Linear:** [SMA-317](https://linear.app/smaschek/issue/SMA-317/anthropic-provider-messages-streaming-tool-use-prompt-caching)
**Branch:** `feature/sma-317-anthropic-provider-messages-streaming-tool-use-prompt`

## Purpose

Second concrete `Model` implementation for the Paigasus Helikon SDK. Wraps Anthropic's Messages API (`POST /v1/messages`) and translates between Paigasus carrier types (`ModelRequest`, `ModelEvent`, `Item`, `Tool`) and Anthropic's wire protocol. Demonstrates provider switching at runtime against the SMA-316 OpenAI provider — same `Model` trait, capabilities-flagged differences.

Lands the missing `ModelCapabilities::prompt_caching` flag that SMA-316 anticipated but did not need. Closes the SMA-316-flagged open question on provider-specific knob placement by keeping Anthropic-only configuration off `ModelSettings` and on the `AnthropicModelBuilder` instead.

## Architectural decisions

### Wire layer — hand-rolled on reqwest + eventsource-stream

No Anthropic equivalent to async-openai has the upstream cadence to keep up with Anthropic's beta-feature shape (cache_control granularity, extended thinking budget format, beta headers). We sit directly on `reqwest::Client` for the request and `eventsource-stream` for SSE parsing. Wire-format snapshot tests (an acceptance criterion) are simpler when we own the JSON shape, and the API surface is small enough that hand-rolling stays cheaper than vendoring upstream churn.

`reqwest` is already a workspace dep (`async-openai` brings it in transitively, and SMA-316 added the explicit pin). `eventsource-stream` is the only new dep; it is futures-based, has no retry/reconnect logic (we honor ADR-10's "no provider-level retry"), and declares `rust-version = "1.65"`, comfortably under our 1.75 MSRV.

The community crates `clust` and `async-anthropic` were evaluated and rejected: both lag Anthropic's beta cadence on the precise features SMA-317 needs.

### Type shape — single `AnthropicModel`, builder-baked settings

Anthropic has one endpoint family (`/v1/messages`), so no backend-enum split. `AnthropicModel::messages(model_id) → AnthropicModelBuilder → AnthropicModel`.

Anthropic-specific knobs (`CacheStrategy`, `ExtendedThinking`, `top_k`, `beta` headers, API-version override) live on the builder and are baked into the `AnthropicModel` at `build()` time. Per-call overrides require rebuilding the model — cheap, no I/O. This keeps `ModelRequest::model_settings` cross-provider and resolves the SMA-316 open question without growing the core surface.

### Cross-crate change — one new `ModelCapabilities` field

```rust
// crates/paigasus-helikon-core/src/model.rs — ModelCapabilities adds:
/// Provider supports prompt caching of repeated request prefixes.
pub prompt_caching: bool,
```

Plus the matching `pub const fn with_prompt_caching(mut self) -> Self`. That is the **only** core change. `ModelSettings`, `ModelEvent`, `ResponseFormat`, `ToolChoice`, `Item`, `ContentPart` are unchanged — the SMA-316 shapes already cover everything Anthropic needs:

- `ModelEvent::Usage::cached_input_tokens` exists (added by SMA-316 with Anthropic in mind).
- `ModelEvent::ReasoningDelta` exists.
- `ContentPart::ToolUse` / `ContentPart::ToolResult` already model Anthropic's native nesting.
- `ContentPart::Reasoning` exists.

### Structured output — transparent forced-tool synthesis

`ResponseFormat::JsonSchema { schema, name, strict }` and `ResponseFormat::JsonObject` are implemented by synthesizing a private tool (`__paigasus_structured_output__`) with the caller's schema as `input_schema`, appending it to the request's `tools` array, and setting `tool_choice: {type: "tool", name: "__paigasus_structured_output__"}`. The streaming translator recognizes the synthesized tool by name and remaps its `input_json_delta`s into `TokenDelta` events — the caller never sees a `ToolCallDelta` for it.

This matches the OpenAI provider's observable behavior: same `ResponseFormat` produces same caller-visible result on either provider. Preserves ADR-1's "one trait, capabilities-flagged" promise.

### Prompt caching — opt-in via `CacheStrategy` enum

Default is `CacheStrategy::None` — no `cache_control` markers, body byte-identical to the uncached path, no surprising billing or TTL behavior. Caller opts in via `.cache_strategy(...)` on the builder. The variants (`System`, `Tools`, `SystemAndTools`, `LastTurn`) cover the documented Anthropic placements at MVP scope; the enum is `#[non_exhaustive]` so combinations can land later.

`ModelCapabilities::prompt_caching` advertises **support**, not active use. A model with `prompt_caching: true` and `CacheStrategy::None` still claims `prompt_caching: true`.

## Crate layout — `paigasus-helikon-providers-anthropic`

```
crates/paigasus-helikon-providers-anthropic/src/
├── lib.rs                  # re-exports
├── model.rs                # AnthropicModel + impl Model
├── builder.rs              # AnthropicModelBuilder, BuildError
├── capabilities.rs         # KNOWN_MODELS table, lookup, conservative_defaults
├── error.rs                # status + error-body → ModelError mapping
├── http.rs                 # auth headers, reqwest client wrap
├── sse.rs                  # SSE event parsing (eventsource-stream wiring)
├── settings.rs             # CacheStrategy, ExtendedThinking enums
├── stream.rs               # MessageTranslator: SSE events → ModelEvent
└── translate/
    ├── mod.rs              # shared helpers
    ├── request.rs          # Vec<Item> + ModelRequest → Anthropic body
    ├── tools.rs            # ToolDef → Anthropic tool shape
    ├── cache.rs            # CacheStrategy → cache_control placement
    └── response_format.rs  # ResponseFormat::JsonSchema → synthesized forced tool

tests/
├── messages_wire.rs        # wiremock non-streaming
├── messages_streaming.rs   # wiremock SSE fixtures
├── prompt_caching.rs       # second-turn cached-token assertion
├── structured_output.rs    # forced-tool synthesis round-trip
├── live.rs                 # ANTHROPIC_API_KEY-gated #[ignore]
└── fixtures/
    ├── text_only.txt
    ├── parallel_tool_use.txt
    ├── thinking_then_text.txt
    ├── tool_use_then_continuation.txt
    └── stream_error.txt
```

Public surface re-exported through `lib.rs`: `AnthropicModel`, `AnthropicModelBuilder`, `BuildError`, `CacheStrategy`, `ExtendedThinking`.

## Public API

```rust
impl AnthropicModel {
    /// Construct a Messages-API model builder.
    pub fn messages(model_id: impl Into<String>) -> AnthropicModelBuilder;
}

impl AnthropicModelBuilder {
    /// Set the API key explicitly. If unset, `build()` reads `ANTHROPIC_API_KEY`
    /// from the process environment. Mutually exclusive with `bearer`; last-set wins.
    pub fn api_key(mut self, key: impl Into<String>) -> Self;
    /// Use a pre-minted bearer token (Bedrock / Vertex AI proxy). Mutually
    /// exclusive with `api_key`; last-set wins.
    pub fn bearer(mut self, token: impl Into<String>) -> Self;
    /// Override the base URL. Default: `https://api.anthropic.com`.
    pub fn base_url(mut self, url: impl Into<String>) -> Self;
    /// Override the `anthropic-version` header. Default: `"2023-06-01"`.
    pub fn anthropic_version(mut self, v: impl Into<String>) -> Self;
    /// Append a value to the `anthropic-beta` header (comma-separated).
    pub fn beta(mut self, header: impl Into<String>) -> Self;
    /// Use a caller-provided reqwest::Client (custom timeouts, proxies, etc.).
    pub fn http_client(mut self, c: reqwest::Client) -> Self;
    /// Prompt-caching strategy. Default: `CacheStrategy::None`.
    pub fn cache_strategy(mut self, s: CacheStrategy) -> Self;
    /// Extended-thinking configuration (Claude 4 family). Default: `Disabled`.
    pub fn extended_thinking(mut self, t: ExtendedThinking) -> Self;
    /// Set the `top_k` sampling parameter. Anthropic-specific (no ModelSettings slot).
    pub fn top_k(mut self, k: u32) -> Self;
    /// Override the capability snapshot. Wins over the built-in model lookup table.
    pub fn with_capabilities(mut self, c: ModelCapabilities) -> Self;
    /// Resolve auth, validate inputs, look up capabilities, produce AnthropicModel.
    pub fn build(self) -> Result<AnthropicModel, BuildError>;
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum CacheStrategy {
    /// No cache_control markers. Body byte-identical to the uncached path.
    #[default]
    None,
    /// Mark the final block of `system` as a cache breakpoint.
    System,
    /// Mark the final tool in `tools` as a cache breakpoint.
    Tools,
    /// Mark both system and the last tool.
    SystemAndTools,
    /// Mark the final message in `messages` (rolling cache).
    LastTurn,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ExtendedThinking {
    /// No `thinking` field in request. Default.
    Disabled,
    /// `thinking: { type: "enabled", budget_tokens: N }`. Anthropic requires N ≥ 1024.
    Enabled { budget_tokens: u32 },
}

/// Construction-time errors. Runtime errors flow through ModelError.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum BuildError {
    #[error("ANTHROPIC_API_KEY not set in environment")]
    MissingApiKey,
    #[error("invalid base URL: {0}")]
    InvalidBaseUrl(String),
    #[error("extended-thinking budget must be >= 1024")]
    InvalidThinkingBudget,
}
```

`AnthropicModel::messages("claude-sonnet-4-5").build()?` reads `ANTHROPIC_API_KEY`, targets `https://api.anthropic.com/v1/messages`, looks up capabilities in `KNOWN_MODELS`, applies no cache markers. `base_url` override is the seam for Bedrock-proxy / Vertex AI / custom-gateway deployments — combined with `bearer`, it handles the common adjacent-provider cases without code in this crate.

## Wire translation

### Request body shape (Anthropic Messages API)

```json
{
  "model": "claude-sonnet-4-5",
  "max_tokens": 4096,
  "system": "..." | [{"type": "text", "text": "...", "cache_control": {"type": "ephemeral"}}],
  "messages": [{"role": "user"|"assistant", "content": [<blocks>]}],
  "tools": [{"name": "...", "description": "...", "input_schema": {...}, "cache_control": {...}}],
  "tool_choice": {"type": "auto"|"any"|"tool", "name": "..."},
  "stream": true,
  "thinking": {"type": "enabled", "budget_tokens": 8192},
  "temperature": 0.7, "top_p": 0.95, "top_k": 40
}
```

### Messages — `Vec<Item>` → Anthropic input

| `Item` variant | Maps to Anthropic |
| --- | --- |
| `System { content }` | Gathered into top-level `system:` field. Multiple `System` items concatenate. String form when no media + no cache; block-array form when `CacheStrategy::System`/`SystemAndTools` is set (last block carries `cache_control`). |
| `UserMessage { content }` | `{role: "user", content: [<blocks>]}`. `ContentPart::Text → {type: "text", text}`; `Image → {type: "image", source: {type: "base64"|"url", ...}}`; `ToolResult → {type: "tool_result", tool_use_id, content}` (native nesting — no hoisting needed). |
| `AssistantMessage { content, agent }` | `{role: "assistant", content: [<blocks>]}`. `Text → {type: "text", text}`; `ToolUse → {type: "tool_use", id, name, input}`; `Reasoning → {type: "thinking", thinking, signature}` when round-tripping a previous response with extended thinking enabled (otherwise dropped with `tracing::warn!` because Anthropic rejects unsigned thinking blocks on input). `agent` attribution dropped (no Anthropic slot). |
| `ToolCall { call_id, name, args }` | Folds into preceding `AssistantMessage` as `{type: "tool_use", id: call_id, name, input: args}`. Standalone `ToolCall`s (no preceding `AssistantMessage` carrier in the same turn) synthesize a `{role: "assistant", content: [<tool_use blocks>]}` carrier — same rule as SMA-316's OpenAI translator. |
| `ToolResult { call_id, content }` | Hoists into the next `{role: "user", content: [...]}` message as a `{type: "tool_result", tool_use_id: call_id, content}` block. Adjacent `ToolResult`s coalesce into a single user turn (Anthropic's documented pattern). |
| `MediaSource::Base64 { mime_type, data }` | `{type: "base64", media_type: mime_type, data}` inside an image block. |
| `MediaSource::Url { url }` | `{type: "url", url}` inside an image block. |

### Tools — `ToolDef` → Anthropic tool

`{name, description, input_schema: schema}`. No `additionalProperties` rewrite (Anthropic accepts permissive schemas, unlike OpenAI strict mode). `Tool::output_schema()` is ignored — Anthropic does not accept a return-payload schema.

`cache_control` markers added by `translate/cache.rs` per the active `CacheStrategy`.

### Response format → forced-tool synthesis

When `ModelRequest::model_settings.response_format` is set to `JsonSchema` or `JsonObject`:

1. **Request build:** synthesize an extra tool:
   - `name`: `"__paigasus_structured_output__"` (constant)
   - `description`: `format!("Return data matching the {name} schema.", name = <caller's schema name>)` for `JsonSchema`; `"Return a JSON object."` for `JsonObject`
   - `input_schema`: caller's schema for `JsonSchema`; `{"type": "object"}` for `JsonObject`

   Append it to `tools` (after any user-supplied tools). Set `tool_choice: {"type": "tool", "name": "__paigasus_structured_output__"}`.
2. **Stream translation:** `MessageTranslator` records the synthesized tool's block index when `content_block_start` arrives with `name == "__paigasus_structured_output__"`. Subsequent `input_json_delta`s for that index emit `TokenDelta { text: <delta> }`. The synthesized block emits **no** `ToolCallDelta` — the caller sees plain text.
3. **Finish:** `stop_reason: "tool_use"` is rewritten to `FinishReason::Stop` when only the synthesized tool fired. When real tools also fired in the same turn, return `ModelError::Other(anyhow!("structured output: model fired both a real tool and the synthesized output tool"))` — invariant violation surfaced rather than swallowed.
4. **Conflict guard:** caller setting both `response_format = JsonSchema/JsonObject` AND `tool_choice = Tool { name }` returns `ModelError::Other(anyhow!("response_format and tool_choice::Tool are mutually exclusive on Anthropic"))` synchronously, before the HTTP call.

`ResponseFormat::Text` is a no-op (no synthesis, no `tool_choice` override). `strict` is implicit on the synthesized-tool path (Anthropic enforces `input_schema`).

### Settings passthrough

| `ModelSettings` field | Anthropic mapping |
| --- | --- |
| `temperature` | passthrough |
| `top_p` | passthrough |
| `max_output_tokens` | `max_tokens` (Anthropic requires it; default to `4096` when caller leaves it unset) |
| `tool_choice: Auto` | `{"type": "auto"}` |
| `tool_choice: Required` | `{"type": "any"}` |
| `tool_choice: None` | omit (Anthropic has no native "none"; we forbid tool emission by omitting `tools` from the body when this is set) |
| `tool_choice: Tool { name }` | `{"type": "tool", "name": name}` |
| `response_format` | see "forced-tool synthesis" above |
| `previous_response_id` | ignored with `tracing::debug!(target: "paigasus::anthropic", "previous_response_id is Anthropic-irrelevant; ignoring")` |

## Cache-control placement (`translate/cache.rs`)

`CacheStrategy::None` — no markers. Body byte-identical to a no-cache request.

`CacheStrategy::System` — last block of `system:` gets `cache_control: {"type": "ephemeral"}`. Forces system into block-array form even for a single text block.

`CacheStrategy::Tools` — last tool in `tools[]` gets `cache_control: {"type": "ephemeral"}`. With structured-output synthesis active, the marker goes on the last **user-provided** tool, before the synthesized output tool. When the caller supplies zero user tools but enables this strategy + structured output, the marker goes on the synthesized tool — any cache anchor is better than none, and this fallback is tested.

`CacheStrategy::SystemAndTools` — both placements above. Used by the prompt-caching acceptance test.

`CacheStrategy::LastTurn` — last block of the final `messages[]` entry gets the marker. Useful for long conversations where the prefix changes but the recent turns stabilize.

Anthropic caps cache breakpoints at 4 per request. Our placements top out at 3 (system + tools + last turn), well within the limit. The enum is `#[non_exhaustive]` so future strategies (combinations, custom-block markers) can land additively.

## Streaming SSE → `ModelEvent`

Anthropic emits one SSE event per stream chunk:

| Server event | `ModelEvent` |
| --- | --- |
| `message_start` | `Usage { input_tokens, cached_input_tokens: cache_read_input_tokens, output_tokens: 0, reasoning_tokens: None }` |
| `content_block_start` (type=`text`) | internal: register block index as text |
| `content_block_delta` (`text_delta`) | `TokenDelta { text }` |
| `content_block_start` (type=`thinking`) | internal: register block index as thinking |
| `content_block_delta` (`thinking_delta`) | `ReasoningDelta { text }` |
| `content_block_delta` (`signature_delta`) | dropped with `tracing::debug!` (signatures are opaque to consumers; round-trip preservation is out of scope for streaming) |
| `content_block_start` (type=`tool_use`, id, name) | internal: register block index as tool_use, mark name un-emitted |
| `content_block_delta` (`input_json_delta`) for a tool_use block | `ToolCallDelta { call_id, name: Some on first delta / None after, args_delta }` — **unless** this is the synthesized output-tool block, in which case `TokenDelta { text: args_delta }` |
| `content_block_stop` | dropped with `tracing::debug!` |
| `message_delta` with `usage.output_tokens` | `Usage { input_tokens: last seen, cached_input_tokens: last seen, output_tokens: usage.output_tokens, reasoning_tokens: None }` |
| `message_delta` with `stop_reason` | stash; emit `Finish` after the trailing `Usage` |
| `message_stop` | flush pending `Finish` |
| `ping` | dropped |
| `error` (in-stream) | terminate stream with `ModelError` mapped from `error.type` + `error.message` |

`stop_reason` mapping:
- `"end_turn"` → `FinishReason::Stop`
- `"max_tokens"` → `FinishReason::Length`
- `"tool_use"` → `FinishReason::ToolCalls` (rewritten to `Stop` when only the synthesized output tool fired)
- `"stop_sequence"` → `FinishReason::Stop`
- `"refusal"` → terminate stream with `ModelError::Refused { reason }` (model-level refusal is not a normal `Finish`)
- other / unknown → `FinishReason::Other(reason)`

**Cancellation:** identical `tokio::select! { biased; _ = cancel.cancelled() => return, n = upstream.next() => n }` shape as SMA-316. On cancel the response body is dropped (closing the TCP/TLS connection) and the stream ends without `Finish` — runner already handles this per ADR-10.

`MessageTranslator` state:

```rust
struct MessageTranslator {
    blocks: HashMap<u32, BlockState>,
    last_input_tokens: u32,
    last_cached_input_tokens: Option<u32>,
    stop_reason: Option<StopReason>,
    /// Set at construction when ResponseFormat::JsonSchema/JsonObject was used.
    synthesized_tool_name: Option<String>,
    /// Records the block index of the synthesized output tool once seen.
    synthesized_tool_index: Option<u32>,
}

enum BlockState {
    Text,
    Thinking,
    ToolUse { call_id: String, name: String, name_emitted: bool },
}
```

## Error mapping (`error.rs`)

Anthropic returns `{"type": "error", "error": {"type": "...", "message": "..."}}` with an HTTP status. Mapping:

| HTTP status | `error.type` | → `ModelError` |
| --- | --- | --- |
| 400 | `invalid_request_error` with message containing `"prompt is too long"` | `ContextLengthExceeded` |
| 400 | `invalid_request_error` (other) | `Other(anyhow!("anthropic invalid_request: {msg}"))` |
| 401 | `authentication_error` | `Refused { reason }` |
| 403 | `permission_error` | `Refused { reason }` |
| 404 | `not_found_error` | `Other(anyhow!(...))` |
| 429 | `rate_limit_error` | `RateLimited { retry_after_ms: parse_retry_after_header(&resp) }` |
| 500–504 | `api_error` | `Unavailable` |
| 529 | `overloaded_error` | `Unavailable` |
| any | reqwest network/TLS error | `Transport(re.to_string())` |
| any | JSON deserialization of response body fails | `Other(anyhow!("malformed anthropic response: {je}"))` |

In-stream `error` SSE events use the same mapping minus the status code (default `Transport(error.message)` when no other rule fires).

Two deliberate calls, both mirroring SMA-316:
- **401/403 → `Refused`** rather than a new `AuthFailed` variant. Non-retryable per ADR-10 — correct semantic for bad credentials.
- **Generic 5xx → `Unavailable`**. Application-layer retry policies (`RunConfig::retry_policy`) handle revival.

`parse_retry_after_header` reads the `retry-after` response header — Anthropic sends it consistently on 429. Header value is in seconds; we multiply by 1000.

## Capabilities table

```rust
pub(crate) const KNOWN_MODELS: &[(&str, ModelCapabilities)] = &[
    // Claude 3.5 family
    ("claude-3-5-sonnet-latest",
     /* streaming, tools, parallel, structured_output, vision, prompt_caching */),
    ("claude-3-5-sonnet-20241022",   /* same */),
    ("claude-3-5-haiku-latest",
     /* streaming, tools, parallel, structured_output, prompt_caching */),
    ("claude-3-5-haiku-20241022",    /* same */),
    // Claude 3 family
    ("claude-3-opus-latest",
     /* streaming, tools, structured_output, vision, prompt_caching */),
    ("claude-3-opus-20240229",       /* same */),
    ("claude-3-sonnet-20240229",
     /* streaming, tools, structured_output, vision */),
    ("claude-3-haiku-20240307",
     /* streaming, tools, structured_output, vision */),
    // Claude 4 family (extended thinking on Sonnet/Opus)
    ("claude-sonnet-4-5",
     /* streaming, tools, parallel, structured_output, vision, reasoning, prompt_caching */),
    ("claude-opus-4",                /* same */),
];

pub(crate) const fn conservative_defaults() -> ModelCapabilities {
    ModelCapabilities::empty().with_streaming().with_tools()
    // parallel/structured/vision/reasoning/prompt_caching deliberately off
}
```

- `parallel_tool_calls`: `true` for 3.5+ and 4-family; `false` for older 3-family.
- `structured_output`: `true` everywhere — our path is forced-tool synthesis, which any tool-capable model supports.
- `reasoning`: `true` only on Claude 4 family (extended thinking).
- `prompt_caching`: `true` on 3.5+ and 4-family. The two older 3-family Sonnet/Haiku entries are `false`.

`with_capabilities(...)` override always wins. **Table verification.** Ids above are illustrative; the implementer MUST cross-check each entry against Anthropic's published model docs at implementation time, mirroring the SMA-316 discipline. Capability claims that diverge from official docs are bugs and updates are low-ceremony chore-PRs.

## Testing strategy

### Unit tests (in-crate)

- `translate/request.rs` — `Item` → Anthropic body for: text-only user turn; assistant with nested `ContentPart::ToolUse`; standalone `ToolCall` synthesis (assistant carrier); `ToolResult` hoisted into next user turn; adjacent `ToolResult`s coalesce into one user turn; system-block concatenation across multiple `Item::System`; image URL + base64; reasoning round-trip with signature; reasoning drop with `tracing::warn!` when signature missing; `previous_response_id` ignored with debug log; `tool_choice::None` omits `tools` from body.
- `translate/cache.rs` — each `CacheStrategy` variant places markers correctly; `SystemAndTools` with empty system or empty tools does not insert `cache_control` into nothing; `Tools` with structured-output synthesis places marker on last user tool (not synthesized); `Tools` with zero user tools + structured output places marker on synthesized tool.
- `translate/response_format.rs` — `JsonSchema` synthesizes the forced tool; `JsonObject` synthesizes the `{"type":"object"}` tool; `Text` no-op; mutual-exclusion with `ToolChoice::Tool` returns `ModelError::Other` synchronously.
- `translate/tools.rs` — passthrough fidelity (no schema rewriting).
- `error.rs` — table-driven status→`ModelError`; in-stream `error` event; retry-after header parse.
- `capabilities.rs` — known id lookup; unknown id falls through to conservative defaults; `with_capabilities` override wins; `KNOWN_MODELS` has no duplicate ids.
- `stream.rs` — `MessageTranslator` consumes individual `content_block_*` events: text deltas, parallel tool calls interleaved by block index, thinking deltas, name emitted only once per tool block, synthesized-tool `input_json_delta` → `TokenDelta` mapping, `signature_delta` dropped, `stop_reason: tool_use` rewrites to `Stop` when only synthesized tool fired and to `ToolCalls` otherwise.
- `settings.rs` — `ExtendedThinking::Enabled { budget_tokens: 512 }` rejected by `build()` as `InvalidThinkingBudget`; `≥ 1024` accepted.
- `builder.rs` — env-var fallback (`ANTHROPIC_API_KEY`); explicit `api_key` / `bearer` bypass env; invalid base URL → `InvalidBaseUrl`; `with_capabilities` override.

### Wire integration tests (`tests/`, wiremock)

- `messages_wire.rs` — non-streaming happy path; tool-use response; 429 with `retry-after` header; 529 overloaded → `Unavailable`; 400 prompt-too-long → `ContextLengthExceeded`; in-stream `refusal` → `ModelError::Refused`; `anthropic-version` and `anthropic-beta` headers present and correct.
- `messages_streaming.rs` — hand-authored SSE fixtures under `tests/fixtures/`:
  - `text_only.txt` — message_start → text deltas → message_delta(stop) → message_stop
  - `parallel_tool_use.txt` — two interleaved `tool_use` blocks
  - `thinking_then_text.txt` — thinking_delta → text_delta sequence (4-family)
  - `tool_use_then_continuation.txt` — **multi-turn tool-use exchange (acceptance criterion)**
  - `stream_error.txt` — mid-stream `error` event → `ModelError`
  JSON content asserted via `insta::assert_json_snapshot!`. Header comment notes the wiremock limitation: fixtures serve as one buffer; tests prove byte-level correctness, not resilience to slow chunk delivery.
- `prompt_caching.rs` — **acceptance criterion test**. Wiremock serves two responses:
  - Turn 1: response with `usage.cache_creation_input_tokens: 200, cache_read_input_tokens: 0`.
  - Turn 2: same request prefix (system + tools + first user/assistant pair) + a new user message; response with `usage.cache_creation_input_tokens: 0, cache_read_input_tokens: 200`.
  Test asserts: (a) both request bodies contain `cache_control: {"type": "ephemeral"}` markers in the expected positions, captured via wiremock matchers; (b) the second `ModelEvent::Usage.cached_input_tokens == Some(200)`; (c) `ModelCapabilities::prompt_caching == true` for the model.
- `structured_output.rs` — `ResponseFormat::JsonSchema { schema, .. }` flow end-to-end: request body contains the synthesized tool + `tool_choice: {type: "tool", name: "__paigasus_structured_output__"}`; SSE stream of `input_json_delta`s emits `TokenDelta`s (no `ToolCallDelta`); `stop_reason: tool_use` rewrites to `FinishReason::Stop`; caller-set `ToolChoice::Tool` simultaneously returns `ModelError::Other` synchronously.

### Live integration tests (`tests/live.rs`)

Every test `#[ignore]` + guarded by `if std::env::var("ANTHROPIC_API_KEY").is_err() { return; }`:
- messages smoke (text-only turn);
- tool-call round-trip;
- structured-output round-trip (JsonSchema);
- streaming round-trip;
- cache-strategy round-trip — two sequential invocations with identical system+tools assert `cached_input_tokens > 0` on the second `Usage`.

Skipped in CI. Documented in `CONTRIBUTING.md`: set `ANTHROPIC_API_KEY` and `cargo test -p paigasus-helikon-providers-anthropic -- --ignored`.

Fixtures are raw SSE bytes (`event: <name>\ndata: {...}\n\n`), `include_str!`-loaded. Hand-authored — they double as wire-format documentation.

## Facade wiring

Cargo entry already exists in `crates/paigasus-helikon/Cargo.toml`:

```toml
anthropic = ["dep:paigasus-helikon-providers-anthropic"]
paigasus-helikon-providers-anthropic = { workspace = true, optional = true }
```

Add to `crates/paigasus-helikon/src/lib.rs`:

```rust
/// Anthropic provider — [`paigasus-helikon-providers-anthropic`].
#[cfg(feature = "anthropic")]
pub use paigasus_helikon_providers_anthropic as anthropic;
```

Doc-comment on the `pub use` so `-D warnings` on the docs job passes (workspace `missing_docs = "warn"` rule).

## Dependencies

Added to `[workspace.dependencies]`:

```toml
eventsource-stream = "0.2"   # SSE parsing; lightweight, futures-based, no retry/reconnect
```

All other deps already in workspace (`reqwest`, `serde`, `serde_json`, `tokio`, `tokio-util`, `tracing`, `async-trait`, `async-stream`, `futures-core`, `futures-util`, `thiserror`, `anyhow`, `wiremock`, `insta`).

MSRV stays at 1.75. `eventsource-stream` 0.2 declares `rust-version = "1.65"`, comfortably under.

## Out of scope

YAGNI for SMA-317:

- **`LlmAgent::run` integration with `cache_strategy`.** Caching works transparently when the agent loop accumulates the conversation and re-sends each turn (cache hits on the static prefix). Surfacing `cache_read_input_tokens` to the agent loop for cost telemetry is a follow-up.
- **Per-call cache-control override.** All cache markers are determined at `build()` time. A per-call override would require either a new `ModelSettings` field (rejected — exactly the problem SMA-316 deferred) or threading through `ModelRequest`. Defer.
- **`cache_creation_input_tokens` as a separate `Usage` field.** Single `cached_input_tokens` covers the hit-count, which is what callers actually want. A write-count slot is a `ModelEvent` shape change for thin marginal value.
- **AWS Bedrock and Google Vertex AI native shims.** Different auth flow, different base URL, and for Bedrock a different body shape (`anthropic_version` in body, no `model` top-level). The `bearer` + `base_url` builders make a thin adapter possible if a user files a request, but native crate-level support is out of scope.
- **Anthropic batch API and files API.** Not `Model`-trait scope.
- **Signature round-trip preservation for `ContentPart::Reasoning` on streaming output.** When the caller round-trips a previous response, signatures on input thinking blocks are preserved. Capturing signatures from a *streaming response* (so the caller can build the next request's input from the events alone, without buffering the full assistant message) is a separate concern; the current shape requires the caller to hold a snapshot of the assistant message.
- **Schema rewriting / strict-mode for tool `input_schema`.** Anthropic accepts permissive schemas natively — no strict-mode rewriter needed.
- **`Tool::output_schema()` translation.** Anthropic does not accept a return-payload schema (same as OpenAI).
- **`stop_sequences`, `metadata.user_id`.** Easy to add later if a user asks; not load-bearing for the agent loop.

## Known open questions

- **Per-call beta-header overrides.** `beta(...)` on the builder appends to a list baked at `build()` time. If a future feature (e.g. a per-request feature flag) requires per-call beta headers, we'll need a `ModelRequest` extension point. Defer until a concrete need lands.
- **Cache breakpoint at >4.** Anthropic caps cache breakpoints at 4 per request. Our placements top out at 3 (System + Tools + LastTurn). If `LastTurn` later combines with per-message rolling markers, the validator in `translate/cache.rs` must reject placements beyond 4 with `ModelError::Other`. Not relevant for MVP scope.

## Acceptance criteria (ticket-restated)

- `AnthropicModel` implements `Model`. ✓ (`model.rs`)
- Messages API with system prompt + interleaved tool_use / tool_result blocks. ✓ (`translate/request.rs` — native `ContentPart::ToolUse`/`ToolResult` nesting; `ToolResult` hoisting into next user turn)
- Streaming SSE → `MessageStart`, `ContentBlockDelta` (text + tool_use + thinking), `MessageStop`. ✓ (`stream.rs` — `MessageTranslator`)
- Structured output via single forced tool. ✓ (`translate/response_format.rs` + synthesized-tool path)
- Prompt-caching support (cache_control on system + tool blocks) surfaced in `ModelCapabilities`. ✓ (`translate/cache.rs` + new `ModelCapabilities::prompt_caching` flag)
- `ANTHROPIC_API_KEY` env or builder param. ✓ (`builder.rs`)
- Wire-format snapshot tests for a multi-turn tool-use exchange. ✓ (`messages_streaming.rs`, fixture `tool_use_then_continuation.txt`)
- Caching reduces input token count in the second turn, verifiable via mock server. ✓ (`prompt_caching.rs`)
