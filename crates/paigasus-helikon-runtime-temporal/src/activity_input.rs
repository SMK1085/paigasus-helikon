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
/// Activity name used in decode diagnostics and legacy-shape warnings.
const ACT_CALL_MODEL: &str = "call_model";
/// Activity name used in decode diagnostics and legacy-shape warnings.
const ACT_INVOKE_TOOL: &str = "invoke_tool";

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
/// unaffected by that conflict (it isn't a blanket-impl'd trait here) and is
/// derived so tests can call `.expect_err()` on a `Result<Self, _>`.
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
            let payloads = ctx
                .converter
                .to_payloads(ctx, &legacy)
                .expect("encode legacy");
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
            let payload = ctx
                .converter
                .to_payload(ctx, &value)
                .expect("encode literal");
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
            let payload = ctx
                .converter
                .to_payload(ctx, &value)
                .expect("encode literal");
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
        const FROZEN_FUTURE: &str = r#"{"agent_name":"agent-1","ctx_seed":{"tenant":"acme"},"added_in_a_later_release":42}"#;
        with_ctx(|ctx| {
            let value: serde_json::Value =
                serde_json::from_str(FROZEN_FUTURE).expect("literal parses");
            let payload = ctx
                .converter
                .to_payload(ctx, &value)
                .expect("encode literal");
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
