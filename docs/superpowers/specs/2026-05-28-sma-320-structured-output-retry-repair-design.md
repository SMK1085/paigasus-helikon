# SMA-320 — Structured `output_type<T>` with retry/repair

**Status:** Design (approved for plan, 2026-05-28)
**Linear:** [SMA-320](https://linear.app/smaschek/issue/SMA-320/structured-output-typet-with-retryrepair) · Milestone: MVP · Labels: `area:core`, `stage:1`
**Branch:** `feature/sma-320-structured-output_typet-with-retryrepair`
**References:** Notion [Structured Output & Builder](https://www.notion.so/355830e8fbaa818ab932d9c646657ced), [Open Questions & Caveats](https://www.notion.so/355830e8fbaa811b975ec3a0a2d4cf6c)

## Goal

Make `.output_type::<T>()` honest, as the Linear ticket states: *"`RunResult<T>::final_output`
is `T`, not `Value`."* The loop constrains the model to `T`'s schema, validates the
terminal output, repairs **exactly once** on failure, and returns a typed result on the
direct (non-`Runner`) path:

1. The model is constrained to `T`'s JSON Schema **on the finalizing turn** (see the
   two-phase policy below) via the provider's structured-output path.
2. The terminal assistant output is validated against `T`.
3. On failure the loop performs exactly one repair turn, then either succeeds or fails
   with `AgentError::InvalidStructuredOutput { schema_errors, final_text }`.
4. The caller gets `RunResult<T>` directly via `agent.run(…).collect_typed::<T>()` —
   `final_output` **is** the struct, no manual parse step.

## What already exists (do not rebuild)

- `OutputType { schema }` on `LlmAgent`, populated by the typestate builder's
  `.output_type::<T2>()` (already bounds `T2: DeserializeOwned + JsonSchema`). (SMA-314/319)
- `ModelSettings.response_format: Option<ResponseFormat>` with `JsonSchema { name, schema, strict }`. (SMA-316)
- Both providers translate `ResponseFormat::JsonSchema`:
  - OpenAI: `to_openai_response_format` — already calls `to_strict_schema` internally when `strict == true`.
  - Anthropic: `synthesize_for_response_format` appends a synthesized tool and forces
    `tool_choice` to it, remapping its `input_json_delta` back to `TokenDelta` (so
    structured output arrives as assistant **text**). It uses the incoming schema as-is.
    A `validate_conflict` errors on `JsonSchema + ToolChoice::Tool`.
- `RunResult<T>` (generic, `final_output: T`) + `RunResult::<String>::parse_final::<T>()`. (SMA-313)
- `FinalOutput::as_text()` flattens `Vec<ContentPart>` → `String` (`loop_state.rs`).
- `AgentError::InvalidStructuredOutput` — exists as a **unit** variant; never constructed or
  matched anywhere (only the enum def + doc-comments in `tool.rs`).
- Pure resumable `transition()` state machine (`loop_state.rs`) driven by `async_stream` in
  `LlmAgent::run` (`agent.rs`). `MockModel::with_scripts` supports tool calls (`ToolCallDelta`).
- No concrete `Runner` yet (SMA-321 stub). MVP run path: `agent.run() → RunResultStreaming → collect…`.

## Design decisions

| # | Decision | Choice |
|---|----------|--------|
| D1 | Where validate-and-repair lives | **Pure `transition()` state machine** — replayable for future durable runners. |
| D2 | `OutputType` payload | **Validator-only closure** `fn(&Value) -> Result<(), Vec<String>>`. Deviates from the Linear-listed `fn(Value)->Result<Box<dyn Any>,_>` trampoline: the `AgentEvent` stream erases `T`, so a boxed `Any` cannot ride it out; the typed value is materialized by `collect_typed::<T>()`, which re-parses the validated text. A `Box<dyn Any>` would be dead weight. |
| D3 | Validation engine | **Two-tier:** serde `from_value::<T>` is authoritative; `jsonschema` enriches the error list (best-effort). |
| D4 | `strict()` layering | **Core owns one shared helper `core::schema::strict()` that providers *call*; core does NOT pre-apply it.** Core passes the **raw** schemars schema with `strict: true`; each provider normalizes for itself. |
| D5 | Repair budget | **Separate one-shot counter**, not charged to `max_turns`. Observable via a new `AgentEvent::RepairStarted` (not by counting `TurnStarted`). |
| D6 | `response_format` precedence | On the **finalizing/repair turn only**, `output_type` overrides any caller-set `response_format`. Tool-phase turns are unconstrained. |
| D7 | Constraint timing | **Two-phase:** unconstrained tool loop, then a single constrained *finalizing* turn (+ optional one repair). |
| D8 | Typed return | **Deliver now on the direct path:** `RunResultStreaming::collect_typed::<T>() -> Result<RunResult<T>, AgentError>`. The trait-object `Runner::run -> RunResult<T>` stays SMA-321 (the `&dyn Agent<Ctx>` boundary erases `T`). |

## Detailed design

### Typed return on the direct path

`agent.run()` is monomorphic in `T` at the call site, but its returned
`BoxStream<AgentEvent>` is type-erased. So we expose the typed result at *collection*
time, where the caller still knows `T`:

```rust
impl RunResultStreaming {
    /// Drain the stream and deserialize the terminal output into `T`.
    /// On a successful structured run the loop has already validated the text,
    /// so the parse cannot fail; a failed run surfaces the underlying AgentError.
    pub async fn collect_typed<T>(self) -> Result<RunResult<T>, AgentError>
    where
        T: serde::de::DeserializeOwned;
}
```

- MVP usage (satisfies the Linear AC and the spirit of the Notion example):
  ```rust
  let result = agent.run(ctx, input).await?.collect_typed::<LeukemiaSubtypeAnalysis>().await?;
  let analysis: LeukemiaSubtypeAnalysis = result.final_output; // IS the struct
  ```
- `collect()` (→ `RunResult<String>`) and `parse_final::<T>()` stay for back-compat and for
  the `T = String` case.
- **Doc-sync follow-up (required):** the Notion "Structured Output" example shows
  `runner.run(&agent, …) -> RunResult<T>`. That form lands with SMA-321. Update Notion to
  show the SMA-320 MVP form (`agent.run().collect_typed::<T>()`) and note the `runner.run`
  form as SMA-321, so we don't document an API that doesn't exist yet.

### `OutputType` gains a name + validator (`core/src/agent.rs`)

```rust
pub struct OutputType {
    pub name: String,                 // schema title / T ident; → ResponseFormat.name + repair text
    pub schema: schemars::Schema,     // raw schemars schema; → response_format + jsonschema enrichment
    validate: fn(&serde_json::Value) -> Result<(), Vec<String>>, // body: serde_json::from_value::<T>
}
```

- `from_schema::<T>()` captures the `validate` fn-pointer and derives `name` from the
  schema title (stable fallback if absent — unit-tested so `ResponseFormat.name` is never empty).
- `#[derive(Clone)]` holds; **manual `Debug`** (fn-pointers don't `Debug`-derive usefully).
- A method `OutputType::validate(&self, &Value)` keeps the field private.
- Validate-only (not deserialize-into-`T`) because the typed value can't traverse the erased
  event stream; `collect_typed` re-parses. Cheap double-parse, simplest primitive.

### `core::schema::strict()` as a shared helper providers call

- New `core/src/schema.rs`, exported as `paigasus_helikon_core::schema`.
  `pub fn strict(schema: &Value) -> Value` — the normalizer lifted from OpenAI's current
  `to_strict_schema` (additionalProperties:false; promote all `properties` into `required`;
  recurse into `properties`/`items`).
- **Documented honestly as an OpenAI/JSON-Schema strict-mode normalizer**, not a
  provider-neutral transform. Per-provider normalization for future providers
  (Bedrock/Gemini, untagged-enum collapsing — flagged in Notion "Schema interop") is a
  separate future concern, explicitly out of scope for this OpenAI+Anthropic MVP.
- **Single layer:** core passes the **raw** schemars schema in `ResponseFormat::JsonSchema`
  with `strict: true`. Providers normalize for themselves:
  - OpenAI: keeps calling `to_strict_schema` (now delegating to `core::schema::strict`) —
    unchanged behavior, no double-application.
  - Anthropic: continues to use the schema as-is (does not get OpenAI strict-mode semantics
    forced onto its tool-input schema).
- Facade re-exports with a `///` doc comment (docs `-D warnings` gate):
  ```rust
  /// OpenAI/JSON-Schema strict-mode normalizer (see `core::schema::strict`).
  pub mod schema { pub use paigasus_helikon_core::schema::strict; }
  ```
- OpenAI's existing `to_strict_schema` unit tests are preserved (re-pointed at the core
  helper); the snapshot test stays green.

### Two-phase: unconstrained tool loop → constrained finalizing turn

The output constraint and free tool-calling are distinct phases. Asserting
`response_format` on tool-calling turns breaks Anthropic (forces the synthesized output
tool on turn 0, so real tools — `fetch_flow_panel`/`fetch_karyotype` in the canonical
example — can never run) and pushes OpenAI to emit the schema prematurely.

**Policy:**
- **Phase 1 — tool-calling, unconstrained.** Applies only when the agent has tools. The
  existing loop runs with **no** `response_format`, until the model returns a response with
  no tool calls.
- **Phase 2 — finalizing, constrained.** A single model turn with: `response_format`
  derived from `output_type` (raw schema, `strict: true`); the agent's **real tools removed**
  from the request; `tool_choice` left unset (Anthropic's synthesize forces its own; OpenAI
  gets json_schema with no tools). Entered:
  - **immediately** for agents with **no tools** (turn 0 is the finalizing turn); or
  - **after Phase 1 ends** for agents with tools (one extra constrained turn).
- **Cost note (documented):** for tools + output_type, this adds exactly one model
  round-trip after the model stops calling tools. Accepted as the standard, provider-uniform
  pattern.

### State machine (`core/src/loop_state.rs`)

**`TransitionCtx`** gains `output: Option<&'a OutputType>` (validator + raw schema). The
fn-pointer/schema are reconstructed from the agent on durable replay; not serialized.
(Soundness: `TransitionCtx<'a>` already carries a lifetime, is built per-iteration, and is
not held across `.await`.)

**New `LoopState` variants:**
- `Finalizing { turn }` — the single constrained emit turn.
- `RepairingOutput { turn }` — the one repair turn.

**Transitions (including the tool-call arms):**

| From | Input | →  |
|------|-------|----|
| `CallingModel` | `ModelResponse{ tool calls }` | `ExecutingTools` (unchanged) |
| `CallingModel` (unconstrained, tools present) | `ModelResponse{ no tool calls }`, `output` set | → `Finalizing` (constrained CallModel) |
| `Start`/`CallingModel{0}`, **no tools**, `output` set | — | enter `Finalizing{0}` directly (constrained) |
| `CallingModel`, `output` **unset** | `ModelResponse{ no tool calls }` | `Done` (unchanged) |
| `Finalizing` | `ModelResponse{ no tool calls }` | validate (below) |
| `Finalizing` | `ModelResponse{ has tool calls }` | **violation** → same as validation `Err` (tools were withdrawn; a tool call is non-conforming) |
| `RepairingOutput` | `ModelResponse{ no tool calls }` | validate (below) |
| `RepairingOutput` | `ModelResponse{ has tool calls }` | **violation** → repair budget already spent → `Failed(InvalidStructuredOutput)` |

**Validation step** (on a `Finalizing`/`RepairingOutput` text response):
1. Extract terminal text via `FinalOutput::as_text()`. Non-JSON parse →
   `schema_errors = ["response was not valid JSON: <err>"]`.
2. On unconstrained-fallback formats only (`JsonObject`, no-strict providers), apply lenient
   extraction first — strip ```` ```json ```` fences / take the first JSON value — before
   declaring failure. On the two implemented providers output is clean by construction.
3. Run `output.validate(&value)` (serde, authoritative):
   - `Ok` → `Done(FinalOutput { content, usage })`.
   - `Err(serde_errs)` → build `schema_errors` via `jsonschema` against `output.schema`
     (fall back to `serde_errs` if jsonschema reports nothing), then:
     - from `Finalizing` (repair available): → `RepairingOutput`, emit
       `AgentEvent::RepairStarted { attempt: 1 }`, issue a constrained CallModel carrying a
       repair `Item::UserMessage`.
     - from `RepairingOutput` (budget spent): →
       `Failed(AgentError::InvalidStructuredOutput { schema_errors, final_text })`, emit `RunFailed`.

**Repair message + replay persistence:** `transition` synthesizes the repair
`Item::UserMessage` ("Your previous response did not match the `<name>` schema. Errors:
`<…>`. Reply with ONLY a JSON value matching the schema — no prose, no code fences.") and
returns it on `TransitionOutcome`; the **driver appends it** to its owned `conversation`
(mirroring how it already appends model-response and tool-result items). One source, no
derivation drift, so the driver's `conversation` equals the issued request's messages — the
replay invariant D1 relies on. **Determinism requirement:** a durable replay must run a
binary defining the **same** `T`/schema; a changed `T` diverges the reconstructed validator
from persisted state.

### `AgentEvent::RepairStarted`

```rust
/// A structured-output repair turn has begun (validation of the prior output failed).
RepairStarted {
    /// 1-based repair attempt index (only ever 1 under the one-shot budget).
    attempt: u32,
},
```
`AgentEvent` is `#[non_exhaustive]` → additive. AC#2 asserts on this, not on `TurnStarted`
counts (which also fire after tool rounds).

### `AgentError::InvalidStructuredOutput` gains fields (breaking → 0.2.0)

```rust
#[error("invalid structured output after one repair attempt: {schema_errors:?}")]
InvalidStructuredOutput { schema_errors: Vec<String>, final_text: String },
```
Never constructed/matched in-repo, but it is a **public** variant on a crate published at
`0.1.1`. Reshaping it is a **breaking change** → `paigasus-helikon-core` (and the facade
re-export) bump to **`0.2.0`**, not a patch/minor. `#[non_exhaustive]` on the enum does not
protect an existing variant's shape.

### Dependencies

- Add `jsonschema` to `[workspace.dependencies]` + core (`workspace = true`).
- **MSRV risk:** if `jsonschema`'s MSRV exceeds the workspace's `1.75`, **degrade
  gracefully** — drop the dep and emit `schema_errors` from the serde error string alone.
  serde is the authoritative gate (D3), so jsonschema is non-critical (error richness only).
  Do **not** bump MSRV for error-message cosmetics; `cargo msrv … verify` stays green.

## Tests & acceptance criteria

**AC#1 — typed output returns the struct directly.**
- Core integration test (`tests/structured_output.rs`), no-tools agent: `MockModel` scripts
  one valid-JSON terminal response for `LeukemiaSubtypeAnalysis`
  (`#[derive(Deserialize, JsonSchema)]`). Build with `.output_type::<LeukemiaSubtypeAnalysis>()`,
  `agent.run().collect_typed::<LeukemiaSubtypeAnalysis>()`; assert `final_output` equals the
  expected struct and the dispatched `ModelRequest` carried the `response_format`.
- **Tools + output_type (canonical example):** `MockModel` scripts a tool call, then a
  no-tool-call turn, then (on the *constrained* finalizing turn) valid JSON. Assert: real
  tools ran during Phase 1; the finalizing request had `response_format` set **and no real
  tools**; `collect_typed` returns the struct.
- Runnable doc example `crates/paigasus-helikon/examples/leukemia_classifier.rs` (real
  provider, feature-gated, not in CI).

**AC#2 — invalid JSON triggers exactly one retry, then errors.**
- `MockModel` scripts invalid output on the finalizing turn, then invalid again on the repair
  turn. Assert: exactly **one** `AgentEvent::RepairStarted { attempt: 1 }`; run ends
  `RunFailed`; the error is `InvalidStructuredOutput { schema_errors, final_text }` with
  non-empty `schema_errors` and the offending `final_text`; **no** second repair occurs.
- `(RepairingOutput, ModelResponse{has tool calls})` → terminates as `InvalidStructuredOutput`.

**Unit tests.**
- `core::schema::strict`: port OpenAI's `to_strict_schema` cases; OpenAI snapshot stays green.
- `OutputType::from_schema::<T>()` populates `name`+`schema`; `validate` Ok/Err; title fallback non-empty.
- `response_format` precedence: finalizing request reflects the `output_type` schema even when
  the caller set `model_settings.response_format`.
- jsonschema enrichment: a nested-type (`$defs`) schema produces sensible messages; the
  serde-only fallback path is exercised.

**CI gates** (all must pass): fmt, clippy `--all-features --all-targets -D warnings`,
`test --all-features`, `RUSTDOCFLAGS=-D warnings doc`, doc-coverage ≥ 80%, `convco`, deny,
audit. All new `pub` items need `///` docs.

## Out of scope / deferred

- **Trait-object `Runner::run -> RunResult<T>`** — SMA-321 (`&dyn Agent<Ctx>` erases `T`).
  SMA-320 delivers the typed return on the direct path via `collect_typed::<T>()`.
- **Per-provider schema normalization** beyond OpenAI strict (Bedrock/Gemini quirks,
  untagged-enum collapsing) — future, separate.
- **Streaming/partial structured validation** — terminal text validated whole.
- **Multi-repair / configurable budget** — exactly one repair.

## Risks

1. **`jsonschema` MSRV vs 1.75** — mitigated by graceful degradation to serde-only errors.
2. **Real-provider repair coverage ≈ nil** — constrained output is schema-valid by
   construction on both implemented providers, so the repair path is exercised only by
   `MockModel`; the feature-gated example is out of CI. The mock tests are the mitigation;
   bugs in repair will hide on real providers. Accepted, documented.
3. **Replay determinism** — replay binary must define the same `T`/schema.
4. **jsonschema enrichment vs enforced schema** — error text is built from the raw schema,
   which may differ from the per-provider strict-normalized schema actually enforced;
   best-effort only, serde covers correctness.
