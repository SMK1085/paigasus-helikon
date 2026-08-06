# SMA-462 Temporal Activity-Input Envelope Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the three Temporal activities' positional arguments with a single self-describing JSON-object envelope payload, decoded by a codec that also accepts the current 0.2.x positional shapes.

**Architecture:** A new private module `activity_input.rs` owns three envelope pairs — a serde-derived `*Args` struct plus a serde-free newtype wrapper carrying a hand-written `TemporalSerializable`/`TemporalDeserializable` impl. Each `#[activity]` method collapses to exactly one parameter, which is what makes the wrapper the activity's `Input` type. The wrapper encodes to one payload and decodes from either one payload (envelope) or the legacy positional arity.

**Tech Stack:** Rust 2024, `temporalio-* 0.5.0`, `serde` / `serde_json`, `tracing`.

**Spec:** `docs/superpowers/specs/2026-08-06-sma-462-temporal-activity-input-compat-design.md`

## Global Constraints

- **Crate under change:** `crates/paigasus-helikon-runtime-temporal` only. No `paigasus-helikon-core` change, no facade change, no version bumps by hand (§9 of the spec).
- **Visibility:** every new type is `pub(crate)`. There must be **no public Rust API change**.
- **SDK pinning:** all behaviour depends on `temporalio-* = 0.5.0` exactly. Do not bump it in this PR.
- **MSRV:** `1.94` (workspace-inherited). Do not add dependencies; every crate needed is already in `crates/paigasus-helikon-runtime-temporal/Cargo.toml` (`serde`, `serde_json`, `tracing`, `temporalio-common`).
- **Lints:** the workspace sets `missing_docs = "warn"` and CI runs `-D warnings`. Every item, including `pub(crate)` ones, gets a `///` doc comment. Private `mod` declarations in `lib.rs` carry a `///` comment too — follow the existing style at `src/lib.rs:363-368`.
- **Diagnostic hygiene (spec §5.1):** decode error messages may name the activity, the matched arity, the argument index and the expected type. They must **never** embed payload bytes or a serde error's rendering of the input value. This is enforced by always discarding the source error with `.map_err(|_| ...)`.
- **Commit format:** `<type>(<scope>): SMA-462 <lowercase message>`. Allowed scopes include `runtime-temporal`, `docs`, `spec`, `plan`. Commits are signed via a 1Password SSH key — if a commit fails with "failed to fill whole buffer", stop and ask the user to unlock their vault. Never bypass signing.
- **Never `git add -A`.** `.env` and `.claude` are untracked but not gitignored. Always stage explicit paths.
- **Before every commit:** run `cargo fmt --all` (the pre-commit hook is a deliberate no-op and will not do it for you).
- **Work synchronously.** Run `cargo` commands in the foreground and wait for them. Do not offload long builds to a background monitor and end your turn — finish the task.

---

## File Structure

| File | Responsibility | Task |
|---|---|---|
| `crates/paigasus-helikon-runtime-temporal/src/activity_input.rs` | **Create.** The whole wire codec: shared helpers + three envelope pairs + their unit tests. | 1, 2, 3 |
| `crates/paigasus-helikon-runtime-temporal/src/lib.rs` | **Modify.** Declare the new module (Task 1); rewrite the "Upgrade Discipline and Determinism" section (Task 4). | 1, 4 |
| `crates/paigasus-helikon-runtime-temporal/src/activities.rs` | **Modify.** Collapse each `#[activity]` method to one parameter and destructure it. | 1, 2, 3 |
| `crates/paigasus-helikon-runtime-temporal/src/workflow.rs` | **Modify.** Construct `*Args` at the `start_activity` call sites instead of tuples. | 1, 2, 3 |
| `crates/paigasus-helikon-runtime-temporal/README.md` | **Modify.** Upgrade paragraph. | 4 |
| `crates/paigasus-helikon-runtime-temporal/CHANGELOG.md` | **Modify.** `[Unreleased]` entry with the operator guidance. | 4 |
| `docs/book/src/concepts/runtimes.md` | **Modify.** One sentence pointing at the crate docs' upgrade section. | 4 |

Tasks 1–3 are vertical slices, one per activity. Each ends with the crate compiling, all tests passing, and no dead code — the module never contains a type nothing uses.

---

## Task 1: `render_instructions` envelope + shared codec helpers

**Files:**
- Create: `crates/paigasus-helikon-runtime-temporal/src/activity_input.rs`
- Modify: `crates/paigasus-helikon-runtime-temporal/src/lib.rs` (add `mod activity_input;` after the `mod activities;` block at `:368`)
- Modify: `crates/paigasus-helikon-runtime-temporal/src/activities.rs:372-388` (the `render_instructions` `#[activity]` method)
- Modify: `crates/paigasus-helikon-runtime-temporal/src/workflow.rs:307-322` (the `DriverEffect::RenderInstructions` arm)
- Test: unit tests live inside `src/activity_input.rs` under `#[cfg(test)] mod tests`

**Interfaces:**
- Produces, for Tasks 2 and 3:
  - `fn decode_arg<T: TemporalDeserializable + 'static>(ctx: &SerializationContext<'_>, payload: Payload, activity: &str, index: usize, expected: &str) -> Result<T, PayloadConversionError>`
  - `fn warn_legacy(activity: &str, arity: usize)`
  - `fn encode_envelope<T: TemporalSerializable + 'static>(ctx: &SerializationContext<'_>, args: &T) -> Result<Vec<Payload>, PayloadConversionError>`
  - `const ACT_RENDER: &str` **only**. Tasks 2 and 3 each define their own activity-name constant when they first use it — defining all three up front leaves two unused and fails `clippy -D warnings` with `dead_code` at the end of Task 1.
  - `pub(crate) struct RenderInstructionsArgs { pub agent_name: String, pub ctx_seed: Option<serde_json::Value> }`
  - `pub(crate) struct RenderInstructionsInput(pub RenderInstructionsArgs)` with `impl From<RenderInstructionsArgs>`
  - Test helper `fn with_ctx<R>(f: impl FnOnce(&SerializationContext<'_>) -> R) -> R`

- [ ] **Step 1: Create the module with docs and shared helpers**

Create `crates/paigasus-helikon-runtime-temporal/src/activity_input.rs`:

```rust
//! Wire codec for Temporal activity inputs.
//!
//! Each activity takes exactly **one** parameter, whose type is an envelope
//! wrapper defined here. That single-parameter shape is load-bearing:
//! `#[activities]` derives `ActivityDefinition::Input` from the method's
//! parameter list via `multi_args_input_type`
//! (`temporalio-macros-0.5.0/src/activities_definitions.rs:265-278`), which maps
//! `0 => ()`, `1 => the parameter's own type`, and `n => MultiArgs{n}`. There is
//! no `MultiArgs1`, so a one-parameter activity's `Input` *is* our wrapper —
//! which lets us supply a hand-written codec.
//!
//! # Wire shapes
//!
//! Each wrapper encodes to **one** JSON-object payload, and decodes from either
//! that or the legacy 0.2.x positional arity (2 payloads for
//! `render_instructions` / `call_model`, 3 for `invoke_tool`). Both paths build
//! the same `*Args` value, so everything downstream is shape-agnostic.
//!
//! # Why the hand-written impls are reached at all
//!
//! `PayloadConverter::default()` is `Composite([UseWrappers, serde_json()])`
//! (`temporalio-common-wasm-0.5.0/src/data_converters.rs:200-206`). The
//! `Composite` arm tries each sub-converter in order, and `UseWrappers`
//! dispatches to the **overridable** trait methods `T::to_payloads` (`:537`) and
//! `T::from_payloads` (`:572`) *before* the `serde_json` arm applies its hard
//! `payloads.len() != 1` check (`:567-570`). This is the same mechanism
//! `MultiArgs{N}` itself relies on.
//!
//! The re-entrant call inside [`TemporalSerializable::to_payloads`] terminates
//! rather than recursing: the inner serde-derived struct's `to_payload` goes
//! `UseWrappers` -> the struct's *default* `to_payloads` -> default
//! `to_payload` -> `WrongEncoding` -> falls through to `serde_json`. The blanket
//! impls (`:603-627`) override only `as_serde`/`from_serde`, never
//! `to_payloads`/`from_payloads`.
//!
//! # Why the wrappers derive no serde
//!
//! `temporalio-common-wasm` carries blanket impls
//! `impl<T: Serialize> TemporalSerializable for T` (`:603-610`) and
//! `impl<T: DeserializeOwned> TemporalDeserializable for T` (`:611-627`). A type
//! deriving serde therefore *cannot* also hand-implement the Temporal traits —
//! coherence conflict. Hence the split: a serde-derived `*Args` struct holding
//! the data, and a serde-free newtype wrapper carrying the codec.
//!
//! # Field-evolution contract
//!
//! Every future change to an envelope must obey all four rules:
//!
//! 1. New fields carry `#[serde(default)]`.
//! 2. Never add `#[serde(deny_unknown_fields)]`, never `rename` a field, never
//!    change a tagging attribute.
//! 3. A field may only be added if **ignoring** it is semantically safe. Serde
//!    silently drops unknown fields, so for anything posture- or
//!    authorization-adjacent that is a fail-open. Such a change needs a new
//!    activity name or an explicit version discriminant instead.
//! 4. Nested `paigasus-helikon-core` types (`ModelRequest`, `ToolCallRequest`)
//!    are **not** governed by this contract; a core-side serde change breaks the
//!    wire regardless.
//!
//! Rules 1 and 2 are asserted by the tests below. Rules 3 and 4 are review
//! obligations.

use serde::{Deserialize, Serialize};
use temporalio_common::data_converters::{
    GenericPayloadConverter, PayloadConversionError, SerializationContext, TemporalDeserializable,
    TemporalSerializable,
};
use temporalio_common::protos::temporal::api::common::v1::Payload;

/// Activity name used in decode diagnostics and legacy-shape warnings.
const ACT_RENDER: &str = "render_instructions";

/// Warn that a pre-envelope activity input was decoded.
///
/// This is the operator's "safe to remove the legacy decode arms" signal: once
/// no such warning has appeared for a full retention window, no 0.2.x-era worker
/// is scheduling tasks any more and the arms can go.
fn warn_legacy(activity: &str, arity: usize) {
    tracing::warn!(
        activity,
        legacy_arity = arity,
        "decoded a pre-envelope activity input; a 0.2.x-era worker is still scheduling tasks"
    );
}

/// Decode one payload, mapping any failure to a **payload-free**
/// [`PayloadConversionError::EncodingError`].
///
/// The source error is deliberately discarded rather than chained: a serde error
/// renders the offending input value, and this message lands in Temporal history
/// as an `ActivityTaskFailed` event readable by anyone with namespace read. The
/// crate's own docs require activity payloads to be treated as a persistence
/// boundary (see the `ctx_seed` warning in [`crate::payloads`]).
///
/// `EncodingError` (rather than `WrongEncoding`) is deliberate: the composite
/// converter treats `WrongEncoding` as "not my encoding" and falls through to the
/// next converter, but returns any other error immediately — so this surfaces the
/// real diagnostic instead of a bare encoding mismatch.
fn decode_arg<T: TemporalDeserializable + 'static>(
    ctx: &SerializationContext<'_>,
    payload: Payload,
    activity: &str,
    index: usize,
    expected: &str,
) -> Result<T, PayloadConversionError> {
    ctx.converter.from_payload(ctx, payload).map_err(|_| {
        PayloadConversionError::EncodingError(
            format!("{activity}: argument {index} could not be decoded as {expected}").into(),
        )
    })
}

/// Encode one envelope as a single payload.
///
/// Shared by all three wrappers' [`TemporalSerializable::to_payloads`] impls so
/// the encode path exists once rather than being triplicated (spec §4.4).
///
/// The nested `to_payload` call terminates rather than recursing — see the
/// module docs.
fn encode_envelope<T: TemporalSerializable + 'static>(
    ctx: &SerializationContext<'_>,
    args: &T,
) -> Result<Vec<Payload>, PayloadConversionError> {
    Ok(vec![ctx.converter.to_payload(ctx, args)?])
}
```

> **Note on the remaining per-activity repetition.** `decode_arg`,
> `warn_legacy` and `encode_envelope` carry every piece of shared logic. What
> stays per-activity is the `match payloads.len()` skeleton, whose arms
> genuinely differ in field count, names and types — three explicit `match`
> blocks are clearer here than a `macro_rules!` that would hide them. This is a
> deliberate stopping point, not an oversight.

- [ ] **Step 2: Add the `render_instructions` envelope pair**

> **On ordering:** Task 1 is the one task that cannot be strictly tests-first — the
> module and its shared helpers must exist before any test can reference them, and
> the codec's own tests need the envelope types. Tasks 2 and 3 *are* strictly
> tests-first (their Step 1 writes tests that fail to compile). Do not "fix" Task 1
> by writing tests against types that don't exist yet; just follow the steps.

Append to `src/activity_input.rs`:

```rust
/// The `render_instructions` activity's input fields.
///
/// Serialized as one JSON object. `ctx_seed` is `#[serde(default)]` per the
/// module's field-evolution contract.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct RenderInstructionsArgs {
    /// Name of the agent to resolve on the worker's registry.
    pub agent_name: String,
    /// Optional request-scoped seed the worker's ctx factory reconstitutes.
    #[serde(default)]
    pub ctx_seed: Option<serde_json::Value>,
}

/// Temporal `Input` wrapper for [`RenderInstructionsArgs`]. Derives no serde —
/// see the module docs on the blanket-impl coherence conflict. `Debug` is
/// required because the tests call `.expect_err()` on a
/// `Result<RenderInstructionsInput, _>`.
#[derive(Debug)]
pub(crate) struct RenderInstructionsInput(
    /// The wrapped fields.
    pub RenderInstructionsArgs,
);

impl From<RenderInstructionsArgs> for RenderInstructionsInput {
    fn from(args: RenderInstructionsArgs) -> Self {
        Self(args)
    }
}

impl TemporalSerializable for RenderInstructionsInput {
    fn to_payloads(
        &self,
        ctx: &SerializationContext<'_>,
    ) -> Result<Vec<Payload>, PayloadConversionError> {
        encode_envelope(ctx, &self.0)
    }
}

impl TemporalDeserializable for RenderInstructionsInput {
    fn from_payloads(
        ctx: &SerializationContext<'_>,
        payloads: Vec<Payload>,
    ) -> Result<Self, PayloadConversionError> {
        match payloads.len() {
            // Envelope: one JSON object.
            1 => {
                let mut it = payloads.into_iter();
                let args = decode_arg(
                    ctx,
                    it.next().expect("length checked above"),
                    ACT_RENDER,
                    0,
                    "RenderInstructionsArgs",
                )?;
                Ok(Self(args))
            }
            // Legacy 0.2.x: (agent_name, ctx_seed) as two payloads.
            2 => {
                warn_legacy(ACT_RENDER, 2);
                let mut it = payloads.into_iter();
                let agent_name = decode_arg(
                    ctx,
                    it.next().expect("length checked above"),
                    ACT_RENDER,
                    0,
                    "String",
                )?;
                let ctx_seed = decode_arg(
                    ctx,
                    it.next().expect("length checked above"),
                    ACT_RENDER,
                    1,
                    "Option<serde_json::Value>",
                )?;
                Ok(Self(RenderInstructionsArgs {
                    agent_name,
                    ctx_seed,
                }))
            }
            _ => Err(PayloadConversionError::WrongEncoding),
        }
    }
}
```

- [ ] **Step 3: Declare the module**

In `crates/paigasus-helikon-runtime-temporal/src/lib.rs`, insert immediately **after** `mod activities;` (currently line 368) and before the `/// The pure durable-loop step machine.` comment:

```rust
/// Wire codec for activity inputs: one self-describing envelope payload per
/// activity, decoding both that and the legacy 0.2.x positional shapes.
/// Private — the envelope types never cross the public API boundary.
mod activity_input;
```

- [ ] **Step 4: Write the failing tests**

Append to `src/activity_input.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use temporalio_common::data_converters::{
        MultiArgs2, PayloadConverter, SerializationContextData,
    };

    /// Run `f` with a [`SerializationContext`] over the **production**
    /// converter.
    ///
    /// Every codec test must go through this rather than calling
    /// `Wrapper::from_payloads` directly: a direct call exercises none of the
    /// `Composite` -> `UseWrappers` dispatch the design depends on. The crate
    /// never configures a `DataConverter`, so `PayloadConverter::default()` is
    /// the exact converter used in production.
    fn with_ctx<R>(f: impl FnOnce(&SerializationContext<'_>) -> R) -> R {
        let converter = PayloadConverter::default();
        let data = SerializationContextData::Activity;
        let ctx = SerializationContext {
            data: &data,
            converter: &converter,
        };
        f(&ctx)
    }

    fn render_args() -> RenderInstructionsArgs {
        RenderInstructionsArgs {
            agent_name: "agent-1".to_owned(),
            ctx_seed: Some(serde_json::json!({ "tenant": "acme" })),
        }
    }

    #[test]
    fn render_instructions_round_trips_as_exactly_one_payload() {
        with_ctx(|ctx| {
            let args = render_args();
            let input = RenderInstructionsInput(args.clone());

            let payloads = ctx.converter.to_payloads(ctx, &input).expect("encode");
            assert_eq!(
                payloads.len(),
                1,
                "the envelope must serialize to exactly one payload"
            );

            let back: RenderInstructionsInput =
                ctx.converter.from_payloads(ctx, payloads).expect("decode");
            assert_eq!(back.0, args);
        });
    }

    #[test]
    fn render_instructions_decodes_legacy_two_payload_shape() {
        with_ctx(|ctx| {
            let legacy = MultiArgs2(
                "agent-1".to_owned(),
                Some(serde_json::json!({ "tenant": "acme" })),
            );
            let payloads = ctx.converter.to_payloads(ctx, &legacy).expect("encode legacy");
            assert_eq!(payloads.len(), 2, "legacy shape is two payloads");

            let decoded: RenderInstructionsInput = ctx
                .converter
                .from_payloads(ctx, payloads)
                .expect("a 0.2.x-queued task must decode");
            assert_eq!(decoded.0, render_args());
        });
    }

    /// Frozen literal, NOT a value produced by serializing the current struct —
    /// a serialized fixture would track any drift and assert nothing.
    #[test]
    fn render_instructions_decodes_frozen_envelope_literal() {
        const FROZEN: &str = r#"{"agent_name":"agent-1","ctx_seed":{"tenant":"acme"}}"#;
        with_ctx(|ctx| {
            let value: serde_json::Value = serde_json::from_str(FROZEN).expect("literal parses");
            let payload = ctx.converter.to_payload(ctx, &value).expect("encode literal");
            let decoded: RenderInstructionsInput = ctx
                .converter
                .from_payloads(ctx, vec![payload])
                .expect("decode");
            assert_eq!(decoded.0, render_args());
        });
    }

    /// Contract rule 1: absent `#[serde(default)]` fields default.
    #[test]
    fn render_instructions_envelope_defaults_absent_fields() {
        const FROZEN_NO_SEED: &str = r#"{"agent_name":"agent-1"}"#;
        with_ctx(|ctx| {
            let value: serde_json::Value =
                serde_json::from_str(FROZEN_NO_SEED).expect("literal parses");
            let payload = ctx.converter.to_payload(ctx, &value).expect("encode literal");
            let decoded: RenderInstructionsInput = ctx
                .converter
                .from_payloads(ctx, vec![payload])
                .expect("decode");
            assert_eq!(decoded.0.agent_name, "agent-1");
            assert_eq!(decoded.0.ctx_seed, None);
        });
    }

    /// Contract rule 2: unknown fields are ignored, proving no
    /// `deny_unknown_fields` crept in. This is what makes a *future* worker's
    /// added field readable by *this* code.
    #[test]
    fn render_instructions_envelope_ignores_unknown_fields() {
        const FROZEN_FUTURE: &str =
            r#"{"agent_name":"agent-1","ctx_seed":{"tenant":"acme"},"added_in_a_later_release":42}"#;
        with_ctx(|ctx| {
            let value: serde_json::Value =
                serde_json::from_str(FROZEN_FUTURE).expect("literal parses");
            let payload = ctx.converter.to_payload(ctx, &value).expect("encode literal");
            let decoded: RenderInstructionsInput = ctx
                .converter
                .from_payloads(ctx, vec![payload])
                .expect("an envelope with an unknown field must still decode");
            assert_eq!(decoded.0, render_args());
        });
    }

    #[test]
    fn render_instructions_rejects_unrecognized_arity() {
        with_ctx(|ctx| {
            let zero: Result<RenderInstructionsInput, _> = ctx.converter.from_payloads(ctx, vec![]);
            assert!(
                matches!(zero, Err(PayloadConversionError::WrongEncoding)),
                "zero payloads must be WrongEncoding"
            );

            let p = ctx
                .converter
                .to_payload(ctx, &"x".to_owned())
                .expect("encode");
            let four: Result<RenderInstructionsInput, _> = ctx
                .converter
                .from_payloads(ctx, vec![p.clone(), p.clone(), p.clone(), p]);
            assert!(
                matches!(four, Err(PayloadConversionError::WrongEncoding)),
                "four payloads must be WrongEncoding"
            );
        });
    }

    /// A recognized arity whose content is wrong must be `EncodingError`, not
    /// `WrongEncoding` — the former short-circuits the composite converter and
    /// surfaces the real diagnostic.
    #[test]
    fn render_instructions_content_failure_is_encoding_error() {
        with_ctx(|ctx| {
            // Legacy arity, but argument 0 is a number where a String is required.
            let bad = MultiArgs2(42_u32, Option::<serde_json::Value>::None);
            let payloads = ctx.converter.to_payloads(ctx, &bad).expect("encode");
            let err = ctx
                .converter
                .from_payloads::<RenderInstructionsInput>(ctx, payloads)
                .expect_err("a non-String agent_name must fail");
            assert!(
                matches!(err, PayloadConversionError::EncodingError(_)),
                "expected EncodingError, got {err:?}"
            );
        });
    }

    /// Spec §5.1: the diagnostic lands in Temporal history and must never carry
    /// payload bytes.
    #[test]
    fn decode_diagnostics_never_leak_payload_bytes() {
        const SENTINEL: &str = "super-secret-tenant-token";
        with_ctx(|ctx| {
            let bad = MultiArgs2(
                serde_json::json!({ "leak": SENTINEL }),
                Option::<serde_json::Value>::None,
            );
            let payloads = ctx.converter.to_payloads(ctx, &bad).expect("encode");
            let err = ctx
                .converter
                .from_payloads::<RenderInstructionsInput>(ctx, payloads)
                .expect_err("an object where a String is required must fail");

            let display = err.to_string();
            let debug = format!("{err:?}");
            assert!(
                !display.contains(SENTINEL),
                "Display leaked payload bytes: {display}"
            );
            assert!(
                !debug.contains(SENTINEL),
                "Debug leaked payload bytes: {debug}"
            );
        });
    }
}
```

- [ ] **Step 5: Run the codec tests**

Run: `cargo test -p paigasus-helikon-runtime-temporal --lib activity_input`
Expected: **PASS — all eight.** The codec is self-contained; it does not need the activity wiring to work, so these tests are green before Steps 6–7. If any fails, the codec is wrong — fix it here, not after wiring, because a failure after wiring is much harder to localise.

Note you may see `dead_code` warnings for the envelope types at this point: nothing outside the test module uses them yet. That is expected and Steps 6–7 resolve it. Do **not** silence it with `#[allow(dead_code)]`.

- [ ] **Step 6: Wire the activity method**

In `crates/paigasus-helikon-runtime-temporal/src/activities.rs`, replace the `render_instructions` method body header (currently `:372-388`). Before:

```rust
    #[activity]
    pub(crate) async fn render_instructions(
        self: Arc<Self>,
        ctx: ActivityContext,
        agent_name: String,
        ctx_seed: Option<serde_json::Value>,
    ) -> Result<String, ActivityError> {
        let cancel = CancellationToken::new();
```

After:

```rust
    #[activity]
    pub(crate) async fn render_instructions(
        self: Arc<Self>,
        ctx: ActivityContext,
        input: RenderInstructionsInput,
    ) -> Result<String, ActivityError> {
        let RenderInstructionsArgs {
            agent_name,
            ctx_seed,
        } = input.0;
        let cancel = CancellationToken::new();
```

Everything after that line is unchanged. Add to the imports at the top of `activities.rs`:

```rust
use crate::activity_input::{RenderInstructionsArgs, RenderInstructionsInput};
```

- [ ] **Step 7: Wire the workflow call site**

In `crates/paigasus-helikon-runtime-temporal/src/workflow.rs`, in the `DriverEffect::RenderInstructions` arm (currently `:308-314`), replace the tuple argument:

```rust
                    .start_activity(
                        AgentActivities::render_instructions,
                        RenderInstructionsArgs {
                            agent_name: agent_name.to_owned(),
                            ctx_seed: ctx_seed.clone(),
                        },
                        config.instructions_activity_opts.clone(),
                    )
```

Add to the imports at the top of `workflow.rs`:

```rust
use crate::activity_input::RenderInstructionsArgs;
```

- [ ] **Step 8: Run the tests to verify they pass**

Run: `cargo test -p paigasus-helikon-runtime-temporal`
Expected: PASS — all eight new tests plus the existing suite.

- [ ] **Step 9: Run the fast gates**

```bash
cargo fmt --all
cargo clippy -p paigasus-helikon-runtime-temporal --all-features --all-targets -- -D warnings
```
Expected: clean. If `dead_code` fires on any envelope item, the wiring in Steps 6–7 is incomplete — fix it rather than adding an `#[allow]`.

- [ ] **Step 10: Commit**

```bash
git add crates/paigasus-helikon-runtime-temporal/src/activity_input.rs \
        crates/paigasus-helikon-runtime-temporal/src/lib.rs \
        crates/paigasus-helikon-runtime-temporal/src/activities.rs \
        crates/paigasus-helikon-runtime-temporal/src/workflow.rs
git commit -m "feat(runtime-temporal): SMA-462 add envelope input for render_instructions"
```

---

## Task 2: `call_model` envelope

**Files:**
- Modify: `crates/paigasus-helikon-runtime-temporal/src/activity_input.rs` (append the pair and its tests)
- Modify: `crates/paigasus-helikon-runtime-temporal/src/activities.rs:394-409` (the `call_model` `#[activity]` method)
- Modify: `crates/paigasus-helikon-runtime-temporal/src/workflow.rs:324-330` (the `DriverEffect::CallModel` arm)

**Interfaces:**
- Consumes from Task 1: `decode_arg`, `warn_legacy`, `encode_envelope`, and the test helper `with_ctx`. This task **defines** `ACT_CALL_MODEL` itself (Task 1 deliberately does not — an unused constant fails `clippy -D warnings`).
- Produces: `pub(crate) struct CallModelArgs { pub agent_name: String, pub request: ModelRequest }` and `pub(crate) struct CallModelInput(pub CallModelArgs)` with `impl From<CallModelArgs>`.

> **Note on assertions:** `ModelRequest` derives no `PartialEq`, so tests compare with `serde_json::to_value(..)` on both sides rather than `assert_eq!` on the struct. This matches the existing pattern in `src/payloads.rs` tests. `ModelRequest` is also `#[non_exhaustive]`, so construct it with `ModelRequest::new()`, never a struct literal.

- [ ] **Step 1: Write the failing tests**

Append inside the existing `#[cfg(test)] mod tests` block in `src/activity_input.rs`:

```rust
    fn call_model_args() -> CallModelArgs {
        CallModelArgs {
            agent_name: "agent-1".to_owned(),
            request: ModelRequest::new(),
        }
    }

    fn json_of<T: Serialize>(value: &T) -> serde_json::Value {
        serde_json::to_value(value).expect("serializes")
    }

    #[test]
    fn call_model_round_trips_as_exactly_one_payload() {
        with_ctx(|ctx| {
            let args = call_model_args();
            let expected = json_of(&args);
            let payloads = ctx
                .converter
                .to_payloads(ctx, &CallModelInput(args))
                .expect("encode");
            assert_eq!(
                payloads.len(),
                1,
                "the envelope must serialize to exactly one payload"
            );

            let back: CallModelInput = ctx.converter.from_payloads(ctx, payloads).expect("decode");
            assert_eq!(json_of(&back.0), expected);
        });
    }

    #[test]
    fn call_model_decodes_legacy_two_payload_shape() {
        with_ctx(|ctx| {
            let legacy = MultiArgs2("agent-1".to_owned(), ModelRequest::new());
            let payloads = ctx.converter.to_payloads(ctx, &legacy).expect("encode legacy");
            assert_eq!(payloads.len(), 2, "legacy shape is two payloads");

            let decoded: CallModelInput = ctx
                .converter
                .from_payloads(ctx, payloads)
                .expect("a 0.2.x-queued task must decode");
            assert_eq!(json_of(&decoded.0), json_of(&call_model_args()));
        });
    }

    #[test]
    fn call_model_decodes_frozen_envelope_literal() {
        const FROZEN: &str = r#"{"agent_name":"agent-1","request":{}}"#;
        with_ctx(|ctx| {
            let value: serde_json::Value = serde_json::from_str(FROZEN).expect("literal parses");
            let payload = ctx.converter.to_payload(ctx, &value).expect("encode literal");
            let decoded: CallModelInput = ctx
                .converter
                .from_payloads(ctx, vec![payload])
                .expect("decode");
            assert_eq!(decoded.0.agent_name, "agent-1");
        });
    }

    #[test]
    fn call_model_envelope_ignores_unknown_fields() {
        const FROZEN_FUTURE: &str =
            r#"{"agent_name":"agent-1","request":{},"added_in_a_later_release":42}"#;
        with_ctx(|ctx| {
            let value: serde_json::Value =
                serde_json::from_str(FROZEN_FUTURE).expect("literal parses");
            let payload = ctx.converter.to_payload(ctx, &value).expect("encode literal");
            let decoded: CallModelInput = ctx
                .converter
                .from_payloads(ctx, vec![payload])
                .expect("an envelope with an unknown field must still decode");
            assert_eq!(decoded.0.agent_name, "agent-1");
        });
    }

    #[test]
    fn call_model_rejects_unrecognized_arity() {
        with_ctx(|ctx| {
            let zero: Result<CallModelInput, _> = ctx.converter.from_payloads(ctx, vec![]);
            assert!(matches!(zero, Err(PayloadConversionError::WrongEncoding)));

            let p = ctx
                .converter
                .to_payload(ctx, &"x".to_owned())
                .expect("encode");
            let three: Result<CallModelInput, _> = ctx
                .converter
                .from_payloads(ctx, vec![p.clone(), p.clone(), p]);
            assert!(
                matches!(three, Err(PayloadConversionError::WrongEncoding)),
                "call_model has no three-payload shape"
            );
        });
    }

    #[test]
    fn call_model_content_failure_is_encoding_error() {
        with_ctx(|ctx| {
            let bad = MultiArgs2(42_u32, ModelRequest::new());
            let payloads = ctx.converter.to_payloads(ctx, &bad).expect("encode");
            let err = ctx
                .converter
                .from_payloads::<CallModelInput>(ctx, payloads)
                .expect_err("a non-String agent_name must fail");
            assert!(
                matches!(err, PayloadConversionError::EncodingError(_)),
                "expected EncodingError, got {err:?}"
            );
        });
    }
```

Add `ModelRequest` to the test module's imports:

```rust
    use paigasus_helikon_core::ModelRequest;
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p paigasus-helikon-runtime-temporal --lib activity_input`
Expected: FAIL to compile — `cannot find type CallModelArgs / CallModelInput in this scope`.

- [ ] **Step 3: Add the envelope pair**

Append to the non-test part of `src/activity_input.rs` (after the `render_instructions` pair). Add `use paigasus_helikon_core::ModelRequest;` to the module's imports:

```rust
/// Activity name used in decode diagnostics and legacy-shape warnings.
const ACT_CALL_MODEL: &str = "call_model";

/// The `call_model` activity's input fields.
///
/// Serialized as one JSON object with `request` **nested** as an object — never
/// stringified, so per-payload size stays equivalent to the pre-envelope
/// `request` payload against the crate's ~1.5 MB practical budget.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct CallModelArgs {
    /// Name of the agent to resolve on the worker's registry.
    pub agent_name: String,
    /// The model request for this turn.
    pub request: ModelRequest,
}

/// Temporal `Input` wrapper for [`CallModelArgs`]. Derives no serde — see the
/// module docs on the blanket-impl coherence conflict. `Debug` is required
/// because the tests call `.expect_err()` on a `Result<CallModelInput, _>`.
#[derive(Debug)]
pub(crate) struct CallModelInput(
    /// The wrapped fields.
    pub CallModelArgs,
);

impl From<CallModelArgs> for CallModelInput {
    fn from(args: CallModelArgs) -> Self {
        Self(args)
    }
}

impl TemporalSerializable for CallModelInput {
    fn to_payloads(
        &self,
        ctx: &SerializationContext<'_>,
    ) -> Result<Vec<Payload>, PayloadConversionError> {
        encode_envelope(ctx, &self.0)
    }
}

impl TemporalDeserializable for CallModelInput {
    fn from_payloads(
        ctx: &SerializationContext<'_>,
        payloads: Vec<Payload>,
    ) -> Result<Self, PayloadConversionError> {
        match payloads.len() {
            // Envelope: one JSON object.
            1 => {
                let mut it = payloads.into_iter();
                let args = decode_arg(
                    ctx,
                    it.next().expect("length checked above"),
                    ACT_CALL_MODEL,
                    0,
                    "CallModelArgs",
                )?;
                Ok(Self(args))
            }
            // Legacy 0.2.x: (agent_name, request) as two payloads. Unchanged
            // since 0.1.x, but still pre-envelope.
            2 => {
                warn_legacy(ACT_CALL_MODEL, 2);
                let mut it = payloads.into_iter();
                let agent_name = decode_arg(
                    ctx,
                    it.next().expect("length checked above"),
                    ACT_CALL_MODEL,
                    0,
                    "String",
                )?;
                let request = decode_arg(
                    ctx,
                    it.next().expect("length checked above"),
                    ACT_CALL_MODEL,
                    1,
                    "ModelRequest",
                )?;
                Ok(Self(CallModelArgs {
                    agent_name,
                    request,
                }))
            }
            _ => Err(PayloadConversionError::WrongEncoding),
        }
    }
}
```

- [ ] **Step 4: Wire the activity method**

In `src/activities.rs`, replace the `call_model` header (currently `:394-401`). Before:

```rust
    #[activity]
    pub(crate) async fn call_model(
        self: Arc<Self>,
        ctx: ActivityContext,
        agent_name: String,
        request: ModelRequest,
    ) -> Result<ModelTurnResult, ActivityError> {
        let cancel = CancellationToken::new();
```

After:

```rust
    #[activity]
    pub(crate) async fn call_model(
        self: Arc<Self>,
        ctx: ActivityContext,
        input: CallModelInput,
    ) -> Result<ModelTurnResult, ActivityError> {
        let CallModelArgs {
            agent_name,
            request,
        } = input.0;
        let cancel = CancellationToken::new();
```

Extend the `activity_input` import in `activities.rs` to:

```rust
use crate::activity_input::{
    CallModelArgs, CallModelInput, RenderInstructionsArgs, RenderInstructionsInput,
};
```

- [ ] **Step 5: Wire the workflow call site**

In `src/workflow.rs`, in the `DriverEffect::CallModel(request)` arm (currently `:325-329`):

```rust
                    .start_activity(
                        AgentActivities::call_model,
                        CallModelArgs {
                            agent_name: agent_name.to_owned(),
                            request,
                        },
                        config.model_activity_opts.clone(),
                    )
```

Extend the import in `workflow.rs` to:

```rust
use crate::activity_input::{CallModelArgs, RenderInstructionsArgs};
```

- [ ] **Step 6: Run the tests to verify they pass**

Run: `cargo test -p paigasus-helikon-runtime-temporal`
Expected: PASS.

- [ ] **Step 7: Run the fast gates**

```bash
cargo fmt --all
cargo clippy -p paigasus-helikon-runtime-temporal --all-features --all-targets -- -D warnings
```
Expected: clean.

- [ ] **Step 8: Commit**

```bash
git add crates/paigasus-helikon-runtime-temporal/src/activity_input.rs \
        crates/paigasus-helikon-runtime-temporal/src/activities.rs \
        crates/paigasus-helikon-runtime-temporal/src/workflow.rs
git commit -m "feat(runtime-temporal): SMA-462 add envelope input for call_model"
```

---

## Task 3: `invoke_tool` envelope

**Files:**
- Modify: `crates/paigasus-helikon-runtime-temporal/src/activity_input.rs` (append the pair and its tests)
- Modify: `crates/paigasus-helikon-runtime-temporal/src/activities.rs:415-432` (the `invoke_tool` `#[activity]` method)
- Modify: `crates/paigasus-helikon-runtime-temporal/src/workflow.rs:374-379` (inside `execute_tools`)

**Interfaces:**
- Consumes from Task 1: `decode_arg`, `warn_legacy`, `encode_envelope`, `with_ctx`, and `json_of` (from Task 2). This task **defines** `ACT_INVOKE_TOOL` itself (Task 1 deliberately does not — an unused constant fails `clippy -D warnings`).
- Produces: `pub(crate) struct InvokeToolArgs { pub agent_name: String, pub call: ToolCallRequest, pub ctx_seed: Option<serde_json::Value> }` and `pub(crate) struct InvokeToolInput(pub InvokeToolArgs)` with `impl From<InvokeToolArgs>`.

> **Note:** `ToolCallRequest` derives no `PartialEq`, so compare via `json_of` as in Task 2. Unlike the other two, `invoke_tool`'s legacy shape is **three** payloads.

- [ ] **Step 1: Write the failing tests**

Append inside the `#[cfg(test)] mod tests` block:

```rust
    fn tool_call() -> ToolCallRequest {
        ToolCallRequest {
            call_id: "c1".to_owned(),
            name: "echo".to_owned(),
            args: serde_json::json!({ "x": 1 }),
        }
    }

    fn invoke_tool_args() -> InvokeToolArgs {
        InvokeToolArgs {
            agent_name: "agent-1".to_owned(),
            call: tool_call(),
            ctx_seed: Some(serde_json::json!({ "tenant": "acme" })),
        }
    }

    #[test]
    fn invoke_tool_round_trips_as_exactly_one_payload() {
        with_ctx(|ctx| {
            let args = invoke_tool_args();
            let expected = json_of(&args);
            let payloads = ctx
                .converter
                .to_payloads(ctx, &InvokeToolInput(args))
                .expect("encode");
            assert_eq!(
                payloads.len(),
                1,
                "the envelope must serialize to exactly one payload"
            );

            let back: InvokeToolInput = ctx.converter.from_payloads(ctx, payloads).expect("decode");
            assert_eq!(json_of(&back.0), expected);
        });
    }

    #[test]
    fn invoke_tool_decodes_legacy_three_payload_shape() {
        with_ctx(|ctx| {
            let legacy = MultiArgs3(
                "agent-1".to_owned(),
                tool_call(),
                Some(serde_json::json!({ "tenant": "acme" })),
            );
            let payloads = ctx.converter.to_payloads(ctx, &legacy).expect("encode legacy");
            assert_eq!(payloads.len(), 3, "legacy shape is three payloads");

            let decoded: InvokeToolInput = ctx
                .converter
                .from_payloads(ctx, payloads)
                .expect("a 0.2.x-queued task must decode");
            assert_eq!(json_of(&decoded.0), json_of(&invoke_tool_args()));
        });
    }

    #[test]
    fn invoke_tool_decodes_frozen_envelope_literal() {
        const FROZEN: &str = r#"{"agent_name":"agent-1","call":{"call_id":"c1","name":"echo","args":{"x":1}},"ctx_seed":{"tenant":"acme"}}"#;
        with_ctx(|ctx| {
            let value: serde_json::Value = serde_json::from_str(FROZEN).expect("literal parses");
            let payload = ctx.converter.to_payload(ctx, &value).expect("encode literal");
            let decoded: InvokeToolInput = ctx
                .converter
                .from_payloads(ctx, vec![payload])
                .expect("decode");
            assert_eq!(json_of(&decoded.0), json_of(&invoke_tool_args()));
        });
    }

    /// Contract rule 1: the `#[serde(default)]` `ctx_seed` may be absent.
    #[test]
    fn invoke_tool_envelope_defaults_absent_fields() {
        const FROZEN_NO_SEED: &str =
            r#"{"agent_name":"agent-1","call":{"call_id":"c1","name":"echo","args":{"x":1}}}"#;
        with_ctx(|ctx| {
            let value: serde_json::Value =
                serde_json::from_str(FROZEN_NO_SEED).expect("literal parses");
            let payload = ctx.converter.to_payload(ctx, &value).expect("encode literal");
            let decoded: InvokeToolInput = ctx
                .converter
                .from_payloads(ctx, vec![payload])
                .expect("decode");
            assert_eq!(decoded.0.ctx_seed, None);
        });
    }

    #[test]
    fn invoke_tool_envelope_ignores_unknown_fields() {
        const FROZEN_FUTURE: &str = r#"{"agent_name":"agent-1","call":{"call_id":"c1","name":"echo","args":{"x":1}},"ctx_seed":{"tenant":"acme"},"added_in_a_later_release":42}"#;
        with_ctx(|ctx| {
            let value: serde_json::Value =
                serde_json::from_str(FROZEN_FUTURE).expect("literal parses");
            let payload = ctx.converter.to_payload(ctx, &value).expect("encode literal");
            let decoded: InvokeToolInput = ctx
                .converter
                .from_payloads(ctx, vec![payload])
                .expect("an envelope with an unknown field must still decode");
            assert_eq!(json_of(&decoded.0), json_of(&invoke_tool_args()));
        });
    }

    #[test]
    fn invoke_tool_rejects_unrecognized_arity() {
        with_ctx(|ctx| {
            let zero: Result<InvokeToolInput, _> = ctx.converter.from_payloads(ctx, vec![]);
            assert!(matches!(zero, Err(PayloadConversionError::WrongEncoding)));

            let p = ctx
                .converter
                .to_payload(ctx, &"x".to_owned())
                .expect("encode");
            let two: Result<InvokeToolInput, _> =
                ctx.converter.from_payloads(ctx, vec![p.clone(), p.clone()]);
            assert!(
                matches!(two, Err(PayloadConversionError::WrongEncoding)),
                "invoke_tool has no two-payload shape (0.1.x is out of the support window)"
            );

            let four: Result<InvokeToolInput, _> = ctx
                .converter
                .from_payloads(ctx, vec![p.clone(), p.clone(), p.clone(), p]);
            assert!(matches!(four, Err(PayloadConversionError::WrongEncoding)));
        });
    }

    #[test]
    fn invoke_tool_content_failure_is_encoding_error() {
        with_ctx(|ctx| {
            let bad = MultiArgs3(
                42_u32,
                tool_call(),
                Option::<serde_json::Value>::None,
            );
            let payloads = ctx.converter.to_payloads(ctx, &bad).expect("encode");
            let err = ctx
                .converter
                .from_payloads::<InvokeToolInput>(ctx, payloads)
                .expect_err("a non-String agent_name must fail");
            assert!(
                matches!(err, PayloadConversionError::EncodingError(_)),
                "expected EncodingError, got {err:?}"
            );
        });
    }
```

Extend the test module's imports with `MultiArgs3` and `ToolCallRequest`:

```rust
    use paigasus_helikon_core::{ModelRequest, ToolCallRequest};
    use temporalio_common::data_converters::{
        MultiArgs2, MultiArgs3, PayloadConverter, SerializationContextData,
    };
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p paigasus-helikon-runtime-temporal --lib activity_input`
Expected: FAIL to compile — `cannot find type InvokeToolArgs / InvokeToolInput in this scope`.

- [ ] **Step 3: Add the envelope pair**

Append to the non-test part of `src/activity_input.rs`. Extend the core import to `use paigasus_helikon_core::{ModelRequest, ToolCallRequest};`:

```rust
/// Activity name used in decode diagnostics and legacy-shape warnings.
const ACT_INVOKE_TOOL: &str = "invoke_tool";

/// The `invoke_tool` activity's input fields.
///
/// Serialized as one JSON object with `call` **nested** as an object — never
/// stringified (see [`CallModelArgs`] on the payload budget).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct InvokeToolArgs {
    /// Name of the agent to resolve on the worker's registry.
    pub agent_name: String,
    /// The single tool call to execute.
    pub call: ToolCallRequest,
    /// Optional request-scoped seed the worker's ctx factory reconstitutes.
    #[serde(default)]
    pub ctx_seed: Option<serde_json::Value>,
}

/// Temporal `Input` wrapper for [`InvokeToolArgs`]. Derives no serde — see the
/// module docs on the blanket-impl coherence conflict. `Debug` is required
/// because the tests call `.expect_err()` on a `Result<InvokeToolInput, _>`.
#[derive(Debug)]
pub(crate) struct InvokeToolInput(
    /// The wrapped fields.
    pub InvokeToolArgs,
);

impl From<InvokeToolArgs> for InvokeToolInput {
    fn from(args: InvokeToolArgs) -> Self {
        Self(args)
    }
}

impl TemporalSerializable for InvokeToolInput {
    fn to_payloads(
        &self,
        ctx: &SerializationContext<'_>,
    ) -> Result<Vec<Payload>, PayloadConversionError> {
        encode_envelope(ctx, &self.0)
    }
}

impl TemporalDeserializable for InvokeToolInput {
    fn from_payloads(
        ctx: &SerializationContext<'_>,
        payloads: Vec<Payload>,
    ) -> Result<Self, PayloadConversionError> {
        match payloads.len() {
            // Envelope: one JSON object.
            1 => {
                let mut it = payloads.into_iter();
                let args = decode_arg(
                    ctx,
                    it.next().expect("length checked above"),
                    ACT_INVOKE_TOOL,
                    0,
                    "InvokeToolArgs",
                )?;
                Ok(Self(args))
            }
            // Legacy 0.2.x: (agent_name, call, ctx_seed) as three payloads.
            3 => {
                warn_legacy(ACT_INVOKE_TOOL, 3);
                let mut it = payloads.into_iter();
                let agent_name = decode_arg(
                    ctx,
                    it.next().expect("length checked above"),
                    ACT_INVOKE_TOOL,
                    0,
                    "String",
                )?;
                let call = decode_arg(
                    ctx,
                    it.next().expect("length checked above"),
                    ACT_INVOKE_TOOL,
                    1,
                    "ToolCallRequest",
                )?;
                let ctx_seed = decode_arg(
                    ctx,
                    it.next().expect("length checked above"),
                    ACT_INVOKE_TOOL,
                    2,
                    "Option<serde_json::Value>",
                )?;
                Ok(Self(InvokeToolArgs {
                    agent_name,
                    call,
                    ctx_seed,
                }))
            }
            _ => Err(PayloadConversionError::WrongEncoding),
        }
    }
}
```

- [ ] **Step 4: Wire the activity method**

In `src/activities.rs`, replace the `invoke_tool` header (currently `:415-423`). Before:

```rust
    #[activity]
    pub(crate) async fn invoke_tool(
        self: Arc<Self>,
        ctx: ActivityContext,
        agent_name: String,
        call: ToolCallRequest,
        ctx_seed: Option<serde_json::Value>,
    ) -> Result<ToolCallOutcome, ActivityError> {
        let cancel = CancellationToken::new();
```

After:

```rust
    #[activity]
    pub(crate) async fn invoke_tool(
        self: Arc<Self>,
        ctx: ActivityContext,
        input: InvokeToolInput,
    ) -> Result<ToolCallOutcome, ActivityError> {
        let InvokeToolArgs {
            agent_name,
            call,
            ctx_seed,
        } = input.0;
        let cancel = CancellationToken::new();
```

Extend the `activity_input` import in `activities.rs` to:

```rust
use crate::activity_input::{
    CallModelArgs, CallModelInput, InvokeToolArgs, InvokeToolInput, RenderInstructionsArgs,
    RenderInstructionsInput,
};
```

- [ ] **Step 5: Wire the workflow call site**

In `src/workflow.rs`, inside `execute_tools` (currently `:375-379`):

```rust
                    .start_activity(
                        AgentActivities::invoke_tool,
                        InvokeToolArgs {
                            agent_name,
                            call,
                            ctx_seed: ctx_seed_cloned,
                        },
                        opts,
                    )
```

Extend the import in `workflow.rs` to:

```rust
use crate::activity_input::{CallModelArgs, InvokeToolArgs, RenderInstructionsArgs};
```

- [ ] **Step 6: Run the tests to verify they pass**

Run: `cargo test -p paigasus-helikon-runtime-temporal`
Expected: PASS.

- [ ] **Step 7: Confirm no leftover imports**

Run: `cargo clippy -p paigasus-helikon-runtime-temporal --all-features --all-targets -- -D warnings`
Expected: clean. `ToolCallRequest` and `ModelRequest` may now be unused imports at the top of `activities.rs` — if clippy flags them, remove them from that import list (they are still used inside `activities.rs`'s own test module and by `DurableAgentRuntime`, so check before deleting).

- [ ] **Step 8: Commit**

```bash
cargo fmt --all
git add crates/paigasus-helikon-runtime-temporal/src/activity_input.rs \
        crates/paigasus-helikon-runtime-temporal/src/activities.rs \
        crates/paigasus-helikon-runtime-temporal/src/workflow.rs
git commit -m "feat(runtime-temporal): SMA-462 add envelope input for invoke_tool"
```

---

## Task 4: Documentation

**Files:**
- Modify: `crates/paigasus-helikon-runtime-temporal/src/lib.rs:324-354` ("Upgrade Discipline and Determinism")
- Modify: `crates/paigasus-helikon-runtime-temporal/README.md:157-159` ("Upgrade Discipline")
- Modify: `crates/paigasus-helikon-runtime-temporal/CHANGELOG.md:8` (the `## [Unreleased]` section)
- Modify: `docs/book/src/concepts/runtimes.md:37` (end of the "Retry, heartbeats, and payload notes" paragraph)

**Interfaces:** None — documentation only.

- [ ] **Step 1: Rewrite the crate-docs upgrade section**

In `src/lib.rs`, replace the whole block from `//! # Upgrade Discipline and Determinism` through the line ending `//! is pending in the Rust SDK.` with:

```rust
//! # Upgrade Discipline and Determinism
//!
//! The workflow's deterministic core is [`paigasus_helikon_core::loop_state::transition`], which
//! lives in a separately versioned crate. Replaying an in-flight workflow against a worker with a
//! **different version of `paigasus-helikon-core` or `paigasus-helikon-runtime-temporal`** can
//! cause non-determinism errors (the workflow's replayed decisions don't match the new code's
//! logic).
//!
//! **Activity input encoding is not a replay hazard.** Temporal's replay check compares an
//! activity's **id** and **type** only — never its input payloads
//! (`temporalio-sdk-core-0.5.0`, `activity_state_machine.rs`, the
//! `IdAndTypeDeterminismChecks` gate). Changing how an activity's arguments are encoded
//! therefore cannot trip the non-determinism checker; *renaming* an activity would. This
//! statement is pinned to `temporalio-* = 0.5.0` and must be re-verified on any SDK bump.
//!
//! **SMA-462 wire change (activity inputs are now a single envelope payload).** Each of
//! `render_instructions` / `call_model` / `invoke_tool` takes one self-describing JSON-object
//! payload instead of positional arguments. Workers on this version also decode the previous
//! 0.2.x positional shapes, so activity tasks queued by a 0.2.x worker execute normally.
//!
//! The reverse does not hold, and it matters during a rolling deploy: a **0.2.x** worker handed
//! one of the new envelope payloads cannot decode it. It fails the attempt retryably and Temporal
//! re-dispatches until a worker on this version takes it. Four things bound that recovery:
//!
//! 1. A finite `maximum_attempts` on `model_retry_policy` / `tool_retry_policy` can be exhausted.
//! 2. `WorkflowInput::timeout_ms` interrupts the whole run on its own schedule, regardless of
//!    retry policy — so `render_instructions`' unlimited default retries are not the safety net
//!    they appear to be.
//! 3. A terminal `render_instructions` failure ends the run; it is not a degraded step.
//! 4. Exhausted `invoke_tool` retries are folded into a tool-error result and fed to the model
//!    rather than failing loudly.
//!
//! So: **keep the mixed-fleet window short**, and either drain in-flight runs first or ensure
//! retry caps are unlimited and run deadlines generous for the duration of the rollout.
//!
//! **Rolling back.** Once a worker on this version has queued an envelope-shaped activity task,
//! that payload is frozen in the `ActivityTaskScheduled` event and every retry re-delivers it. A
//! rollback to 0.2.x leaves those activities undecodable until the run deadline. **Drain in-flight
//! runs before rolling back.**
//!
//! **What this buys.** Future additive changes to an activity's input are compatible in both
//! directions, because the envelope is self-describing: unknown fields are ignored and absent
//! fields default. That guarantee is scoped to **the envelope's own field set**. It does *not*
//! extend to the `paigasus-helikon-core` types nested inside those envelopes (`ModelRequest`,
//! `ToolCallRequest`), nor to activity **outputs** — a serde change in any of those breaks the
//! wire exactly as before.
//!
//! **Operational guidance:**
//!
//! 1. **Upgrade one release at a time.** Skipping a release skips the overlap window in which
//!    both shapes are readable.
//! 2. **Drain in-flight runs before redeploying** when in doubt. Agent runs are typically
//!    minutes-to-hours, not months. Alternatively use blue-green task queues: point the old
//!    worker to `"queue-v1"` and the runner to `"queue-v2"`, run new workflows on v2 while old
//!    ones drain from v1, then decommission v1.
//! 3. **Check the CHANGELOG.** Any release whose transition behavior changed is flagged as
//!    replay-breaking.
//! 4. **Production path:** [Temporal Worker Versioning (Build IDs)](https://docs.temporal.io/workers#worker-versioning)
//!    is the long-term solution for zero-downtime updates; support is pending in the Rust SDK.
```

- [ ] **Step 2: Update the README upgrade section**

In `crates/paigasus-helikon-runtime-temporal/README.md`, replace the single paragraph under `### Upgrade Discipline` with:

```markdown
Replaying workflows against a different version of `paigasus-helikon-core` or this crate can cause non-determinism errors. Activity **input encoding** is not among those hazards — Temporal's replay check compares an activity's id and type only, never its payloads.

Activity inputs are a single self-describing envelope payload as of SMA-462, and this version also decodes the previous 0.2.x positional shapes. A 0.2.x worker cannot decode the new shape, so **upgrade one release at a time and keep the mixed-fleet window short**; and because a queued envelope payload is frozen in history, **drain in-flight runs before rolling back** to 0.2.x. Blue-green task queues remain available. See the [crate docs](https://docs.rs/paigasus-helikon-runtime-temporal) (§ "Upgrade Discipline and Determinism").
```

- [ ] **Step 3: Add the CHANGELOG entry**

In `crates/paigasus-helikon-runtime-temporal/CHANGELOG.md`, replace the line `## [Unreleased]` with:

```markdown
## [Unreleased]

### Changed

- *(runtime-temporal)* SMA-462 activity inputs are now a **single self-describing envelope payload**
  - `render_instructions`, `call_model` and `invoke_tool` each take one JSON-object payload instead of positional arguments. Workers on this version also decode the previous 0.2.x positional shapes, so activity tasks queued by a 0.2.x worker execute normally.
  - No public Rust API change — the envelope types are crate-internal.
  - Future additive changes to an activity's input are now compatible in both directions (unknown fields ignored, absent fields defaulted). This is scoped to the envelope's own field set: it does **not** cover the `paigasus-helikon-core` types nested inside them (`ModelRequest`, `ToolCallRequest`) or activity outputs.

### Upgrade notes

- **Upgrade one release at a time**, and keep the mixed-fleet window short. A 0.2.x worker handed one of the new envelope payloads cannot decode it; it fails the attempt retryably and Temporal re-dispatches until a worker on this version takes it. Four things bound that recovery: a finite `maximum_attempts` on `model_retry_policy` / `tool_retry_policy` can be exhausted; `WorkflowInput::timeout_ms` interrupts the run regardless of retry policy; a terminal `render_instructions` failure ends the run outright; and exhausted `invoke_tool` retries are folded into a tool-error result fed to the model rather than failing loudly.
- **Prefer draining in-flight runs before this upgrade**, or ensure retry caps are unlimited and run deadlines generous for the duration of the rollout. Blue-green task queues remain available.
- **Rolling back requires a drain.** Once a worker on this version has queued an envelope-shaped activity task, that payload is frozen in the `ActivityTaskScheduled` event and every retry re-delivers it; a rollback to 0.2.x leaves those activities undecodable until the run deadline.
- Activity input encoding is **not** a replay hazard: Temporal's replay check compares an activity's id and type only, never its input payloads.
```

- [ ] **Step 4: Add the book sentence**

In `docs/book/src/concepts/runtimes.md`, append to the end of the paragraph that currently ends `(~15–20 turns with tool outputs ≤ 50 KB each).`:

```markdown
 Those payloads are a single self-describing envelope per activity, which makes future input changes backward- and forward-compatible — but upgrading a worker fleet still wants care: see the [crate docs](https://docs.rs/paigasus-helikon-runtime-temporal) (§ "Upgrade Discipline and Determinism") for the one-release-at-a-time and drain-before-rollback rules.
```

- [ ] **Step 5: Verify the docs build**

```bash
RUSTDOCFLAGS="-D warnings" cargo doc -p paigasus-helikon-runtime-temporal --all-features --no-deps
mdbook build docs/book
```
Expected: both clean. `mdbook` treats link-check warnings as errors (`[output.linkcheck] warning-policy = "error"`), so a broken link fails the build.

- [ ] **Step 6: Commit**

```bash
git add crates/paigasus-helikon-runtime-temporal/src/lib.rs \
        crates/paigasus-helikon-runtime-temporal/README.md \
        crates/paigasus-helikon-runtime-temporal/CHANGELOG.md \
        docs/book/src/concepts/runtimes.md
git commit -m "docs(runtime-temporal): SMA-462 document the envelope wire format and upgrade path"
```

---

## Task 5: Full gate run and live validation evidence

**Files:** None modified unless a gate fails.

**Interfaces:** None.

- [ ] **Step 1: Run every CI gate locally**

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-features --all-targets -- -D warnings
cargo test --workspace --all-features
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps
```
Expected: all clean.

> If `cargo test --workspace --all-features` shows ~48 `bedrock` failures mentioning `NATIVE_ROOTS` on macOS, that is a known environment artifact of the checkout path, not a regression — this plan is being executed inside a scratchpad worktree specifically to avoid it. Re-run in the worktree before investigating.

- [ ] **Step 2: Verify no public API changed**

Run: `cargo public-api --help >/dev/null 2>&1 || echo "not installed"`

If not installed, verify by inspection instead:

```bash
git diff main -- crates/paigasus-helikon-runtime-temporal/src/ | grep -E '^\+\s*pub (fn|struct|enum|trait|mod|use)' | grep -v 'pub(crate)'
```
Expected: **no output**. Any hit means a public item was added and the spec's §9 release-mechanics assumption (no core bump, no facade bump) needs revisiting — stop and report.

- [ ] **Step 3: Run the live Temporal suite**

This is the evidence artifact the spec requires; no CI context runs it.

```bash
temporal server start-dev --headless &
TEMPORAL_TEST_SERVER=localhost:7233 \
  cargo test -p paigasus-helikon-runtime-temporal --test temporal_live -- --test-threads=1
```
Expected: PASS, exercising the new envelope encoding end-to-end (`happy_path_tool_roundtrip` covers encode → schedule → decode → execute).

Capture the command and its summary line — it goes in the PR description. If `temporal` is not installed, report that the live gate could not be run rather than claiming it passed.

- [ ] **Step 4: Confirm the diff matches the plan**

```bash
git diff main --stat
```
Expected exactly: `activity_input.rs` (new), `activities.rs`, `workflow.rs`, `lib.rs`, `README.md`, `CHANGELOG.md`, `docs/book/src/concepts/runtimes.md`, plus the spec and plan docs. Anything else is scope creep — report it.

- [ ] **Step 5: File the follow-up issue**

Create a Linear issue in the `Paigasus Helikon` project (**not** a GitHub issue):

> **Title:** `runtime-temporal: remove the 0.2.x legacy activity-input decode arms`
> **Body:** Follow-up to SMA-462. `activity_input.rs` carries legacy positional decode arms for the 0.2.x wire shapes, each emitting a `tracing::warn!` with `legacy_arity` when hit. Once no such warning has appeared for a full retention window, no 0.2.x-era worker is scheduling tasks and the arms (plus their tests) can be deleted. Trigger: the warning going silent.

---

## Self-Review

**Spec coverage:**

| Spec section | Task |
|---|---|
| §4.1 envelope types | 1, 2, 3 |
| §4.2 `#[activity]` single-parameter signatures | 1.6, 2.4, 3.4 |
| §4.3 encode, one nested payload | 1.2, 2.3, 3.3 |
| §4.4 arity dispatch incl. the `_ =>` arm | 1.2, 2.3, 3.3 |
| §4.5 module docs preserving the dispatch reasoning | 1.1 |
| §4.6 field-evolution contract (4 rules) | 1.1 (docs), 1.4 / 2.1 / 3.1 (rules 1–2 asserted) |
| §4.7 `tracing::warn!` + exit criterion | 1.1 (`warn_legacy`), 5.5 (follow-up issue) |
| §4.8 call sites | 1.7, 2.5, 3.5 |
| §5 error handling, `WrongEncoding` vs `EncodingError` | 1.2, and asserted in 1.4 / 2.1 / 3.1 |
| §5.1 payload-free diagnostics | 1.1 (`decode_arg` discards source), 1.4 (sentinel test) |
| §5.2 four retry bounds | 4.1, 4.3 (crate docs + CHANGELOG) |
| §6.1 all 8 converter-level test categories | 1.4, 2.1, 3.1 |
| §6.2 live coverage + evidence artifact | 5.3 |
| §7 upgrade story incl. rollback constraint | 4.1, 4.2, 4.3 |
| §8 all four doc surfaces | 4 |
| §9 no public API change | 5.2 |
| §10 follow-up issue | 5.5 |

**Placeholder scan:** No "TBD", "TODO", "similar to Task N", or "add error handling". Every code step carries a complete code block; every legacy-decode arm is written out per activity rather than cross-referenced.

**Type consistency:** `RenderInstructionsArgs`/`Input`, `CallModelArgs`/`Input`, `InvokeToolArgs`/`Input` are spelled identically in their definitions (1.2, 2.3, 3.3), the `activities.rs` destructuring (1.6, 2.4, 3.4), the `workflow.rs` construction (1.7, 2.5, 3.5), and the imports. `decode_arg` and `warn_legacy` keep the same signatures across all three uses. Test helpers `with_ctx` (Task 1) and `json_of` (Task 2) are defined once and reused. The import lists in `activities.rs` and `workflow.rs` are shown cumulatively at each task, so a task executed in isolation still produces a compiling file.

**Known ordering note:** Task 3 Step 7 flags that `ToolCallRequest` / `ModelRequest` may become unused imports in `activities.rs` once all three activities are wired — with instructions to check rather than blindly delete, since both are still referenced by `DurableAgentRuntime`'s trait methods and the file's own test module.
