# SMA-449 — Google Gemini provider (`paigasus-helikon-providers-gemini`)

- **Issue:** [SMA-449](https://linear.app/smaschek/issue/SMA-449) — Provider crate: `paigasus-helikon-providers-gemini`
- **Split from:** SMA-329 (delivered Bedrock; PR #120)
- **Status:** Design — revised after adversarial challenge; pending GATE 1 approval
- **Date:** 2026-06-25

## 1. Goal

Add a self-contained crate implementing a Google **Gemini** provider for the
Paigasus Helikon SDK — the third hand-rolled REST provider after OpenAI and
Anthropic — behind a `gemini` Cargo feature on the facade. It implements
`paigasus_helikon_core::Model` with the same public surface shape as the other
providers (`GeminiModel` + builder), passes a wire-format snapshot suite at
scenario parity with the OpenAI/Anthropic providers, and is name-claimed on
crates.io and wired into the facade — mirroring the brand-new-crate packaging
pattern Bedrock established.

## 2. Decisions

| # | Decision | Choice |
|---|----------|--------|
| D1 | API surface | **Both** the Gemini **Developer API** (API key) **and Vertex AI** (OAuth bearer) in this PR. Near-identical request/response/SSE body; only endpoint URL + auth header differ. |
| D1a | Vertex auth | Core mechanism is a caller-supplied `TokenProvider`/bearer. **Plus** an optional `vertex-adc` Cargo feature shipping a `gcp_auth`-backed `AdcTokenProvider`, so Vertex is usable out-of-the-box and its live test can mint a token. *(Scope addition from the challenge — flag at GATE 1.)* |
| D2 | Modalities | text, `FunctionDeclaration` tools, **native** structured output (`responseMimeType` + `responseSchema`), and **inline base64 images** (`inlineData`). Remote-URL images, audio, and non-text parts inside a tool result are dropped-with-a-`warn` and deferred. |
| D3 | Reasoning ("thinking") | **Streaming deferred:** reasoning capability flag `false` (it denotes "emits `ReasoningDelta`"); no `thinkingConfig` sent; no `ReasoningDelta` emitted. **But token accounting is not deferred:** `usageMetadata.thoughtsTokenCount` is mapped to `Usage.reasoning_tokens` so 2.5 turns aren't under-counted. |
| D4 | Structured output mechanism | **Native** (`responseMimeType: "application/json"` + `responseSchema`). No hidden-tool synthesis (unlike Bedrock/Anthropic). |
| D5 | Internal analog | Mirror the **Anthropic** crate (reqwest + `eventsource-stream` SSE, `KNOWN_MODELS` capability table, sync `build()`), not Bedrock's AWS-SDK layout. |
| D6 | Schema handling | Crate-local Gemini OpenAPI-3.0-subset sanitizer (`translate/schema.rs`). Hoisting a shared rewriter into `core` is out of scope but tracked as a follow-up (it would be the **third** copy of the `$ref`/cycle/depth machinery — see §15 R-dup). |

## 3. Scope

**In scope**
- `GeminiModel` (`impl Model`) + `GeminiModelBuilder`, synchronous `build()` (no network in build).
- Two transports selected at build time: Developer API (API key) and Vertex AI (bearer / `TokenProvider`).
- Optional `vertex-adc` feature: `gcp_auth`-backed `AdcTokenProvider` (D1a).
- Request translation: system instruction, user/model turns, tool calls & results (with `id` round-trip + `call_id→name` recovery), inline images, `FunctionDeclaration` tools, `toolConfig` (tool choice), `generationConfig` (temperature, top_p, max output tokens), native structured output.
- SSE streaming translation (`:streamGenerateContent?alt=sse`) → `ModelEvent`s honoring the core ordering/usage/finish/cancellation contracts, including truncated-stream and blocked-prompt handling.
- Error classification → `ModelError`.
- `KNOWN_MODELS` capability table + conservative fallback.
- Wire-format snapshot suite at scenario parity with the other providers, plus `wiremock` transport tests and gated live tests.
- Facade wiring (optional dep + `gemini` feature + `pub use … as gemini`), workspace `Cargo.toml` entries, README, crate docs.
- crates.io name-claim of `paigasus-helikon-providers-gemini`.

**Out of scope (follow-ups)**
- `ReasoningDelta` streaming / `thinkingConfig` / thought signatures (D3 — token accounting *is* in scope).
- Audio and remote-URL image inputs; the Gemini Files API (`fileData`) and context caching (D2).
- Hoisting a shared schema rewriter into `core::schema` (D6).
- `previous_response_id` / server-managed state (Gemini has no equivalent; capability flag `false`, field ignored).

## 4. Module layout

Mirrors the Anthropic crate. New crate `crates/paigasus-helikon-providers-gemini/`:

```
src/
  lib.rs                  -- crate docs + public re-exports (GeminiModel, GeminiModelBuilder, BuildError, TokenProvider)
  builder.rs              -- GeminiModelBuilder, BuildError, Config, Transport mode; from_env / vertex_from_env; build-time auth/transport validation
  model.rs                -- struct GeminiModel(Arc<Config>); impl Model { invoke, capabilities, provider="gemini", model }
  auth.rs                 -- pub(crate) Auth enum; pub trait TokenProvider; (feature="vertex-adc") AdcTokenProvider
  transport.rs            -- endpoint URL construction (Developer vs Vertex), header assembly, request send
  capabilities.rs         -- KNOWN_MODELS table + conservative_defaults()
  error.rs                -- classify(status, status_field, message, retry_after) -> ModelError; parse_retry_after_ms
  sse.rs                  -- SSE line framing helper over eventsource-stream (mirrors Anthropic sse.rs)
  stream.rs               -- StreamTranslator: Gemini chunk JSON -> Vec<ModelEvent>
  translate/
    mod.rs                -- build_request(cfg, &ModelRequest) -> PreparedRequest; to_wire_json() for snapshots; conflict guards
    request.rs            -- items_to_contents(): Vec<Item> -> contents[] + systemInstruction; role mapping; tool call/result; inline images; call_id<->name + id handling
    tools.rs              -- ToolDef[] -> tools[].functionDeclarations[]; functionCallingConfig (tool choice)
    response_format.rs    -- ResponseFormat -> generationConfig.responseMimeType + responseSchema; conflict handling
    schema.rs             -- sanitize_schema(): JSON Schema -> Gemini OpenAPI-3.0 subset
    snapshots/            -- insta .snap golden files
tests/
  live.rs                 -- #[ignore] live tests (Developer API + Vertex), gated on env
  gemini_wire.rs          -- wiremock request-shape assertions (mirrors Anthropic messages_wire.rs)
  gemini_streaming.rs     -- wiremock SSE -> ModelEvent assertions (mirrors Anthropic messages_streaming.rs)
Cargo.toml
README.md
```

## 5. Public API surface

```rust
// lib.rs re-exports
pub use builder::{BuildError, GeminiModelBuilder};
pub use auth::TokenProvider;                 // Auth enum stays pub(crate) (challenge: narrow surface)
pub use model::GeminiModel;
#[cfg(feature = "vertex-adc")] pub use auth::AdcTokenProvider;

#[derive(Debug, Clone)]
pub struct GeminiModel(Arc<Config>); // Clone -> moves cheaply into 'static streams

impl GeminiModel {
    /// Developer API builder (API key).
    pub fn developer(model_id: impl Into<String>) -> GeminiModelBuilder;
    /// Vertex AI builder (project + location + bearer/token-provider).
    pub fn vertex(model_id: impl Into<String>, project: impl Into<String>, location: impl Into<String>) -> GeminiModelBuilder;
    /// Developer API from `GEMINI_API_KEY` (fallback `GOOGLE_API_KEY`).
    pub fn from_env(model_id: impl Into<String>) -> Result<Self, BuildError>;
    /// Vertex from `GOOGLE_CLOUD_PROJECT` + `GOOGLE_CLOUD_LOCATION`; requires
    /// the `vertex-adc` feature to source a token, else returns MissingVertexAuth.
    #[cfg(feature = "vertex-adc")]
    pub async fn vertex_from_env(model_id: impl Into<String>) -> Result<Self, BuildError>;
}

#[async_trait] impl Model for GeminiModel {
    async fn invoke(&self, req: ModelRequest, cancel: CancellationToken)
        -> Result<BoxStream<'static, Result<ModelEvent, ModelError>>, ModelError>;
    fn capabilities(&self) -> ModelCapabilities;
    fn provider(&self) -> &str { "gemini" }
    fn model(&self) -> &str;
}
```

Builder methods (sync `build()`, no network), mirroring Anthropic:
`.api_key(..)`, `.base_url(..)`, `.http_client(reqwest::Client)`, `.bearer_token(..)`,
`.token_provider(impl TokenProvider + 'static)`, `.with_capabilities(ModelCapabilities)`,
`.build() -> Result<GeminiModel, BuildError>`.

**Auth model (representation `pub(crate)`).**
```rust
pub(crate) enum Auth {
    ApiKey(String),                       // Developer API: header `x-goog-api-key`
    Bearer(String),                       // Vertex: static `Authorization: Bearer <token>`
    Token(Arc<dyn TokenProvider>),        // Vertex: fresh bearer fetched per request
}

#[async_trait]
pub trait TokenProvider: Send + Sync + std::fmt::Debug {
    async fn token(&self) -> Result<String, ModelError>; // returns a bearer access token
}

#[cfg(feature = "vertex-adc")]
pub struct AdcTokenProvider { /* wraps gcp_auth ADC; caches+refreshes the token */ }
```

**Build-time validation (challenge: cross-product rules).** The constructor fixes
the transport mode. `build()` validates auth ↔ transport and rejects mismatches:
- Developer mode requires `Auth::ApiKey`; a non-whitespace key. Missing → `MissingApiKey`; empty/whitespace → `EmptyApiKey`. Supplying a bearer/token in Developer mode → `BuildError::AuthTransportMismatch`.
- Vertex mode requires `Auth::Bearer` (non-empty) or `Auth::Token`; missing → `MissingVertexAuth`; empty project/location → `MissingVertexProject` / `MissingVertexLocation`. Supplying an api_key in Vertex mode → `AuthTransportMismatch`.

`BuildError` (thiserror): `MissingApiKey`, `EmptyApiKey`, `MissingVertexAuth`,
`MissingVertexProject`, `MissingVertexLocation`, `AuthTransportMismatch`,
`InvalidBaseUrl(String)`, `EmptyModelId`.

**`GEMINI_API_KEY` vs `GOOGLE_API_KEY`.** `from_env` (Developer mode only) reads
`GEMINI_API_KEY`, then falls back to `GOOGLE_API_KEY`. Vertex never consults
`GOOGLE_API_KEY` (it authenticates by bearer), so there is no ambiguity when both
Vertex env vars and `GOOGLE_API_KEY` are set.

## 6. Transport (Developer API vs Vertex)

Near-identical request/response/SSE **bodies**; the transport layer differs in
URL and auth header (and minor server-side defaults, e.g. safety settings — so the
bodies are *near*-identical, not asserted byte-identical). Both default to an
overridable `base_url`.

| | Developer API | Vertex AI |
|---|---|---|
| Default host | `https://generativelanguage.googleapis.com` | `https://{location}-aiplatform.googleapis.com` (location `global` → `https://aiplatform.googleapis.com`) |
| Non-stream path | `/v1beta/models/{model}:generateContent` | `/v1/projects/{project}/locations/{location}/publishers/google/models/{model}:generateContent` |
| Stream path | `/v1beta/models/{model}:streamGenerateContent?alt=sse` | `…:streamGenerateContent?alt=sse` |
| Auth header | `x-goog-api-key: <key>` | `Authorization: Bearer <token>` |

Streaming uses `alt=sse` so responses are newline-framed `data:` SSE events
(parsed via `eventsource-stream`, like Anthropic), not the default JSON array.

**Token acquisition timing (challenge).** For `Auth::Token`, `invoke` awaits
`provider.token()` **before** issuing the HTTP request, inside the same
`tokio::select!` cancellation guard, so a cancel during token fetch ends cleanly.
A token-fetch failure returns `Err(ModelError)` **from `invoke`** (no stream is
produced), consistent with other early request-build failures.

## 7. Request translation

Core `ModelRequest` → Gemini `GenerateContentRequest`:

```json
{
  "systemInstruction": { "parts": [ { "text": "..." } ] },
  "contents": [
    { "role": "user",  "parts": [ {"text": "..."}, {"inlineData": {"mimeType":"image/png","data":"<b64>"}} ] },
    { "role": "model", "parts": [ {"functionCall": {"id":"fc_0","name":"search","args": {...}}} ] },
    { "role": "user",  "parts": [ {"functionResponse": {"id":"fc_0","name":"search","response": {...}}} ] }
  ],
  "tools": [ { "functionDeclarations": [ {"name":"...","description":"...","parameters": {<schema>}} ] } ],
  "toolConfig": { "functionCallingConfig": {"mode":"AUTO|ANY|NONE","allowedFunctionNames":["..."]} },
  "generationConfig": {
    "temperature": 0.7, "topP": 0.9, "maxOutputTokens": 1024,
    "responseMimeType": "application/json", "responseSchema": {<schema>}
  }
}
```

**Role mapping.** `Item::System` → `systemInstruction` (never a turn). User →
`role:"user"`, Assistant → `role:"model"`. Tool results → a **`role:"user"`** turn
carrying `functionResponse` parts. *(Gemini `Content.role` accepts only `user` and
`model`; there is no `function` role in the REST `contents` API. Confirm against the
live v1beta `generateContent` schema at implementation time — see §16 Q1.)*

**Tool-call identity (rewritten after challenge — BLOCKER fix).** Gemini's
`functionCall`/`functionResponse` carry an **optional `id`** used to disambiguate
parallel calls. The provider:
- **Inbound (streaming):** read `functionCall.id` when present and use it verbatim
  as the core `call_id`. When absent (single-call responses often omit it),
  synthesize a stable id (`fc_{index}`). Always carry the **real** function `name`
  in `ToolCallDelta.name`.
- **Outbound (history → request):** build a `call_id → name` map from the
  `Item::ToolCall` / `ContentPart::ToolUse` entries in the same conversation
  (because `Item::ToolResult` carries only `call_id`, no `name` — verified in
  `core::item`). For each `ToolResult`, emit `functionResponse { id: call_id,
  name: <recovered name>, response: … }`. The `name` is **never** derived by
  string-manipulating the `call_id`. If a `ToolResult.call_id` has no matching
  prior call, return a `ModelError` (malformed history) rather than emit an
  unbindable response.

**`ToolResult` content reduction (BLOCKER fix).** `Item::ToolResult.content` is a
`Vec<ContentPart>`, not a JSON value, and `functionResponse.response` must be a
JSON **object**. Reduction (mirroring Anthropic/Bedrock `text_of`):
1. Concatenate all `ContentPart::Text` parts → a single string `s`.
2. If `s` parses as a JSON **object**, use it as `response`.
3. Otherwise wrap: `response = { "result": s }` (string, or the parsed
   non-object JSON value under `result`).
4. `Image`/`Audio`/other non-text parts inside a tool result are dropped with a
   `tracing::warn!` (the Files API path is deferred).

**Inline images.** `ContentPart::Image { source: Base64 { mime_type, data } }` →
`{"inlineData": {"mimeType": mime_type, "data": data}}`. `MediaSource::Url`,
`ContentPart::Audio`, and (per above) non-text tool-result parts are skipped with a
`tracing::warn!` (mirrors Anthropic's unsupported-variant handling). This silent
data loss is documented in the crate README.

**Empty / system-only guard (MAJOR fix).** If translation yields zero non-system
turns (empty conversation, or system-only), `build_request` returns a
`ModelError` early — Gemini 400s on empty `contents`. Mirrors Bedrock's
`empty_conversation_returns_error` / `system_only_returns_error`.

**generationConfig.** `temperature`→`temperature`, `top_p`→`topP`,
`max_output_tokens`→`maxOutputTokens`. **`maxOutputTokens` is omitted when unset**
(MAJOR fix — Gemini, unlike Anthropic, does *not* require it; forcing a default
would silently truncate long completions; this follows Bedrock's omit-when-unset
`build_inference_config`). `generationConfig` is omitted entirely when empty.

## 8. Tool choice + structured output

**Tool choice** → `toolConfig.functionCallingConfig.mode`:
- `Auto` → `AUTO`
- `Required` → `ANY` (+ `allowedFunctionNames` = all declared tool names)
- `None` → `NONE`
- `Tool { name }` → `ANY` + `allowedFunctionNames: [name]`

`ToolChoice::Tool`/`Required` with no tools → `ModelError` (early, in
`build_request`). `ToolChoice::None` still permits `tools` to be declared (Gemini
allows declaring tools while forbidding calls).

**Structured output (native, D4).**
- `ResponseFormat::JsonObject` → `generationConfig.responseMimeType =
  "application/json"` (no schema).
- `ResponseFormat::JsonSchema { schema, .. }` → `responseMimeType =
  "application/json"` + `responseSchema = sanitize_schema(schema)`.
- `ResponseFormat::Text` (or `None`) → neither field.
- No hidden tool is synthesized; structured output streams as ordinary
  `TokenDelta`s carrying JSON text (the stream translator needs no special mode;
  the loop's `validate_terminal` parses it directly).

**Conflict guard.** Gemini does not support `responseSchema` together with
function calling. `build_request` rejects `JsonSchema`/`JsonObject` combined with a
non-empty **`req.tools`** list **or** any `ToolChoice` other than `None`, with a
clear `ModelError`. The guard inspects only the **active request's** `tools` /
`tool_choice` — **not** conversation history — because the loop's
`constrained_settings` legitimately sends `tools: []` + `JsonSchema` while the
history still contains earlier `functionCall`/`functionResponse` parts; that
finalize-after-tool-use case is allowed (and gets a dedicated test — §13 #11).

## 9. Schema sanitizer (`translate/schema.rs`)

Gemini's `responseSchema` and `FunctionDeclaration.parameters` accept an
**OpenAPI 3.0 Schema subset**, and `responseSchema` is **controlled generation** —
the model is *forced* to the schema, so a wrong schema yields wrong output (it is
**not** a "lossy but safe" situation — MAJOR fix to R2). `sanitize_schema(&Value)
-> Value` recursively:
1. Inlines `$ref` from `$defs`/`definitions` (Gemini rejects `$ref`); guards
   cycles and depth (max 64, mirroring Bedrock).
2. Strips keywords Gemini rejects: `$schema`, `$id`, `$anchor`, `$comment`,
   `additionalProperties`, `unevaluatedProperties`, `patternProperties`,
   `examples`, `$defs`/`definitions` (after inlining).
3. **Preserves meaning** of common combinators rather than collapsing them
   (Gemini's subset supports `anyOf`, `nullable`, `enum`):
   - `oneOf` → `anyOf` (Gemini accepts `anyOf`).
   - `[T, null]` (type array or `oneOf:[T,{type:null}]`) → `T` with `nullable: true`.
   - `const: v` → `enum: [v]`.
   - Only fall back to selecting the first subschema when a true union genuinely
     cannot be expressed in the subset, and `warn` when it does.
4. Maps unsupported `format` values off `string`/`number` to dropped (keeps the
   Gemini-recognized set, e.g. `enum`, `date-time`).

The exact accepted keyword/combinator policy is pinned by snapshot tests so drift
is caught.

## 10. Streaming translation (`stream.rs`)

Each SSE `data:` event is a `GenerateContentResponse` chunk. `StreamTranslator`
consumes chunks and emits `ModelEvent`s honoring the core contracts (`Usage`
cumulative-within-turn, last-wins; `Finish` terminal; cancellation ends the stream
with **no** `Finish`).

Per chunk (guarded — MAJOR fixes):
- **No `candidates` + `promptFeedback.blockReason`** → yield
  `Err(ModelError::Refused { reason })` and end. (Prompt was blocked.)
- **`candidates[0]` with no `content`** (e.g. `finishReason: SAFETY`) → emit no
  parts; map the finish reason as below.
- `candidates[0].content.parts[].text` → `TokenDelta { text }`.
- `candidates[0].content.parts[].functionCall {id?,name,args}` → `ToolCallDelta {
  call_id: id.unwrap_or(synth(index)), name: Some(name), args_delta:
  args.to_string() }` (Gemini delivers whole-args, not incremental — one delta per
  call).
- `usageMetadata` → `Usage { input_tokens: promptTokenCount, output_tokens:
  candidatesTokenCount, cached_input_tokens: cachedContentTokenCount,
  reasoning_tokens: thoughtsTokenCount }` (cumulative snapshot, last-wins). Emitted
  whenever a chunk carries `usageMetadata`; **not** re-emitted at stream end
  (single-emit — MINOR fix). Only `candidates[0]` is read; `candidates[1..]` are
  intentionally ignored (`candidateCount` is not settable via `ModelSettings`).
- `candidates[0].finishReason` → buffered; emitted as `Finish` **only when a
  `finishReason` was actually observed** (MAJOR fix). On premature EOF with no
  `finishReason`, the stream ends **without** `Finish` (the documented cancel-style
  termination), surfacing truncation rather than fabricating `Finish::Stop`.

`finishReason` mapping: `STOP`→`Stop` (or `ToolCalls` when the candidate carried a
`functionCall` part), `MAX_TOKENS`→`Length`,
`SAFETY`/`RECITATION`/`PROHIBITED_CONTENT`/`BLOCKLIST`/`SPII`→`ContentFilter`,
anything else (`MALFORMED_FUNCTION_CALL`, `OTHER`, unknown)→`Other(reason_string)`.

Cancellation: `tokio::select!` with `biased` on `cancel.cancelled()` (mirrors
Bedrock/Anthropic), returning without `Finish`.

## 11. Error classification (`error.rs`)

Pure `classify(status: u16, status_field: Option<&str>, message: &str,
retry_after_ms: Option<u64>) -> ModelError` (testable without a live client),
mapping Google API errors (HTTP status + `error.status`/`error.code` JSON):
- `429 RESOURCE_EXHAUSTED` → `RateLimited { retry_after_ms }` (parse `Retry-After`).
- `503 UNAVAILABLE`, `500 INTERNAL`, `504 DEADLINE_EXCEEDED` → `Unavailable`.
- `403 PERMISSION_DENIED`, `401 UNAUTHENTICATED` → `Refused { reason }`.
- `400 INVALID_ARGUMENT` whose message indicates token/context overflow →
  `ContextLengthExceeded`; other `400` → `Other`.
- Network/timeout (reqwest transport) → `Transport(String)`.
- `TokenProvider::token` failures surface as `Err(ModelError)` returned from
  `invoke` (see §6), not as an in-stream error.

`parse_retry_after_ms(&reqwest::header::HeaderMap)` reused in shape from Anthropic.

## 12. Capabilities (`capabilities.rs`)

Hardcoded `KNOWN_MODELS: &[(&str, ModelEntry)]` keyed by exact model id, with
`conservative_defaults()` (`streaming + tools`), overridable via
`with_capabilities`. Mirrors Anthropic's table. The `reasoning` flag denotes
"surfaces `ReasoningDelta`," so it is `false` in v1 even for 2.5 models (D3);
thought-token accounting is independent and handled in §10. Initial entries
(cross-checked against Google's published docs at implementation time;
divergences are bugs → follow-up chores):
- `gemini-2.5-pro`, `gemini-2.5-flash`: streaming, tools, parallel tool calls,
  structured output, vision.
- `gemini-2.0-flash`, `gemini-2.0-flash-lite`: streaming, tools, parallel tool
  calls, structured output, vision.
Server-managed state, audio, prompt caching, reasoning: `false` in v1.

## 13. Testing strategy

AC #2 requires **scenario parity** with the OpenAI/Anthropic snapshot suites
(there is no shared harness — each provider hand-rolls `tests/*_wire.rs` +
`src/translate/snapshots/`; "shared suite" means coverage parity, not file reuse).
Snapshotted via `to_wire_json(&PreparedRequest)` + `insta` (hand-written stable
JSON, not `Debug`):
1. plain text turn (no tools)
2. generation config (temperature / top_p / **maxOutputTokens omitted when unset**)
3. system instruction extraction
4. tool declarations (`functionDeclarations`)
5. tool call + tool result round-trip (`id` round-trip + `call_id→name` recovery; non-object result wrapping)
6. **parallel same-name tool calls** (two `search` calls, distinct ids, both `functionResponse.name = "search"`)
7. tool choice: auto / required / specific / none
8. native structured output: `JsonObject` and `JsonSchema` (responseMimeType + responseSchema)
9. structured-output + tools **conflict** → error (assert on the error `Display` via `insta::assert_snapshot!`, **not** `assert_json_snapshot!` — there is no request to snapshot)
10. inline base64 image part
11. structured-output **finalize after tool use**: `tools: []` + `JsonSchema` with prior function parts in history → valid request (no conflict error)
12. schema sanitizer: `$ref` inlining, unsupported-keyword stripping, `oneOf→anyOf`, `[T,null]→nullable`, `const→enum` (dedicated `schema.rs` tests + snapshots)
13. empty / system-only conversation → `ModelError`

Plus:
- **Transport tests** (`wiremock`, mirroring Anthropic): assert the correct URL,
  method, and auth header (`x-goog-api-key` vs `Authorization: Bearer`) for both
  Developer and Vertex modes; assert SSE chunks decode to the expected
  `ModelEvent` sequence (incl. blocked-prompt → `Refused`, truncated stream → no
  `Finish`); assert error-status mapping.
- **Live tests** (`tests/live.rs`, `#[ignore]`): skip-if-env-missing. Developer
  API gated on `GEMINI_API_KEY` (+ `GEMINI_MODEL_ID`). **Vertex** gated on the
  `vertex-adc` feature + `GOOGLE_CLOUD_PROJECT` + `GOOGLE_CLOUD_LOCATION`, using
  `AdcTokenProvider` to mint the token (resolving the prior self-contradiction —
  the crate now provides the token path it tests). Smoke: text turn, tool
  round-trip, native structured output.

SSE fixture files pinned LF via `.gitattributes` (`text eol=lf`) so literal `\n`
split delimiters survive Windows checkout.

## 14. Facade wiring + workspace

`crates/paigasus-helikon/Cargo.toml`:
```toml
paigasus-helikon-providers-gemini = { workspace = true, optional = true }
# features:
gemini = ["dep:paigasus-helikon-providers-gemini"]
```
`crates/paigasus-helikon/src/lib.rs`:
```rust
#[cfg(feature = "gemini")]
pub use paigasus_helikon_providers_gemini as gemini;
```
Root `Cargo.toml` `[workspace.dependencies]`:
```toml
paigasus-helikon-providers-gemini = { path = "crates/paigasus-helikon-providers-gemini", version = "0.1.0" }
gcp_auth = "0.12"   # optional, only pulled by the gemini crate's `vertex-adc` feature
```
New crate deps mirror Anthropic: `paigasus-helikon-core`, `async-trait`,
`async-stream`, `eventsource-stream`, `futures-core`, `futures-util`,
`reqwest { json, stream, rustls }`, `serde`, `serde_json`, `thiserror`, `anyhow`,
`tokio`, `tokio-util`, `tracing`; optional `gcp_auth` behind `vertex-adc`. Dev:
`wiremock`, `insta { json, yaml }`, `tokio`, `reqwest`. *(Verify `gcp_auth`’s
license is permissive at implementation time — it is MIT — per the project's
license-discipline practice.)*

The crate's `README.md` is **not** `include_str!`'d into `lib.rs` (matches
Bedrock/Anthropic); the facade README's network example stays ` ```ignore `.

**crates.io name-claim.** Reserve `paigasus-helikon-providers-gemini` on crates.io
(AC #3), following the Bedrock precedent. As a brand-new crate depending only on
the already-published `paigasus-helikon-core`, the first release-plz publish is
clean — the "ascend-a-stub needs a core bump" caveat does **not** apply.

## 15. Risks & mitigations

- **R1 — Vertex auth usability.** Resolved by the optional `vertex-adc`
  `AdcTokenProvider` (D1a); callers without it still implement `TokenProvider`.
- **R2 — Gemini schema dialect drift / wrong (not lossy) output.** Meaning is
  preserved via `anyOf`/`nullable`/`enum` mapping (§9), not collapsed; the policy
  is pinned by snapshot tests.
- **R3 — Tool-call id collisions** under parallel calls. Resolved by round-tripping
  Gemini's native `id` + a history `call_id→name` map (§7); tested by §13 #6.
- **R4 — Structured-output + tools conflict** surprising callers. Explicit,
  well-messaged `ModelError` + a snapshot test (§13 #9); history-aware exemption
  for the finalize-after-tool-use case (§8, §13 #11).
- **R5 — SSE chunk shape variance** between Developer API and Vertex. Body is
  shared; wiremock tests cover both transports against the same translator.
- **R-dup — Schema rewriter is the third copy** of `$ref`/cycle/depth machinery
  (after `bedrock/src/translate/schema.rs`). Accepted for a self-contained PR;
  tracked as a follow-up to parameterize a shared `core` helper by keyword policy.

## 16. Open questions to confirm at implementation time

- **Q1.** Confirm the live v1beta `generateContent` schema requires `role:"user"`
  (not `"function"`) for the turn carrying `functionResponse` parts.
- **Q2.** Confirm Gemini accepts `responseSchema` with prior `functionCall`/
  `functionResponse` parts in history when no `tools` are declared (the
  finalize-after-tool-use case); adjust the conflict guard if not.
- **Q3.** Confirm `gcp_auth` 0.12 covers the needed ADC sources (metadata server,
  `GOOGLE_APPLICATION_CREDENTIALS`, `gcloud` CLI) and is MIT-licensed.

---

## Appendix A — Adversarial challenge changelog

Verdict received: **NEEDS REWORK**. Triage of the `spec-challenger` findings:

**Folded in (justified):**
- *BLOCKER* tool-call id correlation → round-trip Gemini's native `id` + recover
  name via a history `call_id→name` map; never string-strip ids (§7, R3, §13 #6).
- *BLOCKER* `ToolResult`→`functionResponse.response` reduction now fully specified
  (§7).
- *MAJOR* empty/system-only `contents` guard added (§7, §13 #13).
- *MAJOR* truncated-stream: `Finish` only when a `finishReason` is observed (§10).
- *MAJOR* blocked-prompt/empty-candidates handling → `Refused`/`ContentFilter`
  (§10).
- *MAJOR* schema combinators preserved (`oneOf→anyOf`, `[T,null]→nullable`,
  `const→enum`), not collapsed (§9, R2).
- *MAJOR* `thoughtsTokenCount` → `Usage.reasoning_tokens` (D3, §10).
- *MAJOR* Vertex made a real deliverable via optional `vertex-adc` ADC provider;
  live-test token path specified (D1a, §13).
- *MAJOR* `maxOutputTokens` omitted when unset, not defaulted (§7).
- *MINOR* single `Usage` emission (§10); `EmptyApiKey`/`AuthTransportMismatch`
  build errors (§5); builder cross-product validation (§5); `Auth` enum made
  `pub(crate)` (§5); "shared suite"/scenario-8-mechanism wording (§13); "identical
  body" softened to "near-identical" (§6); env-var precedence clarified (§5);
  `candidates[1..]` ignored documented (§10); schema-dup follow-up noted (R-dup).
- *QUESTIONS* token-fetch cancellation/error placement (§6), functionResponse role
  (§16 Q1), finalize-after-tool-use (§8, §16 Q2) all addressed.

**Rejected / not acted on:** none. The one item with residual judgment —
keeping `reasoning: false` for 2.5 models — is retained deliberately (the flag
denotes `ReasoningDelta` streaming, which D3 defers) **but** paired with the
non-negotiable token-accounting fix, so 2.5 turns are not under-counted.
