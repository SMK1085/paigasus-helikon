# SMA-462 — runtime-temporal backward-compatible activity input

**Status:** Draft, revised after adversarial challenge (Stage 1 spec, pending GATE 1 approval)
**Ticket:** [SMA-462](https://linear.app/smaschek/issue/SMA-462) — *runtime-temporal: backward-compatible activity input for zero-downtime worker upgrades*
**Related:** SMA-455 (introduced the arity change this ticket addresses; its design decision **D6** deferred the fix), SMA-332 (shipped the durable runner)
**Crate:** `paigasus-helikon-runtime-temporal` (self-contained; no `paigasus-helikon-core` change)
**SDK baseline:** `temporalio-* = 0.5.0`. Every citation below is against that exact version; a bump requires re-verification (§11).

> **Superseded in part by [SMA-484](https://linear.app/smaschek/issue/SMA-484).** The legacy
> pre-envelope decode arms this design added were removed in 0.3.0; §4.7's warning-goes-silent
> exit criterion was never met and was retired as unobservable to a telemetry-free crates.io
> library. Every claim below about decoding the 0.2.x positional shapes — §2, §4.4, §4.7, §6.1,
> §7 and §10 — describes 0.2.2 only. The envelope design itself stands.

## 1. Context & problem

SMA-455 threaded a request-scoped `ctx_seed` into the durable runtime by adding a **new
positional argument** to two activities:

| activity | 0.1.x signature | 0.2.x signature (current) |
|---|---|---|
| `render_instructions` | `(agent_name)` | `(agent_name, ctx_seed)` |
| `call_model` | `(agent_name, request)` | `(agent_name, request)` — unchanged |
| `invoke_tool` | `(agent_name, call)` | `(agent_name, call, ctx_seed)` |

### 1.1 How the macro chooses an input type

`#[activities]` derives `ActivityDefinition::Input` **solely** from the `#[activity]`
method's parameter list, via `multi_args_input_type`
(`temporalio-macros-0.5.0/src/activities_definitions.rs:265-278`):

```rust
match types.len() {
    0 => quote! { () },
    1 => quote! { #t },                       // the parameter's own type — NOT a wrapper
    n => quote! { MultiArgs#n<#(#types),*> }, // n >= 2
}
```

There is **no `MultiArgs1`** — `data_converters.rs:808-812` defines only `MultiArgs2`
through `MultiArgs6`. This asymmetry is the entire mechanism this design depends on: an
activity with exactly one parameter has that parameter's type as its `Input`, so we can
supply a type carrying our own codec.

For two or more parameters, `MultiArgs{N}` serializes to **N separate payloads** — not one
composite object — and its decoder is strict:

```rust
// temporalio-common-wasm-0.5.0/src/data_converters.rs:792-803 (impl_multi_args!)
fn from_payloads(ctx: &SerializationContext<'_>, payloads: Vec<Payload>)
    -> Result<Self, PayloadConversionError>
{
    if payloads.len() != $count {           // :796-798
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

### 1.2 The compatibility asymmetry

During a rolling deploy both old (vN) and new (vN+1) workers poll the same task queue, so
there are two independent directions:

- **New reads old** — a task queued by an old worker is picked up by a new worker.
  *Retrofittable*: we control the new decoder.
- **Old reads new** — a task queued by a new worker is picked up by an old worker. **Not
  retrofittable.** Published 0.2.x binaries are frozen at `MultiArgs{N}::from_payloads`; no
  change landed now can teach them a new shape.

A single release that changes both encoder and decoder therefore always has one lossy
direction. This release is such a release (D9): it fixes new-reads-old for 0.2.x-queued
tasks and makes every **subsequent** input change compatible in both directions, while the
0.2.x → this-release hop itself is one-directional. §5.2 and §7 record exactly what that
costs and how to bound it.

## 2. Goals / non-goals

### Goals

- Replace positional activity arguments with a **single self-describing envelope payload**
  per activity, so future field additions are compatible in both directions.
- Decode the **0.2.x positional shapes** transparently, so a worker on this codebase
  executes activity tasks queued by a 0.2.x worker.
- Keep every activity **name** byte-identical, so in-flight workflow replay is unaffected
  (D2).
- Change no public Rust API. Every new type is `pub(crate)`.
- Replace the crate's hedged upgrade-discipline prose with an accurate, appropriately
  scoped statement (§8), including an honest account of this release's own one-directional
  hop and its rollback constraint.

### Non-goals

- **Making the 0.2.x → this-release hop itself bidirectional.** Impossible without staging
  the encoder flip across two releases, which was considered and declined (D9).
- **Temporal Worker Versioning (Build IDs)** — unsupported in the Rust SDK; remains named
  future work.
- **0.1.x wire tolerance** (D4).
- Changes to `payloads::WorkflowInput` — its `ctx_seed` is already `#[serde(default)]`.
- Compatibility of activity **outputs**, or of `paigasus-helikon-core` types nested inside
  the envelopes (`ModelRequest`, `ToolCallRequest`). Explicitly out of scope; recorded as
  residual risk in §8 and §11.
- A payload codec, claim-check blob offload, or conversation compaction — still future.

## 3. Design decisions (recorded)

> D-numbers below belong to **this** spec. SMA-455's own D-numbers are always written as
> "SMA-455's D*n*" to avoid collision — in particular, SMA-455's D6 is the decision that
> deferred this work, and is unrelated to this spec's D6.

| # | Decision | Rationale |
|---|---|---|
| D1 | Activity inputs become a **single JSON-object envelope payload** per activity, with a decoder that also accepts the 0.2.x positional shapes. | *(User decision.)* Fixes new-reads-old and makes all future additive changes compatible in both directions. Rejected alternative: minimal forward-compat only, which defers the identical break to the next input change. |
| D2 | Activity **names stay identical** — no `_v2` suffix. | Not merely a preference. `temporalio-sdk-core-0.5.0/src/worker/workflow/machines/activity_state_machine.rs:376-396` compares `act_id` and `act_type`, gated on `CoreInternalFlags::IdAndTypeDeterminismChecks`. A rename **would** trip the non-determinism checker on any history recorded by a flag-aware core against a metadata-capable server (the gate can be inactive — `internal_flags.rs:120-138` — so the claim is conditional, but the conclusion is not). Input payloads are never compared, which is why the *encoding* may change freely. |
| D3 | **All three** activities move to envelopes, including the unchanged `call_model`. | Leaving `call_model` positional guarantees a repeat of this ticket at its first input change. One pattern, one migration, one set of docs. |
| D4 | Legacy tolerance window is **0.2.x only**. | *(User decision — YAGNI.)* 0.1.1 was superseded on 2026-07-09; a 0.1.x ↔ post-SMA-462 rolling deploy is not a real scenario. **Corrected rationale after challenge:** the earlier claim that including 0.1.x would force costly "ordered-attempt decoding" overstated it — only `render_instructions` differs (1 payload: bare JSON string vs. JSON object) and one failed serde attempt distinguishes them. The decision stands on YAGNI, not on cost. |
| D5 | The envelope type is a **serde-free newtype wrapping a serde-derived args struct**. | Forced, not stylistic. `temporalio-common-wasm-0.5.0` carries blanket impls `impl<T: Serialize> TemporalSerializable for T` (`:603-610`) and `impl<T: DeserializeOwned> TemporalDeserializable for T` (`:611-627`). A type deriving serde therefore **cannot** also hand-implement the Temporal traits — coherence conflict. |
| D6 | Unrecognized arity → `WrongEncoding`; recognized arity with bad content → `EncodingError` with a **payload-free** diagnostic. | `PayloadConverter::Composite` treats `WrongEncoding` as "not my encoding" and continues to the next converter (`data_converters.rs:577`), but returns any other error immediately (`:578`). `EncodingError` therefore surfaces the real diagnostic. It must never embed payload bytes — §5.1. |
| D7 | A **field-evolution contract** governs every future envelope change, not just `#[serde(default)]`. | See §4.6. The single-rule version proposed pre-challenge was insufficient: it governs required-ness only, says nothing about `deny_unknown_fields` / `rename` / tagging, and its "unknown fields are ignored" property is a **fail-open** for any field whose meaning is security-relevant. |
| D8 | Codec lives in a **new `activity_input.rs` module**, not in `activities.rs` or `payloads.rs`. | `activities.rs` is already 870 lines; `payloads.rs` holds the *public* wire types. The codec has one job and no dependents beyond the activity layer and the workflow. |
| D9 | **The encoder flips in this single release** — no staged two-release rollout. | *(User decision at GATE 1, after the challenge proposed staging.)* The considered alternative was: release N decodes both but still encodes the 0.2.x shape, release N+1 flips. That would have made every hop bidirectional and removed the rollback constraint, at the cost of one extra release. Declined in favour of landing the change whole. **The accepted consequences are therefore real and must be documented, not minimised:** the 0.2.x → this-release hop is one-directional (§5.2), and rolling back after this release has queued envelope-shaped tasks requires draining first (§7.4). |
| D10 | Three separate envelope types, not one shared `ActivityInput` with every field `#[serde(default)]`. | *(Considered and rejected.)* A shared type would make `request` and `call` optional at the type level, moving a compile-time guarantee into a runtime check for no benefit. Three types cost three near-identical codec impls, mitigated by a shared decode helper (§4.4). |

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

Each wrapper gets `impl From<*Args> for *Input`, satisfying `start_activity`'s
`input: impl Into<AD::Input>` bound (`temporalio-workflow-0.5.0/src/workflow_context.rs:560`).

Trait bounds check out end to end: `ActivityDefinition::Input: TemporalDeserializable +
TemporalSerializable + 'static` (both hand-implemented) and `AD::Input: Send + Sync` at
registration (`temporalio-sdk-0.5.0/src/activities.rs:401`).

### 4.2 The `#[activity]` signatures — the load-bearing change

Because `Input` is derived from the parameter list (§1.1), each `#[activity]` method must
collapse to **exactly one** parameter. Without this, the codec is dead code and `Input`
stays `MultiArgs2`/`MultiArgs3`. In `activities.rs`, the three methods currently at
`:373-388`, `:395-409` and `:416-432` become:

```rust
#[activity]
pub(crate) async fn render_instructions(
    self: Arc<Self>,
    ctx: ActivityContext,
    input: RenderInstructionsInput,
) -> Result<String, ActivityError> {
    let RenderInstructionsArgs { agent_name, ctx_seed } = input.0;
    // ... body unchanged from here on
}
```

`call_model` and `invoke_tool` follow the same pattern. Everything past the destructuring —
`DurableAgentRuntime`, `TypedRuntime`, and the `*_inner` functions — is untouched.

`#[activity(definition = ...)]` is **not** an escape hatch:
`activities_definitions.rs:559-587` emits a compile-time assertion that the generated
`Input` *equals* the definition's `Input`; it never substitutes it.

### 4.3 Encode

`TemporalSerializable::to_payloads` is overridden on each wrapper to emit exactly one
payload:

```rust
fn to_payloads(&self, ctx: &SerializationContext<'_>) -> Result<Vec<Payload>, PayloadConversionError> {
    Ok(vec![ctx.converter.to_payload(ctx, &self.0)?])
}
```

The envelope **nests**: `ctx.converter.to_payload` resolves the inner serde-derived struct
through the composite's `serde_json` arm, producing one JSON object with `request` / `call`
as nested objects. It must not stringify them — per-payload size then stays equivalent to
today's largest payload, which matters against the ~1.5 MB practical budget documented at
`src/lib.rs:300-319`.

### 4.4 Decode

`TemporalDeserializable::from_payloads` is overridden on each wrapper and dispatches on
`payloads.len()` alone:

| activity | 1 payload | legacy 0.2.x | anything else |
|---|---|---|---|
| `render_instructions` | envelope | 2 → `(agent_name, ctx_seed)` | `WrongEncoding` |
| `call_model` | envelope | 2 → `(agent_name, request)` | `WrongEncoding` |
| `invoke_tool` | envelope | 3 → `(agent_name, call, ctx_seed)` | `WrongEncoding` |

The `_ =>` arm covers 0 payloads and any arity ≥ 4. The converter's `()` special-cases
(`data_converters.rs:496-506`, `:531-535`, `:558-563`) can never fire here — all three are
keyed on `TypeId::of::<T>() == TypeId::of::<()>()`, and the wrappers are named types.

Both arms construct the same `*Args`, so **everything downstream of the codec is
shape-agnostic**. A shared generic helper carries the arity-match/diagnostic scaffolding so
the three impls do not triplicate it.

Every legacy-shape decode emits `tracing::warn!` naming the activity and the matched arity
(§4.7).

### 4.5 Why the override is reached at all

`PayloadConverter::default()` is `Composite([UseWrappers, serde_json()])`
(`data_converters.rs:200-206`). The `Composite` arm tries each sub-converter in order, and
`UseWrappers` dispatches to the **overridable** trait methods `T::to_payloads` (`:537`) /
`T::from_payloads` (`:572`) *before* the `serde_json` arm applies its hard
`payloads.len() != 1` check (`:567-570`). This is the same mechanism `MultiArgs{N}` relies on.

The re-entrant call in the envelope encoder terminates rather than recursing: the inner
serde struct's `to_payload` goes `UseWrappers` → the struct's *default* `to_payloads` →
default `to_payload` → `WrongEncoding` → falls through to `serde_json`. The blanket impls
(`:603-627`) override only `as_serde`/`from_serde`, never `to_payloads`/`from_payloads`.
**Preserve this reasoning verbatim in the module docs** — it is the non-obvious part.

The crate never configures a `DataConverter` (verified: no reference anywhere under
`src/`), so it always receives the client default. `PayloadConverter::default()` is
therefore the exact converter in production, not an approximation — which is what makes the
converter-level tests in §6.1 authoritative.

### 4.6 The field-evolution contract (D7)

Every future change to an envelope must obey all four rules:

1. **New fields carry `#[serde(default)]`.** Absent fields default on the way up.
2. **Never add `#[serde(deny_unknown_fields)]`**, and never `rename` a field or change a
   tagging attribute. Any of these silently breaks forward compatibility in a way rule 1
   does not cover.
3. **A field may only be added if *ignoring* it is semantically safe.** Serde's ignoring of
   unknown fields means an old worker silently drops the field's meaning. For anything
   posture- or authorization-adjacent that is a **fail-open**, not graceful degradation.
   Such a change requires a new activity name or an explicit version discriminant instead.
4. **Nested `paigasus-helikon-core` types are not governed by this contract.** `ModelRequest`
   and `ToolCallRequest` have serde shapes this crate does not control; a core-side change
   inside them breaks the wire exactly as before. Recorded as residual risk (§11).

Rules 1 and 2 are asserted by tests (§6.1); rules 3 and 4 are review obligations, documented
on the module.

### 4.7 Observability and the shim's exit criterion

The legacy decode arms are a bounded shim, and a shim with no removal signal becomes
permanent. Every legacy decode emits:

```
tracing::warn!(activity = %name, legacy_arity = n,
    "decoded a pre-envelope activity input; a 0.2.x-era worker is still scheduling tasks");
```

Documented as the operator's "safe to remove the shim" signal: once no such warning has
appeared for a full retention window, the arms can go. A follow-up Linear issue records the
removal.

### 4.8 Call sites changed

Only `workflow.rs` constructs activity inputs (`run_effects` at `:307-341`, `execute_tools`
at `:367-398`); the tuple literals become struct literals. `driver.rs` is SDK-free and
unaffected. A grep for the tuple construction sites is part of Task 1's verification.

## 5. Error handling

- **Unrecognized arity** → `WrongEncoding`. The composite falls through to the `serde_json`
  arm, which also declines (the wrapper has no serde impl), yielding a clean decode failure
  with no misleading fallback.
- **Recognized arity, bad content** → `EncodingError`, which short-circuits the composite
  (D6) so the operator sees the real diagnostic.

Both surface identically at the Temporal layer: `temporalio-sdk-0.5.0/src/activities.rs:416`
does `let input: AD::Input = pc.from_payloads(&ctx, payloads)?;`, and the `?` routes through
`impl<E: Into<anyhow::Error>> From<E> for ActivityError` (`activity_definition.rs:63-72`)
into `ApplicationFailure::new(...)`, which sets `non_retryable: false`
(`temporalio-common-wasm-0.5.0/src/error.rs:171-184`; the SDK's own test at `:1127-1133`
asserts it). Decode failures are therefore **retryable**.

### 5.1 The diagnostic must not carry payload bytes

Temporal history is a persistence boundary: the crate's own docs warn that `ctx_seed` "is
recorded in Temporal history — keep it small and secret-free" (`src/lib.rs:316-319`,
`src/payloads.rs:35-40`). An `EncodingError` message becomes an `ActivityTaskFailed` event
readable in the Web UI by anyone with namespace read.

The diagnostic may name: the activity, the matched arity, the failing argument index, and
the expected type. It must **never** include payload bytes or the underlying serde error's
rendering of the input value. A unit test asserts a sentinel value planted in the payload
does not appear in the message.

### 5.2 The reverse direction during this release's rollout

Because the encoder flips in this release (D9), a 0.2.x worker will be handed
envelope-shaped payloads for as long as the fleet is mixed. It fails the attempt retryably
and Temporal re-dispatches until a worker on the new code takes it. Four things bound that
recovery, and only the first was identified pre-challenge:

1. **Finite `maximum_attempts`.** `worker::RetryPolicyConfig` exposes
   `maximum_attempts: Option<u32>` for `model_retry_policy` / `tool_retry_policy`. A finite
   cap can be exhausted against old workers during a long rollout.
2. **The run deadline.** `WorkflowInput::timeout_ms` drives `run_deadline`
   (`workflow.rs:286-293`) and interrupts the whole run with `InterruptKind::TimedOut`
   regardless of retry policy. This applies to `render_instructions` too, whose unlimited
   default retry policy is therefore *not* the safety net it appears to be.
3. **`render_instructions` failure is terminal for the run.** `workflow.rs:320` routes it to
   `driver.apply_model_failure(...)` — it is not a degraded step.
4. **Exhausted `invoke_tool` retries fail quietly.** `workflow.rs:389-395` folds the failure
   into `"tool activity failed: …"` and feeds it to the model as a tool result rather than
   failing loudly.

So "degrades to retry latency, not a dead run" holds only for `invoke_tool` with unlimited
attempts and no run deadline. Each failed attempt also writes an `ActivityTaskFailed` event,
so a slow rollout consumes workflow history length as well as wall-clock.

**Operator guidance, which goes in the CHANGELOG:** keep the mixed-fleet window short, and
either drain in-flight runs first or ensure `maximum_attempts` is unlimited and run
deadlines are generous for the duration. Drain-before-upgrade / blue-green task queues
remain the conservative path for *this* upgrade specifically — the change's benefit lands on
the *next* one.

## 6. Testing

### 6.1 Converter-level unit tests (`activity_input.rs`, run on every PR)

**Every codec test routes through `PayloadConverter::default()`** via the
`GenericPayloadConverter` methods — never by calling `Wrapper::from_payloads` directly.
Calling the trait method directly would exercise none of the `Composite`→`UseWrappers`
dispatch (`data_converters.rs:565-584`) that §4.5 depends on, leaving the design's
top risk (§11) unmitigated by the very tests claimed to mitigate it.

Per envelope:

1. **Round-trip** through the default converter, asserting the payload count is **exactly 1**
   and an identical `*Args` on the way back.
2. **Legacy decode** — build the 0.2.x payload vector via `MultiArgs2/3::to_payloads` and
   assert it decodes to the identical `*Args`. This is the acceptance criterion in miniature.
3. **Envelope decode** — a **frozen JSON string literal** (not a value produced by
   serializing the current struct, which would track any drift and assert nothing) decodes
   to the expected `*Args`.
4. **Contract rule 1** — a frozen JSON literal omitting the `#[serde(default)]` fields
   decodes with defaults.
5. **Contract rule 2** — a frozen JSON literal carrying an unknown extra field still decodes,
   proving no `deny_unknown_fields` crept in.
6. **Arity rejection** — 0 payloads and a 4-payload vector both yield `WrongEncoding`.
7. **Content rejection** — a recognized arity whose `agent_name` slot holds a non-string
   yields `EncodingError`, not `WrongEncoding`. `agent_name` is named explicitly because it
   is the only field that can fail for `render_instructions`: `ctx_seed:
   Option<serde_json::Value>` accepts every JSON value, and unknown fields must be ignored
   per contract rule 2. D6 (strict diagnostics) and D7 (permissive forward compat) pull in
   opposite directions, and this is where the line sits.
8. **Diagnostic hygiene** — the §5.1 sentinel test.

### 6.2 Live coverage

The existing env-gated suite (`tests/temporal_live.rs`, gated on `TEMPORAL_TEST_SERVER`)
exercises the new envelope encoding end-to-end on a real server for free —
`happy_path_tool_roundtrip` alone covers encode→schedule→decode→execute.

**A bespoke legacy-marker live test is explicitly rejected, not silently dropped.** The
pre-challenge plan — a test-only `ActivityDefinition` marker scheduled from a test workflow
— is not implementable against the current API: `TemporalAgentWorker` wraps a private
`temporalio_sdk::Worker` (`worker.rs:267-273`) and `build()` registers exactly
`DurableAgentWorkflow` + `AgentActivities` (`:550-558`), with no hook for a test workflow.
It would require standing up a second raw worker plus a second task queue and an explicit
`ActivityOptions.task_queue` override. That is substantial work to verify one line
(`activities.rs:416`), and §6.1's converter-level tests now cover the decode contract
through the production converter. Not worth it.

Running the live suite before merge remains a manual step with no required CI context (none
of the contexts in `.github/rulesets/main-protection-checks.json` runs it). The PR
description records the command run and its output as the evidence artifact.

## 7. Upgrade story

Stated plainly, including the parts that are not favourable:

1. **In-flight workflows replay fine.** Replay compares `act_id` and `act_type` only, never
   input payloads (D2). The encoding change cannot cause a non-determinism error.
2. **Completed activities are unaffected** — they replay from recorded results.
3. **Activity tasks queued by a 0.2.x worker** are decoded transparently by a new worker
   (§4.4) — the direction this ticket set out to fix.
4. **Activity tasks queued by a new worker and picked up by a 0.2.x worker fail** and are
   re-dispatched, subject to the four bounds in §5.2. This is the accepted cost of D9.
   **Rollback constraint:** once a new worker has queued an envelope-shaped task, that
   payload is frozen in the `ActivityTaskScheduled` event and every retry re-delivers it. A
   rollback to 0.2.1 therefore leaves those activities permanently undecodable until the run
   deadline. **Drain in-flight runs before rolling back**, or accept that in-flight runs fail.
5. **Drain-before-upgrade / blue-green task queues remain the conservative path for *this*
   upgrade.** The benefit of this change lands on the *next* input change, which will be
   bidirectional by construction.

## 8. Documentation impact

- `src/lib.rs` "Upgrade Discipline and Determinism" — rewrite. The current hedge (*"a
  reasoned claim, not a guarantee proven against every upgrade path"*) exists because
  SMA-455's D6 could not settle the replay question; that part can now be stated with a
  quoted source. **The hedge is narrowed, not deleted**: the new guarantee is scoped to
  *the envelope field set of activity inputs* and to *changes after this release*, and an
  explicit caveat remains for nested `paigasus-helikon-core` types and for activity outputs
  (§4.6 rule 4), which this design does not govern. Replacing a correct hedge with a narrow
  guarantee stated broadly would be worse documentation than the hedge.
- `README.md:159` — the upgrade paragraph gains the supported compat window and a pointer to
  the §7.4 rollback constraint.
- `CHANGELOG.md` — upgrade notes: the envelope shape, the 0.2.x-only window, all four
  retry-budget bounds from §5.2, the §5.2 operator guidance, and the §7.4 rollback
  constraint. This is the most important doc surface in the change; an operator who reads
  only the CHANGELOG must still learn that this specific upgrade wants a drain.
- `docs/book/src/concepts/runtimes.md` — **one sentence added** to the retry/heartbeat/payload
  paragraph (`:35-37`) pointing at the crate docs' upgrade section. Revised from the
  pre-challenge "no change needed" call: the page has no upgrade section so the letter of
  the rule allowed a skip, but zero-downtime worker upgrade is a user-facing operational
  property and the book is the guide surface.
- Root `README.md` / facade README — no change; crate roster and feature → module map are
  untouched.

## 9. Release mechanics

`paigasus-helikon-runtime-temporal` is a released crate (currently `0.2.1`), shipping through
release-plz's normal flow — no stub-ascend ritual. Every new type is `pub(crate)`, so there
is **no public Rust API change**: no `paigasus-helikon-core` bump, and therefore no same-PR
manual bump and no manual facade bump (the release-plz `dependencies_update` cascade is not
defeated). release-plz bumps a `feat` on a `0.x` crate as a **patch**.

## 10. Follow-up issues to file

1. **Remove the 0.2.x legacy decode arms** — triggered by the §4.7 warning going silent for
   a full retention window.

## 11. Open risks

- **The design rests on reading SDK source.** The `Composite`/`UseWrappers` dispatch order,
  the macro's `1 => #t` input-type rule, and the replay-comparison scope were established by
  reading `temporalio-common-wasm-0.5.0`, `temporalio-macros-0.5.0` and
  `temporalio-sdk-core-0.5.0`. §6.1's converter-level tests are the standing mitigation: they
  use the production converter and assert exact payload counts, so a dispatch-order change in
  a future `temporalio-*` bump fails them loudly.
- **A `temporalio-*` version bump invalidates every citation here.** Re-verify §1.1, §4.5 and
  D2 on any bump.
- **Nested core types are ungoverned** (§4.6 rule 4). A `ModelRequest` / `ToolCallRequest`
  serde change breaks the wire regardless of this design. Accepted residual risk; the open
  question of whether those types are a stable wire contract is worth a separate decision.
- **This release's own rollout is one-directional** (D9, §5.2, §7.4). Accepted by decision,
  bounded by documentation and the drain recommendation — not fixed in code.
