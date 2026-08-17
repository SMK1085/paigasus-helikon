# SMA-451 — LiteLLM provider (`paigasus-helikon-providers-litellm`)

- **Issue:** [SMA-451](https://linear.app/smaschek/issue/SMA-451) — Provider crate: `paigasus-helikon-providers-litellm`
- **Split from:** SMA-329 (delivered Bedrock; PR #120). Sibling follow-ups: SMA-449 (Gemini, delivered), SMA-450 (Ollama, open).
- **Status:** Design — revised after adversarial challenge **and verified against a live LiteLLM proxy**; pending GATE 1 approval
- **Date:** 2026-08-16
- **Wire claims verified against:** LiteLLM `1.97.0` (`ghcr.io/berriai/litellm:main-stable`, digest `sha256:468c25f3…`) running locally in Docker. Raw evidence in Appendix B.

## 1. Goal

Add a self-contained crate implementing a **LiteLLM proxy** provider for the
Paigasus Helikon SDK, behind a `litellm` Cargo feature on the facade. It
implements `paigasus_helikon_core::Model` with the same public surface shape as
the other providers (`LiteLlmModel` + builder), passes a wire-format snapshot
suite at scenario parity with the OpenAI/Anthropic/Gemini providers, and is
wired into the facade — mirroring the brand-new-crate packaging pattern Bedrock
and Gemini established.

### 1.1 Why this crate has the shape it does

SMA-451 mandates the crate, so this section justifies its *shape*, not its
existence. The relevant tension: `OpenAiModelBuilder::base_url()` already
exists and names LiteLLM in its own doc comment
(`crates/paigasus-helikon-providers-openai/src/builder.rs:72`). Two structural
facts about `async-openai` decide that a wrapper is not viable:

1. **LiteLLM's router controls cannot be expressed.**
   `CreateChatCompletionRequest` (verified against async-openai 0.41.3) carries
   30 typed fields and **no generic extra-body escape hatch**, so `fallbacks`,
   `num_retries`, and arbitrary provider passthrough are unreachable without
   forking its request type.
2. **Two response-side limits, both structural.**
   `ChatCompletionStreamResponseDelta` (async-openai 0.41.3, `chat_.rs:1140`)
   has exactly `content`, `function_call`, `tool_calls`, `role`, `refusal` —
   **no reasoning field**, so reasoning streaming through a proxy is
   impossible via the OpenAI crate no matter how it is configured. And
   `FinishReason` is a closed enum (`openai/src/backend/chat.rs:264-274`, whose
   own comment notes it is not `#[non_exhaustive]`), so a proxied backend
   emitting an undefined stop reason fails deserialization and kills the
   stream. LiteLLM exists to front non-OpenAI backends, so **leniency is a
   requirement here**, not a nicety.

Capability handling is *different* under LiteLLM but is deliberately **not**
claimed as a justification. `capabilities::lookup`
(`openai/src/capabilities.rs:154-160`) already falls through to
`conservative_defaults()` for unknown ids, and that function's doc comment
(`:24-33`) already names LiteLLM. The only real failure is an operator alias
that collides with a `KNOWN_MODELS` id, and `with_capabilities` already
overrides it. See §11 for what capabilities do and do not do in this codebase.

## 2. Decisions

| # | Decision | Rationale |
|---|---|---|
| D1 | **Own the LiteLLM wire surface** — a self-contained provider, not a wrapper over `providers-openai`. | §1.1. |
| D2 | **No `async-openai` dependency.** Own `reqwest` + `eventsource-stream` client with genuinely permissive `serde` chunk types (§9.1). | §1.1 #2; matches the `providers-gemini` posture. |
| D3 | **Capabilities: conservative default + explicit `.with_capabilities()` override.** No table, no discovery. `build()` stays synchronous and performs no I/O. | Aliases defeat table lookup. `/v1/model/info` is documented-buggy for proxied models ([#11370](https://github.com/BerriAI/litellm/issues/11370), [#9297](https://github.com/BerriAI/litellm/issues/9297)) and would add a network round-trip plus a failure mode at construction. |
| D4 | **LiteLLM extras in v1: `fallbacks`, `num_retries`, `metadata`, `tags`, `extra_body`, `header`.** | Router controls are the headline justification; `extra_body` + `header` are the escape hatches that make the uncertain wire shapes (§7.3) recoverable without a release. |
| D5 | **`x-litellm-*` response headers are not surfaced as typed API, but ARE logged and attached to errors** (§10.3). | `ModelEvent` has no header carrier (`core/src/model.rs:168-224`) and adding one is a core change. But `tracing::debug!` and `anyhow` context need no core change, and a routing chokepoint with no correlation id is not debuggable. |
| D6 | **Duplicate the OpenAI-chat translation into this crate**, rather than hoisting into core — *and port the OpenAI crate's unit tests verbatim alongside it* plus a cross-crate parity test (§13.1). | Keeps the PR to one new crate + facade wiring. The unqualified version of this decision was rejected at challenge: see §13.1 for the real duplication accounting and why a snapshot suite alone cannot detect drift. |
| D7 | **`base_url` is required**; explicit → `LITELLM_API_BASE` → `LITELLM_PROXY_API_BASE` → `BuildError::MissingBaseUrl`. Validation is `Url`-based and rejects non-`http(s)` schemes (§6.1). | No `http://localhost:4000` default: silently targeting a local port is worse than a build error. Scheme validation is required because `Url::parse("localhost:4000")` **succeeds** (scheme `localhost`, path `4000`), so bare `Url::parse` would admit the single most likely typo. |
| D8 | **Auth is optional.** No `MissingApiKey`; absence means no `Authorization` header. Empty/whitespace keys are treated as absent. | Self-hosted LiteLLM commonly runs without `master_key` inside a cluster. |
| D9 | **`extra_body` never silently drops a caller's key.** Keys the provider computes per-request are rejected at `build()`; the LiteLLM-extras keys are *not* reserved and merge with caller precedence (§7.2). | Preserves the principle while keeping the escape hatch genuinely usable — the unqualified reserved-set from the first draft walled off exactly the keys whose wire shape is least certain. |
| D10 | **Chat Completions only.** Constructor named `chat()`, not `new()`. | Leaves room for a future `responses()` backend without a breaking rename. |
| D11 | **No new CI job in this PR** — `tests/live.rs` stays env-gated and loud-skips. **But the original justification was wrong** and is retracted: see §14.1. | The first draft claimed a meaningful job "needs a container plus at least one real upstream key". Building the verification rig disproved that — LiteLLM's `mock_response` deployments serve full streaming responses with a fake key, so a keyless containerised job **is** feasible. Deferred to a follow-up rather than assumed impossible; flagged for GATE 1. |
| D12 | **`Finish` is emitted only at `[DONE]`/EOF, never inline with a chunk.** | Core's contract is "`Finish` is the terminal event; nothing follows it" (`core/src/model.rs:63`). With `include_usage`, usage arrives in a **separate trailing chunk after** the `finish_reason` chunk, so an inline `Finish` would be followed by `Usage`. Adopts the pattern already shipping in `gemini/src/stream.rs:73-87`. |

## 3. Scope

**In scope**

- New crate `paigasus-helikon-providers-litellm` at `version = "0.1.0"`, publishing normally.
- `LiteLlmModel` + `LiteLlmModelBuilder` + `BuildError`, implementing `paigasus_helikon_core::Model`.
- Streaming Chat Completions against a LiteLLM proxy, with cancellation.
- LiteLLM extras per D4.
- Facade wiring: optional dep, `litellm` feature, documented `pub use`.
- Documentation per §15; test suite per §13.

**Out of scope** — decisions, not omissions

- `/v1/responses` backend; embeddings, moderations, audio, image endpoints.
- `/v1/model/info` capability discovery (D3).
- `x-litellm-*` headers as *typed* API (D5 — they are still logged).
- Object-form `fallbacks` entries (per-fallback `messages` overrides). Only the
  simple `["model-name", …]` string form.
- Batch / comma-separated model lists (`"model": "gpt-4,llama3"`).
- Non-streaming invocation. The provider always streams.
- Multi-choice responses: `n > 1` is not supported (§9.4).

## 4. Module layout

Mirrors `providers-gemini`.

```
crates/paigasus-helikon-providers-litellm/
  Cargo.toml
  README.md
  .gitattributes            # *.snap text eol=lf
  src/
    lib.rs                  crate docs, re-exports
    builder.rs              LiteLlmModelBuilder, Config, BuildError
    capabilities.rs         conservative_defaults()  — no KNOWN_MODELS table
    error.rs                classify() — pure fn, status + body → ModelError
    model.rs                LiteLlmModel(Arc<Config>), impl Model
    transport.rs            base-URL normalisation, auth + extra headers
    sse.rs                  SSE framing, [DONE] sentinel
    stream.rs               ChatTranslator: chunk → ModelEvent, + finish()
    translate/
      mod.rs
      request.rs            Item[] → messages (duplicated, per D6/§13.1)
      response_format.rs    ResponseFormat → response_format
      tools.rs              ToolDef → tools[]; delegates to core::schema::strict
      extras.rs             fallbacks/num_retries/metadata/tags/extra_body merge
      snapshots/            insta
  tests/
    litellm_wire.rs
    streaming.rs
    cancellation.rs
    live.rs
    fixtures/*.txt
```

## 5. Public API surface

`LiteLlmModel` is `#[derive(Clone)]` over an `Arc<Config>`, mirroring
`GeminiModel(Arc<Config>)` (`gemini/src/model.rs:22-23`): `Model::invoke`
returns `BoxStream<'static, …>` (`core/src/model.rs:72`), so the config must be
cheaply cloned into the stream.

```rust
#[derive(Clone)]
pub struct LiteLlmModel(Arc<Config>);   // Debug is manual + redacting (§16)

impl LiteLlmModel {
    /// Chat Completions builder for a proxy model alias.
    pub fn chat(model_id: impl Into<String>) -> LiteLlmModelBuilder;

    /// One-call path: reads `LITELLM_API_BASE` (falling back to
    /// `LITELLM_PROXY_API_BASE`) and `LITELLM_API_KEY` (falling back to
    /// `LITELLM_PROXY_API_KEY`). Per D8 the key is optional, so the only
    /// failures are `MissingBaseUrl` and `InvalidBaseUrl` — an unset key
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
    /// Override the request path appended to `base_url`.
    /// Default `"/v1/chat/completions"`; escape hatch for gateways §6.1's
    /// normalisation gets wrong.
    pub fn chat_completions_path(self, path: impl Into<String>) -> Self;
    /// Arbitrary extra request header — the header-side escape hatch,
    /// e.g. `x-litellm-tags`, `x-litellm-timeout`.
    pub fn header(self, name: impl Into<String>, value: impl Into<String>) -> Self;

    // capabilities
    pub fn with_capabilities(self, caps: ModelCapabilities) -> Self;

    // LiteLLM extras
    pub fn fallbacks<I, S>(self, models: I) -> Self
        where I: IntoIterator<Item = S>, S: Into<String>;
    pub fn num_retries(self, n: u8) -> Self;
    pub fn metadata(self, key: impl Into<String>, value: impl Into<serde_json::Value>) -> Self;
    pub fn tags<I, S>(self, tags: I) -> Self
        where I: IntoIterator<Item = S>, S: Into<String>;
    pub fn extra_body(self, value: serde_json::Value) -> Self;

    pub fn build(self) -> Result<LiteLlmModel, BuildError>;
}

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum BuildError {
    /// Neither `.base_url()` nor `LITELLM_API_BASE`/`LITELLM_PROXY_API_BASE`.
    MissingBaseUrl,
    /// `base_url` failed to parse, used a scheme other than http/https,
    /// or carried a query string or fragment. Carries the offending input.
    InvalidBaseUrl(String),
    /// `.extra_body()` collided with a key the provider computes per-request.
    ReservedExtraBodyKey(String),
    /// `.extra_body()` was given a non-object JSON value.
    ExtraBodyNotAnObject,
    /// `.metadata("tags", …)` — use `.tags()` instead.
    ReservedMetadataKey(String),
    /// `.header()` name or value is not a valid HTTP header.
    InvalidHeader(String),
}
```

`metadata` takes `impl Into<serde_json::Value>` rather than `impl Into<String>`
so LiteLLM's nested metadata (Langfuse `session_id`, `spend_logs_metadata`,
numeric values) is expressible. A `String`-only signature would have been
strictly less expressive than the field it exists to expose.

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
operators write base URLs both ways.

**Algorithm — `Url`-based, not string-based.** Parse with `reqwest::Url::parse`;
reject any scheme other than `http`/`https`, and reject a non-empty query or
fragment (all → `InvalidBaseUrl`). Then operate on `path_segments_mut`: drop a
trailing empty segment, drop a trailing `v1` segment, and push the configured
`chat_completions_path` segments (default `v1`, `chat`, `completions`).

Scheme validation is load-bearing: `Url::parse("localhost:4000")` **succeeds**
with scheme `localhost` and path `4000`, so without it D7's "a loud build error
beats a silently wrong endpoint" claim would be false for the most common typo.
Operating on path segments rather than raw strings is likewise load-bearing:
string concatenation would turn `http://gw/litellm?key=x` into
`…?key=x/v1/chat/completions` (that input is now rejected outright, but the
segment API is what makes the rule total).

| `base_url` | resolved endpoint |
|---|---|
| `http://localhost:4000` | `http://localhost:4000/v1/chat/completions` |
| `http://localhost:4000/` | `http://localhost:4000/v1/chat/completions` |
| `http://localhost:4000/v1` | `http://localhost:4000/v1/chat/completions` |
| `http://localhost:4000/v1/` | `http://localhost:4000/v1/chat/completions` |
| `https://gw.example.com/litellm` | `https://gw.example.com/litellm/v1/chat/completions` |
| `localhost:4000` | `InvalidBaseUrl` (scheme not http/https) |
| `http://gw/litellm?key=x` | `InvalidBaseUrl` (query present) |

The trailing-`v1` strip is a heuristic and will be wrong for a gateway genuinely
mounted under a path segment named `v1`. `.chat_completions_path()` is the
documented escape hatch; the README says so.

Table-driven unit test, one row per case above.

### 6.2 Headers

- `Authorization: Bearer <key>` when a key resolves (explicit `.api_key()` /
  `.bearer()`, else `LITELLM_API_KEY`, else `LITELLM_PROXY_API_KEY`). Empty or
  whitespace-only keys are treated as **absent**, not sent as `Bearer ` — a
  malformed header would 401 and D8's "a loud 401 means a misconfigured
  deployment" framing would misattribute it.
- `Accept: text/event-stream`, `Content-Type: application/json`.
- `x-litellm-num-retries: <n>` when `.num_retries()` is set — see §7.3.
- Any `.header()` entries, applied last. `.header("authorization", …)` is
  permitted and overrides the resolved auth (it is an escape hatch).

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
  "num_retries": 2,                // LiteLLM router (also sent as a header, §7.3)
  "metadata": {                    // LiteLLM observability
    "trace_id": "…",
    "tags": ["team:research"]
  }
}
```

Every optional field is omitted when unset — no explicit `null`s.

### 7.1 Fields deliberately not sent

- **`parallel_tool_calls`.** Never sent. **Measured caveat:** with
  `drop_params: false`, the proxy itself accepts both `parallel_tool_calls:
  false` and an entirely invented param with a 200 (Appendix B, P6) — so the
  first draft's "risks a 400" claim is *not* true at the proxy layer. The risk
  is downstream, at whichever backend the alias fronts, and a mocked backend
  cannot exercise it. The decision stands on a weaker but sufficient basis:
  `parallel_tool_calls: false` carries no caller instruction that omitting it
  would violate, so sending it buys nothing and can only add downstream risk.
  This argument does **not** rest on the capability flag being consumed (§11).
- **`previous_response_id`.** `ModelSettings::previous_response_id` is an
  OpenAI Responses-API concept; this provider has no Responses backend and
  ignores it. Documented in the crate docs and asserted by a test.
- **`n`.** Never sent, and reserved against `extra_body` (§7.2) because §9.4
  reads only the first choice.

**`max_tokens` is sent unconditionally**, and this is a deliberate asymmetry
with `parallel_tool_calls`: OpenAI's o-series and gpt-5 reject `max_tokens` in
favour of `max_completion_tokens`, which is the same failure mode. The
difference is that `max_tokens` carries a caller instruction that silently
dropping would violate, whereas `parallel_tool_calls: false` carries no
instruction the caller can observe. Operators fronting o-series models set
`drop_params` on the proxy or use `.extra_body()`. Stated in the crate docs.

### 7.2 `extra_body` merge rules (D9)

- Must be a JSON **object**, else `BuildError::ExtraBodyNotAnObject`.
- Keys merge at the **request root**.
- **Reserved** — rejected at `build()` with `ReservedExtraBodyKey(key)`:
  `model`, `messages`, `stream`, `stream_options`, `tools`, `tool_choice`,
  `response_format`, `temperature`, `top_p`, `max_tokens`, `n`.
  These are the fields the provider computes from the `ModelRequest`; letting a
  caller forge them makes the translator's output unpredictable.
- **Not reserved** — `fallbacks`, `num_retries`, `metadata`, `tags`. These are
  exactly the keys whose correct wire shape is least certain (§7.3), so
  reserving them would wall off the escape hatch precisely where it is needed.
  Precedence: **`extra_body` wins**, except `metadata`, which is
  *deep-merged* with builder-supplied metadata (caller keys win per key) so
  `.metadata()` and an `extra_body` `metadata` object compose rather than
  clobber.

The rejection is **unconditional and evaluated at `build()`** — it does not
depend on whether a given request would have populated the field. This is
stated explicitly because the check necessarily precedes any `ModelRequest`:
`.extra_body(json!({"temperature": 0.2}))` fails at build even if no request
ever sets `temperature`. Determinism and an early error are worth that
strictness; the alternative (deferring to request assembly and surfacing a
`ModelError`) was considered and rejected.

`.metadata("tags", …)` fails with the distinct `ReservedMetadataKey("tags")`,
not `ReservedExtraBodyKey` — the error must name the builder method that was
actually misused.

### 7.3 The extras' wire shapes — measured, not assumed

The first draft treated `tags` placement and body-`num_retries` as open
uncertainties to be resolved after release. They were instead measured against
LiteLLM 1.97.0 (Appendix B, P3–P5/P14). Results:

| Question | Measured answer |
|---|---|
| `metadata.tags` routes? | **Yes** — selects the tagged deployment (P4). |
| Top-level `tags` routes? | **Yes** — identical result (P4). |
| `x-litellm-tags` header routes? | **Yes** — identical result (P4). |
| `num_retries` in body accepted? | **Yes**, 200; reaches `router.py`'s retry machinery (P3, logs). |
| `x-litellm-num-retries` header accepted? | **Yes**, 200 (P3). |
| `fallbacks` in body accepted **and honoured**? | **Yes** — `x-litellm-attempted-fallbacks: 1` and the backup deployment answered (P14). |

**All three tag forms are equivalent on 1.97.0**, which dissolves the
uncertainty rather than resolving it in one direction. **Decision: emit
`metadata.tags`**, the form upstream documents as supporting negation (`!`) and
required (`&`) prefixes — now a preference among verified-equivalent options
rather than a bet.

**`num_retries`: emit both** the body field and the `x-litellm-num-retries`
header. Both are accepted, and upstream documents the header as outranking the
body, so they cannot disagree. The *number of attempts actually made* was not
directly measurable — mocked errors return 500 without the
`x-litellm-attempted-*` headers — so dual emission stays as cheap insurance
rather than being narrowed to the body alone.

Escape hatches remain regardless, since `tags`/`metadata`/`num_retries`/
`fallbacks` are unreserved (§7.2) and `.header()` reaches the header forms.

## 8. Tool choice + structured output

`translate/tools.rs` maps `ToolDef` → OpenAI `tools[]` entries
(`{"type":"function","function":{name,description,parameters}}`), delegating
schema normalisation to `paigasus_helikon_core::schema::strict` — the canonical
normaliser the OpenAI provider also uses, so no schema logic is duplicated.

`ToolChoice` maps as: `Auto` → `"auto"`, `Required` → `"required"`, `None` →
`"none"`, `Tool { name }` → `{"type":"function","function":{"name":…}}`.

**`ToolChoice::Required` or `Tool { .. }` with an empty `request.tools` emits a
`tracing::warn!`** rather than being silently dropped — the caller believes a
tool call is guaranteed. (Gemini errors outright here; a warning is chosen
because a proxied backend may legitimately have server-side tools the SDK
cannot see.)

`translate/response_format.rs` maps `ResponseFormat`: `Text` → field omitted,
`JsonObject` → `{"type":"json_object"}`, `JsonSchema { name, schema, strict }` →
`{"type":"json_schema","json_schema":{name,schema,strict}}`, with the schema run
through `core::schema::strict` when `strict` is set. Unknown future variants
(the enum is `#[non_exhaustive]`) fall through to "no constraint".

Unlike Gemini, no structured-output/tools conflict is pre-rejected — LiteLLM
passes both through and any incompatibility surfaces from the proxied backend.

**Empty `messages`** is passed through rather than pre-rejected: LiteLLM may
front backends that accept it, and a 400 from the proxy is a clearer signal
than an SDK-invented error. Deliberate, and noted in the crate docs.

## 9. Streaming translation (`sse.rs`, `stream.rs`)

POST with `Accept: text/event-stream`. Framing via `eventsource-stream`;
`data: [DONE]` is matched explicitly and terminates the stream — the same
handling already shipping at `gemini/src/model.rs:155`.

### 9.1 Chunk types — permissive means `#[serde(default)]` everywhere

```rust
#[derive(Deserialize, Default)]
#[serde(default)]
struct StreamChunk { choices: Vec<Choice>, usage: Option<Usage> }

#[derive(Deserialize, Default)]
#[serde(default)]
struct Choice { index: Option<u32>, delta: Option<Delta>, finish_reason: Option<String> }

#[derive(Deserialize, Default)]
#[serde(default)]
struct Delta {
    content: Option<String>,
    reasoning_content: Option<String>,   // LiteLLM normalises thinking here
    reasoning: Option<String>,           // fallback: some builds/backends
    tool_calls: Option<Vec<ToolCallChunk>>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct ToolCallChunk { index: Option<u32>, id: Option<String>, function: Option<FunctionChunk> }

#[derive(Deserialize, Default)]
#[serde(default)]
struct FunctionChunk { name: Option<String>, arguments: Option<String> }
```

Every field is `#[serde(default)]` including `choices`. A first draft that made
`choices`/`index`/`delta` mandatory would have been the opposite of D2's stated
posture.

**Measured chunk shapes** (Appendix B, P1/P13), which the types must absorb:

- The **first** delta carries `"role":"assistant"` alongside `content` — an
  extra field, ignored by serde but worth knowing it is there.
- The **finish** chunk is `{"index":0,"delta":{},"finish_reason":"stop"}` —
  `delta` present but empty.
- The **trailing usage** chunk is
  `{"choices":[{"index":0,"delta":{}}],"usage":{…}}` — it carries a `choices`
  array with an empty delta and **no `finish_reason` key at all**.
- `usage` contained `completion_tokens_details.reasoning_tokens` but **no
  `prompt_tokens_details`** — so `cached_input_tokens` must tolerate the whole
  sub-object being absent, not merely the field.

**An unparseable frame is warned and skipped, not fatal** — `tracing::warn!` on
`paigasus::litellm::sse` then `continue`, matching
`gemini/src/model.rs:159-169`.

### 9.2 Event mapping

| Chunk field | `ModelEvent` |
|---|---|
| `delta.content` | `TokenDelta { text }` (skipped when empty) |
| `delta.reasoning_content`, else `delta.reasoning` | `ReasoningDelta { text }` |
| `delta.tool_calls[]` | `ToolCallDelta { call_id, name, args_delta }` — see §9.3 |
| `usage` | `Usage { … }` |
| `finish_reason` | **buffered**; emitted as `Finish` only at `[DONE]`/EOF — §9.5 |

`reasoning_content` is why this provider gets reasoning streaming the Gemini
provider still lacks — async-openai's delta type has no field for it at all
(§1.1 #2).

**Finish-reason mapping is lenient** — the payoff for D2: `stop` → `Stop`;
`length` → `Length`; `tool_calls` | `function_call` → `ToolCalls`;
`content_filter` → `ContentFilter`; **anything else → `FinishReason::Other(s)`**.

**Usage mapping**: `prompt_tokens` → `input_tokens`, `completion_tokens` →
`output_tokens`, `prompt_tokens_details.cached_tokens` → `cached_input_tokens`,
`completion_tokens_details.reasoning_tokens` → `reasoning_tokens`. Each `Usage`
is forwarded as it arrives; core's last-wins contract
(`core/src/model.rs:198-204`) covers the normal single-terminal-snapshot case
and the cumulative-updates case. A backend emitting true per-chunk *deltas*
would under-count — recorded as a risk in §14, not defended against, because
detecting it requires knowing the backend.

### 9.3 Tool-call correlation

The OpenAI Chat streaming format does not guarantee `tool_calls[].id` arrives
before `function.name`/`function.arguments` for the same index, and fragments
both (`"sea"` + `"rch"` → `"search"`). The translator therefore carries the
same three-map state machine the OpenAI provider needed
(`openai/src/backend/chat.rs:196-375`):

- `tool_calls: HashMap<Key, String>` — key → `call_id`, once known.
- `name_emitted: HashSet<Key>` — indices whose `name` has been sent, so `name`
  is `Some` exactly once per call.
- `pending: HashMap<Key, PendingToolCall { name: String, args: String }>` —
  `push_str`-concatenated fragments buffered until the `id` is observed, then
  flushed into the first emitted `ToolCallDelta`.

**`Key` resolution, resolving §17's former open question now rather than at
implementation time:** use `index` when present; else use `id` when present;
else use the position of the entry within `delta.tool_calls`. Positional
fallback is last because it is only correct for single-tool-call turns, and it
is logged at `debug` when taken.

This state machine is ~170 lines and is part of the duplication D6 authorises —
see §13.1.

### 9.4 Multi-choice (`n > 1`)

**This is a real, reachable situation, not a hypothetical**: `n: 2` against the
proxy returns two choices with `index` `0` and `1` (Appendix B, P17).

**Only the first choice is read.** Chunks whose `choices` carry more than one
entry have entries beyond the first ignored, with a one-time `tracing::warn!`.
Iterating all choices (as the OpenAI provider does) would interleave two
independent completions into a single `TokenDelta` stream and emit two `Finish`
events. `n` is reserved against `extra_body` (§7.2) so the situation cannot be
requested through the SDK. Gemini set the precedent of documenting this
explicitly.

### 9.5 Terminal semantics (D12)

`finish_reason` is **buffered**, and `ChatTranslator::finish()` is called at
`[DONE]` and at stream EOF — mirroring `gemini/src/stream.rs:73-87` and its
call sites at `gemini/src/model.rs:150,156`.

- If a `finish_reason` was observed, `finish()` emits exactly one `Finish`, as
  the last event of the stream.
- **If no `finish_reason` was observed (premature EOF), no `Finish` is
  emitted** and the stream simply ends. Fabricating `Finish::Stop` would make a
  truncated generation indistinguishable from a clean completion — and because
  `ModelTurnAccumulator` initialises `finish_reason: FinishReason::Stop`
  (`core/src/model.rs:558`), the truncated text would be committed to session
  history as final.

This is what makes the usage/finish ordering correct, and it is **measured, not
inferred** (Appendix B, P1). The tail of a real stream is:

```
data: {…"choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}
data: {…"choices":[{"index":0,"delta":{}}],"usage":{"completion_tokens":6,"prompt_tokens":8,…}}
data: [DONE]
```

The usage snapshot arrives in a **separate trailing chunk after** the chunk
carrying `finish_reason`. Any inline-`Finish` design therefore emits `Finish`
then `Usage` on **every single turn**, violating `core/src/model.rs:63` — this
is not an edge case. An error frame arriving after the finish chunk is likewise
safe, because `Finish` has not been emitted yet.

**Cancellation** follows the OpenAI provider: `tokio::select!` with `biased` on
the cancel arm, at both the initial request future and each upstream poll. Per
`core/src/model.rs:66-68`, a cancelled stream ends **without** `Finish`.

## 10. Error handling (`error.rs`)

### 10.1 The error envelope, as measured

LiteLLM returns `{"error": {"message", "type", "param", "code"}}`. The
first draft's classification rested on a wrong reading of two of those fields.
Measured on 1.97.0 (Appendix B, P7/P10/P11):

| Provoked condition | HTTP | `code` | `type` |
|---|---|---|---|
| `litellm.InternalServerError` | 500 | `"500"` | `null` |
| `litellm.RateLimitError` | 429 | `"429"` | `"throttling_error"` |
| `litellm.ContextWindowExceededError` | 400 | `"400"` | `null` |
| unknown model name | 400 | `"400"` | `"None"` (the **string**) |
| bad virtual key (DB-less deploy) | 400 | `"400"` | `"no_db_connection"` |

**`error.code` is the HTTP status restated as a string — not a semantic code.**
The first draft's primary signal (`error.code == "context_window_exceeded"`)
therefore never matches anything, and would have silently degraded every
context-overflow to the fallback path it called "fragile". `error.type` is
no better: `null` for the context-window case, and sometimes the literal
string `"None"`, which must not be confused with a real type.

**The reliable marker is the LiteLLM exception class name, which is prefixed
onto `message`:** `"litellm.ContextWindowExceededError: litellm.BadRequestError:
…"`. That is a stable, greppable token, unlike the prose that follows it.

Revised classification. `classify(status, code, err_type, message,
retry_after_ms) -> ModelError` is a **pure function**, mirroring
`gemini/src/error.rs:9`, so §13's per-row tests are cheap. Rules are evaluated
top-down:

| Condition | `ModelError` |
|---|---|
| `message` contains `ContextWindowExceededError`, or (fallback) matches a context-overflow prose substring | `ContextLengthExceeded` |
| 429 **and** a budget signal (`ExceededBudget` / `budget_exceeded` in `type`, or `message` contains "budget") | `Refused { reason }` |
| 429 otherwise | `RateLimited { retry_after_ms }` — from `Retry-After` seconds; HTTP-date form → `None` |
| 500 / 502 / 503 / 504 | `Unavailable` |
| 401 / 403, or `type == "content_policy_violation"`, or `type == "no_db_connection"` | `Refused { reason }` |
| other non-2xx | `Other(anyhow)` carrying status + body |

Note the context-window row is **first**, not filtered by status: the measured
status for that case is 400, and keying it off a status would collide with
every other 400.

`type` is normalised before matching: `null` and the literal string `"None"`
both become `None`.

Two further measured points:

- **500 is real and common** — `litellm.InternalServerError` returns it
  (P10). Leaving 500 in `Other` would make it non-retryable under
  `runtime-tokio/src/retry.rs:80-85`, and would disagree with
  `gemini/src/error.rs:17`, which already maps `500 | 502 | 503 | 504`.
- **Auth failures are deployment-dependent.** No `Authorization` header at all
  → 401 (P9/P21), but a *wrong* key on a DB-less deployment → 400 with
  `type: "no_db_connection"` (P11). Hence that type is routed to `Refused`
  rather than left in `Other`, so an auth misconfiguration is never reported as
  a generic upstream failure.

**Transport failures — including connection refused — map to
`Transport(String)`**, never `Unavailable`. The first draft listed
connect-refused under both, which is contradictory; `gemini/src/model.rs:118`
already settles it as `Transport`.

### 10.2 Errors are HTTP responses, not SSE frames

Measured (P12/P19): a streaming request whose model fails returns

```
HTTP/1.1 500 Internal Server Error
content-type: application/json

{"error":{"message":"litellm.InternalServerError: …","type":null,"param":null,"code":"500"}}
```

— **not** an SSE stream carrying an error frame. The first draft assumed a
mid-stream `data: {"error": …}` frame was the normal error path; it is not.

The implementation therefore **checks the HTTP status and `content-type`
before starting SSE parsing**: on a non-2xx status, or a `content-type` that is
not `text/event-stream`, the body is read to completion as JSON and passed to
`classify()`. Only a 2xx `text/event-stream` response enters the framing loop.

A genuine mid-stream `data: {"error": …}` frame is still handled defensively —
a backend failing *after* tokens have been emitted can produce one — but this
is **unverified**: mocked failures always fail before the stream opens, so no
mid-stream error could be provoked. Marked as such rather than asserted.

**Errors surface as the first `Err` item on the returned stream**, not as an
`Err` from `invoke` itself — matching `gemini/src/model.rs:136`. `RetryingModel`
handles both (`runtime-tokio/src/retry.rs:190,234`), but call-site code differs,
so the choice is stated.

### 10.3 Correlation (D5)

On every response, `x-litellm-call-id` and `x-litellm-model-id` are recorded at
`tracing::debug!` on target `paigasus::litellm::http` — **verified present on
streaming responses as well as non-streaming ones** (P18), which is what makes
this useful at all, since streaming is the only path this provider uses. On
every non-2xx path they are additionally attached to the `anyhow` context of
`ModelError::Other`.

Two further headers are logged because they are the only way to see whether the
router actually did anything: **`x-litellm-attempted-retries` and
`x-litellm-attempted-fallbacks`** (measured at `1` when a fallback engaged,
P14). Without them, `.fallbacks()` and `.num_retries()` are unobservable from
the client side. Note they are emitted on **successful** responses only — a
failing request carries no `x-litellm-attempted-*` headers (P-round-4), so
absence is not evidence of no retry.

No core change is required, and this is what makes §14's "capability
mis-declaration surfaces as an upstream 400 mapped to `Other`" actually
diagnosable — the operator can find the call in LiteLLM's own logs.

## 11. Capabilities (`capabilities.rs`)

No `KNOWN_MODELS` table (D3):

```rust
pub(crate) const fn conservative_defaults() -> ModelCapabilities {
    ModelCapabilities::empty().with_streaming().with_tools()
}
```

**What capabilities actually do in this codebase.** `grep '\.capabilities()'`
over `crates/` returns only the provider crates themselves,
`runtime-tokio/src/retry.rs:260` (pure forwarding on the decorator), and
`cli/src/model.rs:52-54`. **`paigasus-helikon-core` never calls it, and the
agent loop does not read it.** The flags are advisory metadata for application
code and traces. The first draft justified §7.1 with "the capability flag
informs the agent loop"; that was false, and §7.1 now stands on the
`drop_params`/400 argument alone, which is independent of any consumer.

**The flags describe the unknown proxied backend, not the translator.** The
crate implements `response_format` (§8), emits `ReasoningDelta` (§9.2), and
snapshots an inline image part (§13) while defaulting `structured_output`,
`reasoning`, and `vision` to `false`. That is not a contradiction: the
translator can express those things; whether the backend behind the alias
supports them is unknowable from the alias. Stated in the crate docs so the
apparent mismatch reads as a decision.

`parallel_tool_calls` is intentionally unset, on the reasoning the OpenAI crate
already documents: most OpenAI-compatible proxies do not support parallel tool
calls, and a loop expecting multiple calls fails worse than one expecting a
single call.

`.with_capabilities()` wins outright, **except that `streaming` is forced back
to `true`** — the provider has no non-streaming path (§3), so
`ModelCapabilities::empty().with_tools()` would otherwise advertise a
self-contradictory model. Documented and tested.

The crate README and the mdBook state that declaring capabilities is the
operator's job on this provider, because the alias carries no information the
SDK can act on.

## 12. Concurrency and resource posture

- `LiteLlmModel` is `Clone + Send + Sync + 'static`; `Config` lives behind an
  `Arc` and is cloned into each returned `BoxStream<'static, …>`.
- The default `reqwest::Client` is built with a **`connect_timeout` of 10 s**
  and no overall request timeout (a long generation is not a hang). Callers
  needing different behaviour supply `.http_client()`. Without this a hung
  self-hosted proxy would hang `invoke` indefinitely with cancellation as the
  only escape — a higher-probability failure for an operator-run gateway than
  for a hyperscaler.
- Redirects are **disabled** (`redirect::Policy::none()`): a proxy redirecting
  an authenticated POST is not a flow this crate should follow silently.

## 13. Testing strategy

1. **`insta` request snapshots** (`src/translate/snapshots/`) — plain text turn;
   system prompt; tool declarations with each `ToolChoice` variant; tool call +
   tool result round-trip; structured output via strict `json_schema`; inline
   image part; and one per LiteLLM extra (`fallbacks`, `num_retries`,
   `metadata` incl. `metadata.tags`, `extra_body` merged at root,
   `extra_body.metadata` deep-merged with builder metadata).
2. **`tests/litellm_wire.rs`** (wiremock) — every row of §6.1's URL table
   including the two rejection rows; `Accept`/`Content-Type`;
   `x-litellm-num-retries`; `.header()` passthrough; and the auth cases below.
3. **`tests/streaming.rs`** — SSE fixtures → `ModelEvent` sequences:
   multi-chunk text; interleaved tool-call deltas across two indices;
   **`id`-arrives-late** and **fragmented `function.name`** (ported from the
   OpenAI crate's tests at `backend/chat.rs:402,447,484,516`);
   `reasoning_content` → `ReasoningDelta` and the `reasoning` fallback;
   **usage in a trailing chunk after the finish chunk**, asserting `Finish` is
   last and `Usage` precedes it; unknown `finish_reason` → `FinishReason::Other`;
   **truncated stream with no `finish_reason` → no `Finish` emitted**;
   unparseable frame skipped without killing the stream; multi-choice chunk with
   only the first choice honoured; mid-stream error frame.
4. **`tests/cancellation.rs`** — cancel before the request future resolves.
   A true mid-stream cancel is **not** attempted: wiremock serves the whole body
   in one `set_body_raw`, as `openai/tests/chat_streaming.rs:3` states outright,
   so such a test would not exercise pacing. Recorded here so its absence is a
   decision rather than an oversight.
5. **Unit tests** — every `BuildError` variant, including each reserved
   `extra_body` key, `ReservedMetadataKey("tags")`, empty/whitespace api-key
   treated as absent, and `with_capabilities` forcing `streaming = true`;
   `from_env` with each env var present/absent including the `LITELLM_PROXY_*`
   fallbacks; one `classify()` test per row of §10.1 — **using the error
   envelopes captured in Appendix B verbatim**, including the `code: "400"`
   (status-as-string) and `type: "None"` (literal string) cases, so the tests
   pin real LiteLLM output rather than an idealised envelope.
6. **Non-SSE error-response test** — a wiremock returning HTTP 500 with
   `content-type: application/json` and the captured error body must yield a
   single `Err(Unavailable)` on the stream and must **not** enter SSE framing.
   This covers §10.2, the path the first draft did not know existed.
7. **Invariant tests** — `parallel_tool_calls` absent from the body;
   `previous_response_id` never sent; and a **reserved-set exhaustiveness test**
   that builds a maximally-populated request and asserts every emitted
   top-level key is either reserved or a known LiteLLM extra. That last one is
   what catches "we added `seed` to the body and forgot to reserve it".
8. **`tests/live.rs`** — env-gated on `LITELLM_API_BASE`, loud-skips when unset.
   No CI job in this PR (D11 / §14.1).

**Streaming fixtures are transcribed from real captured traffic** (Appendix B),
not hand-written. The pre-challenge draft's ordering bug survived precisely
because the OpenAI crate's hand-written fixture co-locates `usage` with
`finish_reason` — a shape the wire does not actually produce. Fixtures that
originate from a live proxy cannot encode that mistake.

**The "`Authorization` absent" assertion must use `server.received_requests()`**
and assert the header is missing from the recorded request. Wiremock has no
negative header matcher, so a `Mock::given(method("POST"))` with no header
condition matches whether or not the header was sent — the test would pass
against an implementation that always sends auth. This is the security-relevant
assertion for D8, and it gets a mutation check: temporarily emit the header and
confirm the test fails.

Env-mutating tests take a process-wide `Mutex` guard, matching
`providers-openai/src/builder.rs:147`.

### 13.1 Duplication accounting and drift control (D6)

The honest inventory of what is copied from `providers-openai`:

| Copied | Source | ~lines |
|---|---|---|
| `to_chat_messages` | `translate/request.rs:26-243` | 120 |
| `ChatTranslator` incl. tool-call state machine | `backend/chat.rs:196-375` | 170 |
| `translate_tool_choice` | `backend/chat.rs:181-194` | 15 |
| `to_openai_response_format` | `translate/response_format.rs` | 40 |

≈ 345 lines, not the ~120 the first draft implied.

The first draft claimed snapshot tests mitigate drift. They do not: a snapshot
of this crate pins *this* crate's shape and says nothing about the OpenAI
crate's, so both suites stay green while the two diverge arbitrarily. Two real
controls replace that claim:

1. **Port the OpenAI crate's unit tests verbatim** alongside each copied
   function, so behavioural parity is pinned at the moment of copying rather
   than assumed.
2. **A cross-crate parity test** in the facade's test directory, run with both
   `openai` and `litellm` features enabled, asserting both translators produce
   byte-identical `messages` for a shared fixture set. This is the only
   construct that can actually fail when the two drift.

If either control proves impractical during implementation, the fallback is to
hoist into `paigasus_helikon_core::wire::openai_chat` and pay the documented
same-PR core + facade bump — which is a ~5-line Cargo edit, not the burden the
first draft implied. SMA-450 (Ollama) will be a third consumer and is the
natural forcing function; a follow-up ticket should be filed rather than left
implicit.

## 14. Risks & mitigations

| Risk | Mitigation |
|---|---|
| ~~`metadata.tags` is the wrong shape~~ — **retired.** Measured working on 1.97.0, alongside the top-level and header forms (§7.3). | Residual: a *future* LiteLLM could change it. `tags`/`metadata` stay unreserved and `.header()` stays available, so a workaround needs no release. |
| **Streaming tool-call `index` may be absent** (§17 #1) — unverified, since the mock backend could not emit tool calls. | §9.3's key fallback (`index` → `id` → position) is already defensive; the positional branch logs at `debug` when taken. |
| **Duplicated translation drifts** from the OpenAI provider's. | §13.1's ported unit tests + cross-crate parity test. Follow-up ticket for the hoist once Ollama lands. |
| **A backend emits true per-chunk usage deltas**, breaking last-wins and under-counting (`core/src/model.rs:198-204`). | Not defended against — detection requires knowing the backend. Documented in the crate docs as a known limitation of fronting arbitrary backends. |
| **`num_retries` × client-side `RetryingModel` multiply.** A 3-attempt policy around `.num_retries(2)` with two fallbacks is up to 18 upstream calls per turn. | Crate docs warn explicitly and recommend treating server-side and client-side retry as mutually exclusive. |
| **LiteLLM changes an extra's wire shape.** | Extras confined to `translate/extras.rs`, one snapshot each; `.extra_body()`/`.header()` let operators work around it with no release. |
| **`Retry-After` HTTP-date form yields `None`.** | Accepted; `RateLimited.retry_after_ms` is already `Option` and callers must handle `None` regardless. |
| **Capability mis-declaration by the operator.** | Surfaces as an upstream 400 → `Other`, now carrying `x-litellm-call-id` (§10.3) so it is traceable. Documented as the operator's responsibility (§11). |
| **No live CI coverage** (D11). | Wiremock covers wire shape and the translator state machine; `tests/live.rs` exists for manual validation. See §14.1 — the reason this is a *deferral* rather than an impossibility has changed. |

### 14.1 A keyless containerised CI job is feasible — retracting D11's premise

D11's original reasoning was that a live LiteLLM job "needs a container plus at
least one real upstream key", so it could never be more than a loud-skip.
Building the verification rig for this spec disproved that: every probe in
Appendix B ran against `ghcr.io/berriai/litellm:main-stable` configured purely
with `mock_response` deployments and `api_key: fake-key`. **No real upstream
credential was involved at any point**, and the proxy still produced genuine
streaming SSE, genuine router fallbacks, and genuine LiteLLM error envelopes —
which is exactly what a wire-conformance job needs to assert against.

So the honest position is that a `litellm-it` job in `integration.yml` is
**possible and would have caught two of this spec's defects**, and its absence
is a scope decision for a Low-priority ticket, not a technical limit. What it
could *not* cover is the §17 list — tool-call shape, reasoning fields,
mid-stream errors — all of which need a real backend.

Follow-up filed as **SMA-523** — a signal-only `litellm-it` job following
`temporal-it`'s pattern (step-level `if:` guards, not job-level, so a skipped
job never blocks a promoted context), reusing the Appendix B config. Deferred
out of this PR at GATE 1 to keep a Low-priority provider PR reviewable.

## 15. Facade wiring, packaging, documentation

### 15.1 Workspace and release

- Root `Cargo.toml`: add to `[workspace.dependencies]` —
  `paigasus-helikon-providers-litellm = { path = "crates/paigasus-helikon-providers-litellm", version = "0.1.0" }`.
  `members = ["crates/*", …]` is a glob, so no members edit.
- `crates/paigasus-helikon/Cargo.toml`: optional dep + `litellm = ["dep:paigasus-helikon-providers-litellm"]`.
- `crates/paigasus-helikon/src/lib.rs`: `pub use … as litellm` **with a `///`
  doc comment** — the docs job runs `RUSTDOCFLAGS=-D warnings` and an
  undocumented re-export fails it.
- Commit `Cargo.lock`. No `release-plz.toml` change.
- Crate deps (all `workspace = true`): `paigasus-helikon-core`, `async-trait`,
  `async-stream`, `eventsource-stream`, `futures-core`, `futures-util`,
  `reqwest` (`json`, `stream`, `rustls`), `serde`, `serde_json`, `thiserror`,
  `anyhow`, `tokio`, `tokio-util`, `tracing`. Dev-deps: `wiremock`, `insta`
  (`json`, `yaml`), `tokio`, `reqwest`. `[lints] workspace = true`. **No new
  third-party crate is introduced**, so no new licence, advisory, or rustls
  `CryptoProvider` surface.
- `.gitattributes` (crate-local): `*.snap text eol=lf`. The **root**
  `.gitattributes` gains `crates/paigasus-helikon-providers-litellm/tests/fixtures/*.txt text eol=lf`,
  extending the existing rule that currently covers
  `crates/paigasus-helikon-providers-anthropic/tests/fixtures/*.txt` — the
  streaming tests `include_str!` SSE fixtures and split on literal `\n`.
- crates.io name preflight: `paigasus-helikon-providers-litellm` was confirmed
  unclaimed on 2026-08-16 (queried with a custom User-Agent; the default curl
  UA 403s and would read as unavailable).

**Release mechanics — the first draft cited the wrong mechanism.** It claimed
release-plz's `dependencies_update` cascade bumps the facade. That cascade
fires only when release-plz itself bumps a sibling; here the new crate is
authored at `0.1.0` by the PR, so release-plz never bumps it. The facade is
bumped by **path attribution** on the squashed commit touching
`crates/paigasus-helikon/{Cargo.toml,src/lib.rs}`. Consequences:

- The squashed PR title **must** be a `feat` (see §15.2). A `chore(...)`-titled
  squash would publish the new crate and leave the facade with no `litellm`
  feature — the drift that needed follow-up PR #50 after SMA-346.
- No core bump is needed: core is untouched (D6), the new crate depends only on
  already-published core, and release-plz's dependency-ordered publish handles
  the rest.

### 15.2 Commit and PR title scope

**All commits and the PR title use scope `providers`** (or `facade` for a
wiring-only commit) — **not** `providers-litellm`. Verified: `.versionrc:18`'s
`scopeRegex` and `pr-title.yml`'s `scopes:` list `providers`,
`providers-openai`, and `providers-anthropic`, but neither
`providers-gemini` nor `providers-bedrock` nor `providers-litellm`. Because
`pr-title.yml` runs on `pull_request_target`, its allowlist is read from `main`,
so this PR **cannot** register a new scope and then use it in its own title.
Target PR title: `feat(providers): SMA-451 add litellm proxy model provider`.

### 15.3 Documentation

Per CLAUDE.md's two standing "keep it current" rules. Each site is enumerated
because several pages carry a hardcoded provider count that goes stale silently:

- **New** `crates/paigasus-helikon-providers-litellm/README.md` — the crates.io
  landing page. Must cover: required `base_url` (D7), the operator's
  capability-declaration responsibility (§11), the `num_retries` ×
  `RetryingModel` multiplication warning (§14), and the `.extra_body()` /
  `.header()` escape hatches. Snippets use `cargo add`, no hardcoded versions.
- `docs/book/src/concepts/model-providers.md` — **"Four adapters ship today" at
  both line 6 and line 85**, the "Switching providers is one line" block
  (~305-317), and the "Enabling the providers" toml (~334-343). Plus a LiteLLM
  section stating **when to use this crate versus `OpenAiModel::base_url()`**:
  the OpenAI provider when the proxy fronts OpenAI models; this crate when you
  need router fallbacks, retries, spend metadata, reasoning streaming, or
  non-OpenAI backends behind arbitrary aliases.
- `docs/book/src/getting-started/workspace-layout.md:~57` — crate list.
- `docs/book/src/reference/crates.md` — **two** rows: the roster (~27) and the
  feature → module map (~57).
- `crates/paigasus-helikon/README.md:~22` — feature → module map. Note this
  README is `include_str!`'d into rustdoc, so any ```rust fence is compiled as
  a doctest; a network/key-bearing example must be fenced ` ```ignore `.
- Root `README.md:~29` and `~38` — both enumerate providers.
- `mdbook build docs/book` must stay clean (`warning-policy = "error"`).

## 16. Secret hygiene

- `Config` and `LiteLlmModelBuilder` implement `Debug` **manually**, redacting
  the API key and any `.header()` value whose name matches
  `authorization`/`api-key`/`x-litellm-key`. A derived `Debug` would leak the
  key through `{model:?}` and through `#[instrument]` fields.
- No `tracing` event logs the `Authorization` header or a full request body.
  `extra_body` is arbitrary caller JSON and is never logged.
- The §13 snapshots serialise the request body, which includes `extra_body`.
  Snapshot fixtures use only synthetic values; the README says not to put
  secrets in `extra_body`.

## 17. Remaining unknowns

The first draft deferred four questions to "before release". Two are now
**closed by measurement** (§7.3: all three tag forms route identically and
`metadata.tags` works; `fallbacks` and `num_retries` are accepted, and
`fallbacks` demonstrably engages the router). Two others were closed by
existing in-tree precedent: `eventsource-stream`'s `[DONE]` handling
(`gemini/src/model.rs:155`) and the tool-call key fallback (§9.3).

What genuinely remains unverified, stated plainly rather than assumed away:

1. **Streaming tool-call chunk shape.** The probe could not provoke one —
   LiteLLM ignored a request-level `mock_response` carrying `tool_calls` and
   replayed the configured string mock instead (Appendix B, P15/P16). So
   **whether LiteLLM populates `tool_calls[].index` on streaming deltas is
   untested.** This is precisely why §9.3 specifies the three-way key fallback
   (`index` → `id` → position) rather than assuming `index`; the design is
   already defensive against the unknown. Resolve via `tests/live.rs` against a
   proxy fronting a real tool-calling backend.
2. **Which reasoning field a given LiteLLM build emits.**
   `delta.reasoning_content` is primary with `delta.reasoning` as fallback
   (§9.1). Not exercisable with a mock backend. `thinking_blocks` is a third
   form seen on some Anthropic-backed builds; if `tests/live.rs` shows it, add
   it to the chain (a `stream.rs`-local change).
3. **True mid-stream error frames.** Every mocked failure fails *before* the
   stream opens, returning non-2xx JSON (§10.2). A backend failing after tokens
   have flowed should produce `data: {"error": …}`, and the design handles it,
   but that path is unverified.

All three are backend-behaviour questions that a mocked proxy structurally
cannot answer, so they are not deficiencies in the verification — they are the
boundary of what it could reach.

---

## Appendix A — Adversarial challenge changelog

A fresh Opus reviewer attacked the first draft (verdict: **needs rework**).
Findings verified against the tree before folding in.

**Folded in — blockers**

- **Usage-after-`Finish` ordering violation.** With `include_usage`, usage
  arrives in a trailing chunk *after* the finish chunk, so the draft's inline
  `Finish` would be followed by `Usage`, violating `core/src/model.rs:63`.
  Confirmed the OpenAI crate's fixture
  (`tests/fixtures/chat_parallel_tool_calls.txt`) puts them on the *same*
  chunk, so the draft's "usage-then-finish" test would have passed against a
  broken implementation. → D12 + §9.5, adopting `gemini/src/stream.rs:73`'s
  deferred `finish()`; test now uses a trailing-usage fixture.
- **"Permissive" chunk types had three required fields**, and `ToolCallChunk`
  was never defined. → §9.1, `#[serde(default)]` throughout, all types defined,
  warn-and-continue on unparseable frames.
- **Tool-call correlation unspecified**, and D6 under-counted the duplication it
  authorised. → §9.3 specifies the state machine and the `index`/`id`/position
  key fallback; §13.1 restates the duplication as ≈345 lines and replaces the
  unworkable snapshot-based drift claim with ported unit tests + a cross-crate
  parity test.
- **The two least-certain wire shapes were both reserved against
  `extra_body`**, so the escape hatch was walled off exactly where needed. →
  §7.2 unreserves `fallbacks`/`num_retries`/`metadata`/`tags`; §7.3 adds
  dual-emission for `num_retries` and a `.header()` escape hatch. Subsequently
  measured outright — see Appendix B.

**Folded in — majors**

- **"The capability flag informs the agent loop" is false.** Verified: core
  never calls `.capabilities()`; only the CLI and `retry.rs`'s pass-through do.
  → §11 restates capabilities as advisory; §7.1 now stands on the
  `drop_params`/400 argument alone.
- **§1.1 rationale #3 (capability tables) did not survive reading the code it
  criticised** — `lookup` already falls through to `conservative_defaults`,
  whose doc comment already names LiteLLM. → replaced with the verified and
  much stronger point that `ChatCompletionStreamResponseDelta` has no reasoning
  field at all, making proxy reasoning-streaming structurally impossible via
  async-openai.
- **Connect-refused mapped to two different variants**; **500 missing** (Gemini
  maps it, and `retry.rs:80-85` makes `Other` non-retryable);
  **`context_window_exceeded` spelling**; **budget-exhaustion 429s**. → §10.1.
- **Reserved-key rejection incoherent** (justified per-request, evaluated at
  build). → §7.2 states it is unconditional and why; adds
  `ReservedMetadataKey`.
- **`metadata` was `String`-only** while the wire is not. → `impl Into<Value>`.
- **`n > 1` unhandled**; **truncated stream had no rule**; **base-URL
  normalisation lossy and `Url::parse("localhost:4000")` succeeds**. → §9.4,
  §9.5, §6.1 + `.chat_completions_path()`.
- **The "`Authorization` absent" test was vacuous** — wiremock has no negative
  header matcher. → §13 mandates `received_requests()` plus a mutation check.
- **Release mechanics cited the wrong mechanism**; **`providers-litellm` is not
  an allowed scope**. → §15.1, §15.2.
- **D5 discarded correlation ids** that need no core change. → §10.3.

**Folded in — minors**: doc pages enumerated with their stale provider counts
(§15.3); `with_capabilities` forcing `streaming` (§11); capability flags
describe the backend not the translator (§11); `LITELLM_PROXY_*` env fallbacks
(D7); empty-key handling (D8); `ToolChoice::Required` with no tools warns (§8);
`max_tokens`/`drop_params` asymmetry made explicit (§7.1); `Arc<Config>` (§5);
timeouts and redirect policy (§12); secret redaction (§16); mid-stream-cancel
test dropped with a reason (§13); reserved-set exhaustiveness test (§13);
`[DONE]` question closed by existing precedent (§17).

**Initially deferred, then done**

The reviewer asked that §7.3's wire shapes be verified against a live proxy
*before* GATE 1. This was first declined on the grounds that no LiteLLM
deployment was available — then a container was stood up and every claim
measured. See Appendix B; the reviewer's insistence was correct and the
deferral was wrong.

**Out of scope — surfaced for separate triage**

The ordering defect behind D12 exists in the **shipped OpenAI provider** too:
`ChatTranslator::consume` emits `Finish` inline
(`openai/src/backend/chat.rs:238-241`), so a real OpenAI stream — whose usage
arrives in a trailing chunk, now directly observed for the OpenAI-compatible
shape in Appendix B P1 — yields `Finish` then `Usage`, contrary to
`core/src/model.rs:63`. Its fixtures do not catch this because they co-locate
usage with `finish_reason`. This is a pre-existing bug in another crate, not
SMA-451's to fix; filed as **SMA-522**.

---

## Appendix B — Live-proxy verification log

**Rig:** `ghcr.io/berriai/litellm:main-stable`, reported version **1.97.0**,
digest `sha256:468c25f35f3e5ec4e414974f00deab93337b1b4d9953cabcfd3722e59415f834`,
run locally on port 4000. All deployments use `mock_response` with
`api_key: fake-key` — **no real upstream credential was used**. Config had
`drop_params: false`, `enable_tag_filtering: true`, `master_key: sk-probe-1234`.
Config and probe scripts are reproducible from this appendix; they intentionally
live outside the repo (throwaway rig, and the config contains a dummy master key
that should not be mistaken for a fixture).

### What the probes changed in this spec

| Probe | Result | Effect |
|---|---|---|
| **P1** streaming tail | `finish_reason` chunk, then a **separate** `usage` chunk, then `[DONE]` | **Confirmed D12.** The pre-challenge design would have violated `core/src/model.rs:63` on every turn. |
| **P2** paths | `/chat/completions` → 200, `/v1/chat/completions` → 200 | §6.1 normalisation is safe either way. |
| **P3** `num_retries` | body → 200; header → 200 | §7.3: both accepted; dual emission retained. |
| **P4** tag routing | `metadata.tags`, top-level `tags`, and `x-litellm-tags` **all** select the tagged deployment | §7.3 uncertainty **dissolved**; risk retired from §14. |
| **P5/P14** `fallbacks` | accepted; `x-litellm-attempted-fallbacks: 1`, backup answered | §7.3: verified to actually engage the router. |
| **P6** unknown params | invented param → 200; `parallel_tool_calls:false` → 200 | **Falsified** §7.1's "risks a 400" claim; re-justified. |
| **P7/P10/P11** errors | `code` is the HTTP status as a **string**; `type` is `null`/`"None"`/`"throttling_error"`; class name prefixes `message` | **Rewrote §10.1.** The draft's primary signal could never have matched. |
| **P9/P21** auth | no header → 401; empty bearer → 401; wrong key (DB-less) → **400** `no_db_connection` | §10.1 routes that type to `Refused`; D8 confirmed. |
| **P12/P19** stream errors | failing stream → **HTTP 500 + `application/json`**, not SSE | **Added §10.2's** status/content-type check before framing. |
| **P13** usage fields | `completion_tokens_details.reasoning_tokens` present; **no** `prompt_tokens_details` | §9.1: the whole sub-object may be absent. |
| **P15/P16** tool calls | request-level `mock_response` **ignored**; could not provoke a tool call | §17 #1 stays **open and stated**, rather than silently assumed. |
| **P17** `n=2` | returns 2 choices, indexes `[0,1]` | §9.4 confirmed reachable, not hypothetical. |
| **P18** stream headers | `x-litellm-call-id` + `x-litellm-model-id` present on SSE responses | §10.3 is implementable on the only path this provider uses. |

### Reproduction

```yaml
# litellm-config.yaml (abridged — full set in §14.1's follow-up ticket)
model_list:
  - model_name: mock-fast
    litellm_params: {model: openai/gpt-4o-mini, api_key: fake-key,
                     mock_response: "Hello from the mock backend."}
  - model_name: mock-ctxwindow          # `litellm.<ExcName>` raises that exception
    litellm_params: {model: openai/gpt-4o-mini, api_key: fake-key,
                     mock_response: "litellm.ContextWindowExceededError"}
  - model_name: tagged                  # tags belong under litellm_params,
    litellm_params: {model: openai/gpt-4o-mini, api_key: fake-key,   # NOT model_info
                     mock_response: "FREE-TIER-DEPLOYMENT", tags: ["free"]}
router_settings:  {enable_tag_filtering: true}
general_settings: {master_key: sk-probe-1234}
litellm_settings: {drop_params: false}
```

```bash
docker run -d --name litellm-probe -p 4000:4000 \
  -v "$PWD/litellm-config.yaml:/app/config.yaml" \
  ghcr.io/berriai/litellm:main-stable --config /app/config.yaml --port 4000
```

The decisive trace (P1), verbatim:

```
data: {…"choices":[{"index":0,"delta":{"role":"assistant","content":"Hel"}}]}
…
data: {…"choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}
data: {…"choices":[{"index":0,"delta":{}}],"usage":{"completion_tokens":6,"prompt_tokens":8,"total_tokens":14,"completion_tokens_details":{"reasoning_tokens":0}}}
data: [DONE]
```
