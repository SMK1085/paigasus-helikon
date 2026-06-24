# SMA-329 — `paigasus-helikon-providers-bedrock` (Amazon Bedrock Converse provider) — design

**Ticket:** [SMA-329](https://linear.app/smaschek/issue/SMA-329/additional-providers-bedrock-gemini-ollama-litellm)
**Branch:** `feature/sma-329-providers-bedrock`
**Scope decision (2026-06-24):** SMA-329 originally bundled four provider crates (Bedrock, Gemini, Ollama, LiteLLM). That is too much for one reviewable PR, so this issue was **scoped down to deliver only the Bedrock provider** end-to-end. The other three are split into follow-ups: Gemini → [SMA-449](https://linear.app/smaschek/issue/SMA-449), Ollama → [SMA-450](https://linear.app/smaschek/issue/SMA-450), LiteLLM → [SMA-451](https://linear.app/smaschek/issue/SMA-451). Bedrock leads because it is the hardest (AWS SDK + the explicitly-tested schema rewriter), so doing it first de-risks the design and establishes the brand-new-crate pattern the follow-ups reuse.
**Crate:** `paigasus-helikon-providers-bedrock` (**new**, version `0.1.0`, publishes to crates.io), feature `bedrock` on the facade.
**Models in scope:** Amazon Bedrock **Converse** / **ConverseStream** API via `aws-sdk-bedrockruntime`.

> **Revised after the adversarial spec-challenge** (feature-pipeline Stage 2). §12 records the finding-by-finding disposition. The challenge surfaced one **project-shaping blocker that requires a human decision before implementation**: the AWS SDK's MSRV is **1.91.1**, above the workspace's **1.85** (§0, §11/R2). It also corrected the schema-rewriter design (it must *not* re-implement the existing `core::schema::strict`; §4), the test strategy (`aws-smithy-mocks` has no event-stream support — test the pure translator directly instead; §8), and several AWS-SDK facts that were asserted rather than verified (§6).

---

## 0. ⚠️ MSRV — RESOLVED at GATE 1 (option A: raise workspace MSRV to 1.91)

> **RESOLVED (GATE 1, 2026-06-24): option (A) — raise the workspace MSRV from 1.85 → 1.91**, landed as a clearly-scoped `chore` commit within this PR (root `Cargo.toml` `rust-version`, `ci.yml` `test` matrix `1.85`→`1.91` rows, `msrv.yml`, README/CLAUDE.md MSRV text). Also at GATE 1: **R1 release path = manual name-claim pre-publish** of `bedrock 0.1.0` (§11/R1). The options analysis below is retained for the record.

`aws-sdk-bedrockruntime` (latest 1.135.0) and `aws-config` (1.8.18) both declare **`rust-version = 1.91.1`**. The workspace MSRV is **1.85** (`[workspace.package] rust-version`), and the **required** CI gate `test (…, 1.85)` builds `--all-features`, which pulls the facade's whole graph — so the AWS SDK would fail that gate. This was unknown when the transport was chosen (D1) and is the gating decision for the whole crate. Options (see §11/R2 for detail):

- **(A) Raise the workspace MSRV to 1.91.** Repo policy (CLAUDE.md: "if a dep raises MSRV, bump `rust-version` to what cargo demands rather than downgrading the dep"). Workspace-wide and externally visible: touches root `Cargo.toml` `rust-version`, the `ci.yml` `test` matrix `1.85` rows, `msrv.yml`, README/CLAUDE.md MSRV statements, and every downstream consumer's MSRV. Rust 1.91 is ~7 months old (adoptable). **Recommended, but it is Sven's call** — and arguably its own `chore` commit within this PR (or a precursor).
- **(B) Pin a stale `aws-sdk-bedrockruntime` (~1.86, MSRV 1.81, May 2025).** Keeps MSRV 1.85 but freezes on a year-old SDK, fights Dependabot (needs an ignore/group rule), and `aws-config` compatibility at ≤1.85 is uncertain. Not recommended.
- **(C) Reconsider transport (hand-rolled `reqwest` + `aws-sigv4`).** `aws-sigv4`/`aws-smithy-*` are the same fast-moving family and likely share the high MSRV, so this probably does **not** escape the problem while reopening the large cost of owning SigV4 + event-stream decoding. Not recommended.
- **(D) Defer the crate** until a workspace MSRV bump is decided independently.

Everything below assumes the crate ships on the AWS SDK (D1); it is written to be valid under option (A). If Sven chooses (B)/(C)/(D), §4–§9 still hold but §10/§11 change.

---

## 1. Problem & outcome

The SDK ships OpenAI and Anthropic `Model` implementations. Bedrock is the highest-value next provider: it fronts Anthropic Claude, Amazon Nova/Titan, Meta Llama, Mistral, and Cohere behind one AWS-authenticated API, and it is the enterprise on-ramp (IAM, VPC endpoints, data residency).

**Outcome:** a new crate `paigasus-helikon-providers-bedrock` exposing `BedrockModel` — a `paigasus_helikon_core::Model` implementation over the Bedrock **Converse** API — with the same public-surface *shape* as the OpenAI/Anthropic providers (`BedrockModel` + builder), a **per-model-capable tool-input-schema rewriter** that makes `schemars`/serde-derived schemas acceptable to Bedrock's strict validators, and a wire-format snapshot test suite of equivalent depth. Wired into the facade behind a `bedrock` feature and documented (crate README/CHANGELOG, facade + root README, the mdBook providers page).

### Non-goals (this run)
- Gemini / Ollama / LiteLLM (SMA-449/450/451).
- A runnable `examples/` binary (live AWS creds make it awkward; an `ignore`-fenced example lives in the lib/README docs — a conscious scope call).
- Bedrock features beyond text+tools+structured-output+streaming: **Guardrails**, document/image (vision) *input plumbing beyond the capability flag*, embeddings/async, the legacy `InvokeModel*` (non-Converse) APIs, and cross-region inference profiles as first-class config. Left as capability-flag-only / future tickets.
- Family-specific *lenient* schema rulesets (only the one **Strict** ruleset ships now; the per-family seam is built so a `Lenient` ruleset is an additive change — §4).
- Hoisting the `$ref`-inlining primitive into `core::schema` (forward pointer in §4.0 — done when Gemini/SMA-449 becomes the second consumer).

---

## 2. Key decisions (resolved during brainstorming)

| # | Decision | Choice | Rationale |
|---|----------|--------|-----------|
| D1 | Transport | **`aws-sdk-bedrockruntime`** | Ticket-named. Correct SigV4, binary `vnd.amazon.eventstream` decode, retry, and full credential chain for free. **New cost surfaced by the challenge: MSRV 1.91 (§0).** |
| D2 | Builder / credentials | **DI: inject `Client`/`SdkConfig`, sync `.build()`**; async `from_env` convenience | `Client::new(&SdkConfig)` is sync, so injecting keeps `.build()` sync and tests mockable without a transport mock. The AWS credential chain is **lazy** — auth failures surface at `invoke()`, not at `from_env` (§5). |
| D3 | Schema rewriter | **Bedrock-specific transform in this crate, per-model-capable, one `Strict` ruleset now** | Does **not** reuse `core::schema::strict` (that encodes OpenAI strict-mode quirks Bedrock doesn't want); the shareable `$ref`-inlining primitive is flagged for a later hoist to core (§4.0). |
| D4 | Streaming | **True streaming via `ConverseStream`** | Maps onto `ModelEvent`. Event taxonomy reconciled against the pinned SDK as task 1 (§6). |
| D5 | Structured output | **Forced-tool synthesis (Anthropic-style), family-gated** | Bedrock has no universal JSON-schema response mode; the forced-tool trick works where `toolChoice: tool` is supported (Claude/Mistral/Nova) and degrades to `Text` elsewhere. |
| D6 | Response-mapping tests | **Unit-test the pure stream translator directly** (no transport mock) | `aws-smithy-mocks` 0.2.6 has no event-stream support; constructing `ConverseStreamOutput` events and feeding the translator is cleaner and sidesteps that gap (§8). |

---

## 3. Crate layout & module responsibilities

```
crates/paigasus-helikon-providers-bedrock/
  Cargo.toml          # version 0.1.0, publish = true, [lints] workspace = true
  README.md           # crates.io landing page (disambiguates vs runtime-agentcore)
  CHANGELOG.md        # Keep-a-Changelog: [Unreleased] + [0.1.0]
  src/
    lib.rs            # crate docs (ignore-fenced quickstart) + pub use surface
    model.rs          # BedrockModel: impl Model; provider()="bedrock", model(), capabilities()
    builder.rs        # BedrockModel::converse(), BedrockModelBuilder, Config, BuildError, from_env
    family.rs         # ModelFamily { Anthropic, AmazonNova, AmazonTitan, Llama, Mistral, Cohere, Unknown } + from_model_id()
    capabilities.rs   # (ModelFamily) → ModelCapabilities + max_output default; conservative fallback
    error.rs          # SdkError<…> + in-stream error events → ModelError; retry-after parsing
    stream.rs         # ConverseStreamOutput → ModelEvent translator (block-index → tool_use_id map)
    document.rs       # serde_json::Value ⇄ aws_smithy_types::Document adapter (its own unit tests — §7)
    translate/
      mod.rs          # ModelRequest → prepared Converse request (+ a serde_json::Value wire projection for snapshots)
      request.rs      # Item[] → Vec<Message> (text / toolUse / toolResult; alternating-turn discipline — §7)
      tools.rs        # ToolDef[] → toolConfig.tools — runs each schema through the rewriter, builds ToolInputSchema::Json
      response_format.rs  # ResponseFormat → forced-tool synthesis (family-gated) + conflict guards
      schema.rs       # rewrite_tool_schema(&Value, Ruleset) -> Value + Ruleset::for_family(); the AC centerpiece
      snapshots/      # insta .snap files (schema rewriter + request wire projection)
  tests/
    converse_request.rs   # request-shape: ModelRequest → wire-JSON projection snapshots (NOT SDK Debug)
    converse_streaming.rs # translator unit tests: constructed ConverseStreamOutput events → ModelEvent sequence
    structured_output.rs  # forced-tool synthesis path
    cancellation.rs       # cancel mid-stream → stream ends without Finish
    live.rs               # env-gated (AWS creds + BEDROCK_MODEL_ID), loud-skip without
```

Every module has one purpose and a small public surface. `schema.rs`, `family.rs`, `capabilities.rs`, `document.rs`, the `translate/*` builders, and the `stream.rs` translator are **pure** (no I/O), which is what lets the snapshot/unit suite be the heart of testing (§8).

---

## 4. Schema rewriter — `translate/schema.rs` (acceptance-criteria centerpiece)

### 4.0 Relationship to `core::schema::strict` (challenge BLOCKER #1)

`paigasus_helikon_core::schema::strict` (`crates/paigasus-helikon-core/src/schema.rs`) already exists and is the canonical normalizer the **OpenAI** provider delegates to (`openai/src/translate/tools.rs:14`). It is **deliberately not reused** here, because:
- It encodes **OpenAI strict-mode quirks** — it forces `additionalProperties: false` and promotes *every* property into `required`. Bedrock does **not** want this (Claude-on-Bedrock uses schemas as-is, like the Anthropic provider); applying it would over-constrain Bedrock tool schemas.
- Its docstring explicitly scopes itself out of this: *"Per-provider normalization for future providers (Bedrock/Gemini, untagged-enum collapsing) is a separate concern"* and documents the exact **`$defs`/`$ref` gap** (it does not traverse them) that this rewriter must close.

The one genuinely shareable primitive is **`$ref`/`$defs` inlining** (neither OpenAI nor core has it; OpenAI silently mis-handles `$defs`-bearing schemas today because of the documented gap). **Decision:** implement it inside the Bedrock crate for this ticket (a Bedrock-specific transform with different goals), and leave a **forward pointer** to hoist `$ref`-inlining into `core::schema` when Gemini (SMA-449) becomes the second consumer — at which point it can also fix OpenAI's latent `$defs` gap. Hoisting now is rejected as scope creep: it would change OpenAI's output (snapshot churn + risk) and trigger the same-PR core-bump release dance (CLAUDE.md "5th step") on an already-release-sensitive new-crate PR.

### 4.1 API
```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Ruleset { Strict }                       // per-family seam; only Strict ships now
impl Ruleset { pub fn for_family(_f: ModelFamily) -> Self { Ruleset::Strict } }

/// Make a JSON Schema acceptable to Bedrock's tool-input validator.
/// Pure, idempotent: schema in, rewritten schema out.
pub fn rewrite_tool_schema(schema: &serde_json::Value, ruleset: Ruleset) -> serde_json::Value;
```

### 4.2 Strict ruleset transform (recursive, depth-bounded)
1. **Inline all `$ref`.** Collect top-level `$defs`/`definitions`; replace every `{"$ref":"#/$defs/Name"}` (and `#/definitions/Name`) with a deep clone of the target, **recursing into the inlined fragment** (chained refs resolve). After inlining, drop the orphaned `$defs`/`definitions`. → *deeply nested generics*. Edge cases the rewriter MUST handle (each gets a fixture):
   - **`$ref` with sibling keywords** (JSON Schema 2020-12 / schemars 1.x can emit `{"$ref":…, "description":…}`): the inlined target is merged with the siblings (siblings win on key conflict).
   - **`$ref` inside `items`** (object and tuple/array `items` forms) and inside `additionalProperties`.
   - **External / non-`#/` / unresolvable `$ref`** (`http…`, or a missing `#/$defs/X`): replace the node with permissive `{"type":"object"}` (never hard-error the build) so the *no-`$ref`* invariant holds.
2. **Cycle / depth guard.** Track the resolution chain; on a self-referential cycle or beyond `MAX_DEPTH` (const, e.g. 64) substitute a **terminal** `{"type":"object"}` (not re-expanded) and continue. Recursive types (e.g. a tree) terminate.
3. **Collapse tagged-enum combinators.** At a node carrying `oneOf`/`anyOf`/`allOf`, replace it with a single relaxed object, *precisely*:
   - keep `type: "object"`;
   - **tag field** (the discriminant — typically a `const`/single-`enum` string property shared across variants) → one property `{"type":"string","enum":[<all variant tags>]}` when a common tag key is detectable, else omit;
   - **payload fields** → union of the variants' `properties`, all **optional** (none added to `required`);
   - guarantee `properties` is **non-empty** (Bedrock rejects `{"type":"object","properties":{}}` on strict families); if the union is empty, emit `{"type":"object"}` (no `properties` key) instead;
   - **do not** inject `additionalProperties:false` (Bedrock does not require it; injecting over-constrains);
   - drop the `oneOf`/`anyOf`/`allOf` key.
   This is **lossy** on cross-variant constraints — accepted: the AC is "handles the known-bad case" (Bedrock *accepts* the schema), not "preserves every constraint." → *tagged enums*.
4. **Strip unsupported keywords.** Remove `$schema`, `$id`, `$anchor`, `format`, `examples`, `default`, `$comment` (exact set pinned by snapshot). Keep `type`, `properties`, `required`, `items`, `enum`, `description`, `additionalProperties`.
5. **Recurse** into `properties.*`, `items` (object/array), `additionalProperties` (when an object).

**Guarantees** (asserted by tests, not only snapshots):
- *Structural* (offline, fully testable): under `Strict` the output contains **no `$ref`, no `$defs`/`definitions`, no `oneOf`/`anyOf`/`allOf`** anywhere; no node has empty `properties`; and `rewrite(rewrite(x)) == rewrite(x)` (idempotent — asserted **including on the recursive-type fixture**).
- *Acceptance* (online): the env-gated live test (§8.4) sends a rewritten tagged-enum + nested-generic tool schema to ≥1 real Bedrock family and asserts the call is accepted. Offline tests cannot prove Bedrock acceptance; this split is explicit.

### 4.3 Per-model seam
`tools.rs` calls `Ruleset::for_family(self.family)` once and threads it. Adding a `Ruleset::Lenient` later (e.g. keep `oneOf` for Claude/Mistral) is purely additive — extend the enum, branch in `rewrite_tool_schema`, map families in `for_family`. No caller change.

---

## 5. Builder & credentials — `builder.rs`

```rust
let model = BedrockModel::converse("anthropic.claude-3-5-sonnet-20241022-v2:0")
    .client(my_bedrock_client)   // DI: inject a pre-built aws_sdk_bedrockruntime::Client
    .build()?;                    // sync
let model = BedrockModel::converse(model_id).sdk_config(&sdk_config).build()?; // Client::new is sync
let model = BedrockModel::from_env("anthropic.claude-3-5-sonnet-20241022-v2:0").await?; // ergonomic
```

- `BedrockModel::converse(model_id) -> BedrockModelBuilder`.
- `.client(Client)` (DI) **or** `.sdk_config(&SdkConfig)` → `Client::new(&cfg)`.
- `.region(impl Into<Region>)`, `.capabilities(ModelCapabilities)`, `.max_output_tokens_default(u32)`.
- `.build() -> Result<BedrockModel, BuildError>` — **sync**; `BuildError::MissingClient` if neither client nor sdk_config set (and not built via `from_env`); `BuildError::EmptyModelId` if blank. `BuildError` is **construction-only**.
- `BedrockModel::from_env(model_id).await` / `BedrockModelBuilder::build_from_env().await` — `aws_config::defaults(<pinned BehaviorVersion>).region(...).load().await`, then build the client. Only the config *load* is async; returns `BuildError` for the same sync validations.

**Credential-chain laziness (challenge BLOCKER #3).** The AWS default credential chain is **lazy**: `load().await` does **not** eagerly validate credentials, so `from_env` will **not** surface bad/expired/missing creds — those surface on the first `invoke()` as `ModelError` (mapped per §9). Documented on `from_env` so reviewers don't expect eager auth validation. `BuildError` therefore stays purely synchronous (no network variants); runtime/auth failures are `ModelError`, not `BuildError`.

**Precedence:** an injected `.client()` already has a region/credentials baked in; `.region()` then applies **only** to the `.sdk_config()`/`from_env` paths and is ignored (with a `tracing::debug!`) when a client is injected. Documented.

**`BehaviorVersion` pin.** Pin an **explicit dated** `BehaviorVersion::vYYYY_MM_DD()` (the exact value chosen in the plan), **not** `latest()`, so a Dependabot `aws-config` bump cannot silently move behavior.

**Object-safety / bounds.** `BedrockModel` holds an owned, `Clone` aws `Client` (the SDK `Client` is `Clone + Send + Sync + 'static`) in an `Arc`-cheap `Config`, so `BedrockModel: Send + Sync` and the `invoke` future + returned `BoxStream<'static, …>` (which capture an owned `Client` + `EventReceiver`) are `'static + Send`, satisfying the core contract.

---

## 6. Streaming & event mapping — `stream.rs`, `model.rs`

> **Implementation task 1 (before coding the translator):** paste the real `ConverseStreamOutput` taxonomy from the **pinned** `aws-sdk-bedrockruntime` into this section and reconcile field names/locations. Verified so far (docs.rs, latest): the enum variants are `MessageStart`, `ContentBlockStart`, `ContentBlockDelta`, `ContentBlockStop`, `MessageStop`, `Metadata`, **`Unknown`** (forward-compat — translator must ignore it), each wrapping a typed event struct (`ContentBlockStartEvent`, `ContentBlockDeltaEvent`, `ContentBlockStopEvent`, `MessageStartEvent`, `MessageStopEvent`, `ConverseStreamMetadataEvent`). The exact location of `contentBlockIndex` (expected as a field on the *event* structs, not nested in `start`/`delta`), how tool-use id/name arrive on `ContentBlockStart`, how partial tool-input JSON arrives on `ContentBlockDelta` (a `toolUse.input` string fragment), the reasoning-content delta shape, and whether `TokenUsage` carries `cacheReadInputTokens` (family-dependent) are confirmed against the SDK types in task 1 — the mapping *intent* below is stable, the field plumbing is verified then.

`invoke()`:
1. Build the Converse request from `ModelRequest` (§7).
2. `client.converse_stream()…send().await` → map an `SdkError` to `ModelError` (§9) on failure.
3. Wrap the resulting `EventReceiver` in an `async_stream` that pulls events, runs each through `StreamTranslator`, and yields `Result<ModelEvent, ModelError>`. **Cancellation:** `tokio::select!{ _ = cancel.cancelled() => break, ev = receiver.recv() => … }`; on cancel, drop the receiver and end the stream **without** `Finish` (core contract `model.rs:65-67`). The cancel-safety of `EventReceiver::recv()` under `select!` is confirmed in task 1; `tests/cancellation.rs` asserts no `Finish` is emitted on mid-stream cancel.

`StreamTranslator::consume(ConverseStreamOutput) -> Vec<Result<ModelEvent, ModelError>>` (state: `HashMap<i32 idx, String tool_use_id>`, `synthesizing_output: bool`, real-tool-fired flag, pending `stop_reason`):

| Converse stream event | ModelEvent |
|---|---|
| `MessageStart{role}` | — |
| `ContentBlockStart{ toolUse{ tool_use_id, name } }` | `ToolCallDelta{ call_id, name: Some, args_delta:"" }` (record idx→id; set real-tool flag unless it's the synthesized tool) |
| `ContentBlockDelta{ text }` | `TokenDelta{ text }` (or remap to `TokenDelta` when `synthesizing_output`) |
| `ContentBlockDelta{ toolUse{ input } }` | `ToolCallDelta{ call_id: idx→id, name: None, args_delta: input }` (or `TokenDelta{ text:input }` when synthesizing) |
| `ContentBlockDelta{ reasoningContent{ text } }` | `ReasoningDelta{ text }` |
| `ContentBlockStop{ idx }` | — |
| `Metadata{ usage }` | `Usage{ input_tokens, output_tokens, cached_input_tokens: usage.cacheReadInputTokens (if present), reasoning_tokens: None }` |
| `MessageStop{ stop_reason }` | `Finish{ reason }` (below) |
| `Unknown` | — (ignored) |

`stop_reason` → `FinishReason`: `end_turn`/`stop_sequence` → `Stop`; `tool_use` → `ToolCalls` (→ `Stop` if **only** the synthesized structured-output tool fired); `max_tokens` → `Length`; `guardrail_intervened`/`content_filtered` → `ContentFilter`; else `Other(s)`. **Both-tools-fired guard (challenge MINOR):** if a real tool *and* the synthesized structured-output tool both fired, emit `ModelError::Other` (port of `anthropic/src/stream.rs:191-194`), not a silent `Stop`.

**Usage ordering** matches the core contract: `Metadata` arrives near the end, so `Usage` precedes `Finish`; each `Usage` is a complete snapshot (last-wins); Bedrock emits no per-chunk usage, so the cumulative-within-turn rule holds.

---

## 7. Request translation — `translate/{mod,request,tools,response_format}.rs`, `document.rs`

`build_request(cfg, req) -> PreparedConverse` plus a **`to_wire_json(&PreparedConverse) -> serde_json::Value`** projection used only for snapshot tests (§8.2) — we snapshot wire-stable JSON, never the SDK types' `Debug`.

- **messages** (`request.rs`): `Item[]` → `Vec<Message>`. System `Item`s collect into the Converse top-level `system` blocks. `UserMessage` → `Message{role:User, content:[text|toolResult…]}`; `AssistantMessage`/`ToolCall` → `Message{role:Assistant, content:[text|toolUse…]}`; `ToolResult` → a `toolResult` block on the next user turn. **Converse ordering is stricter than Anthropic's** and must be enforced: turns strictly alternate user/assistant; the **first** message must be `user`; a `toolResult` must sit in the user turn immediately following its `toolUse`. The translator (a) merges adjacent same-role items into one `Message`, (b) handles a **leading assistant** turn (synthesize a minimal preceding user turn, or document rejection), and (c) handles the **empty conversation** (`BuildError`/validation error before any network call). Mirrors the Anthropic flush/queue discipline, extended for these rules — explicitly tested.
- **tools** (`tools.rs`): each `ToolDef` → `ToolSpecification{ name, description, input_schema: ToolInputSchema::Json(value_to_document(rewrite_tool_schema(&def.schema, ruleset))) }`.
- **`document.rs`** (challenge MAJOR): `value_to_document(&Value) -> Document` is its **own module with its own unit tests** — not a "thin adapter." `Document` is a recursive enum with `Number = PosInt(u64) | NegInt(i64) | Float(f64)`; the `serde_json::Number → Document::Number` mapping is specified and tested for: `u64` > `i64::MAX`, negative `i64`, `f64`, integer-vs-float distinction, nested object, `null`, empty array, bool, string. `arbitrary_precision` is not enabled; if encountered, fall back to `Float`/string with a documented choice.
- **tool_choice** (`mod.rs`): `ModelSettings::tool_choice` → Converse `ToolChoice` (`Auto`/`Any`/`Tool{name}`), **only when the family supports it** (else omitted; `tracing::debug!`).
- **inference_config**: `max_tokens` (settings override → family default), `temperature`, `top_p`.
- **response_format** (`response_format.rs`): `JsonSchema`/`JsonObject` → synthesize a forced tool `__paigasus_structured_output__` with the **rewritten** schema and `tool_choice: Tool{name}` **iff the family supports forced tool choice**; set `synthesizing_output=true` so the stream translator remaps the tool-use input to `TokenDelta`s. Unsupported families: no synthesis, capability `structured_output=false`, degrade to `Text`.
- **Guards** (validation errors before any network call, mirroring Anthropic): a user tool named `__paigasus_structured_output__` → `reserved tool name`; `ResponseFormat::JsonSchema` + `ToolChoice::Tool` → `conflicting tool choice`.

---

## 8. Testing strategy — "same wire-format snapshot suite"

Bedrock's transport (SigV4 + binary `vnd.amazon.eventstream`) cannot reuse the OpenAI/Anthropic SSE+wiremock fixture mechanism, and `aws-smithy-mocks` 0.2.6 has **no event-stream support** (verified) — so the AC is read as **equivalent depth**, achieved by testing the **pure** layers directly (which is where the bugs live and is *more* robust than transport mocking):

1. **Schema-rewriter snapshots** (`insta` `.snap`, like OpenAI's `to_strict_schema`) — *the heart*. Fixtures: (a) serde adjacently-tagged enum with `$defs`/`$ref`; (b) deeply-nested-generic with chained `$ref`s; (c) recursive type (cycle guard + idempotency); (d) `$ref`-with-siblings; (e) external/unresolvable `$ref`; (f) keyword-stripping. Plus invariant assertions (§4.2 Guarantees).
2. **Request wire-projection snapshots** — `to_wire_json(build_request(req))` (a `serde_json::Value`, **not** SDK `Debug`) via `insta::assert_json_snapshot!`: tool calls, tool results, system handling, alternating-turn merges, structured-output synthesis, tool_choice.
3. **Stream-translator unit tests** (challenge D6) — construct `ConverseStreamOutput` events with the SDK type builders, feed `StreamTranslator::consume`, assert the `ModelEvent` sequence: text-only; parallel tool calls; reasoning-then-text; max-tokens finish; mid-stream error; structured-output synthesis; both-tools-fired error. **No HTTP/transport mock.** Plus `tests/cancellation.rs` (cancel mid-stream → no `Finish`).
4. **Live test** (`tests/live.rs`) — env-gated on AWS creds + `BEDROCK_MODEL_ID`, loud-skips without (pattern shared with the existing `live.rs`/forkd harness). Validates real Bedrock acceptance of a rewritten tagged-enum + nested-generic schema (the §4.2 acceptance half). Not run in CI.

---

## 9. Error mapping — `error.rs`

`map_sdk_error(SdkError<E, R>) -> ModelError`, applied to the initial `send()` error and to mid-stream error events:

| Source | `ModelError` |
|---|---|
| `ThrottlingException` (service or stream) | `RateLimited { retry_after_ms }` (parsed from `Retry-After`/`x-amzn-…` when present) |
| `ServiceUnavailable`, `ModelNotReady`, `InternalServer`, `ModelStreamError`, `ModelTimeout` | `Unavailable` |
| `SdkError::DispatchFailure`/`TimeoutError`/`ConstructionFailure`/`ResponseError` | `Transport(formatted)` |
| `AccessDenied` | `Refused { reason }` |
| `ValidationException` marking over-long input (string-match, as Anthropic does for "prompt is too long") | `ContextLengthExceeded` |
| other `ValidationException` (our malformed request/schema) | `Other(anyhow)` with the service message |
| anything else | `Other(anyhow)` |

Content-filter / guardrail outcomes are **not** errors — they arrive via `MessageStop{stop_reason}` → `Finish{ContentFilter}` (§6).

---

## 10. Facade, workspace & docs wiring

- **Root `Cargo.toml`** `[workspace.dependencies]`: add `paigasus-helikon-providers-bedrock = { path = "…", version = "0.1.0" }`, plus **exact** pins (no `"*"` — `deny.toml` `wildcards = "deny"` for registry deps) for `aws-config`, `aws-sdk-bedrockruntime`, `aws-smithy-types`; the exact versions (and the MSRV-resolved choice per §0) recorded here in the plan.
- **Facade `Cargo.toml`**: `paigasus-helikon-providers-bedrock = { workspace = true, optional = true }` + `bedrock = ["dep:paigasus-helikon-providers-bedrock"]`.
- **Facade `src/lib.rs`**: `/// Bedrock provider (Converse model). Enabled via the `bedrock` feature.` + `#[cfg(feature = "bedrock")] pub use paigasus_helikon_providers_bedrock as bedrock;` (the `///` doc is mandatory for the `-D warnings` docs job). The doc text **disambiguates** the `bedrock` *model provider* from the existing `runtime-agentcore` ("AWS Bedrock AgentCore runtime", `lib.rs:55`).
- **New crate `Cargo.toml`**: inherits all `[workspace.package]` fields; sets only `name`, `description`, `version = "0.1.0"`; `[lints] workspace = true`.
- **TLS / licenses / deny (challenge MAJOR, R3):** configure the AWS SDK onto **rustls** (matching the workspace's existing rustls/`ring` usage) — the specific `aws-config`/`aws-smithy-runtime` `rustls` features + `default-features = false` dance is pinned in the plan. Then run `cargo deny check licenses` against the **resolved rustls-configured** graph and enumerate any licenses needing allowlisting in `deny.toml`, **each with justification** (matching the existing comment style), across all **five** `deny.toml` targets. This is an **early-plan gate**, not a post-hoc fixup.
- **Docs (same PR, per CLAUDE.md):** new crate `README.md` (drift-free `cargo add`, `ignore`-fenced example, agentcore disambiguation) + `CHANGELOG.md`; facade `README.md` + root `README.md` crate-roster and feature→module rows; the mdBook providers page **`docs/book/src/concepts/model-providers.md`** (`SUMMARY.md:15`). `mdbook build` must stay link-clean.

---

## 11. Risks & assumptions

- **R2 — MSRV 1.91 vs 1.85 (BLOCKER; see §0).** Verified: `aws-sdk-bedrockruntime` 1.135.0 and `aws-config` 1.8.18 require **1.91.1**; the highest `aws-sdk-bedrockruntime` with MSRV ≤ 1.85 is ~1.86.0 (MSRV 1.81, May 2025). **Decision (GATE 1): option A — raise the workspace MSRV to 1.91** (scoped `chore` commit in this PR). The plan verifies with the CI command (`cargo +1.91 build -p paigasus-helikon-providers-bedrock`, lib-only per the prior MSRV lesson), not `cargo metadata`/`--no-run`, and updates the `ci.yml` `test` matrix, `msrv.yml`, and README/CLAUDE.md MSRV statements in the same PR.
- **R1 — brand-new crate, never name-claimed.** `paigasus-helikon-providers-bedrock` was never pre-published at `0.0.0` (unlike the SMA-385 stubs), and `release-plz.toml` has **no** net-new-member handling — the workspace's only precedent is *stub-ascend* (pre-claimed at 0.0.0). **Decision (GATE 1): manual name-claim pre-publish.** Before/with merge, publish `paigasus-helikon-providers-bedrock 0.1.0` to crates.io (interactive `cargo login` only — never token-as-arg) so the facade's `cargo publish --verify` is guaranteed to find it and we don't bet the release on release-plz's untested net-new-member path. The plan sequences this pre-publish and notes the crate ships at `0.1.0` (not a `0.0.0` placeholder), so its own release thereafter follows the normal release-plz flow.
- **R3 — cargo-deny / licenses / SBOM.** AWS SDK is Apache-2.0 but the default `aws-lc-sys` TLS backend brings ISC/OpenSSL-flavored licenses → use rustls (§10). Resolved as an early-plan `cargo deny check licenses` gate.
- **R4 — capability table is best-effort.** Hand-maintained `(family) → ModelCapabilities`; unknown models fall back to `streaming + tools`. Documented, not a correctness guarantee.
- **R5 — RESOLVED.** `aws-smithy-mocks` has no event-stream support; response mapping is tested by unit-testing the pure translator (§8.3), removing the dependency on transport mocking entirely.
- **R6 — combinator collapse is lossy.** The Strict ruleset's tagged-enum collapse loses cross-variant constraints. Accepted (AC = Bedrock accepts; §4.2). Noted in rewriter docs.

---

## 12. Adversarial spec-challenge disposition

Challenger verdict: **NEEDS REWORK**. All findings I verified against the codebase / crates.io before acting.

**BLOCKERs — all folded:**
- *Rewriter ignores `core::schema::strict`* → **folded** (§4.0): verified the module exists and OpenAI delegates to it; decided deliberate non-reuse (OpenAI-specific quirks) + forward pointer to hoist `$ref`-inlining to core at SMA-449. (Challenger's "extend core now" alternative **rejected** as scope creep + same-PR core-bump release friction; documented.)
- *`oneOf`/`anyOf` collapse underspecified / may emit rejected schemas* → **folded** (§4.2 step 3): precise post-collapse shape, non-empty `properties` guarantee, no spurious `additionalProperties`, structural-vs-acceptance test split, live-test acceptance check.
- *`from_env` returning `BuildError` is incoherent; chain is lazy* → **folded** (§5): `BuildError` stays construction-only; documented that credential failures surface at `invoke()` as `ModelError`, chain is lazy (no eager validation).

**MAJORs — folded:** event taxonomy asserted-not-verified → **folded** (§6, verified variant list, made field-plumbing reconciliation implementation-task-1); cancellation construct unproven → **folded** (§6 `select!` + `tests/cancellation.rs`, cancel-safety confirmed in task 1); `Value→Document` non-trivial → **folded** (§7 own `document.rs` + number-mapping tests); `aws-smithy-mocks` streaming unverified → **folded/RESOLVED** (verified no event-stream support; §8.3 tests the pure translator instead, dependency dropped); `assert_debug_snapshot` brittle → **folded** (§8.2 snapshots a `to_wire_json` `Value` projection, not SDK `Debug`); deny/TLS under-specified → **folded** (§10 rustls + early `cargo deny` gate, 5 targets, exact pins); MSRV under-mitigated → **folded/escalated** (§0, R2).

**MINORs — folded:** agentcore naming collision (§10 disambiguation, verified `lib.rs:55`); idempotency on recursive fixture + terminal substitution (§4.2); `$ref` edge cases — siblings/items/chained/external (§4.2 step 1); both-tools-fired guard (§6); precise mdBook path `concepts/model-providers.md` (§10, verified `SUMMARY.md:15`).

**QUESTIONs — addressed:** `'static + Send` stream bounds (§5); alternating-turn / leading-assistant / empty-conversation / toolResult ordering (§7); `BehaviorVersion` pin policy → explicit dated version (§5); region-vs-injected-client precedence (§5); R1 release decision → escalated to GATE 1 (§11/R1).

**Nothing rejected silently.** The only pushed-back item is "extend `core::schema` now," rejected with the §4.0 justification.

---

## 13. Acceptance criteria (Definition of Done)

1. `crates/paigasus-helikon-providers-bedrock` exists; `BedrockModel: Model` over Converse/ConverseStream; `provider()=="bedrock"`, `model()` returns the configured id.
2. Builder per §5: DI `.client()`/`.sdk_config()` + sync `.build()`, async `from_env` (lazy-chain documented); `BuildError` construction-only.
3. Schema rewriter per §4: inlines `$ref`/`$defs` (incl. siblings/items/chained/external edge cases), collapses tagged-enum combinators to the precise shape, strips unsupported keywords; **snapshot-tested** on tagged-enum + nested-generic + recursive + edge-case inputs; structural invariants (no `$ref`/`$defs`/`oneOf`; non-empty `properties`; idempotent incl. recursive) asserted; live-test acceptance check present. Does **not** duplicate `core::schema::strict` (§4.0).
4. Streaming maps Converse events → `ModelEvent` per §6 (taxonomy reconciled against the pinned SDK), honoring cancellation (no `Finish`) and the usage/finish ordering; both-tools-fired guard present.
5. Structured output via family-gated forced-tool synthesis; degrades to `Text` on unsupported families.
6. Error mapping per §9. `Value↔Document` adapter unit-tested per §7.
7. Wire-format snapshot suite of equivalent depth (schema-rewriter + request wire-projection `insta` snapshots; pure stream-translator unit tests; `cancellation.rs`; env-gated `live.rs`). No transport-mock dependency.
8. Facade `bedrock` feature + re-export (agentcore-disambiguated); new-crate + facade + root README + mdBook `concepts/model-providers.md` updated; `version = "0.1.0"`, `publish = true`.
9. **MSRV decision (§0) implemented and verified**; licenses/deny green across 5 targets (§10); R1 release path chosen (§11).
10. All CI gates green: `fmt`, `clippy --all-features --all-targets -D warnings`, `test` (incl. macOS + the MSRV row), `docs` (`-D warnings`), `doc-coverage ≥ 80%`, `book-build`, `commits`, `pr-title`, `audit`, `deny`.
