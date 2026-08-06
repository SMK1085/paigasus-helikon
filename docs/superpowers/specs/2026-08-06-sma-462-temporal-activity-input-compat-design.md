# SMA-462 — runtime-temporal backward-compatible activity input

**Status:** Draft (Stage 1 spec, pending GATE 1 approval)
**Ticket:** [SMA-462](https://linear.app/smaschek/issue/SMA-462) — *runtime-temporal: backward-compatible activity input for zero-downtime worker upgrades*
**Related:** SMA-455 (introduced the arity change this ticket addresses; design decision **D6** deferred it), SMA-332 (shipped the durable runner)
**Crate:** `paigasus-helikon-runtime-temporal` (self-contained; no `paigasus-helikon-core` change)
**SDK baseline:** `temporalio-* 0.5.0`

## 1. Context & problem

SMA-455 threaded a request-scoped `ctx_seed` into the durable runtime by adding a **new
positional argument** to two activities:

| activity | 0.1.x signature | 0.2.x signature (current) |
|---|---|---|
| `render_instructions` | `(agent_name)` | `(agent_name, ctx_seed)` |
| `call_model` | `(agent_name, request)` | `(agent_name, request)` — unchanged |
| `invoke_tool` | `(agent_name, call)` | `(agent_name, call, ctx_seed)` |

`temporalio-macros`' `#[activities]` maps an N-argument activity to
`temporalio_common::data_converters::MultiArgs{N}`, which serializes to **N separate
payloads** — not one composite object. Its decoder is strict:

```rust
// temporalio-common-wasm-0.5.0/src/data_converters.rs:792-798 (impl_multi_args!)
fn from_payloads(ctx: &SerializationContext<'_>, payloads: Vec<Payload>)
    -> Result<Self, PayloadConversionError>
{
    if payloads.len() != $count {
        return Err(PayloadConversionError::WrongEncoding);
    }
    ...
}
```

So an activity task queued by a 0.1.x worker cannot be decoded by a 0.2.x worker, and vice
versa. **Any** future change to an activity's argument list reproduces the failure. Today's
mitigation is the crate's documented drain-before-upgrade / blue-green-task-queue
discipline; SMA-455's D6 accepted that and deferred the fix, which CodeRabbit had flagged
as a Major on [PR #139](https://github.com/SMK1085/paigasus-helikon/pull/139#issuecomment-4901494017).

### 1.1 The compatibility asymmetry

During a rolling deploy both old (vN) and new (vN+1) workers poll the same task queue, so
there are two independent directions:

- **New reads old** — a task queued by an old worker is picked up by a new worker.
  *Retrofittable*: we control the new decoder.
- **Old reads new** — a task queued by a new worker is picked up by an old worker. **Not
  retrofittable for the 0.2.x hop.** Those binaries are published and frozen at
  `MultiArgs{N}::from_payloads`; no change landed now can teach them a new shape.

This spec therefore cannot make the *0.2.x → this release* rollout itself bidirectionally
zero-downtime. It fixes the retrofittable direction now and establishes an envelope that
makes every **subsequent** activity-input change compatible in both directions. §7 records
what the one-time hop actually costs, which is less than it first appears.

## 2. Goals / non-goals

### Goals

- Replace positional activity arguments with a **single self-describing envelope payload**
  per activity, so future field additions are compatible in both directions.
- Decode the current **0.2.x positional shapes** transparently, so a new worker executes
  activity tasks queued by a 0.2.x worker without failing the attempt.
- Keep every activity **name** byte-identical, so in-flight workflow replay is unaffected
  (§3 D2).
- Change no public Rust API. Every new type is `pub(crate)`.
- Replace the crate's hedged upgrade-discipline prose with an evidence-backed statement.

### Non-goals

- **Temporal Worker Versioning (Build IDs)** — unsupported in the Rust SDK; remains named
  future work.
- **0.1.x wire tolerance** (§3 D4).
- Changes to `payloads::WorkflowInput` — its `ctx_seed` is already `#[serde(default)]` and
  needs nothing.
- Making the 0.2.x → this-release hop bidirectionally zero-downtime. Impossible (§1.1).
- A payload codec, claim-check blob offload, or conversation compaction — still future.

## 3. Design decisions (recorded)

> D-numbers below belong to **this** spec. SMA-455's own D-numbers are always written as
> "SMA-455's D*n*" to avoid collision — in particular, SMA-455's D6 is the decision that
> deferred this work, and is unrelated to this spec's D6.

| # | Decision | Rationale |
|---|---|---|
| D1 | Activity inputs become a **single JSON-object envelope payload** per activity, with a decoder that also accepts the 0.2.x positional shapes. | *(User decision.)* Fixes new-reads-old now and makes all future additive changes compatible in both directions. The alternatives were rejected: minimal forward-compat only defers the same break to the next change; two-phase expand/contract costs two deploys per wire change forever. |
| D2 | Activity **names stay identical** — no `_v2` suffix. | Not merely a preference. `temporalio-sdk-core-0.5.0/src/worker/workflow/machines/activity_state_machine.rs:375-395` gates replay on `CoreInternalFlags::IdAndTypeDeterminismChecks` and compares `act_id` and `act_type`. A renamed activity **would** trip the non-determinism checker on replay of in-flight workflows — strictly worse than the status quo. Input payloads are never compared, which is why the encoding may change freely. |
| D3 | **All three** activities move to envelopes, including the unchanged `call_model`. | Leaving `call_model` positional guarantees a repeat of this ticket at its first input change. One pattern, one migration, one set of docs. |
| D4 | Legacy tolerance window is **0.2.x only**. | *(User decision — YAGNI.)* Keeps dispatch to pure arity with no JSON content sniffing: with 0.1.x excluded, `1 payload` unambiguously means "envelope" for every activity. Including 0.1.x would collide on `render_instructions` (legacy 1 payload = bare JSON string vs. envelope 1 payload = object) and force ordered-attempt decoding. 0.1.1 was superseded on 2026-07-09; a 0.1.x ↔ post-SMA-462 rolling deploy is not a real scenario. |
| D5 | The envelope type is a **serde-free newtype wrapping a serde-derived args struct**. | Forced, not stylistic. `temporalio-common` carries blanket impls `impl<T: Serialize> TemporalSerializable for T` and `impl<T: DeserializeOwned> TemporalDeserializable for T` (`data_converters.rs:603-611`). A type deriving serde therefore **cannot** also hand-implement the Temporal traits — coherence conflict. |
| D6 | Unrecognized arity → `WrongEncoding`; recognized arity with bad content → `EncodingError` with a diagnostic message. | `PayloadConverter::Composite` treats `WrongEncoding` as "not my encoding" and falls through to the next converter, but returns any other error immediately (`data_converters.rs:572-580`). Using `EncodingError` for content failures surfaces the real diagnostic instead of a bare `WrongEncoding`. |
| D7 | **Every field added to an envelope after this change MUST carry `#[serde(default)]`.** | This single rule is what makes future changes bidirectionally compatible: serde ignores unknown fields on the way down and defaults absent ones on the way up. Documented on the module and asserted by a per-envelope test. |
| D8 | Codec lives in a **new `activity_input.rs` module**, not in `activities.rs` or `payloads.rs`. | `activities.rs` is already 870 lines; `payloads.rs` holds the *public* wire types. The codec has one job and no dependents beyond the activity layer and the workflow. |

## 4. Architecture

One new private module, `crates/paigasus-helikon-runtime-temporal/src/activity_input.rs`,
owning the wire codec for activity inputs. `activities.rs` (worker side) and `workflow.rs`
(scheduler side) both depend on it; it depends on neither.

### 4.1 Envelope types

Three pairs, all `pub(crate)`:

```rust
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub(crate) struct RenderInstructionsArgs {
    pub agent_name: String,
    #[serde(default)]
    pub ctx_seed: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct CallModelArgs {
    pub agent_name: String,
    pub request: ModelRequest,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct InvokeToolArgs {
    pub agent_name: String,
    pub call: ToolCallRequest,
    #[serde(default)]
    pub ctx_seed: Option<serde_json::Value>,
}

// Wrappers derive NOTHING serde (D5); they carry the hand-written Temporal codec.
pub(crate) struct RenderInstructionsInput(pub RenderInstructionsArgs);
pub(crate) struct CallModelInput(pub CallModelArgs);
pub(crate) struct InvokeToolInput(pub InvokeToolArgs);
```

Each wrapper gets `impl From<*Args> for *Input` so `start_activity`'s
`input: impl Into<AD::Input>` bound is satisfied by the args struct directly.

### 4.2 Encode

`TemporalSerializable::to_payloads` is overridden on each wrapper to emit exactly one
payload:

```rust
fn to_payloads(&self, ctx: &SerializationContext<'_>) -> Result<Vec<Payload>, PayloadConversionError> {
    Ok(vec![ctx.converter.to_payload(ctx, &self.0)?])
}
```

`ctx.converter` is the top-level `PayloadConverter` (`Composite`), whose `to_payload`
resolves the inner serde-derived struct through its `serde_json` arm — one JSON object.

### 4.3 Decode

`TemporalDeserializable::from_payloads` is overridden on each wrapper and dispatches on
`payloads.len()` alone:

| activity | 1 payload | legacy 0.2.x |
|---|---|---|
| `render_instructions` | envelope | 2 → `(agent_name, ctx_seed)` |
| `call_model` | envelope | 2 → `(agent_name, request)` |
| `invoke_tool` | envelope | 3 → `(agent_name, call, ctx_seed)` |

Both arms construct the same `*Args` value, so **everything downstream of the codec is
shape-agnostic** — `TypedRuntime`, `DurableAgentRuntime`, and the `*_inner` functions are
untouched.

### 4.4 Why the override is reached at all

`PayloadConverter::default()` is `Composite([UseWrappers, serde_json()])`
(`data_converters.rs:200-206`). The `Composite` arm tries each sub-converter in order, and
`UseWrappers` dispatches to the **overridable** trait methods `T::to_payloads` /
`T::from_payloads` (`data_converters.rs:537`, `:572`) *before* the `serde_json` arm applies
its hard `payloads.len() != 1` check. This is the same mechanism `MultiArgs{N}` itself
relies on.

### 4.5 Call sites changed

Only `workflow.rs` constructs activity inputs (`run_effects` and `execute_tools`); the
tuple literals become struct literals. `driver.rs` is SDK-free and unaffected.

## 5. Error handling

- **Unrecognized arity** → `PayloadConversionError::WrongEncoding`. The composite falls
  through to the `serde_json` arm, which also declines (the wrapper has no serde impl), so
  the net result is a clean decode failure with no misleading fallback.
- **Recognized arity, bad content** → `PayloadConversionError::EncodingError` carrying a
  message naming the activity and the matched shape, e.g. `"invoke_tool: matched legacy
  0.2.x 3-payload shape, but argument 1 is not a ToolCallRequest: <serde error>"`. This
  short-circuits the composite (D6), so the operator sees the real diagnostic.

Both classes surface identically at the Temporal layer. `temporalio-sdk-0.5.0/src/
activities.rs:416` does `let input: AD::Input = pc.from_payloads(&ctx, payloads)?;`, and the
`?` routes through `impl<E: Into<anyhow::Error>> From<E> for ActivityError`
(`activity_definition.rs:63-72`) into `ApplicationFailure::new(...)` — which is
**retryable**.

That retryability is what makes the non-retrofittable direction survivable: an old 0.2.x
worker handed a new-shape payload fails the attempt, Temporal reschedules it, and a new
worker — which reads both shapes — takes it. The direction degrades to retry latency and
noisy failure events rather than a dead run.

**Caveat to document, not paper over.** That recovery depends on the retry budget.
`render_instructions` carries no retry policy (Temporal's unlimited default — fine), but
`worker::RetryPolicyConfig` exposes `maximum_attempts: Option<u32>` for
`model_retry_policy` / `tool_retry_policy`. An operator who has set a **finite** cap can
exhaust it against old workers during a long rolling deploy. The upgrade notes must say so
explicitly and recommend either an unlimited cap for the duration of the rollout or the
existing drain/blue-green discipline.

## 6. Testing

### 6.1 Unit (`activity_input.rs`, runs on every PR)

Per envelope:

1. **Round-trip** — `to_payloads` yields exactly 1 payload; `from_payloads` recovers the
   identical `*Args`.
2. **Legacy decode** — build the 0.2.x payload vector via `MultiArgs2/3::to_payloads` and
   assert it decodes to the identical `*Args`. This is the acceptance criterion in
   miniature.
3. **Forward contract (D7)** — a JSON object omitting the `#[serde(default)]` fields
   decodes with defaults.
4. **Arity rejection** — a payload count matching neither shape yields `WrongEncoding`.
5. **Content rejection** — a recognized arity carrying garbage yields `EncodingError`, not
   `WrongEncoding`.

### 6.2 Live (`temporal_live.rs`, env-gated on `TEMPORAL_TEST_SERVER`)

A rolling-upgrade test proving forward-compat end-to-end through the real SDK rather than
through a reading of its source. `ActivityDefinition`
(`temporalio-common-wasm-0.5.0/src/activity_definition.rs:13-23`) is a three-item public
trait, so a test-only marker is cheap:

```rust
struct LegacyInvokeTool;
impl ActivityDefinition for LegacyInvokeTool {
    type Input = MultiArgs3<String, ToolCallRequest, Option<serde_json::Value>>;
    type Output = ToolCallOutcome;
    fn name() -> &'static str { "AgentActivities::invoke_tool" }
}
```

A test workflow schedules that marker against a worker built from the **new** code and
asserts the tool executes and returns a well-formed `ToolCallOutcome`. Follows the existing
harness conventions: unique uuid task queue, loud skip when unconfigured, so CI stays
green.

## 7. Upgrade story (what actually changes for an operator)

Stated plainly, because the current docs hedge on a question that is now answered:

1. **In-flight workflows replay fine.** Replay compares `act_id` and `act_type` only, never
   input payloads (D2). The encoding change cannot cause a non-determinism error.
2. **Completed activities are unaffected.** They replay from recorded results and do not
   re-execute.
3. **Activity tasks queued by a 0.2.x worker** are decoded transparently by a new worker
   (§4.3).
4. **Activity tasks queued by a new worker and picked up by a 0.2.x worker** fail the
   attempt retryably and are re-dispatched until a new worker takes them (§5) — subject to
   the `maximum_attempts` caveat.
5. **Drain-before-upgrade / blue-green task queues remain the conservative path**, unchanged
   — but they are now a belt-and-braces measure for this class of change rather than a
   requirement.

## 8. Documentation impact

- `crates/paigasus-helikon-runtime-temporal/src/lib.rs` — rewrite "Upgrade Discipline and
  Determinism". The current text hedges (*"a reasoned claim, not a guarantee proven against
  every upgrade path"*) because SMA-455's D6 could not settle the replay question. It can
  now be stated with a source citation, plus the §7 story and the envelope/`#[serde(default)]`
  contract.
- `crates/paigasus-helikon-runtime-temporal/README.md:159` — the upgrade paragraph gains the
  supported compat window.
- `crates/paigasus-helikon-runtime-temporal/CHANGELOG.md` — upgrade notes: envelope shape,
  the 0.2.x-only window, the finite-`maximum_attempts` caveat.
- `docs/book/src/concepts/runtimes.md` — **reviewed, no change needed.** It documents
  activity payload *semantics* (what each payload carries, the ~1.5 MB budget), not the
  encoding, and has no upgrade-discipline section. A conscious call per CLAUDE.md, not a
  silent skip.
- Root `README.md` / facade README — no change; the crate roster and feature → module map
  are untouched.

## 9. Release mechanics

`paigasus-helikon-runtime-temporal` is a released crate (currently `0.2.1`), so it ships
through release-plz's normal flow — no stub-ascend ritual. Every new type is `pub(crate)`,
so there is **no public Rust API change**: no `paigasus-helikon-core` bump, and therefore
no same-PR manual bump and no manual facade bump (the release-plz `dependencies_update`
cascade is not defeated here). release-plz bumps a `feat` on a `0.x` crate as a **patch**.

## 10. Open risks

- **The design rests on reading SDK source.** The `Composite`/`UseWrappers` dispatch order
  and the replay-comparison scope were established by reading `temporalio-common-wasm-0.5.0`
  and `temporalio-sdk-core-0.5.0`. The live test (§6.2) is the mitigation and should be run
  before merge, not merely compiled.
- **A future `temporalio-*` bump could change the converter dispatch order** and silently
  break the envelope path. The unit round-trip tests (§6.1) fail loudly if it does, since
  they assert the exact payload count.
- **Finite `maximum_attempts`** during a long rolling deploy (§5) — documented, not fixed.
