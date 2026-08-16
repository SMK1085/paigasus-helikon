# SMA-451 — LiteLLM provider (`paigasus-helikon-providers-litellm`)

- **Issue:** [SMA-451](https://linear.app/smaschek/issue/SMA-451) — Provider crate: `paigasus-helikon-providers-litellm`
- **Split from:** SMA-329 (delivered Bedrock; PR #120). Sibling follow-ups: SMA-449 (Gemini, delivered), SMA-450 (Ollama, open).
- **Status:** Design — pending GATE 1 approval
- **Date:** 2026-08-16

## 1. Goal

Add a self-contained crate implementing a **LiteLLM proxy** provider for the
Paigasus Helikon SDK, behind a `litellm` Cargo feature on the facade. It
implements `paigasus_helikon_core::Model` with the same public surface shape as
the other providers (`LiteLlmModel` + builder), passes a wire-format snapshot
suite at scenario parity with the OpenAI/Anthropic/Gemini providers, and is
wired into the facade — mirroring the brand-new-crate packaging pattern Bedrock
and Gemini established.

### 1.1 Why this crate exists at all

`OpenAiModelBuilder::base_url()` already exists, and its own doc comment names
LiteLLM as the use case
(`crates/paigasus-helikon-providers-openai/src/builder.rs:72`). The OpenAI
crate's `conservative_defaults()` also names LiteLLM in its rationale. So
"point the OpenAI provider at a LiteLLM proxy" is already possible, and the
design has to justify a second crate rather than assume it.

It earns its place on three counts, in descending order of weight:

1. **LiteLLM's router controls cannot be expressed through `async-openai`.**
   `CreateChatCompletionRequest` (verified against async-openai 0.41.3) carries
   30 typed fields and **no generic extra-body escape hatch**. `fallbacks`,
   `num_retries`, `tags`, and arbitrary provider passthrough are therefore
   structurally unreachable from the OpenAI crate without forking its request
   type.
2. **Strict response enums are the wrong posture for a heterogeneous gateway.**
   `async-openai` deserializes `finish_reason` into a closed enum. A proxied
   backend emitting a stop reason OpenAI has never defined would fail
   deserialization and kill the whole stream. LiteLLM's entire purpose is
   fronting non-OpenAI backends, so **leniency is a feature here**, not a
   nicety.
3. **Capability resolution is fundamentally different.** Every other provider
   keys a hardcoded table on a known model id. LiteLLM model names are
   arbitrary operator-chosen aliases (`prod-fast`, `team-a/gpt`), so no table
   can be correct, and the OpenAI crate's `KNOWN_MODELS` lookup is actively
   misleading when pointed at a proxy.

## 2. Decisions

| # | Decision | Rationale |
|---|---|---|
| D1 | **Own the LiteLLM wire surface** — a real self-contained provider, not a wrapper over `providers-openai`. | §1.1. A wrapper cannot carry router controls and inherits strict response enums. |
| D2 | **No `async-openai` dependency.** Own `reqwest` + `eventsource-stream` client with permissive `serde` chunk types. | Same as D1; matches the `providers-gemini` posture. |
| D3 | **Capabilities: conservative default + explicit `.with_capabilities()` override.** No table, no discovery. `build()` stays synchronous and performs no I/O. | Aliases defeat table lookup. `/v1/model/info` is documented-buggy for proxied models ([#11370](https://github.com/BerriAI/litellm/issues/11370), [#9297](https://github.com/BerriAI/litellm/issues/9297)) and would add a network round-trip plus a failure mode at construction. |
| D4 | **LiteLLM extras in v1: `fallbacks`, `num_retries`, `metadata`, `tags`, `extra_body`.** | Router controls are the headline justification; metadata/tags fit the repo's existing Langfuse observability story; `extra_body` future-proofs against LiteLLM params we did not model. |
| D5 | **`x-litellm-*` response headers are out of scope.** | `ModelEvent` has no carrier for response headers; adding one is a `paigasus-helikon-core` change that widens this ticket well beyond a provider crate. |
| D6 | **Duplicate the `Vec<Item>` → OpenAI-chat-`messages` mapping** rather than hoisting `providers-openai`'s `to_chat_messages` into core. | Keeps the PR to one new crate + facade wiring — no core public API in the same PR, so no same-PR core/facade bump ritual (CLAUDE.md). Also, LiteLLM fronts heterogeneous backends, so the two mappings should be free to diverge without regressing the OpenAI provider. Revisit when SMA-450 (Ollama) makes a third consumer. |
| D7 | **`base_url` is required**; explicit → `LITELLM_API_BASE` → `BuildError::MissingBaseUrl`. | No `http://localhost:4000` default: silently targeting a local port is a worse failure than a build error. |
| D8 | **Auth is optional.** No `MissingApiKey` variant; absence means no `Authorization` header. | Self-hosted LiteLLM commonly runs without `master_key` inside a cluster. A misconfigured deployment surfaces as a loud 401 → `ModelError::Refused` on first invoke. |
| D9 | **`extra_body` collisions are rejected at `build()`, never silently dropped.** | A silently-dropped `"model"` override is an expensive debugging afternoon. |
| D10 | **Chat Completions only.** Constructor named `chat()`, not `new()`. | Leaves room for a future `responses()` backend without a breaking rename, and echoes the `OpenAiModel::chat` surface the AC asks us to mirror. |
| D11 | **No new CI job.** `tests/live.rs` is env-gated and loud-skips. | A meaningful LiteLLM job needs a container plus at least one real upstream key. Per `integration.yml`'s own reasoning, a skipped test that passes is worse than no test — not worth it for a Low-priority provider. |

## 3. Scope

**In scope**

- New crate `paigasus-helikon-providers-litellm` at `version = "0.1.0"`, publishing normally.
- `LiteLlmModel` + `LiteLlmModelBuilder` + `BuildError`, implementing `paigasus_helikon_core::Model`.
- Streaming Chat Completions against a LiteLLM proxy, with cancellation.
- LiteLLM extras per D4.
- Facade wiring: optional dep, `litellm` feature, documented `pub use`.
- Crate README, facade README, root README, mdBook `concepts/model-providers.md` + `reference/crates.md`.
- Test suite per §10.

**Out of scope** — recorded as decisions, not omissions

- `/v1/responses` backend; embeddings, moderations, audio, image endpoints.
- `/v1/model/info` capability discovery (D3).
- `x-litellm-*` response-header surfacing (D5).
- Object-form `fallbacks` entries (per-fallback `messages` overrides). Only the
  simple `["model-name", …]` string form is supported.
- Batch / comma-separated model lists (`"model": "gpt-4,llama3"`).
- Non-streaming invocation. The provider always streams, matching every other
  provider in the workspace.

## 4. Module layout

Mirrors `providers-gemini`.

```
crates/paigasus-helikon-providers-litellm/
  Cargo.toml
  README.md
  .gitattributes            # *.snap text eol=lf ; tests/fixtures/*.txt text eol=lf
  src/
    lib.rs                  crate docs, re-exports
    builder.rs              LiteLlmModelBuilder, Config, BuildError
    capabilities.rs         conservative_defaults()  — no KNOWN_MODELS table
    error.rs                HTTP status + LiteLLM error body → ModelError
    model.rs                LiteLlmModel, impl Model
    transport.rs            base-URL normalisation + auth header
    sse.rs                  SSE framing, [DONE] sentinel
    stream.rs               ChatTranslator: chunk → ModelEvent
    translate/
      mod.rs
      request.rs            Item[] → messages (own copy, per D6)
      response_format.rs    ResponseFormat → response_format
      tools.rs              ToolDef → tools[]; delegates to core::schema::strict
      extras.rs             fallbacks/num_retries/metadata/tags/extra_body merge
      snapshots/            insta
  tests/
    litellm_wire.rs
    litellm_streaming.rs
    cancellation.rs
    live.rs
    fixtures/*.txt
```

## 5. Public API surface

```rust
/// LiteLLM proxy provider. `provider()` == "litellm"; `model()` == the alias.
pub struct LiteLlmModel { /* … */ }

impl LiteLlmModel {
    /// Chat Completions builder for a proxy model alias.
    pub fn chat(model_id: impl Into<String>) -> LiteLlmModelBuilder;

    /// One-call path: reads `LITELLM_API_BASE` and `LITELLM_API_KEY`.
    /// Per D8 the key is optional, so the only failures are
    /// `MissingBaseUrl` and `InvalidBaseUrl` — an unset `LITELLM_API_KEY`
    /// yields an unauthenticated model, not an error.
    pub fn from_env(model_id: impl Into<String>) -> Result<Self, BuildError>;
}

pub struct LiteLlmModelBuilder { /* … */ }

impl LiteLlmModelBuilder {
    // transport / auth
    pub fn base_url(self, url: impl Into<String>) -> Self;
    pub fn api_key(self, key: impl Into<String>) -> Self;   // last-set auth wins
    pub fn bearer(self, token: impl Into<String>) -> Self;  // last-set auth wins
    pub fn http_client(self, client: reqwest::Client) -> Self;

    // capabilities
    pub fn with_capabilities(self, caps: ModelCapabilities) -> Self;

    // LiteLLM extras
    pub fn fallbacks<I, S>(self, models: I) -> Self
        where I: IntoIterator<Item = S>, S: Into<String>;
    pub fn num_retries(self, n: u8) -> Self;
    pub fn metadata(self, key: impl Into<String>, value: impl Into<String>) -> Self;
    pub fn tags<I, S>(self, tags: I) -> Self
        where I: IntoIterator<Item = S>, S: Into<String>;
    pub fn extra_body(self, value: serde_json::Value) -> Self;

    pub fn build(self) -> Result<LiteLlmModel, BuildError>;
}

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum BuildError {
    /// Neither `.base_url()` nor `LITELLM_API_BASE` supplied one.
    MissingBaseUrl,
    /// `base_url` failed to parse as a URL.
    InvalidBaseUrl(String),
    /// `.extra_body()` (or `.metadata("tags", …)`) collided with a key the
    /// provider owns. Carries the dotted path, e.g. `"model"` or
    /// `"metadata.tags"`.
    ReservedExtraBodyKey(String),
    /// `.extra_body()` was given a non-object JSON value.
    ExtraBodyNotAnObject,
}
```

Usage:

```rust
let model = LiteLlmModel::chat("claude-sonnet-4")   // proxy alias, not an OpenAI id
    .base_url("http://litellm:4000")
    .api_key(key)
    .fallbacks(["gpt-4o-mini"])
    .num_retries(2)
    .tags(["team:research"])
    .metadata("trace_id", trace_id)
    .with_capabilities(
        ModelCapabilities::empty()
            .with_streaming()
            .with_tools()
            .with_vision(),
    )
    .build()?;
```

## 6. Transport (`transport.rs`)

### 6.1 Base-URL normalisation

LiteLLM serves both `/chat/completions` and `/v1/chat/completions`, and
operators write base URLs both ways. Rule: **trim a trailing `/`, strip a
trailing `/v1` if present, then append `/v1/chat/completions`.** `base_url` is
validated with `reqwest::Url::parse` at `build()`; failure →
`BuildError::InvalidBaseUrl`.

| `base_url` | resolved endpoint |
|---|---|
| `http://localhost:4000` | `http://localhost:4000/v1/chat/completions` |
| `http://localhost:4000/` | `http://localhost:4000/v1/chat/completions` |
| `http://localhost:4000/v1` | `http://localhost:4000/v1/chat/completions` |
| `http://localhost:4000/v1/` | `http://localhost:4000/v1/chat/completions` |
| `https://gw.example.com/litellm` | `https://gw.example.com/litellm/v1/chat/completions` |

Table-driven unit test, one row per case.

### 6.2 Auth

`Authorization: Bearer <key>` when a key resolves (explicit `.api_key()` /
`.bearer()`, else `LITELLM_API_KEY`); the header is **omitted entirely**
otherwise (D8). `.api_key()` and `.bearer()` produce the same header — they
differ only in intent and in which one was set last.

## 7. Request body

Assembled as a `serde_json::Value`. Always streaming, always `include_usage`.

```jsonc
{
  "model": "<alias>",
  "messages": [ … ],
  "stream": true,
  "stream_options": { "include_usage": true },

  "tools": [ … ],                  // only when request.tools is non-empty
  "tool_choice": "auto",           // only when ModelSettings.tool_choice is Some
                                   //   AND request.tools is non-empty; see §8
  "response_format": { … },        // only when ModelSettings.response_format is Some

  "temperature": 0.7,              // ModelSettings.temperature
  "top_p": 0.9,                    // ModelSettings.top_p
  "max_tokens": 512,               // ModelSettings.max_output_tokens

  "fallbacks": ["gpt-4o-mini"],    // LiteLLM router
  "num_retries": 2,                // LiteLLM router
  "metadata": {                    // LiteLLM observability
    "trace_id": "…",
    "tags": ["team:research"]
  }
}
```

Every optional field is omitted when unset — no explicit `null`s.

**`parallel_tool_calls` is never sent.** The conservative default is `false`,
but emitting `parallel_tool_calls: false` risks a 400 from proxied backends
that do not know the param (LiteLLM only strips unknown params when the
operator sets `drop_params`). The capability flag informs the agent loop; it
does not go on the wire.

**`tags` nest under `metadata.tags`**, not top-level. LiteLLM documents
top-level `tags` as legacy and `metadata.tags` as the shape supporting negation
(`!`) and required (`&`) prefixes. Consequently `"tags"` is a **reserved
metadata key**: `.metadata("tags", …)` fails at `build()` with
`ReservedExtraBodyKey("metadata.tags")` rather than being silently clobbered by
`.tags()`.

**`ModelSettings::previous_response_id` is ignored** — it is an OpenAI
Responses-API concept and this provider has no Responses backend. Documented in
the crate docs, consistent with how other non-OpenAI providers treat it.

### 7.1 `extra_body` merge rules (D9)

- Must be a JSON **object**, else `BuildError::ExtraBodyNotAnObject`.
- Keys merge at the **request root**.
- Any key the provider owns is **rejected at `build()`** with
  `BuildError::ReservedExtraBodyKey(key)`. Reserved set:

  `model`, `messages`, `stream`, `stream_options`, `tools`, `tool_choice`,
  `response_format`, `temperature`, `top_p`, `max_tokens`, `fallbacks`,
  `num_retries`, `metadata`

- Rejection is build-time and total: we never silently drop a caller's key, and
  we never let a caller's key override a field the provider computed
  per-request.

## 8. Tool choice + structured output

`translate/tools.rs` maps `ToolDef` → OpenAI `tools[]` entries
(`{"type":"function","function":{name,description,parameters}}`), delegating
schema normalisation to `paigasus_helikon_core::schema::strict` — the same
canonical normaliser the OpenAI provider uses, so there is no duplicated
schema logic (D6 duplicates only the message mapping).

`ToolChoice` maps as: `Auto` → `"auto"`, `Required` → `"required"`, `None` →
`"none"`, `Tool { name }` → `{"type":"function","function":{"name":…}}`.

`translate/response_format.rs` maps `ResponseFormat`: `Text` → field omitted,
`JsonObject` → `{"type":"json_object"}`, `JsonSchema { name, schema, strict }` →
`{"type":"json_schema","json_schema":{name,schema,strict}}` with the schema run
through `core::schema::strict` when `strict` is set. Unknown future variants
(the enum is `#[non_exhaustive]`) fall through to "no constraint".

Unlike Gemini, LiteLLM imposes **no conflict** between structured output and
active tools — the request is passed through and any incompatibility surfaces
as an upstream error from the proxied backend. We do not pre-reject.

## 9. Streaming translation (`sse.rs`, `stream.rs`)

POST with `Accept: text/event-stream`. Framing via `eventsource-stream`;
`data: [DONE]` terminates the stream.

Chunk types are deliberately permissive — every field `Option`, `finish_reason`
deserialized as a bare `String`, unknown fields ignored:

```rust
struct StreamChunk { choices: Vec<Choice>, usage: Option<Usage> }
struct Choice { index: u32, delta: Delta, finish_reason: Option<String> }
struct Delta {
    content: Option<String>,
    reasoning_content: Option<String>,     // LiteLLM normalises thinking here
    tool_calls: Option<Vec<ToolCallChunk>>,
}
```

Mapping:

| Chunk field | `ModelEvent` |
|---|---|
| `delta.content` | `TokenDelta { text }` |
| `delta.reasoning_content` | `ReasoningDelta { text }` |
| `delta.tool_calls[]` | `ToolCallDelta { call_id, name, args_delta }` — index-keyed; `name` on the first delta for a given index only, `None` thereafter |
| `finish_reason` | `Finish { reason }` — see below |
| final `usage` | `Usage { … }`, emitted immediately before `Finish` |

`reasoning_content` is why this provider gets reasoning streaming that the
Gemini provider still lacks: LiteLLM normalises Anthropic extended thinking and
DeepSeek reasoning into that one field.

**Finish-reason mapping is lenient** — this is the payoff for D2:

`stop` → `Stop`; `length` → `Length`; `tool_calls` | `function_call` →
`ToolCalls`; `content_filter` → `ContentFilter`; **anything else →
`FinishReason::Other(s)`**.

**Usage mapping**: `prompt_tokens` → `input_tokens`, `completion_tokens` →
`output_tokens`, `prompt_tokens_details.cached_tokens` →
`cached_input_tokens`, `completion_tokens_details.reasoning_tokens` →
`reasoning_tokens`. One terminal snapshot satisfies core's
cumulative-within-turn / last-wins contract trivially.

**Cancellation** follows the OpenAI provider exactly: `tokio::select!` with
`biased` on the cancel arm, at both the initial request future and each poll of
the upstream SSE stream.

## 10. Error classification (`error.rs`)

LiteLLM returns `{"error": {"message": …, "type": …, "code": …}}`.

| Condition | `ModelError` |
|---|---|
| HTTP 429 | `RateLimited { retry_after_ms }` — parsed from `Retry-After` seconds; the HTTP-date form yields `None` |
| HTTP 502 / 503 / 504, or connect-refused | `Unavailable` |
| `error.code == "context_length_exceeded"`, else message substring match | `ContextLengthExceeded` |
| HTTP 401 / 403, or `error.type == "content_policy_violation"` | `Refused { reason }` |
| other non-2xx | `Other(anyhow)` carrying status + body |
| `reqwest` transport failure | `Transport(String)` |

Order matters: the structured `code` is checked **before** the substring match,
because LiteLLM prefixes upstream errors (`litellm.BadRequestError: …`) and the
substring match is the fragile fallback, not the primary signal.

Mid-stream error frames (a `data:` payload carrying an `error` object rather
than `choices`) are mapped through the same classifier and yielded as a
terminal `Err` on the stream.

## 11. Capabilities (`capabilities.rs`)

No `KNOWN_MODELS` table (D3). One function:

```rust
pub(crate) const fn conservative_defaults() -> ModelCapabilities {
    ModelCapabilities::empty().with_streaming().with_tools()
}
```

`parallel_tool_calls` is intentionally unset — the same reasoning the OpenAI
crate already documents: most OpenAI-compatible proxies do not support parallel
tool calls, and a loop expecting multiple calls fails worse than one expecting
a single call.

`.with_capabilities()` wins outright when supplied. The crate README and the
mdBook state plainly that **declaring capabilities is the operator's job** on
this provider, because the alias carries no information the SDK can act on.

## 12. Testing strategy

1. **`insta` request snapshots** (`src/translate/snapshots/`) — serialized
   request body for: plain text turn; system prompt; tool declarations with
   each `ToolChoice` variant; tool call + tool result round-trip; structured
   output via strict `json_schema`; inline image part; and one snapshot per
   LiteLLM extra (`fallbacks`, `num_retries`, `metadata` incl. `metadata.tags`,
   `extra_body` merged at root).
2. **`tests/litellm_wire.rs`** (wiremock) — every row of §6.1's URL table;
   `Authorization` present when a key is configured and **absent** when none
   is; `Accept: text/event-stream`.
3. **`tests/litellm_streaming.rs`** — SSE fixtures → `ModelEvent` sequences:
   multi-chunk text; interleaved tool-call deltas across two indices;
   `reasoning_content` → `ReasoningDelta`; usage-then-finish ordering; an
   **unknown `finish_reason` landing in `FinishReason::Other`**; a mid-stream
   error frame.
4. **`tests/cancellation.rs`** — cancel before the request future resolves, and
   cancel mid-stream.
5. **Unit tests** — build-time rejections (`MissingBaseUrl`, `InvalidBaseUrl`,
   `ReservedExtraBodyKey` for each reserved key, `ExtraBodyNotAnObject`,
   reserved `metadata.tags`); `from_env` with and without the env vars; one
   error-mapping test per row of §10.
6. **`tests/live.rs`** — env-gated on `LITELLM_API_BASE`, loud-skips when
   unset. Same posture as `openai`/`gemini`/`bedrock`. No CI job (D11).

Env-mutating tests take a process-wide `Mutex` guard, matching the pattern in
`providers-openai/src/builder.rs`.

### 12.1 Fixture line endings

`crates/paigasus-helikon-providers-litellm/.gitattributes` gets
`*.snap text eol=lf`, and — because the streaming tests `include_str!` SSE
fixtures split on literal `\n` — the root `.gitattributes` rule currently
pinning `crates/paigasus-helikon-providers-anthropic/tests/fixtures/*.txt` is
extended to cover this crate. This is CLAUDE.md's standing instruction;
skipping it reproduces the Windows CRLF bug the rule exists to prevent.

## 13. Facade wiring + workspace

- `Cargo.toml` (root): add to `[workspace.dependencies]` —
  `paigasus-helikon-providers-litellm = { path = "crates/paigasus-helikon-providers-litellm", version = "0.1.0" }`.
  `members = ["crates/*", …]` is a glob, so no members edit.
- `crates/paigasus-helikon/Cargo.toml`: optional dep + `litellm = ["dep:paigasus-helikon-providers-litellm"]`.
- `crates/paigasus-helikon/src/lib.rs`: `pub use … as litellm` **with a `///` doc comment** — the docs job runs `RUSTDOCFLAGS=-D warnings` and an undocumented re-export fails it.
- Commit `Cargo.lock`.
- **No `release-plz.toml` change.** New crates publish through the normal flow;
  the stub `publish = false` / `release = false` ritual applies only to
  `0.0.0` placeholders, of which none remain.
- **No core bump and no manual facade bump.** D6 leaves core untouched, so
  release-plz's `dependencies_update` cascade handles the facade on its own.

Crate `Cargo.toml` dependencies: `paigasus-helikon-core`, `async-trait`,
`async-stream`, `eventsource-stream`, `futures-core`, `futures-util`,
`reqwest` (`json`, `stream`, `rustls`), `serde`, `serde_json`, `thiserror`,
`anyhow`, `tokio`, `tokio-util`, `tracing` — all `workspace = true`. Dev-deps:
`wiremock`, `insta` (`json`, `yaml`), `tokio`, `reqwest`. `[lints] workspace = true`.

## 14. Documentation

Per CLAUDE.md's two standing "keep it current" rules:

- **New** `crates/paigasus-helikon-providers-litellm/README.md` — the crates.io
  landing page. Must state the capability-declaration responsibility (§11) and
  the required `base_url` (D7). Snippets use `cargo add` (no hardcoded
  versions).
- `crates/paigasus-helikon/README.md` and root `README.md` — feature → module
  map gains `litellm`. **Note:** the facade README is `include_str!`'d into
  rustdoc, so any ```rust fence there is compiled as a doctest — a
  network/key-bearing example must be fenced ` ```ignore `.
- `docs/book/src/concepts/model-providers.md` — a LiteLLM section that states
  plainly **when to use this crate versus `OpenAiModel::base_url()`**: use the
  OpenAI provider when the proxy fronts OpenAI models and you want the
  `KNOWN_MODELS` capability table; use this crate when you need router
  fallbacks, retries, spend metadata, or non-OpenAI backends behind arbitrary
  aliases.
- `docs/book/src/reference/crates.md` — roster entry.
- `mdbook build docs/book` must stay clean (`warning-policy = "error"`).

## 15. Risks & mitigations

| Risk | Mitigation |
|---|---|
| **Duplicated message mapping drifts from the OpenAI provider's.** | Accepted per D6, with the divergence framed as intentional. Snapshot tests pin this crate's shape so drift is visible rather than silent. Revisit when SMA-450 (Ollama) becomes a third consumer. |
| **LiteLLM changes its extras' wire shape** (e.g. promotes top-level `tags` back, renames `num_retries`). | Extras are confined to `translate/extras.rs` with one snapshot each; a shape change is a single-file edit. `extra_body` gives operators an escape hatch that needs no release. |
| **`metadata.tags` vs top-level `tags` is a judgement call** on ambiguous upstream docs. | Documented explicitly in §7 and in the crate README, so a future reader sees a decision rather than an accident. `extra_body` cannot express the alternative (`tags` is reserved), which is a deliberate trade — noted here so it can be revisited if operators report otherwise. |
| **`Retry-After` HTTP-date form yields `None`.** | Accepted; core's `RateLimited.retry_after_ms` is already `Option`, and callers must handle `None` regardless. |
| **No live CI coverage** (D11). | Wiremock covers wire shape and the streaming translator; `tests/live.rs` exists for manual validation against a real proxy before release. |
| **Capability mis-declaration by the operator** (claiming `vision` a backend lacks). | Surfaces as an upstream 400 mapped to `Other`. Loudly documented as the operator's responsibility in §11's README/mdBook copy. |

## 16. Open questions to confirm at implementation time

1. **`eventsource-stream` behaviour on the `[DONE]` sentinel** — confirm it
   yields `[DONE]` as an ordinary `data` event (as it does for the Gemini
   provider's usage) rather than erroring, and terminate on it explicitly.
2. **`tool_calls` index semantics across proxied backends** — OpenAI keys
   streaming tool-call deltas by `index`. Confirm against a real LiteLLM proxy
   fronting Anthropic that `index` is populated and stable; if a backend omits
   it, fall back to `id` for correlation.
3. **Whether `num_retries` is honoured in the request body** on current LiteLLM
   versions, or only via the `x-litellm-num-retries` header. Upstream docs
   state the header "outranks a `num_retries` in the request body", implying
   the body field is valid — confirm on a live proxy, and if it is not, switch
   `.num_retries()` to emit the header instead (a `transport.rs`-local change).
