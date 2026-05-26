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

### Cross-crate changes

**Core crate (`paigasus-helikon-core`):** one new `ModelCapabilities` field —

```rust
// crates/paigasus-helikon-core/src/model.rs — ModelCapabilities adds:
/// Provider supports prompt caching of repeated request prefixes.
pub prompt_caching: bool,
```

Plus the matching `pub const fn with_prompt_caching(mut self) -> Self`.

**OpenAI provider crate (`paigasus-helikon-providers-openai`):** backfill `prompt_caching: true` on every entry in `KNOWN_MODELS` that OpenAI documents as cache-eligible — at minimum `gpt-4o`, `gpt-4o-mini`, `gpt-4.1`, `gpt-4.1-mini`, `o1`, `o1-mini`, `o3`, `o3-mini`, `gpt-5`. Without this backfill, the new field defaults to `false` for OpenAI models that genuinely support automatic prefix caching, which would lie about provider behavior. The implementer cross-checks against OpenAI's current docs at implementation time (treat as part of the same chore-PR class as the Anthropic `KNOWN_MODELS` verification).

Those are the **only** cross-crate changes. `ModelSettings`, `ModelEvent`, `ResponseFormat`, `ToolChoice`, `Item`, `ContentPart` are unchanged — the SMA-316 shapes already cover everything Anthropic needs:

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
    /// Override the `anthropic-version` header. Default: `"2023-06-01"`
    /// (the only documented stable version as of 2026-05; Anthropic uses
    /// additive versioning — new features land on the same string).
    pub fn anthropic_version(mut self, v: impl Into<String>) -> Self;
    /// Append a value to the `anthropic-beta` header. Multiple calls
    /// accumulate; the header is rendered as a comma-separated list at
    /// `build()` time. A single call containing a comma-separated string
    /// (e.g. `.beta("foo,bar")`) is treated as a literal one-value entry
    /// and concatenated as-is — callers wanting two values must make two
    /// calls.
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
    /// `thinking: { type: "enabled", budget_tokens: N }`. Anthropic requires
    /// `budget_tokens < max_tokens` (no documented absolute minimum).
    /// **Deprecated on Sonnet/Opus 4.6 and unsupported on Opus 4.7** — those
    /// models require `Adaptive`. The provider does not enforce this at
    /// build time (capability tables aren't fine-grained enough); a 400
    /// from Anthropic surfaces as `ModelError::Other`.
    Enabled { budget_tokens: u32 },
    /// `thinking: { type: "adaptive" }`. Required by Claude Opus 4.7,
    /// recommended for Sonnet/Opus 4.6+. Model picks the thinking budget.
    Adaptive,
}

/// Construction-time errors. Runtime errors flow through ModelError.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum BuildError {
    #[error("ANTHROPIC_API_KEY not set in environment")]
    MissingApiKey,
    #[error("invalid base URL: {0}")]
    InvalidBaseUrl(String),
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
  "tool_choice": {"type": "auto"|"any"|"none"|"tool", "name": "..."},
  "stream": true,
  "thinking": {"type": "enabled", "budget_tokens": 8192},
  "temperature": 0.7, "top_p": 0.95, "top_k": 40
}
```

### Messages — `Vec<Item>` → Anthropic input

| `Item` variant | Maps to Anthropic |
| --- | --- |
| `System { content }` | Gathered into top-level `system:` field. Multiple `System` items concatenate in order, but **all `System` items collapse into the single top-level slot regardless of their position in the `Vec<Item>`** — Anthropic has no in-conversation system role. Mid-conversation `System` items lose ordering relative to surrounding user/assistant turns. String form when no media + no cache; block-array form when `CacheStrategy::System`/`SystemAndTools` is set (last block carries `cache_control`). |
| `UserMessage { content }` | `{role: "user", content: [<blocks>]}`. `ContentPart::Text → {type: "text", text}`; `Image → {type: "image", source: {type: "base64"\|"url", ...}}`; `ToolResult → {type: "tool_result", tool_use_id, content}` (native nesting — no hoisting needed). |
| `AssistantMessage { content, agent }` | `{role: "assistant", content: [<blocks>]}`. `Text → {type: "text", text}`; `ToolUse → {type: "tool_use", id, name, input}`; `Reasoning → ` **always dropped with `tracing::warn!(target: "paigasus::anthropic", "dropping ContentPart::Reasoning on input — signature round-trip not yet supported")`**. Anthropic rejects unsigned `thinking` blocks on input, and `ContentPart::Reasoning` carries no signature field today, so signed round-trip is impossible without a core-trait change. Tracked as follow-up. `agent` attribution dropped (no Anthropic slot). |
| `ToolCall { call_id, name, args }` | Folds into preceding `AssistantMessage` as `{type: "tool_use", id: call_id, name, input: args}`. Standalone `ToolCall`s (no preceding `AssistantMessage` carrier in the same turn) synthesize a `{role: "assistant", content: [<tool_use blocks>]}` carrier — same rule as SMA-316's OpenAI translator. |
| `ToolResult { call_id, content }` | Hoists into the **immediately-following** `{role: "user", content: [...]}` message as a `{type: "tool_result", tool_use_id: call_id, content}` block at the front of that message's content array. When no `UserMessage` follows in the `Vec<Item>`, synthesize a new `{role: "user", content: [<tool_result blocks>]}` turn. Adjacent `ToolResult`s coalesce into one user turn — never two consecutive `user` roles (Anthropic rejects them). When the input is `[AssistantMessage(tool_use), ToolResult, UserMessage(text)]`, the result is `[assistant(tool_use), user(tool_result + text)]`, with the tool_result blocks preceding the text blocks. |
| `MediaSource::Base64 { mime_type, data }` | `{type: "base64", media_type: mime_type, data}` inside an image block. |
| `MediaSource::Url { url }` | `{type: "url", url}` inside an image block. **Per-model URL support varies** — older Claude 3 models (`claude-3-opus-20240229`, `claude-3-sonnet-20240229`, `claude-3-haiku-20240307`) accept base64 only and 400 on URL inputs. The provider does not pre-validate; callers must base64-encode when targeting those models. |

### Tools — `ToolDef` → Anthropic tool

`{name, description, input_schema: schema}`. No `additionalProperties` rewrite (Anthropic accepts permissive schemas, unlike OpenAI strict mode). `Tool::output_schema()` is ignored — Anthropic does not accept a return-payload schema.

`cache_control` markers added by `translate/cache.rs` per the active `CacheStrategy`.

### Response format → forced-tool synthesis

When `ModelRequest::model_settings.response_format` is set to `JsonSchema` or `JsonObject`:

1. **Request build:** synthesize an extra tool:
   - `name`: `"__paigasus_structured_output__"` (constant — reserved name; see conflict guards below)
   - `description`: `format!("Return data matching the {name} schema.", name = <caller's schema name>)` for `JsonSchema`; `"Return a JSON object."` for `JsonObject`
   - `input_schema`: caller's schema for `JsonSchema`; `{"type": "object"}` for `JsonObject`

   Append it to `tools` (after any user-supplied tools). Set `tool_choice: {"type": "tool", "name": "__paigasus_structured_output__"}`.
2. **Stream translation:** `MessageTranslator` records the synthesized tool's block index when `content_block_start` arrives with `name == "__paigasus_structured_output__"`. Subsequent `input_json_delta`s for that index emit `TokenDelta { text: <delta> }`. The synthesized block emits **no** `ToolCallDelta` — the caller sees plain text.
3. **Finish:** `stop_reason: "tool_use"` is rewritten to `FinishReason::Stop` when only the synthesized tool fired. When real tools also fired in the same turn, return `ModelError::Other(anyhow!("structured output: model fired both a real tool and the synthesized output tool"))` — invariant violation surfaced rather than swallowed.
4. **Conflict guards (synchronous, before HTTP call, all return `ModelError::Other` with a descriptive message):**
   - `response_format = JsonSchema/JsonObject` AND `tool_choice = Tool { name }` → mutually exclusive.
   - `response_format = JsonSchema/JsonObject` AND any user-supplied `ToolDef` with `name == "__paigasus_structured_output__"` → reserved-name collision (a user tool with the synthesized tool's name would silently shadow the synthesizer's tool when forced-tool dispatch fires). The check runs even when `response_format` is not set, because a stray user tool with that name would still pollute the request schema — defense in depth.

`ResponseFormat::Text` is a no-op (no synthesis, no `tool_choice` override). `strict` is implicit on the synthesized-tool path (Anthropic enforces `input_schema`).

### Settings passthrough

| `ModelSettings` field | Anthropic mapping |
| --- | --- |
| `temperature` | passthrough |
| `top_p` | passthrough |
| `max_output_tokens` | `max_tokens` (Anthropic requires it). When caller leaves it unset, fall back to the **per-model `max_output_default`** stored alongside the `KNOWN_MODELS` capability row (e.g. 32K for Claude Sonnet 4.6, 32K for Opus 4.7, 8K for Haiku 4.5, 4K for Claude 3 family). Unknown models fall back to a conservative `4096`. Document the chosen value in the rustdoc for `ModelSettings::max_output_tokens` so users discover the truncation cause when they hit `FinishReason::Length`. |
| `tool_choice: Auto` | `{"type": "auto"}` |
| `tool_choice: Required` | `{"type": "any"}` |
| `tool_choice: None` | `{"type": "none"}` (native — verified against current Anthropic Messages API). `tools` is still sent so the request body's prefix matches the prior turn's, preserving prompt-cache hits when `CacheStrategy::Tools` or `SystemAndTools` is active. |
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

**Cache write minimum.** Anthropic's prompt cache will not write entries below a per-model minimum (~1024 tokens for Sonnet, ~2048 for Opus, larger for Haiku). When a `CacheStrategy` enum is active but the marked prefix is smaller than the minimum, no cache entry is created and `cache_creation_input_tokens` remains 0 — caching is a no-op without an error. Document this in the `CacheStrategy` rustdoc so users understand why short prefixes don't hit the cache.

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
| `error` (in-stream) | terminate stream with `ModelError` via the **shared `map_error_type(status: Option<u16>, error_type: &str, message: &str, retry_after: Option<u64>) → ModelError` helper** that the HTTP error path also calls (see "Error mapping"). The stream path passes `status: None`; the helper still routes `overloaded_error → Unavailable`, `rate_limit_error → RateLimited { retry_after_ms: None }`, and so on. Default fallback is `Transport(message)`. |

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
    /// True when ResponseFormat::JsonSchema/JsonObject was set on the request.
    /// The synthesized tool's name is a compile-time constant — no need to
    /// store it; matching against the constant when content_block_start
    /// arrives is sufficient.
    synthesizing_output: bool,
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

Both the HTTP-response path and the in-stream `error` event path route through a single helper:

```rust
fn map_error_type(
    status: Option<u16>,
    error_type: &str,
    message: &str,
    retry_after_ms: Option<u64>,
) -> ModelError {
    match (status, error_type) {
        (_, "overloaded_error")              => ModelError::Unavailable,
        (_, "rate_limit_error")              => ModelError::RateLimited { retry_after_ms },
        (_, "authentication_error")
        | (_, "permission_error")            => ModelError::Refused { reason: message.to_owned() },
        (_, "invalid_request_error") if message.contains("prompt is too long")
                                             => ModelError::ContextLengthExceeded,
        (Some(s), _) if (500..=504).contains(&s) | matches!(s, 529)
                                             => ModelError::Unavailable,
        (Some(_), _)                         => ModelError::Other(anyhow!("anthropic {error_type}: {message}")),
        (None, _)                            => ModelError::Transport(message.to_owned()),
    }
}
```

The HTTP path provides `status` from the response code and parses `retry-after` (in seconds → milliseconds) from the response headers on 429. The stream path provides `status: None` and `retry_after_ms: None` (Anthropic does not send a retry-after value in mid-stream `error` events). This means a mid-stream `overloaded_error` correctly maps to `ModelError::Unavailable` rather than falling through to `Transport`.

Two deliberate calls, both mirroring SMA-316:
- **401/403 → `Refused`** rather than a new `AuthFailed` variant. Non-retryable per ADR-10 — correct semantic for bad credentials.
- **Generic 5xx → `Unavailable`**. Application-layer retry policies (`RunConfig::retry_policy`) handle revival.

`parse_retry_after_header` reads the `retry-after` response header — Anthropic sends it consistently on 429. Header value is in seconds; we multiply by 1000.

## Capabilities table

The Anthropic crate's capability lookup returns a `(ModelCapabilities, u32)` pair — capabilities + a `max_output_default` token cap. The default is consulted when `ModelSettings::max_output_tokens` is `None`.

```rust
struct ModelEntry {
    caps: ModelCapabilities,
    max_output_default: u32,
}

pub(crate) const KNOWN_MODELS: &[(&str, ModelEntry)] = &[
    // Claude 4 family — primary lineup as of 2026-05
    ("claude-opus-4-7",        /* streaming, tools, parallel, structured_output, vision, reasoning, prompt_caching | max_output 32768 */),
    ("claude-opus-4-6",        /* same | 32768 */),
    ("claude-opus-4-5",        /* same | 32768 */),
    ("claude-opus-4-1",        /* same | 32768 */),
    ("claude-sonnet-4-6",      /* same | 32768 */),
    ("claude-sonnet-4-5",      /* same | 32768 */),
    ("claude-haiku-4-5",       /* streaming, tools, parallel, structured_output, vision, prompt_caching | 8192 */),
    // Claude 3.5 family (vision varies — haiku has no vision)
    ("claude-3-5-sonnet-latest",   /* streaming, tools, parallel, structured_output, vision, prompt_caching | 8192 */),
    ("claude-3-5-sonnet-20241022", /* same | 8192 */),
    ("claude-3-5-haiku-latest",    /* streaming, tools, parallel, structured_output, prompt_caching | 8192 */),
    ("claude-3-5-haiku-20241022",  /* same | 8192 */),
    // Claude 3 family — older; URL-form image inputs may 400 (use base64)
    ("claude-3-opus-latest",       /* streaming, tools, structured_output, vision, prompt_caching | 4096 */),
    ("claude-3-opus-20240229",     /* streaming, tools, structured_output, vision | 4096 */),
    ("claude-3-sonnet-20240229",   /* streaming, tools, structured_output, vision | 4096 */),
    ("claude-3-haiku-20240307",    /* streaming, tools, structured_output, vision | 4096 */),
];

pub(crate) const fn conservative_defaults() -> ModelEntry {
    ModelEntry {
        caps: ModelCapabilities::empty().with_streaming().with_tools(),
        max_output_default: 4096,
    }
    // parallel/structured/vision/reasoning/prompt_caching deliberately off
}
```

- `parallel_tool_calls`: `true` for 3.5+ and 4-family; `false` for older 3-family.
- `structured_output`: `true` everywhere except where the model has no `tools` capability — our path is forced-tool synthesis, which any tool-capable model supports.
- `reasoning`: `true` only on Claude 4 family (extended thinking / adaptive thinking).
- `prompt_caching`: `true` on 3.5+, 4-family, and `claude-3-opus-latest` / `claude-3-opus-20240229`. The two older 3-family Sonnet/Haiku entries are `false`.
- `vision`: `true` on 4-family, 3.5-sonnet, 3-family Opus, 3-Sonnet, 3-Haiku. `false` on 3.5-Haiku (no vision support in that model).

`with_capabilities(...)` override always wins (operates on `ModelCapabilities` only; `max_output_default` is not overridable — `ModelSettings::max_output_tokens` is the per-call escape hatch).

**Table verification.** Ids above and their `max_output_default` token counts are illustrative; the implementer MUST cross-check each entry against Anthropic's published model docs at implementation time, mirroring the SMA-316 discipline. Capability claims and `max_output_default` values that diverge from official docs are bugs and updates are low-ceremony chore-PRs. The Claude 4 lineup in particular evolves quickly (Opus 4.7 deprecates manual-mode thinking — see `ExtendedThinking::Adaptive`).

## Testing strategy

### Unit tests (in-crate)

- `translate/request.rs` — `Item` → Anthropic body for: text-only user turn; assistant with nested `ContentPart::ToolUse`; standalone `ToolCall` synthesis (assistant carrier); `ToolResult` hoisted into next user turn; adjacent `ToolResult`s coalesce into one user turn; **`[Assistant(tool_use), ToolResult, UserMessage(text)]` produces `[assistant(tool_use), user(tool_result + text)]` with no consecutive user roles**; **trailing `ToolResult` with no following `UserMessage` synthesizes a user turn carrying just the tool_result block**; mid-conversation `Item::System` collapses into the top-level slot (order-loss confirmation); image URL + base64; **`ContentPart::Reasoning` is always dropped on input with `tracing::warn!`** (no signed round-trip yet); `previous_response_id` ignored with debug log; `tool_choice::None` emits native `{"type": "none"}` and keeps `tools` in the body.
- `translate/cache.rs` — each `CacheStrategy` variant places markers correctly; `SystemAndTools` with empty system or empty tools does not insert `cache_control` into nothing; `Tools` with structured-output synthesis places marker on last user tool (not synthesized); `Tools` with zero user tools + structured output places marker on synthesized tool.
- `translate/response_format.rs` — `JsonSchema` synthesizes the forced tool; `JsonObject` synthesizes the `{"type":"object"}` tool; `Text` no-op; mutual-exclusion with `ToolChoice::Tool` returns `ModelError::Other` synchronously; **a user `ToolDef` named `__paigasus_structured_output__` returns `ModelError::Other` synchronously regardless of whether `response_format` is set** (reserved-name guard).
- `translate/tools.rs` — passthrough fidelity (no schema rewriting).
- `error.rs` — table-driven `(status, error_type)` → `ModelError` via the shared `map_error_type` helper; in-stream `error` event with `error.type = overloaded_error` correctly maps to `Unavailable` (not `Transport`); retry-after header parse.
- `capabilities.rs` — known id lookup; unknown id falls through to conservative defaults; `with_capabilities` override wins; `KNOWN_MODELS` has no duplicate ids.
- `stream.rs` — `MessageTranslator` consumes individual `content_block_*` events: text deltas, parallel tool calls interleaved by block index, thinking deltas, name emitted only once per tool block, synthesized-tool `input_json_delta` → `TokenDelta` mapping, `signature_delta` dropped, `stop_reason: tool_use` rewrites to `Stop` when only synthesized tool fired and to `ToolCalls` otherwise.
- `settings.rs` — `ExtendedThinking::Enabled { budget_tokens: 100 }` builds without `BuildError` (validation deferred to Anthropic); `ExtendedThinking::Adaptive` builds without `BuildError`; the `thinking:` payload in the serialized request body matches the variant.
- `builder.rs` — env-var fallback (`ANTHROPIC_API_KEY`); explicit `api_key` / `bearer` bypass env; invalid base URL → `InvalidBaseUrl`; `with_capabilities` override.

### Wire integration tests (`tests/`, wiremock)

- `messages_wire.rs` — non-streaming happy path; tool-use response; 429 with `retry-after` header; 529 overloaded → `Unavailable`; 400 prompt-too-long → `ContextLengthExceeded`; in-stream `refusal` → `ModelError::Refused`; `anthropic-version` and `anthropic-beta` headers present and correct.
- `messages_streaming.rs` — hand-authored SSE fixtures under `tests/fixtures/`:
  - `text_only.txt` — message_start → text deltas → message_delta(stop) → message_stop
  - `parallel_tool_use.txt` — two interleaved `tool_use` blocks
  - `thinking_then_text.txt` — thinking_delta → text_delta sequence (4-family)
  - `tool_use_then_continuation.txt` — **multi-turn tool-use exchange (acceptance criterion)**. **Two SSE streams concatenated in the fixture**, separated by a `# --- turn 2 ---` comment line. The test invokes `Model::invoke` twice: the first stream ends with `stop_reason: "tool_use"` and a `Finish { reason: ToolCalls }`; the test then constructs a continuation `ModelRequest` containing the assistant's tool_use + a synthesized `ToolResult` Item and invokes the model again, consuming the second stream which contains the final text response and `stop_reason: "end_turn"`. The wiremock mock returns the two responses in sequence based on request-body matchers (presence of `tool_result` in the second request's `messages`).
  - `stream_error.txt` — mid-stream `error` event → `ModelError`
  JSON content asserted via `insta::assert_json_snapshot!`. Header comment notes the wiremock limitation: fixtures serve as one buffer; tests prove byte-level correctness, not resilience to slow chunk delivery.
- `prompt_caching.rs` — **acceptance criterion test**. Wiremock serves two responses:
  - Turn 1: response with `usage.cache_creation_input_tokens: 2048, cache_read_input_tokens: 0`.
  - Turn 2: same request prefix (system + tools + first user/assistant pair) + a new user message; response with `usage.cache_creation_input_tokens: 0, cache_read_input_tokens: 2048`.
  Token counts are chosen above Anthropic's documented per-model write minimum (~1024 Sonnet, ~2048 Opus) so the assertion remains realistic against any model. Test asserts: (a) both request bodies contain `cache_control: {"type": "ephemeral"}` markers in the expected positions, captured via wiremock matchers; (b) the second `ModelEvent::Usage.cached_input_tokens == Some(2048)`; (c) `ModelCapabilities::prompt_caching == true` for the model.
- `structured_output.rs` — `ResponseFormat::JsonSchema { schema, .. }` flow end-to-end: request body contains the synthesized tool + `tool_choice: {type: "tool", name: "__paigasus_structured_output__"}`; SSE stream of `input_json_delta`s emits `TokenDelta`s (no `ToolCallDelta`); `stop_reason: tool_use` rewrites to `FinishReason::Stop`; caller-set `ToolChoice::Tool` simultaneously returns `ModelError::Other` synchronously.

### Live integration tests (`tests/live.rs`)

Every test `#[ignore]` + guarded by `if std::env::var("ANTHROPIC_API_KEY").is_err() { return; }`:
- messages smoke (text-only turn);
- tool-call round-trip;
- structured-output round-trip (JsonSchema);
- streaming round-trip;
- cache-strategy round-trip — two sequential invocations with identical system+tools, prefix sized ≥ 2048 tokens (above Anthropic's documented per-model write minimum so the cache actually writes), assert `cached_input_tokens > 0` on the second `Usage`. If the live-test environment cannot construct a 2048-token prefix reproducibly, the assertion soft-fails with `tracing::info!("cache_prefix_too_small")` and the test passes — caching at < write-minimum is a documented no-op, not a regression.

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
- **Signed round-trip for `ContentPart::Reasoning`.** `ContentPart::Reasoning` is currently `Reasoning { text: String }` with no `signature` field. Anthropic rejects unsigned `thinking` blocks on input, so the translator always drops `ContentPart::Reasoning` on input today. Full signed round-trip needs both: (a) a `signature: Option<String>` field on `ContentPart::Reasoning` in core, and (b) the SSE translator capturing `signature_delta` events into a `BlockState::Thinking` accumulator that emits a final `Reasoning { text, signature }` on `content_block_stop`. Both are core-trait changes — out of scope for SMA-317; tracked as a follow-up ticket. Until then, callers wanting to round-trip extended-thinking responses must hold provider-native blocks themselves.
- **Schema rewriting / strict-mode for tool `input_schema`.** Anthropic accepts permissive schemas natively — no strict-mode rewriter needed.
- **`Tool::output_schema()` translation.** Anthropic does not accept a return-payload schema (same as OpenAI).
- **`stop_sequences`, `metadata.user_id`.** Easy to add later if a user asks; not load-bearing for the agent loop.

## Known open questions

- **Per-call overrides for Anthropic-specific knobs.** `extended_thinking`, `top_k`, `cache_strategy`, and `beta` headers are all baked into the `AnthropicModel` at `build()` time. Rebuilding per-call is cheap (no I/O) but `AnthropicModel` is typically held inside `LlmAgent.model: Arc<M>` and isn't easily swapped per-turn. The practical impact: workloads with variable thinking budgets (complex turns want more) or rolling cache strategies can't tune per-call without re-architecting how the agent holds its model. The SMA-316 follow-up question on `ModelSettings::provider_extensions: HashMap<String, Value>` or a typed `ProviderExtensions` enum is the natural next ticket if a user files this.
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
