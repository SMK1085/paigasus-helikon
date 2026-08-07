# SMA-484 — runtime-temporal: remove the 0.2.x legacy activity-input decode arms

**Status:** Draft (Stage 1 spec, pending GATE 1 approval)
**Ticket:** [SMA-484](https://linear.app/smaschek/issue/SMA-484/runtime-temporal-remove-the-02x-legacy-activity-input-decode-arms)
**Related:** [SMA-462](https://linear.app/smaschek/issue/SMA-462) (added the arms this ticket removes; PR [#176](https://github.com/SMK1085/paigasus-helikon/pull/176)), SMA-455 (introduced the arity change SMA-462 addressed)
**Crate:** `paigasus-helikon-runtime-temporal` (self-contained; no `paigasus-helikon-core` change)
**SDK baseline:** `temporalio-* = 0.5.0` — unchanged from SMA-462; this design adds no new SDK-behaviour claims beyond those §11 of the SMA-462 design already carries.

## 1. Context

SMA-462 replaced the positional activity inputs with a single self-describing
envelope payload, and shipped **legacy decode arms** that also accept the
pre-envelope (0.2.0–0.2.1) positional shapes:

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

Because `0.2.2` decodes **both** shapes and `0.3.0` decodes only the envelope,
the version matrix works out as:

| from → to | activity inputs |
| -- | -- |
| `0.2.2` → `0.3.0` | compatible in **both** directions — both speak envelope |
| `0.2.0` / `0.2.1` → `0.3.0` | **broken** — legacy-queued tasks are undecodable |
| `0.2.0` / `0.2.1` → `0.2.2` → `0.3.0` | works — `0.2.2` decodes the legacy shape |

So the shim is not retracted into uselessness. `0.2.2` stays on crates.io
permanently as the one-hop bridge off the pre-envelope wire, and the crate's
existing **"upgrade one release at a time"** rule already routes operators
through it. Making that hop explicit in the docs (§5) is what turns this from a
withdrawn promise into a normal deprecation.

## 2. Goals / non-goals

### Goals

- Delete the three legacy positional decode paths, so no pre-envelope shape is
  ever decoded.
- Fail **closed and legibly**: an operator who meets a pre-envelope task gets a
  diagnostic naming the shape and the remedy, in Temporal history.
- Preserve the test suite's power: the legacy shapes must be proven *refused*,
  not merely absent from the `match`.
- Signal the wire break in the version number.
- Leave the crate docs, README and CHANGELOG describing the wire truthfully.

### Non-goals

- No change to **encode**. The envelope still serializes to exactly one payload.
- No change to activity **outputs**, to `payloads::WorkflowInput`, or to the
  `#[activity]` signatures.
- No change to the field-evolution contract, and no attempt to govern nested
  `paigasus-helikon-core` types (`ModelRequest`, `ToolCallRequest`) — they remain
  ungoverned, per SMA-462 §4.6 rule 4.
- 0.1.x shapes stay unhandled; they already fail closed and stay that way.
- No Temporal Worker Versioning / Build IDs work.

## 3. Design decisions (recorded)

| id | decision | rationale |
| -- | -- | -- |
| **D1** | The removed arities get **named fail-closed arms** returning `EncodingError`, not a fall-through to the generic `_ => WrongEncoding`. | `WrongEncoding` means "not my encoding" to the `Composite` converter, which then falls through to the `serde_json` arm (also declining) and the operator sees a bare `Wrong encoding` in `ActivityTaskFailed` — no activity name, no arity, no remedy. Since §1.1's premise (no legacy workers left) is *assumed* rather than verified, the diagnostic is the only mitigation left once the decode is gone. |
| **D2** | Ship as `feat(runtime-temporal)!` → **0.3.0**. | The crate's own "upgrade one release at a time" rule makes the version number the operator's primary compatibility signal; a compat *removal* hidden in a patch makes that rule unreadable. Precedent: SMA-482's `feat(runtime)!` took `runtime-actix` `0.1.0` → `0.2.0`. (SMA-462 shipped its own wire change as a patch, but it *broadened* compatibility; this narrows it.) |
| **D3** | Docs name **`0.2.2` as a required hop** for operators on 0.2.0/0.2.1. | §1.2. Generic "drain first" wording leaves those operators to derive the two-hop path themselves. |
| **D4** | The diagnostic message names the **shapes'** origin (`0.2.0`/`0.2.1`) but **not** the remedy version `0.2.2`. | The shape-to-version mapping is a historical fact and stays true forever; the *upgrade path* is docs' job and will age as releases accumulate. So the message states what arrived and the immediate remedy (drain), and leaves "which version to hop through" to §5.1. |
| **D5** | `warn_legacy` is **replaced** by `reject_legacy`, not deleted outright. | Follows from D1: three call sites need the same message, so it belongs in one helper. This is a deliberate deviation from the ticket's literal "the `warn_legacy` helper … can be deleted". |
| **D6** | The legacy tests are **converted**, not deleted (see §4). | Deleting them would silently drop coverage, because all three `*_content_failure_is_encoding_error` tests use a legacy `MultiArgs{N}` as their vehicle for "recognized arity, bad content". |
| **D7** | SMA-462's design doc gets a one-line "superseded" note at §4.7 and §10. | Prevents a future reader concluding the shim still exists. The rest stays as the historical record; design docs in this repo are dated per-ticket snapshots, not living documents. |

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
fn reject_legacy(activity: &str, arity: usize) -> PayloadConversionError {
    PayloadConversionError::EncodingError(
        format!(
            "{activity}: received {arity} payloads — the pre-envelope (0.2.0/0.2.1) \
             positional shape. This worker decodes only the single-payload envelope; \
             drain in-flight runs before upgrading."
        )
        .into(),
    )
}
```

The message carries **no payload bytes** — only the activity name and the
payload *count* — so it keeps SMA-462 §5.1's property that diagnostics landing
in Temporal history never echo input content.

### 4.2 The three decode arms

Each `from_payloads` keeps a named arm for its former legacy arity, decoding
nothing:

```rust
// RenderInstructionsInput
match payloads.len() {
    1 => { /* envelope — unchanged */ }
    // Pre-envelope (0.2.0–0.2.1): (agent_name, ctx_seed). No longer decoded.
    2 => Err(reject_legacy(ACT_RENDER, 2)),
    _ => Err(PayloadConversionError::WrongEncoding),
}
```

`CallModelInput` is identical with `ACT_CALL_MODEL`/`2`; `InvokeToolInput` uses
`ACT_INVOKE_TOOL`/`3`.

The envelope arm and `decode_arg` are untouched. `encode_envelope` and all three
`TemporalSerializable` impls are untouched.

### 4.3 Module documentation

The `# Wire shapes` section (currently: "decodes from either that or the legacy
pre-envelope (0.2.0–0.2.1) positional arity…") is rewritten to state that each
wrapper encodes to and decodes from exactly one JSON-object payload, and that
the former legacy arities are recognized only to produce a named error. The
`warn_legacy` doc comment describing the removal signal goes with the helper.

## 5. Documentation impact

| file | change |
| -- | -- |
| `src/activity_input.rs` module docs | §4.3 |
| `src/lib.rs` §"Upgrade Discipline and Determinism" (~L339–366) | invert the compat claim; state the §1.2 matrix and the `0.2.0`/`0.2.1` → `0.2.2` → `0.3.0` hop |
| `src/lib.rs` `mod activity_input` doc (L401–403) | drop "decoding both that and the legacy pre-envelope … shapes" |
| `crates/paigasus-helikon-runtime-temporal/README.md` §"Upgrade Discipline" (~L157–161) | same as lib.rs, in brief |
| `crates/paigasus-helikon-runtime-temporal/CHANGELOG.md` | `### Changed` + `### Upgrade notes` under `[Unreleased]`, naming `0.2.2` as the required hop (D3) |
| `docs/superpowers/specs/2026-08-06-sma-462-…-design.md` §4.7, §10 | one-line "superseded by SMA-484" note (D7) |

**mdBook: no edit.** `docs/book/src/concepts/runtimes.md:37` describes the
envelope's field-evolution property — still true — and defers upgrade rules to
the crate docs, which this change updates. Recorded as a conscious call per
CLAUDE.md, not a silent skip.

### 5.1 What the upgrade notes must say

- `0.3.0` no longer decodes the pre-envelope (0.2.0/0.2.1) positional shapes. A
  worker on `0.3.0` handed such a task fails the attempt with a named
  `EncodingError`.
- **Operators on `0.2.0` or `0.2.1` must upgrade to `0.2.2` first**, let
  in-flight runs drain, and only then take `0.3.0`. `0.2.2` is the bridge: it
  decodes both shapes.
- `0.2.2` ↔ `0.3.0` is compatible in **both** directions for activity inputs,
  because both encode and decode the envelope. A `0.2.2`→`0.3.0` rolling
  upgrade needs no drain on account of *this* change.
- The pre-existing drain/one-release-at-a-time discipline and the
  replay-determinism caveats are unchanged.

## 6. Error handling

- **Former legacy arity** (2 for `render_instructions`/`call_model`, 3 for
  `invoke_tool`) → `EncodingError` with the §4.1 message. Short-circuits the
  composite; reaches Temporal history.
- **Any other unrecognized arity** (0, 4, …) → `WrongEncoding`, unchanged. Falls
  through to the `serde_json` arm, which declines on its hard
  `payloads.len() != 1` check, yielding a clean decode failure.
- **Arity 1, bad content** → `EncodingError` from `decode_arg`, unchanged.

Retry semantics are deliberately unchanged: the failure is a normal
(retryable) activity failure, so Temporal re-dispatches per policy. No worker
will ever decode the task, so retries are futile and bounded by
`maximum_attempts` / `WorkflowInput::timeout_ms` — exactly the same shape as the
reverse-direction failure `0.2.2` already documents. Making it non-retryable
would be a behaviour change beyond this ticket's scope.

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
inverts:

```rust
let err = ctx
    .converter
    .from_payloads::<RenderInstructionsInput>(ctx, payloads)
    .expect_err("the pre-envelope shape must no longer decode");
assert!(
    matches!(err, PayloadConversionError::EncodingError(_)),
    "expected EncodingError, got {err:?}"
);
assert!(
    err.to_string().contains("pre-envelope"),
    "the diagnostic must name the pre-envelope shape: {err}"
);
```

Asserting the **specific variant and message**, not merely "some error", is the
point. A test that accepted any `Err` would pass against a regression where the
arm returns `WrongEncoding` and the composite silently falls through — which is
precisely the failure D1 exists to prevent.

`MultiArgs2` / `MultiArgs3` stay imported.

### 7.2 Re-point the three content-failure tests

`*_content_failure_is_encoding_error` currently feeds a legacy `MultiArgs{N}`
whose argument 0 is a `42_u32` where a `String` is required. After removal that
arity no longer reaches content decoding, so each is rewritten to feed a
**single** payload — a `serde_json::json!` object, encoded through
`ctx.converter.to_payload` — whose content is wrong for the envelope, keeping
the `EncodingError` assertion:

| test | arity-1 bad-content case |
| -- | -- |
| `render_instructions_…` | object **missing** the required `agent_name` |
| `call_model_…` | object with `agent_name: 42` (preserves the original intent) |
| `invoke_tool_…` | object with `agent_name: 42` |

`render_instructions`'s case must be **distinct** from
`decode_diagnostics_never_leak_payload_bytes`, which already feeds a bare JSON
string at arity 1 — hence missing-field rather than wrong-type there. Otherwise
the two tests duplicate each other and one stops earning its place.

### 7.3 Extend the arity-rejection tests

`*_rejects_unrecognized_arity` keeps its existing assertions unchanged — the
arities each one probes differ, and none of them collides with a former legacy
arity:

| test | arities probed, all `WrongEncoding` |
| -- | -- |
| `render_instructions_…` | 0, 4 |
| `call_model_…` | 0, 3 |
| `invoke_tool_…` | 0, 2, 4 |

Each gains a doc-comment note that its activity's former legacy arity is
deliberately **not** in this set — that case is covered by §7.1 and now yields
`EncodingError`, not `WrongEncoding`.

### 7.4 Unchanged

`*_round_trips_as_exactly_one_payload`, `*_decodes_frozen_envelope_literal`,
`*_envelope_defaults_absent_fields`, `*_envelope_ignores_unknown_fields` and
`decode_diagnostics_never_leak_payload_bytes` are untouched. The frozen-literal
tests remain the guard on the envelope wire shape.

### 7.5 Coverage after the change

Net test count is unchanged. Every legacy shape that was previously proven to
*decode* is now proven to be *refused with a named diagnostic*, and the
arity-1 error path gains two tests it did not have (`call_model`,
`invoke_tool`).

### 7.6 Live coverage

None added. `tests/temporal_live.rs` is env-gated and does not exercise
mixed-version fleets; simulating a 0.2.1 worker against a 0.3.0 worker is out of
scope, same as it was for SMA-462.

## 8. Release mechanics

`paigasus-helikon-runtime-temporal` is a released crate at `0.2.2`, shipping
through release-plz's normal flow — no stub-ascend ritual.

- Commit type `feat(runtime-temporal)!` (D2) → release-plz bumps `0.2.2` →
  `0.3.0` and marks the entry `[**breaking**]`.
- Every envelope type is `pub(crate)`, so there is **no public Rust API change**.
  Therefore: no `paigasus-helikon-core` bump, no same-PR manual bump, and no
  manual facade bump — the `dependencies_update` cascade is not defeated and
  handles the facade automatically.
- PR title scope `runtime-temporal` is already in `main`'s `.versionrc`
  `scopeRegex`, so `pr-title.yml` (which reads the allowlist from the base
  branch) accepts it.

## 9. Open risks

- **The premise is assumed, not measured** (§1.1). If a `0.2.0`/`0.2.1` worker
  *is* in the field, its queued tasks become undecodable on a direct hop to
  `0.3.0`. Mitigations: the D1 diagnostic names the problem at the moment it
  occurs, and `0.2.2` remains available as the bridge. Accepted by decision.
- **`0.1.x` remains unhandled.** A straggling 0.1.x `render_instructions` task
  is one payload, so it enters the envelope arm and fails there as an
  `EncodingError` — the arity collision noted in SMA-462's envelope-arm comment.
  Unchanged by this design.
- **A `temporalio-*` bump invalidates the dispatch-order reasoning** inherited
  from SMA-462 §11. The converter-level tests are the standing mitigation.
