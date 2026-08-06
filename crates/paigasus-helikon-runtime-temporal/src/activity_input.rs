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
//! that or the legacy pre-envelope (0.2.0–0.2.1) positional arity (2 payloads
//! for `render_instructions` / `call_model`, 3 for `invoke_tool`). Both paths
//! build the same `*Args` value, so everything downstream is shape-agnostic.
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

use paigasus_helikon_core::{ModelRequest, ToolCallRequest};
use serde::{Deserialize, Serialize};
use temporalio_common::data_converters::{
    GenericPayloadConverter, PayloadConversionError, SerializationContext, TemporalDeserializable,
    TemporalSerializable,
};
use temporalio_common::protos::temporal::api::common::v1::Payload;

/// Activity name used in decode diagnostics and legacy-shape warnings.
///
/// Fully qualified to match the `ActivityType` Temporal actually registers:
/// `#[activities]` with no name override derives it as
/// `"{ImplType}::{method}"` (`temporalio-macros-0.5.0/src/activities_definitions.rs:556`),
/// i.e. `AgentActivities::render_instructions` here — not the bare method name
/// — so this string is what an operator would actually grep for in the
/// Temporal Web UI or an `ActivityTaskFailed` history event.
const ACT_RENDER: &str = "AgentActivities::render_instructions";

/// Warn that a pre-envelope activity input was decoded.
///
/// This is the operator's "safe to remove the legacy decode arms" signal: once
/// no such warning has appeared for a full retention window, no
/// 0.2.1-and-earlier worker is scheduling tasks any more and the arms can go.
fn warn_legacy(activity: &str, arity: usize) {
    tracing::warn!(
        activity,
        legacy_arity = arity,
        "decoded a pre-envelope activity input; a 0.2.1-and-earlier worker is still scheduling tasks"
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
            // Envelope: one JSON object. This arity collides with pre-SMA-455
            // (0.1.x) `render_instructions`, which took a single
            // `agent_name: String` argument — one payload, same as here. The
            // collision is arity-only, not shape: a bare JSON string cannot
            // deserialize into `RenderInstructionsArgs`, so a straggling
            // 0.1.x-shaped task fails safe here as an `EncodingError` (see
            // `decode_arg`) rather than being silently misread. 0.1.x is out
            // of the support window, so this is not handled beyond failing
            // safe.
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
            // Legacy pre-envelope (0.2.0–0.2.1): (agent_name, ctx_seed) as two payloads.
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

/// Activity name used in decode diagnostics and legacy-shape warnings. Fully
/// qualified to match the registered `ActivityType` — see `ACT_RENDER`'s doc.
const ACT_CALL_MODEL: &str = "AgentActivities::call_model";

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
            // Legacy pre-envelope (0.2.0–0.2.1): (agent_name, request) as two
            // payloads. Unchanged since 0.1.x, but still pre-envelope.
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

/// Activity name used in decode diagnostics and legacy-shape warnings. Fully
/// qualified to match the registered `ActivityType` — see `ACT_RENDER`'s doc.
const ACT_INVOKE_TOOL: &str = "AgentActivities::invoke_tool";

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
            // Legacy pre-envelope (0.2.0–0.2.1): (agent_name, call, ctx_seed) as three payloads.
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

#[cfg(test)]
mod tests {
    use super::*;
    use paigasus_helikon_core::{ModelRequest, ToolCallRequest};
    use temporalio_common::data_converters::{
        MultiArgs2, MultiArgs3, PayloadConverter, SerializationContextData,
    };

    /// Run `f` with a [`SerializationContext`] over the **default** converter.
    ///
    /// Every codec test must go through this rather than calling
    /// `Wrapper::from_payloads` directly: a direct call exercises none of the
    /// `Composite` -> `UseWrappers` dispatch the design depends on. This crate
    /// never configures a `DataConverter` itself — the caller supplies the
    /// connected `temporalio_client::Client` (`TemporalAgentWorkerBuilder::client`)
    /// — so a worker built against a client left on the SDK's defaults yields
    /// exactly `PayloadConverter::default()`. That is the configuration these
    /// tests target; a caller-supplied client with a non-default
    /// `DataConverter` is out of scope here, same as it always was for the
    /// legacy `MultiArgs{N}` shapes this design builds on.
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
                .expect("a task queued by a 0.2.1-and-earlier worker must decode");
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
    ///
    /// The payload is a bare JSON **string** decoded against the envelope arm,
    /// which expects a struct. That combination is the one that actually leaks:
    /// serde_json renders it as `invalid type: string "<content>", expected
    /// struct ...`, embedding the value verbatim. A map-vs-string mismatch would
    /// NOT prove anything here — `Unexpected::Map` never echoes nested content,
    /// so such a test passes whether or not `decode_arg` discards its source
    /// error.
    ///
    /// This is also the only test that exercises the **envelope** arm's
    /// (arity 1) error path — every `*_content_failure_is_encoding_error` test
    /// feeds a legacy `MultiArgs{N}` shape instead. So it additionally asserts
    /// the error variant is `EncodingError`, not just sentinel-absent: if the
    /// envelope arm ever regressed to returning `WrongEncoding`, the composite
    /// converter would silently fall through to the `serde_json` arm (which
    /// also yields `WrongEncoding`), and production would lose this
    /// diagnostic — a bare "Wrong encoding" in Temporal history — while every
    /// other test here kept passing.
    #[test]
    fn decode_diagnostics_never_leak_payload_bytes() {
        const SENTINEL: &str = "super-secret-tenant-token";
        with_ctx(|ctx| {
            let payload = ctx
                .converter
                .to_payload(ctx, &SENTINEL.to_owned())
                .expect("encode");
            let err = ctx
                .converter
                .from_payloads::<RenderInstructionsInput>(ctx, vec![payload])
                .expect_err("a bare string is not an envelope");

            assert!(
                matches!(err, PayloadConversionError::EncodingError(_)),
                "expected EncodingError from the envelope arm, got {err:?}"
            );

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
            let payloads = ctx
                .converter
                .to_payloads(ctx, &legacy)
                .expect("encode legacy");
            assert_eq!(payloads.len(), 2, "legacy shape is two payloads");

            let decoded: CallModelInput = ctx
                .converter
                .from_payloads(ctx, payloads)
                .expect("a task queued by a 0.2.1-and-earlier worker must decode");
            assert_eq!(json_of(&decoded.0), json_of(&call_model_args()));
        });
    }

    /// The `request` value is a frozen literal of `ModelRequest`'s wire shape as
    /// of this commit, NOT a value serialized at test time — a serialized
    /// fixture tracks the struct's drift and asserts nothing.
    ///
    /// If a future change to `paigasus-helikon-core`'s `ModelRequest` breaks this
    /// test, that is the test working as intended: `ModelRequest` is a nested
    /// core type, explicitly outside this module's field-evolution contract
    /// (rule 4), so a change to it IS a wire-compatibility break. Update the
    /// literal deliberately and treat the break as a release note, rather than
    /// regenerating the fixture to make the red go away.
    ///
    /// Asserts full JSON equality (`json_of`), not just `agent_name`: if
    /// `ModelRequest` ever grew a `#[serde(default)]` field, a same-fields-only
    /// check would let the frozen literal decode with that field silently
    /// defaulted, and this canary would stay green through the exact drift it
    /// exists to catch.
    #[test]
    fn call_model_decodes_frozen_envelope_literal() {
        const FROZEN: &str = r#"{"agent_name":"agent-1","request":{"messages":[],"tools":[],"model_settings":{"temperature":null,"top_p":null,"max_output_tokens":null,"tool_choice":null,"response_format":null,"previous_response_id":null}}}"#;
        with_ctx(|ctx| {
            let value: serde_json::Value = serde_json::from_str(FROZEN).expect("literal parses");
            let payload = ctx
                .converter
                .to_payload(ctx, &value)
                .expect("encode literal");
            let decoded: CallModelInput = ctx
                .converter
                .from_payloads(ctx, vec![payload])
                .expect("decode");
            assert_eq!(json_of(&decoded.0), json_of(&call_model_args()));
        });
    }

    #[test]
    fn call_model_envelope_ignores_unknown_fields() {
        const FROZEN_FUTURE: &str = r#"{"agent_name":"agent-1","request":{"messages":[],"tools":[],"model_settings":{"temperature":null,"top_p":null,"max_output_tokens":null,"tool_choice":null,"response_format":null,"previous_response_id":null}},"added_in_a_later_release":42}"#;
        with_ctx(|ctx| {
            let value: serde_json::Value =
                serde_json::from_str(FROZEN_FUTURE).expect("literal parses");
            let payload = ctx
                .converter
                .to_payload(ctx, &value)
                .expect("encode literal");
            let decoded: CallModelInput = ctx
                .converter
                .from_payloads(ctx, vec![payload])
                .expect("an envelope with an unknown field must still decode");
            assert_eq!(json_of(&decoded.0), json_of(&call_model_args()));
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
            let payloads = ctx
                .converter
                .to_payloads(ctx, &legacy)
                .expect("encode legacy");
            assert_eq!(payloads.len(), 3, "legacy shape is three payloads");

            let decoded: InvokeToolInput = ctx
                .converter
                .from_payloads(ctx, payloads)
                .expect("a task queued by a 0.2.1-and-earlier worker must decode");
            assert_eq!(json_of(&decoded.0), json_of(&invoke_tool_args()));
        });
    }

    #[test]
    fn invoke_tool_decodes_frozen_envelope_literal() {
        const FROZEN: &str = r#"{"agent_name":"agent-1","call":{"call_id":"c1","name":"echo","args":{"x":1}},"ctx_seed":{"tenant":"acme"}}"#;
        with_ctx(|ctx| {
            let value: serde_json::Value = serde_json::from_str(FROZEN).expect("literal parses");
            let payload = ctx
                .converter
                .to_payload(ctx, &value)
                .expect("encode literal");
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
            let payload = ctx
                .converter
                .to_payload(ctx, &value)
                .expect("encode literal");
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
            let payload = ctx
                .converter
                .to_payload(ctx, &value)
                .expect("encode literal");
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
            let bad = MultiArgs3(42_u32, tool_call(), Option::<serde_json::Value>::None);
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
}
