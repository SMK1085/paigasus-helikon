# SMA-329 — `paigasus-helikon-providers-bedrock` (Amazon Bedrock Converse provider) — design

**Ticket:** [SMA-329](https://linear.app/smaschek/issue/SMA-329/additional-providers-bedrock-gemini-ollama-litellm)
**Branch:** `feature/sma-329-providers-bedrock`
**Scope decision (2026-06-24):** SMA-329 originally bundled four provider crates (Bedrock, Gemini, Ollama, LiteLLM). That is too much for one reviewable PR, so this issue was **scoped down to deliver only the Bedrock provider** end-to-end. The other three are split into follow-ups: Gemini → [SMA-449](https://linear.app/smaschek/issue/SMA-449), Ollama → [SMA-450](https://linear.app/smaschek/issue/SMA-450), LiteLLM → [SMA-451](https://linear.app/smaschek/issue/SMA-451). Bedrock leads because it is the hardest (AWS SDK + the explicitly-tested schema rewriter), so doing it first de-risks the design and establishes the brand-new-crate pattern the follow-ups reuse.
**Crate:** `paigasus-helikon-providers-bedrock` (**new**, version `0.1.0`, publishes to crates.io), feature `bedrock` on the facade.
**Models in scope:** Amazon Bedrock **Converse** / **ConverseStream** API via `aws-sdk-bedrockruntime`.

> This spec is written to be attacked by the adversarial spec-challenger (feature-pipeline Stage 2). §12 records the finding-by-finding disposition after that pass.

---

## 1. Problem & outcome

The SDK ships OpenAI and Anthropic `Model` implementations. Bedrock is the highest-value next provider: it fronts Anthropic Claude, Amazon Nova/Titan, Meta Llama, Mistral, and Cohere behind one AWS-authenticated API, and it is the enterprise on-ramp (IAM, VPC endpoints, data residency).

**Outcome:** a new crate `paigasus-helikon-providers-bedrock` exposing `BedrockModel` — a `paigasus_helikon_core::Model` implementation over the Bedrock **Converse** API — with the same public-surface *shape* as the OpenAI/Anthropic providers (`BedrockModel` + builder), a **per-model-capable tool-input-schema rewriter** that makes `schemars`/serde-derived schemas acceptable to Bedrock's strict validators, and a wire-format snapshot test suite of equivalent depth to the existing providers. The crate is wired into the facade behind a `bedrock` feature and documented (crate README/CHANGELOG, facade + root README, mdBook providers page).

### Non-goals (this run)
- Gemini / Ollama / LiteLLM (SMA-449/450/451).
- A runnable `examples/` binary (live AWS creds make it awkward; an `ignore`-fenced example lives in the lib/README docs instead — a conscious scope call).
- Bedrock features beyond text+tools+structured-output+streaming: **Guardrails**, document/image (vision) *input plumbing beyond the capability flag*, async `InvokeModel`/embeddings, the legacy `InvokeModel`/`InvokeModelWithResponseStream` (non-Converse) APIs, and cross-region inference profiles as first-class config. Vision/guardrails are left as capability-flag-only / future tickets.
- Family-specific *lenient* schema rulesets (only the one **Strict** ruleset ships now; the per-family seam is built so a `Lenient` ruleset is an additive change — see §4).

---

## 2. Key decisions (resolved during brainstorming)

| # | Decision | Choice | Rationale |
|---|----------|--------|-----------|
| D1 | Transport | **`aws-sdk-bedrockruntime`** (official AWS SDK) | Ticket-named. Correct SigV4 signing, binary `vnd.amazon.eventstream` decoding, retry, and the full credential chain (env/profile/SSO/IMDS) are not worth re-implementing. Cost: large Apache-2.0 dep tree; tests use the SDK mock client rather than wiremock. |
| D2 | Builder / credentials | **DI: inject `Client`/`SdkConfig`, sync `.build()`**; async `from_env` convenience | AWS config *load* is async but `Client::new(&SdkConfig)` is sync, so injecting a config/client keeps `.build()` sync like the other providers and makes tests trivially mockable. `from_env` covers the ergonomic common path. |
| D3 | Schema rewriter | **Per-model-capable architecture, one `Strict` ruleset now** | Satisfies "per-model schema rewriter" structurally (ruleset selected by `ModelFamily`) while bounding scope; the two named AC cases (tagged enums, deeply nested generics) are covered by the single Strict ruleset. |
| D4 | Streaming | **True streaming via `ConverseStream`** | Matches OpenAI/Anthropic; maps cleanly onto `ModelEvent`. |
| D5 | Structured output | **Forced-tool synthesis (Anthropic-style), family-gated** | Bedrock has no universal JSON-schema response mode; the forced-tool trick works on families that support `toolChoice: tool` (Claude/Mistral/Nova) and degrades to `Text` elsewhere, per the core `ResponseFormat` contract. |

---

## 3. Crate layout & module responsibilities

```
crates/paigasus-helikon-providers-bedrock/
  Cargo.toml          # version 0.1.0, publish = true, [lints] workspace = true
  README.md           # crates.io landing page
  CHANGELOG.md        # Keep-a-Changelog: [Unreleased] + [0.1.0]
  src/
    lib.rs            # crate docs (ignore-fenced quickstart) + pub use surface
    model.rs          # BedrockModel: impl Model; provider()="bedrock", model(), capabilities()
    builder.rs        # BedrockModel::converse(), BedrockModelBuilder, Config, BuildError, from_env
    family.rs         # ModelFamily { Anthropic, AmazonNova, AmazonTitan, Llama, Mistral, Cohere, Unknown } + from_model_id()
    capabilities.rs   # (ModelFamily) → ModelCapabilities + max_output default; conservative fallback
    error.rs          # SdkError<…> + in-stream error events → ModelError; retry-after parsing
    stream.rs         # ConverseStreamOutput → ModelEvent translator (block-index → tool_use_id map)
    translate/
      mod.rs          # ModelRequest → Converse input parts (messages, system, toolConfig, inferenceConfig, toolChoice)
      request.rs      # Item[] → Vec<Message> (text / toolUse / toolResult content blocks)
      tools.rs        # ToolDef[] → toolConfig.tools — runs each schema through the rewriter, builds ToolInputSchema::Json(Document)
      response_format.rs  # ResponseFormat → forced-tool synthesis (family-gated) + conflict guards
      schema.rs       # rewrite_tool_schema(&Value, Ruleset) -> Value + Ruleset::for_family(); the AC centerpiece
      snapshots/      # insta .snap files (schema rewriter + request translation)
  tests/
    converse_request.rs   # request-shape: ModelRequest → built Converse input (assert_debug_snapshot / field asserts via mock client)
    converse_streaming.rs # response-mapping: stubbed converse_stream events → ModelEvent sequence
    structured_output.rs  # forced-tool synthesis path end-to-end
    live.rs               # env-gated (AWS creds + model id), loud-skip without
```

Each module has one purpose, a small public surface, and is unit-testable in isolation. `schema.rs`, `family.rs`, `capabilities.rs`, and the `translate/*` functions are pure (no I/O), which is what makes the snapshot suite the heart of testing (§8).

---

## 4. Schema rewriter — `translate/schema.rs` (acceptance-criteria centerpiece)

Bedrock's Converse `toolSpec.inputSchema.json` runs a strict JSON-Schema validator whose tolerance **varies by underlying model family**. The two failure classes the ticket names:
- **Tagged enums** — serde's adjacently/internally-tagged enums (and plain enums of structs) make `schemars` emit `oneOf`/`anyOf`/`allOf` (often with `$ref` + a tag `const`). Strict Bedrock families reject these combinators.
- **Deeply nested generics** — reused/nested types make `schemars` emit `$ref` into a `$defs`/`definitions` block; Bedrock chokes on `$ref`.

### 4.1 API
```rust
/// Per-family rewrite ruleset. Today only `Strict` exists; the enum is the
/// seam for adding family-lenient rulesets later without touching callers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Ruleset { Strict }

impl Ruleset {
    pub fn for_family(family: ModelFamily) -> Self { Ruleset::Strict } // all families → Strict, for now
}

/// Make a JSON Schema acceptable to Bedrock's tool-input validator.
/// Pure function: input schema in, rewritten schema out. Idempotent.
pub fn rewrite_tool_schema(schema: &serde_json::Value, ruleset: Ruleset) -> serde_json::Value;
```

### 4.2 Strict ruleset transform (recursive, depth-bounded)
1. **Inline all `$ref`.** Collect the top-level `$defs` / `definitions` maps; replace every `{"$ref": "#/$defs/Name"}` (and `#/definitions/Name`) with a deep clone of the target, recursing into the inlined fragment. After inlining, drop the now-orphaned `$defs`/`definitions` keys. → *deeply nested generics*.
2. **Cycle / depth guard.** Track the `$ref` resolution chain; on a self-referential cycle, or beyond `MAX_DEPTH` (const, e.g. 64), substitute a permissive `{"type": "object"}` for the offending node and continue. Prevents infinite recursion on recursive types (e.g. a tree). The substitution is recorded so tests can assert it.
3. **Collapse combinators.** At any node carrying `oneOf` / `anyOf` / `allOf`, replace the node with a relaxed object: merge the candidate variants' `properties` into one object, mark **none** required, keep `type: "object"`, drop the combinator key. (Lossy on cross-variant constraints, but Bedrock-accepted — "handles the known-bad case" = produces a schema Bedrock validates, not a constraint-faithful one.) → *tagged enums*.
4. **Strip unsupported keywords.** Remove `$schema`, `$id`, `$anchor`, `format`, `examples`, `default`, `$comment` (exact set pinned by snapshot). Keep `type`, `properties`, `required`, `items`, `enum`, `const`→(kept only inside `enum` lowering), `description`, `additionalProperties`.
5. **Recurse** into `properties.*`, `items` (object or array form), and `additionalProperties` (when an object).

**Invariants** (asserted by tests, not only snapshots): under `Strict`, the output contains no `$ref`, no `$defs`/`definitions`, and no `oneOf`/`anyOf`/`allOf` anywhere; and `rewrite(rewrite(x)) == rewrite(x)` (idempotent).

### 4.3 Per-model seam
`tools.rs` calls `Ruleset::for_family(self.family)` once and threads it through. Adding a `Ruleset::Lenient` later (e.g. keep `oneOf` for Claude/Mistral) is purely additive: extend the enum, branch in `rewrite_tool_schema`, map families in `for_family`. No caller change. This is what makes the rewriter "per-model" today without paying for five rulesets we cannot live-test.

---

## 5. Builder & credentials — `builder.rs`

```rust
let model = BedrockModel::converse("anthropic.claude-3-5-sonnet-20241022-v2:0")
    .client(my_bedrock_client)   // DI: inject a pre-built aws_sdk_bedrockruntime::Client
    .build()?;                    // sync

// or:
let model = BedrockModel::converse(model_id).sdk_config(&sdk_config).build()?; // Client::new is sync

// ergonomic common path (async — loads the default credential chain):
let model = BedrockModel::from_env("anthropic.claude-3-5-sonnet-20241022-v2:0").await?;
```

- `BedrockModel::converse(model_id) -> BedrockModelBuilder`.
- `.client(Client)` (DI) **or** `.sdk_config(&SdkConfig)` → `Client::new(&cfg)`; at most one needed.
- `.region(impl Into<Region>)`, `.capabilities(ModelCapabilities)` override, `.max_output_tokens_default(u32)` override.
- `.build() -> Result<BedrockModel, BuildError>` — **sync**; errors `BuildError::MissingClient` if neither client nor sdk_config was set and this wasn't built via `from_env`; `BuildError::EmptyModelId` if blank.
- `BedrockModel::from_env(model_id) -> impl Future<Output = Result<BedrockModel, BuildError>>` and `BedrockModelBuilder::build_from_env()` — `aws_config::defaults(BehaviorVersion::latest()).region(...).load().await`, then build the client. Only the config *load* is async.
- `Config` (internal): `client`, `model_id`, `family`, `capabilities`, `max_output_default`. `BedrockModel` holds an `Arc`-cheap `Config` so it is `Clone` + `Send + Sync`.

`BehaviorVersion::latest()` is pinned via the `aws-config` dep; bumping it is a conscious chore, not implicit.

---

## 6. Streaming & event mapping — `stream.rs`, `model.rs`

`invoke()`:
1. Build the `ConverseStream` request from `ModelRequest` (§7).
2. `client.converse_stream()…send().await` → map the initial `SdkError` to `ModelError` (§9) on failure.
3. Wrap the resulting `EventReceiver` in an `async_stream` that pulls events, runs each through the `StreamTranslator`, and yields `Result<ModelEvent, ModelError>`. Honors the `CancellationToken`: on fire, drop the receiver and end the stream **without** `Finish` (core contract).

`StreamTranslator::consume(ConverseStreamOutput) -> Vec<Result<ModelEvent, ModelError>>` (state: `HashMap<i32 contentBlockIndex, String tool_use_id>`, `synthesizing_output: bool`, pending `stop_reason`):

| Converse stream event | ModelEvent |
|---|---|
| `MessageStart{role}` | — (role noted) |
| `ContentBlockStart{ start: ToolUse{ tool_use_id, name } }` | `ToolCallDelta{ call_id, name: Some(name), args_delta: "" }` (record index→id) |
| `ContentBlockDelta{ delta: Text(s) }` | `TokenDelta{ text: s }` (or `TokenDelta` remap when `synthesizing_output`) |
| `ContentBlockDelta{ delta: ToolUse{ input } }` | `ToolCallDelta{ call_id: idx→id, name: None, args_delta: input }` (or `TokenDelta{ text: input }` when synthesizing structured output) |
| `ContentBlockDelta{ delta: ReasoningContent{ text } }` | `ReasoningDelta{ text }` |
| `ContentBlockStop{ index }` | — (close block) |
| `Metadata{ usage, metrics }` | `Usage{ input_tokens, output_tokens, cached_input_tokens: usage.cacheReadInputTokens, reasoning_tokens: None }` |
| `MessageStop{ stop_reason }` | `Finish{ reason }` (mapping below) |

`stop_reason` → `FinishReason`: `end_turn`/`stop_sequence` → `Stop`; `tool_use` → `ToolCalls` (→ `Stop` if only the synthesized structured-output tool fired); `max_tokens` → `Length`; `guardrail_intervened`/`content_filtered` → `ContentFilter`; anything else → `Other(s)`.

**Usage ordering** matches the core contract: Bedrock emits `Metadata` near the end, so `Usage` precedes `Finish`; each `Usage` is a complete snapshot (last-wins), and Bedrock does not emit per-chunk usage, so the cumulative-within-turn rule holds.

---

## 7. Request translation — `translate/{mod,request,tools,response_format}.rs`

`build_request(cfg, req) -> PreparedConverse { /* messages, system, tool_config, inference_config, synthesizing_output */ }`:

- **messages** (`request.rs`): `Item[]` → `Vec<Message>`. System `Item`s collect into the Converse top-level `system` blocks (Converse separates `system` from `messages`). `UserMessage` → `Message{role: User, content:[text|toolResult…]}`; `AssistantMessage`/`ToolCall` → `Message{role: Assistant, content:[text|toolUse…]}`; `ToolResult` → a `toolResult` content block on the next user turn (`tool_use_id`, `content`). Mirrors the Anthropic translator's flush/queue discipline. Bedrock requires strictly alternating user/assistant turns — adjacent same-role items are merged into one `Message`.
- **tools** (`tools.rs`): each `ToolDef` → `ToolSpecification{ name, description, input_schema: ToolInputSchema::Json(value_to_document(rewrite_tool_schema(&def.schema, ruleset))) }`. `value_to_document` is a thin `serde_json::Value` → `aws_smithy_types::Document` adapter.
- **tool_choice** (`mod.rs`): `ModelSettings::tool_choice` → Converse `ToolChoice` (`Auto`/`Any`/`Tool{name}`), **only when the family supports it** (else omitted; logged at debug).
- **inference_config**: `max_tokens` (settings override → family default), `temperature`, `top_p`, `stop_sequences` (none from core today).
- **response_format** (`response_format.rs`): `JsonSchema`/`JsonObject` → synthesize a forced tool named `__paigasus_structured_output__` with the rewritten schema and set `tool_choice: Tool{name}` **iff the family supports forced tool choice**; set `synthesizing_output = true` so the stream translator remaps the tool-use input to `TokenDelta`s. Unsupported families: no synthesis, capability `structured_output=false`, degrade to `Text`.
- **Guards** (validation errors before any network call): a user tool named `__paigasus_structured_output__` is rejected (`reserved tool name`); `ResponseFormat::JsonSchema` combined with `ToolChoice::Tool` is rejected (`conflicting tool choice`). Same guards as the Anthropic provider.

---

## 8. Testing strategy — interpreting "same wire-format snapshot suite"

Bedrock's transport (SigV4 + binary `application/vnd.amazon.eventstream`) cannot reuse the literal SSE+wiremock fixture mechanism the OpenAI/Anthropic crates use. The AC is read as **equivalent depth of wire testing**, with the bulk of snapshotting on the pure layers (which is also where the bugs live):

1. **Schema-rewriter snapshots** (`insta` `.snap`, like OpenAI's `to_strict_schema`) — *the heart of the suite*. Inputs: (a) a serde adjacently-tagged enum with `$defs`/`$ref`; (b) a deeply-nested-generic schema with chained `$ref`s; (c) a recursive type (cycle guard); (d) a keyword-stripping case. Plus invariant assertions (no `$ref`/`$defs`/`oneOf`; idempotent).
2. **Request-translation snapshots** — `ModelRequest` → built Converse input, via `insta::assert_debug_snapshot!` on the prepared request (messages, system, toolConfig, inferenceConfig, toolChoice). Covers tool calls, tool results, system handling, structured-output synthesis.
3. **Response-mapping tests** — stub `converse_stream` with **`aws-smithy-mocks`** returning a constructed `ConverseStreamOutput` event sequence, run `invoke()`, assert the `ModelEvent` sequence (text-only; parallel tool calls; reasoning-then-text; max-tokens finish; mid-stream error; structured-output synthesis). No binary fixtures needed. *(The exact `aws-smithy-mocks` API surface — `mock!`/`Rule`/`RuleMode` and how it constructs an event-stream response — is verified in the implementation plan; if it cannot cleanly produce a streaming response, the fallback is `aws_smithy_runtime::client::http::test_util::StaticReplayClient` with recorded `vnd.amazon.eventstream` bytes captured once from a live call, stored under `tests/fixtures/` and pinned LF via `.gitattributes`.)*
4. **Live test** (`tests/live.rs`) — env-gated on AWS creds + `BEDROCK_MODEL_ID`, loud-skip without (pattern shared with the existing `live.rs` and the forkd live harness). Not run in CI.

This yields a suite of equivalent depth to the existing providers; it does not literally reuse their SSE fixtures because Bedrock's wire format is fundamentally different.

---

## 9. Error mapping — `error.rs`

`map_sdk_error(SdkError<E, R>) -> ModelError`, applied both to the initial `converse_stream().send()` error and to mid-stream error events:

| Source | `ModelError` |
|---|---|
| `ThrottlingException` (service or stream) | `RateLimited { retry_after_ms }` (parsed from `Retry-After`/`x-amzn-…` header when present) |
| `ServiceUnavailableException`, `ModelNotReadyException`, `InternalServerException`, `ModelStreamErrorException`, `ModelTimeoutException` | `Unavailable` |
| `SdkError::DispatchFailure` / `TimeoutError` / `ConstructionFailure` / `ResponseError` | `Transport(formatted)` |
| `AccessDeniedException` | `Refused { reason }` |
| `ValidationException` whose message marks an over-long input (string-match, as Anthropic does for "prompt is too long") | `ContextLengthExceeded` |
| other `ValidationException` (our malformed request / schema) | `Other(anyhow)` carrying the service message |
| anything else | `Other(anyhow)` |

Content-filter / guardrail outcomes are **not** errors — they arrive via `MessageStop{stop_reason}` and become `Finish{ ContentFilter }` (§6).

---

## 10. Facade, workspace & docs wiring

- **Root `Cargo.toml`** `[workspace.dependencies]`: add `paigasus-helikon-providers-bedrock = { path = "crates/paigasus-helikon-providers-bedrock", version = "0.1.0" }`, plus third-party pins `aws-config`, `aws-sdk-bedrockruntime`, `aws-smithy-types`, and (dev) `aws-smithy-mocks` — exact versions resolved in the plan; pinned at the workspace level per repo convention.
- **Facade `crates/paigasus-helikon/Cargo.toml`**: `paigasus-helikon-providers-bedrock = { workspace = true, optional = true }` + `bedrock = ["dep:paigasus-helikon-providers-bedrock"]`.
- **Facade `src/lib.rs`**: `/// Bedrock provider. Enabled via the `bedrock` feature.` + `#[cfg(feature = "bedrock")] pub use paigasus_helikon_providers_bedrock as bedrock;` (the `///` doc is mandatory or the `-D warnings` docs job fails).
- **New crate `Cargo.toml`**: inherits all `[workspace.package]` fields; sets only `name`, `description`, `version = "0.1.0"`; `[lints] workspace = true`.
- **Docs (same PR, per CLAUDE.md):** new crate `README.md` (drift-free `cargo add` snippet, `ignore`-fenced example) + `CHANGELOG.md`; facade `README.md` + root `README.md` crate-roster and feature→module rows; mdBook providers page under `docs/book/src/` (the plan locates the exact page; `mdbook build` must stay link-clean).

---

## 11. Risks & assumptions (explicitly unverified — confirm at GATE 1 / verify in plan)

- **R1 — brand-new crate, never name-claimed.** `paigasus-helikon-providers-bedrock` was never pre-published at `0.0.0` (unlike the SMA-385 stubs). **Assumption:** release-plz publishes a net-new workspace member in dependency order (bedrock before the facade), so the facade's `cargo publish --verify` finds `bedrock 0.1.0` already on crates.io. **Default plan:** trust this; watch the release PR's CI after merge. **Fallback:** a one-time manual name-claim pre-publish (`cargo publish` for the bedrock crate; interactive `cargo login` only — never token-as-arg). *Sven owns the release lore; this default stands unless he vetoes at GATE 1.*
- **R2 — MSRV 1.85.** `aws-sdk-bedrockruntime` / `aws-config` must compile on the workspace MSRV (1.85). **Verify** with the CI command (`cargo +1.85 …`, not `cargo metadata`/`--no-run`, per prior lesson) in the plan. If the AWS SDK demands >1.85, that escalates to a workspace-MSRV decision and is surfaced, not silently bumped.
- **R3 — cargo-deny / licenses / SBOM.** AWS SDK crates are Apache-2.0, but the default TLS backend (`aws-lc-sys`) brings ISC/OpenSSL-flavored licenses. **Plan:** configure the AWS SDK onto rustls (matching the workspace's existing `ring`/rustls usage) and add any genuinely-required license to `deny.toml`'s allowlist (documented, not blanket). `cargo deny check` and the SBOM job must stay green.
- **R4 — capability table is best-effort.** The `(family) → ModelCapabilities` map is hand-maintained and approximate; unknown models fall back to `streaming + tools` only. Documented as such; not a correctness guarantee.
- **R5 — `aws-smithy-mocks` streaming.** Whether the mock harness can construct a streaming Converse response is verified early in the plan; §8 names the `StaticReplayClient` fallback.
- **R6 — combinator collapse is lossy.** The Strict ruleset's `oneOf` collapse loses cross-variant constraints. Accepted: the AC is "handles the known-bad cases" (Bedrock accepts the schema), not "preserves all constraints". Noted in rewriter docs.

---

## 12. Adversarial spec-challenge disposition

*(Filled in after feature-pipeline Stage 2. Each BLOCKER/MAJOR/MINOR/QUESTION listed with: folded-in (and where) / rejected (with one-line reason).)*

---

## 13. Acceptance criteria (Definition of Done)

1. `crates/paigasus-helikon-providers-bedrock` exists; `BedrockModel: Model` over Converse/ConverseStream; `provider()=="bedrock"`, `model()` returns the configured id.
2. Builder per §5: DI `.client()`/`.sdk_config()` + sync `.build()`, async `from_env`; `BuildError` covers missing-client / empty-model-id.
3. Schema rewriter per §4: inlines `$ref`/`$defs`, normalizes tagged-enum combinators, strips unsupported keywords; **snapshot-tested on tagged-enum + deeply-nested-generic inputs**; invariants (no `$ref`/`$defs`/`oneOf`; idempotent) asserted.
4. Streaming maps Converse events → `ModelEvent` per §6, honoring cancellation and the usage/finish ordering contract.
5. Structured output via family-gated forced-tool synthesis; degrades to `Text` on unsupported families.
6. Error mapping per §9.
7. Wire-format snapshot suite of equivalent depth (schema-rewriter + request-translation `insta` snapshots; response-mapping tests via `aws-smithy-mocks`; env-gated `live.rs`).
8. Facade `bedrock` feature + re-export; new-crate + facade + root README + mdBook updated; `version = "0.1.0"`, `publish = true`.
9. All CI gates green: `fmt`, `clippy --all-features --all-targets -D warnings`, `test` (incl. macOS), `docs` (`-D warnings`), `doc-coverage ≥ 80%`, `book-build`, `commits`, `pr-title`, `audit`, `deny`. MSRV (R2), licenses (R3) verified.
