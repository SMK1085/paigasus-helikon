# SMA-484 — runtime-temporal: remove the 0.2.x legacy activity-input decode arms

**Status:** Draft, revised after adversarial challenge (Stage 1 spec, pending GATE 1 approval)
**Ticket:** [SMA-484](https://linear.app/smaschek/issue/SMA-484/runtime-temporal-remove-the-02x-legacy-activity-input-decode-arms)
**Related:** [SMA-462](https://linear.app/smaschek/issue/SMA-462) (added the arms this ticket removes; PR [#176](https://github.com/SMK1085/paigasus-helikon/pull/176)), SMA-455 (introduced the arity change SMA-462 addressed)
**Crate:** `paigasus-helikon-runtime-temporal` (self-contained; no `paigasus-helikon-core` change)
**SDK baseline:** `temporalio-* = 0.5.0` — unchanged from SMA-462; this design adds no new SDK-behaviour claims beyond those §11 of the SMA-462 design already carries.

## 1. Context

SMA-462 replaced the positional activity inputs with a single self-describing
envelope payload, and shipped **legacy decode arms** that also accept the
pre-envelope positional shapes:

| activity | envelope arity | legacy arity |
| -- | -- | -- |
| `AgentActivities::render_instructions` | 1 | 2 — `(agent_name, ctx_seed)` |
| `AgentActivities::call_model` | 1 | 2 — `(agent_name, request)` |
| `AgentActivities::invoke_tool` | 1 | 3 — `(agent_name, call, ctx_seed)` |

Each legacy arm calls `warn_legacy(...)`, whose `tracing::warn!` was designed
(SMA-462 design §4.7) as the operator's "safe to remove the shim" signal. This
ticket removes those arms.

### 1.1 The stated trigger has not fired — recorded, not re-litigated

SMA-484's own trigger is *"that warning going silent … for a full retention
window."* It has not fired and could not have: SMA-462 merged on **2026-08-07**
(`db77ec8`) and released as `0.2.2` the same day. The shim was hours old when
this ticket was picked up.

Two further facts were surfaced at intake and are recorded here so the decision
is auditable:

1. **The criterion is unobservable to this project.** The warning fires in
   *downstream operators'* logs. `paigasus-helikon-runtime-temporal` is a
   crates.io library with no telemetry, so "the warning has gone silent" is a
   condition the maintainers can never verify — which would make the shim
   permanent by default, the exact outcome SMA-462 §4.7 was written to prevent.
2. **`0.2.2` was published hours earlier**, so the population of workers
   depending on the legacy arms is presumed empty.

The maintainer reviewed both and **decided to remove the arms now**, treating
`0.2.2` as un-adopted. That decision is settled and is the premise of this
design. What follows is how the removal lands safely, not whether it should.

### 1.2 Why the removal is coherent anyway: `0.2.2` is a migration bridge

**Terminology — "drain".** Throughout this document, *drain* means: stop
starting new workflow executions on the task queue, and wait until every
execution already on it has reached a **terminal** state (completed, failed,
timed out, terminated). It does **not** mean merely pausing new runs. An
operator verifies it with `temporal workflow list` against the task queue
returning no open executions.

Because `0.2.2` decodes **both** shapes and `0.3.0` decodes only the envelope,
the version matrix for **activity inputs** works out as:

| from → to | activity inputs |
| -- | -- |
| `0.2.2` → `0.3.0` | compatible in **both** directions — both encode and decode the envelope. No drain needed on account of this change. |
| `0.2.0` / `0.2.1` → `0.3.0` (direct) | **broken in both directions** — `0.3.0` cannot decode legacy-queued tasks, and (pre-existing since `0.2.2`) a `0.2.0`/`0.2.1` worker cannot decode an envelope. |
| `0.2.0` / `0.2.1` → `0.2.2` → `0.3.0` | works **if in-flight runs are drained while the fleet is on `0.2.2`**. `0.2.2` decoding the legacy shape lets already-queued legacy tasks *complete*; the drain is what guarantees none is still pending at the `0.3.0` cutover. |

So the shim is not retracted into uselessness. `0.2.2` stays on crates.io as the
one-hop bridge off the pre-envelope wire, and the crate's existing **"upgrade
one release at a time"** rule already routes operators through it. Making that
hop explicit in the docs (§5.1) is what turns this from a withdrawn promise into
a normal deprecation.

**One bound on that promise:** `0.2.2` is a fixed artifact pinned to the
`paigasus-helikon-core` version it shipped with, and nested core types
(`ModelRequest`, `ToolCallRequest`) are explicitly ungoverned by the
field-evolution contract (SMA-462 §4.6 rule 4). A future core serde change could
leave `0.2.2` unable to decode a `0.3.x`-era `call_model` payload. The bridge is
reliable for the legacy→envelope hop taken promptly; it is not a guarantee that
`0.2.2` interoperates with arbitrarily distant future releases.

### 1.3 The mixed-fleet self-heal — the recovery this design leans on

When a `0.3.0` worker rejects a legacy-shaped task, the attempt fails
**retryably** and Temporal re-dispatches it to the task queue. **Any `0.2.2`
worker still polling that queue will decode and execute it.**

Two consequences, both load-bearing:

- A `0.2.2` → `0.3.0` **rolling** deploy is safe even for straggler legacy
  tasks, for as long as at least one `0.2.2` worker is still polling.
- It gives §9's residual risk a concrete one-step recovery: **re-join a worker
  on `0.2.2` to the task queue and let in-flight runs drain.** This is the
  remedy the §4.1 diagnostic points at, and the reason D4 was amended.

## 2. Goals / non-goals

### Goals

- Delete the three legacy positional decode paths, so no pre-envelope shape is
  ever decoded.
- Fail **closed, loudly, and legibly**: an operator who meets a pre-envelope
  task gets a diagnostic naming the shape and a remedy that is actually
  available at that moment — in worker logs *and* in Temporal history.
- Preserve the test suite's power: the legacy shapes must be proven *refused*,
  not merely absent from the `match`.
- Signal the wire break in the version number, and make it visible to facade
  consumers too.
- Leave the crate docs, README and CHANGELOG describing the wire truthfully.

### Non-goals

- No change to **encode**. The envelope still serializes to exactly one payload.
- No change to activity **outputs**, to `payloads::WorkflowInput`, or to the
  `#[activity]` signatures.
- No change to the field-evolution contract, and no attempt to govern nested
  `paigasus-helikon-core` types — they remain ungoverned, per SMA-462 §4.6
  rule 4.
- 0.1.x shapes stay unhandled; they already fail closed and stay that way (§9).
- No Temporal Worker Versioning / Build IDs work.
- No change to retry semantics or activity options (§6).

## 3. Design decisions (recorded)

| id | decision | rationale |
| -- | -- | -- |
| **D1** | The removed arities get **named fail-closed arms** returning `EncodingError`, not a fall-through to the generic `_ => WrongEncoding`. | `WrongEncoding` means "not my encoding" to the `Composite` converter, which then falls through to the `serde_json` arm (also declining) and the operator sees a bare `Wrong encoding` in `ActivityTaskFailed` — no activity name, no arity, no remedy. Verified against `temporalio-common-wasm-0.5.0/src/data_converters.rs:200-206,573-583` (composite continues on `WrongEncoding`, returns any other error immediately) and `:220` (`Display` for `WrongEncoding` is the bare string). Since §1.1's premise is *assumed* rather than verified, this diagnostic is the primary mitigation. |
| **D2** | Ship as `feat(runtime-temporal)!` → **0.3.0**. | The crate's own "upgrade one release at a time" rule makes the version number the operator's primary compatibility signal; a compat *removal* hidden in a patch makes that rule unreadable. Precedent: SMA-482's `feat(runtime)!` took `runtime-actix` `0.1.0` → `0.2.0`. (SMA-462 shipped its own wire change as a patch, but it *broadened* compatibility; this narrows it.) |
| **D3** | Docs name **`0.2.2` as a required hop** for operators on 0.2.0/0.2.1. | §1.2. Generic "drain first" wording leaves those operators to derive the two-hop path themselves. |
| **D4** | **Amended after challenge.** The diagnostic message **does** name `0.2.2`, as the recovery version. | Originally D4 forbade naming any remedy version, on the grounds that version paths age. The challenge showed this conflicted with D1: the original message ("drain in-flight runs before upgrading") describes an action that is *impossible at the moment it fires* — the message only ever appears **after** the upgrade, when the legacy payload is already frozen in `ActivityTaskScheduled` and re-delivered on every retry (`src/lib.rs:363-366`). The only available recovery is §1.3's self-heal, which is inherently version-specific. A diagnostic naming an unavailable action is not a mitigation, so legibility wins over durability. |
| **D5** | `warn_legacy` is **replaced** by `reject_legacy`, not deleted outright — and `reject_legacy` **also emits `tracing::error!`**. | Follows from D1: three call sites need the same message, so it belongs in one helper. The log is a challenge finding: without it, removing `warn_legacy` leaves the codec with **no** worker-side observability at all, and the only evidence lives in Temporal history — invisible to any log pipeline or alerting the operator actually runs. A deliberate deviation from the ticket's literal "the `warn_legacy` helper … can be deleted". |
| **D6** | The legacy tests are **converted**, not deleted (see §7). | Deleting them would silently drop coverage, because all three `*_content_failure_is_encoding_error` tests use a legacy `MultiArgs{N}` as their vehicle for "recognized arity, bad content". |
| **D7** | SMA-462's design doc gets a **"Superseded in part by SMA-484" banner** under its `**Status:**` line. | Originally two inline notes at §4.7/§10. The challenge showed a reader hits §2, §4.4, §6.1 and §7 — all still asserting the shim — long before reaching those. One banner covers every claim in a single edit. The rest stays as the historical record. |
| **D8** | Retry semantics and activity options are **not** changed. | §6. |
| **D9** | A `#[cfg(feature = "legacy-activity-input")]` gate (default-on in 0.3.x, removed in 0.4.0) was **considered and rejected** as a landing strategy. | It is a way of *not* removing the shim, which is the decision §1.1 records as settled. It also recreates precisely the problem SMA-462 §4.7 was written to prevent — a compatibility gate with no removal criterion — and would need its own removal ticket, plus a feature flag and a CI matrix entry, to reach the same end state. |

## 4. Architecture

Everything in production code is confined to
`crates/paigasus-helikon-runtime-temporal/src/activity_input.rs`. Verified by
grep: only `workflow.rs:50` (imports the `*Args` structs to build encode-side
struct literals) and `activities.rs:36-38,378,403,427` (the `#[activity]`
parameter types) reference this module, and neither is affected by a change to
`from_payloads`.

### 4.1 `warn_legacy` → `reject_legacy`

```rust
/// Reject a pre-envelope activity input with an actionable diagnostic.
///
/// `EncodingError` rather than `WrongEncoding` is deliberate and load-bearing:
/// the composite converter treats `WrongEncoding` as "not my encoding" and falls
/// through to the next converter, swallowing the message; any other error is
/// returned immediately, so this is what actually reaches the
/// `ActivityTaskFailed` history event.
///
/// The `tracing::error!` is not redundant with that event: the history event is
/// only visible to someone querying Temporal, whereas this reaches the worker's
/// own log pipeline where alerting lives.
fn reject_legacy(activity: &str, arity: usize) -> PayloadConversionError {
    tracing::error!(
        activity,
        legacy_arity = arity,
        "refused a pre-envelope activity input; a worker on 0.2.1 or earlier queued this task"
    );
    PayloadConversionError::EncodingError(
        format!(
            "{activity}: received {arity} payloads — the pre-envelope positional shape \
             (0.2.1 and earlier). This worker decodes only the single-payload envelope. \
             Recovery: re-join a worker on 0.2.2, which decodes both shapes, to this task \
             queue and let in-flight runs drain."
        )
        .into(),
    )
}
```

Three properties this message must keep, each asserted by a test in §7:

1. It names the **activity** (`AgentActivities::…`) and the **payload count**.
2. It says **"0.2.1 and earlier"**, not "0.2.0/0.2.1" — see §9 on `call_model`,
   whose 2-payload shape is unchanged since 0.1.x, so a version range excluding
   0.1.x would mislabel a real case.
3. It carries **no payload bytes** — only the activity name and the payload
   *count* — preserving SMA-462 §5.1's property that diagnostics landing in
   Temporal history never echo input content.

**Log volume.** Under §6's unbounded default retries this logs once per attempt.
That is accepted deliberately: an unthrottled error log is strictly better than
silence for a condition that requires operator intervention, and the volume is
itself the alerting signal.

### 4.2 The three decode arms

Each `from_payloads` keeps a named arm for its former legacy arity, decoding
nothing:

```rust
// RenderInstructionsInput
match payloads.len() {
    1 => { /* envelope — unchanged */ }
    // Pre-envelope (0.2.1 and earlier): (agent_name, ctx_seed). No longer decoded.
    2 => Err(reject_legacy(ACT_RENDER, 2)),
    _ => Err(PayloadConversionError::WrongEncoding),
}
```

`CallModelInput` is identical with `ACT_CALL_MODEL`/`2`; `InvokeToolInput` uses
`ACT_INVOKE_TOOL`/`3`.

The envelope arm and `decode_arg` are untouched. `encode_envelope` and all three
`TemporalSerializable` impls are untouched.

**`decode_arg`'s `index` parameter becomes vestigial** — after removal it is only
ever called with `index = 0`. Left in place deliberately: removing it would
churn the envelope arm this ticket is not otherwise touching, and the parameter
costs nothing. Recorded as a conscious call, not an oversight.

### 4.3 Documentation inside `activity_input.rs`

- **Module docs, `# Wire shapes`** (currently: "decodes from either that or the
  legacy pre-envelope (0.2.0–0.2.1) positional arity…") — rewritten to state
  that each wrapper encodes to and decodes from exactly one JSON-object payload,
  and that the former legacy arities are recognized only to produce a named
  error.
- **`warn_legacy`'s doc comment** describing the removal signal goes with the
  helper; `reject_legacy`'s replaces it.
- **The three `ACT_*` constants' doc comments** (`:71`, `:227-228`, `:315-316`)
  each say "used in decode diagnostics and **legacy-shape warnings**". Reword to
  "decode diagnostics and pre-envelope rejections".

## 5. Documentation impact

| file | change |
| -- | -- |
| `src/activity_input.rs` | §4.3 |
| `src/lib.rs` §"Upgrade Discipline and Determinism" | per-paragraph, §5.2 below |
| `src/lib.rs` `mod activity_input` doc (L401-403) | drop "decoding both that and the legacy pre-envelope … shapes" |
| `crates/paigasus-helikon-runtime-temporal/README.md` §"Upgrade Discipline" (L159-161) | same dispositions as lib.rs, in brief |
| `crates/paigasus-helikon-runtime-temporal/CHANGELOG.md` | `### Changed` + `### Upgrade notes` under `[Unreleased]` — content per §5.1 |
| `crates/paigasus-helikon/CHANGELOG.md` | `### Upgrade notes` under `[Unreleased]` — §8 |
| `docs/book/src/reference/crates.md:34` | version column reads `0.1.0`, already stale against `0.2.2`; correct to `0.3.0` |
| `docs/superpowers/specs/2026-08-06-sma-462-…-design.md` | superseded banner (D7) |

**mdBook `concepts/runtimes.md:37`: no edit.** It describes the envelope's
field-evolution property — still true — and defers upgrade rules to the crate
docs, which this change updates. `reference/crates.md:34` *is* edited (stale
version). Both evaluated explicitly per CLAUDE.md's "conscious call, not a
silent skip" rule.

### 5.1 What the upgrade notes must say

1. `0.3.0` no longer decodes the pre-envelope positional shapes (0.2.1 and
   earlier). A worker on `0.3.0` handed such a task logs an error and fails the
   attempt with a named `EncodingError`.
2. **Operators on `0.2.0` or `0.2.1` must upgrade to `0.2.2` first**, drain
   in-flight runs **while the fleet is on `0.2.2`**, and only then take `0.3.0`.
   `0.2.2` is the bridge: it decodes both shapes. ("Drain" as defined in §1.2.)
3. `0.2.2` ↔ `0.3.0` is compatible in **both** directions **for activity
   inputs** — both encode and decode the envelope. A `0.2.2`→`0.3.0` rolling
   upgrade needs no drain on account of this change. (Scope: activity inputs
   only. `payloads::WorkflowInput` and activity outputs are unchanged by this
   release and carry their own pre-existing compatibility story.)
4. **If a legacy task is met anyway**, recovery is §1.3's self-heal: re-join a
   worker on `0.2.2` to the task queue and let in-flight runs drain. This is
   what the error message says.
5. **If runs cannot be drained** (a long-running agent run that will not reach a
   terminal state in an acceptable window), the supported options are a
   blue-green task queue — stand up `0.3.0` on a new queue, point new runs at
   it, let the old queue drain on `0.2.2`, then decommission — or terminating
   the affected executions. Both are pre-existing, documented practice for this
   crate.
6. **Retries do not bound this** — see §6. Do not rely on the failure
   self-terminating.
7. The pre-existing drain / one-release-at-a-time discipline and the
   replay-determinism caveats are unchanged.

### 5.2 Per-paragraph disposition for `src/lib.rs` §"Upgrade Discipline"

Spelled out because "invert the compat claim" would plausibly lead an
implementer to delete a paragraph that is still correct:

| lines | disposition |
| -- | -- |
| L339-345 (SMA-462 wire change; "workers on this version also decode the previous pre-envelope positional shapes") | **rewrite** — envelope-only, plus the §1.2 matrix and the two-hop path |
| L347-361 (a 0.2.1-and-earlier worker cannot decode an envelope; retry/bounding discussion) | **rescope and correct** — still true for ≤0.2.1, and its retry-bounding wording must match §6 |
| L363-366 ("**Rolling back.** … drain in-flight runs before rolling back") | **keep** — still true; rescope from "to 0.2.1 and earlier" to "below 0.2.2" |
| L368-373 (field-evolution scope; nested core types ungoverned) | **unchanged** |
| L375-382 (the numbered upgrade rules) | **unchanged** in substance; add the two-hop path as a worked example |

`README.md:159-161` compresses the same dispositions into its shorter §Upgrade
Discipline paragraph.

## 6. Error handling and retry semantics

- **Former legacy arity** (2 for `render_instructions`/`call_model`, 3 for
  `invoke_tool`) → `tracing::error!` + `EncodingError` with the §4.1 message.
  Short-circuits the composite; reaches Temporal history.
- **Any other unrecognized arity** (0, 4, …) → `WrongEncoding`, unchanged. Falls
  through to the `serde_json` arm, which declines on its hard
  `payloads.len() != 1` check, yielding a clean decode failure.
- **Arity 1, bad content** → `EncodingError` from `decode_arg`, unchanged.

### 6.1 The failure is retryable and, by default, unbounded

The failure is a normal **retryable** activity failure, so Temporal
re-dispatches per policy. This is what makes §1.3's self-heal work — and it also
means the failure does **not** self-terminate. Correcting a claim that was wrong
in this spec's first draft:

- `render_instructions` is built with `activity_opts(timeouts.instructions, None, None)`
  (`src/workflow.rs:138`) — **no retry policy at all**, so the Temporal server
  default applies: unlimited attempts. Only `call_model` and `invoke_tool` carry
  a configurable `RetryPolicyConfig`.
- `WorkflowInput::timeout_ms` is `Option<u64>` where `None` means **no deadline**
  (`src/payloads.rs:33`).

So on the **default** configuration both bounds are absent and a workflow will
retry an undecodable task indefinitely, writing one `ActivityTaskFailed` event
per attempt and consuming workflow history. SMA-462 §5.2 states this for the
reverse direction ("`render_instructions`' unlimited default retries are
therefore *not* the safety net they appear to be"); it applies unchanged here.
The four bounds SMA-462 §5.2 enumerates — finite `maximum_attempts`, the run
deadline, `render_instructions` failure being terminal for the run, and
exhausted `invoke_tool` retries folding into a tool-error result — must be
carried into the upgrade notes **verbatim rather than paraphrased**, since only
the first two bound anything and neither is on by default.

Recovery is operator action (§5.1 items 4-5), not exhaustion.

### 6.2 Non-retryable is not available at this layer

Making the rejection non-retryable is not merely out of scope — there is **no
hook for it at the converter layer**. `temporalio-sdk-0.5.0/src/activities.rs:416`
does `let input: AD::Input = pc.from_payloads(&ctx, payloads)?;`, and the `?`
conversion path constructs an `ApplicationFailure` via
`ApplicationFailure::new`, which hard-codes `non_retryable: false`
(`temporalio-common-wasm-0.5.0/src/error.rs:171-184`). A `non_retryable`
constructor exists but nothing in the converter path reaches it. Stated as
infeasible rather than deferred, so no implementer or reviewer goes looking.

This is also *desirable* here: a retryable failure is exactly what lets a
re-joined `0.2.2` worker pick the task up (§1.3).

## 7. Testing

All tests live in `activity_input.rs`'s `mod tests` and run on every PR via
`cargo test --workspace --all-features`. They must go through `with_ctx`, which
uses the **production** `PayloadConverter::default()`, so they exercise the real
`Composite` → `UseWrappers` dispatch rather than calling `from_payloads`
directly.

### 7.1 Convert the three legacy tests (D6)

`{render_instructions,call_model,invoke_tool}_decodes_legacy_*_payload_shape`
become `…_rejects_legacy_*_payload_shape`. Construction is **unchanged** — a real
`MultiArgs2`/`MultiArgs3` encoded through the production converter, with the
existing `assert_eq!(payloads.len(), N)` retained — and only the assertion
inverts. Each asserts all three §4.1 properties, not just that an error occurred:

```rust
let err = ctx
    .converter
    .from_payloads::<RenderInstructionsInput>(ctx, payloads)
    .expect_err("the pre-envelope shape must no longer decode");
assert!(
    matches!(err, PayloadConversionError::EncodingError(_)),
    "expected EncodingError, got {err:?}"
);
let msg = err.to_string();
assert!(msg.contains(ACT_RENDER), "must name the activity: {msg}");
assert!(msg.contains("2 payloads"), "must name the payload count: {msg}");
assert!(msg.contains("0.2.2"), "must name the recovery version: {msg}");
```

Asserting the **specific variant and the message's content**, not merely "some
error", is the point. Two regressions this catches that a bare `is_err()` would
not:

- The arm returning `WrongEncoding`, letting the composite silently fall through
  and losing the diagnostic in production — the exact failure D1 exists to
  prevent.
- A copy-paste error passing the wrong `ACT_*` constant or the wrong arity into
  `reject_legacy` — which the activity-name and payload-count assertions catch
  and a variant-only assertion would not.

`MultiArgs2` / `MultiArgs3` stay imported.

### 7.2 Re-point the three content-failure tests

`*_content_failure_is_encoding_error` currently feeds a legacy `MultiArgs{N}`
whose argument 0 is a `42_u32` where a `String` is required. After removal that
arity no longer reaches content decoding, so each is rewritten to feed a
**single** payload — a `serde_json::json!` object encoded through
`ctx.converter.to_payload` — whose content is wrong for the envelope, keeping
the `EncodingError` assertion:

| test | arity-1 bad-content case |
| -- | -- |
| `render_instructions_…` | `{}` — object **missing** the required `agent_name` |
| `call_model_…` | `{"agent_name": 42, "request": <valid ModelRequest>}` — wrong **type**, all fields present |
| `invoke_tool_…` | `{"agent_name": 42, "call": <valid ToolCallRequest>}` — wrong **type**, `ctx_seed` legitimately absent (it is `#[serde(default)]`) |

The `call_model` / `invoke_tool` cases include the other required field
deliberately: `{"agent_name": 42}` alone would *also* be missing `request` /
`call`, so serde could report either failure and the test would no longer pin
the wrong-type case its predecessor tested.

`render_instructions`'s case must be **distinct** from
`decode_diagnostics_never_leak_payload_bytes`, which already feeds a bare JSON
string at arity 1 — hence missing-field rather than wrong-type there. Otherwise
the two tests duplicate each other and one stops earning its place.

### 7.3 New: the rejection diagnostic must be payload-free

The existing `decode_diagnostics_never_leak_payload_bytes` covers the arity-1
envelope arm only. `reject_legacy` is a **brand-new error path** whose input
(`MultiArgs2`/`MultiArgs3`) carries real content, and Temporal history is a
documented persistence boundary readable by anyone with namespace read
(`src/lib.rs:316-319`, `src/payloads.rs:35-40`). Without a test, a later
"helpful" edit appending the first payload's bytes to the message ships
silently.

Add one test: build `MultiArgs2(SENTINEL.to_owned(), None)`, feed it to
`RenderInstructionsInput`, and assert neither `Display` nor `Debug` of the error
contains `SENTINEL`.

### 7.4 Arity-rejection tests keep their existing assertions

`*_rejects_unrecognized_arity` is unchanged — the arities each one probes differ,
and none collides with a former legacy arity:

| test | arities probed, all `WrongEncoding` |
| -- | -- |
| `render_instructions_…` | 0, 4 |
| `call_model_…` | 0, 3 |
| `invoke_tool_…` | 0, 2, 4 |

Each gains a doc-comment note that its activity's former legacy arity is
deliberately **not** in this set — that case is covered by §7.1 and now yields
`EncodingError`, not `WrongEncoding`.

Note `invoke_tool`'s arity-2 probe keeps its existing comment ("0.1.x is out of
the support window") — see §9.

### 7.5 Unchanged

`*_round_trips_as_exactly_one_payload`, `*_decodes_frozen_envelope_literal`,
`*_envelope_defaults_absent_fields`, `*_envelope_ignores_unknown_fields` and
`decode_diagnostics_never_leak_payload_bytes` are untouched. The frozen-literal
tests remain the guard on the envelope wire shape.

### 7.6 Coverage after the change

Net test count is +1 (§7.3). Every legacy shape previously proven to *decode* is
now proven to be *refused with a named, payload-free diagnostic*, and the
arity-1 error path gains two tests it did not have (`call_model`,
`invoke_tool`).

### 7.7 Live coverage

**No new live tests, and the existing env-gated suite need not be re-run for
this change.** SMA-462 §6.2 required a manual run with output recorded in the PR
body because that change flipped the *encoder*. This one does not: encode is
untouched (§2), and the unit tests exercise the production converter through the
real `Composite` dispatch. Simulating a mixed 0.2.1/0.3.0 fleet remains out of
scope, as it was for SMA-462. Recorded as an explicit call rather than a silent
omission.

## 8. Release mechanics

`paigasus-helikon-runtime-temporal` is a released crate at `0.2.2`, shipping
through release-plz's normal flow — no stub-ascend ritual.

- Commit type `feat(runtime-temporal)!` (D2) → release-plz bumps `0.2.2` →
  `0.3.0` and marks the entry `[**breaking**]`.
- Every envelope type is `pub(crate)`, so there is **no public Rust API change**.
  Therefore no `paigasus-helikon-core` bump and no *manual* same-PR bump — the
  `dependencies_update` cascade is not defeated.
- PR title scope `runtime-temporal` is accepted: `pr-title.yml` validates
  against its **own inline `scopes:` list** (`.github/workflows/pr-title.yml:57`),
  kept in sync with `.versionrc`'s `scopeRegex` by a `# keep-in-sync-with:`
  comment. Because `pr-title.yml` runs on `pull_request_target`, that list is
  read from `main` — `runtime-temporal` is already there, so no new-scope
  problem. Separately, `.versionrc`'s `scopeRegex` is what gates the `commits`
  job against the branch's commits.

### 8.1 The facade must carry the warning too

The root `Cargo.toml:152` pins
`paigasus-helikon-runtime-temporal = { path = …, version = "0.2.2" }` — a caret
requirement that excludes `0.3.0`. release-plz's `dependencies_update` will
rewrite that pin and give the facade a **patch** bump (0.x cascade bumps are
patch, per CLAUDE.md), whose generated CHANGELOG entry reads only "updated the
following local packages: paigasus-helikon-runtime-temporal" — see the existing
precedent at `crates/paigasus-helikon/CHANGELOG.md:14,:50`.

Left alone, a `paigasus-helikon` user on the `runtime-temporal` feature receives
a **wire-breaking change as an unremarked patch** — directly undercutting D2's
rationale that the version number is the operator's signal. Most consumers reach
this crate through the facade.

Two mitigations, both in scope:

1. Add an `### Upgrade notes` block to `crates/paigasus-helikon/CHANGELOG.md`'s
   `[Unreleased]` section in this PR, pointing at the runtime-temporal `0.3.0`
   notes. release-plz preserves hand-written `[Unreleased]` content.
2. **Post-merge verification step:** confirm the release-plz PR actually
   rewrote the `[workspace.dependencies]` pin to `0.3.0`. A stale pin makes the
   whole workspace fail to build, because the path dependency carries a
   `version` field that must be satisfiable.

## 9. Open risks

- **The premise is assumed, not measured** (§1.1). If a `0.2.0`/`0.2.1` worker
  *is* in the field, its queued tasks become undecodable on a direct hop to
  `0.3.0`, and §6.1 means that does not self-terminate. Mitigations: the D1/D5
  diagnostic names the problem in both the worker log and Temporal history at
  the moment it occurs, and §1.3's self-heal gives a one-step recovery.
  Accepted by decision.
- **`0.1.x` remains unhandled, and the three activities behave differently.**
  Enumerated so nobody has to re-derive it:

  | activity | 0.1.x arity | outcome on 0.3.0 |
  | -- | -- | -- |
  | `render_instructions` | 1 (`agent_name` only) | enters the **envelope** arm; a bare JSON string cannot deserialize into `RenderInstructionsArgs`, so it fails as an `EncodingError` from `decode_arg` — fails closed, but with the envelope arm's message, not `reject_legacy`'s |
  | `call_model` | 2 — **unchanged since 0.1.x** | hits the **named** `reject_legacy` arm. This is why §4.1's message says "0.2.1 and earlier" rather than "0.2.0/0.2.1", which would have mislabelled it |
  | `invoke_tool` | 2 (`agent_name`, `call`) | falls to `_ => WrongEncoding` — bare and unnamed, since arity 2 is not `invoke_tool`'s legacy arity |

  All three fail closed. The diagnostic quality differs, and that is accepted:
  0.1.x is outside the support window.
- **A `temporalio-*` bump invalidates the dispatch-order reasoning** inherited
  from SMA-462 §11. The converter-level tests are the standing mitigation.
- **Release rollback is by pinning, not withdrawal.** A published crates.io
  version can be yanked but not removed. If `0.3.0` proves wrong, the remedy is
  for operators to pin `0.2.2` (which is forward-compatible with `0.3.0`'s
  envelope, §1.2) while a fix ships; yanking `0.3.0` only stops *new*
  resolutions.
