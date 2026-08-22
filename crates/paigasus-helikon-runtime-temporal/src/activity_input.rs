//! Wire codec for Temporal activity inputs.
//!
//! Each activity takes exactly **one** parameter, whose type is an envelope
//! wrapper defined here. That single-parameter shape is load-bearing:
//! `#[activities]` derives `ActivityDefinition::Input` from the method's
//! parameter list via `multi_args_input_type`
//! (`temporalio-macros-0.7.0/src/activities_definitions.rs:265-278`), which maps
//! `0 => ()`, `1 => the parameter's own type`, and `n => MultiArgs{n}`. There is
//! no `MultiArgs1`, so a one-parameter activity's `Input` *is* our wrapper —
//! which lets us supply a hand-written codec.
//!
//! # Wire shapes
//!
//! Each wrapper encodes to — and decodes from — **one** JSON-object payload.
//! The pre-envelope positional arities (2 payloads for `render_instructions` /
//! `call_model`, 3 for `invoke_tool`) are still recognized, but only to produce
//! a named [`reject_legacy`] error; they are no longer decoded. Upgrading a
//! fleet from 0.2.0 or 0.2.1 therefore requires a stop at 0.2.2, which decodes
//! both shapes; 0.1.x cannot use that bridge and must drain outright — see the
//! crate docs, § "Upgrade Discipline and Determinism".
//!
//! Note the two version scopes are not interchangeable: [`reject_legacy`]'s own
//! message says "0.2.1 and earlier" because it describes the shape that
//! *arrived* (`call_model`'s 2-payload shape dates back to 0.1.x), whereas the
//! supported *migration* path is 0.2.0/0.2.1 only.
//!
//! # Why the hand-written impls are reached at all
//!
//! `PayloadConverter::default()` is `Composite([UseWrappers, serde_json()])`
//! (`temporalio-common-wasm-0.7.0/src/data_converters.rs:200-206`). The
//! `Composite` arm tries each sub-converter in order, and `UseWrappers`
//! dispatches to the **overridable** trait methods `T::to_payloads` (`:541`) and
//! `T::from_payloads` (`:576`) *before* the `serde_json` arm applies its hard
//! `payloads.len() != 1` check (`:570-572`). This is the same mechanism
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

/// Activity name used in decode diagnostics and pre-envelope rejections.
///
/// Fully qualified to match the `ActivityType` Temporal actually registers:
/// `#[activities]` with no name override derives it as
/// `"{ImplType}::{method}"` (`temporalio-macros-0.7.0/src/activities_definitions.rs:548`),
/// i.e. `AgentActivities::render_instructions` here — not the bare method name
/// — so this string is what an operator would actually grep for in the
/// Temporal Web UI or an `ActivityTaskFailed` history event.
const ACT_RENDER: &str = "AgentActivities::render_instructions";

/// Reject a pre-envelope activity input with an actionable diagnostic.
///
/// `EncodingError` rather than `WrongEncoding` is deliberate and load-bearing:
/// the composite converter treats `WrongEncoding` as "not my encoding" and falls
/// through to the next converter, swallowing the message; any other error is
/// returned immediately, so this is what actually reaches the
/// `ActivityTaskFailed` history event.
///
/// The `tracing::error!` is not redundant with that event: the history event is
/// visible only to someone querying Temporal, whereas this reaches the worker's
/// own log pipeline, where alerting lives. Under an unbounded retry policy this
/// logs once per attempt — accepted deliberately, since the volume is itself the
/// signal for a condition that requires operator intervention.
///
/// The message carries the activity name and the payload *count* only, never
/// payload bytes: it lands in Temporal history, a persistence boundary.
fn reject_legacy(activity: &str, arity: usize) -> PayloadConversionError {
    tracing::error!(
        target: "paigasus::runtime_temporal::activity_input",
        activity,
        legacy_arity = arity,
        "refused a pre-envelope activity input; a worker on 0.2.1 or earlier queued this task"
    );
    PayloadConversionError::EncodingError(
        format!(
            "{activity}: received {arity} payloads — the pre-envelope positional shape \
             (0.2.1 and earlier). This worker decodes only the single-payload envelope. \
             Recovery: re-join a worker built against `paigasus-helikon-runtime-temporal` \
             0.2.2, which decodes both shapes, to this task queue and let in-flight runs \
             drain."
        )
        .into(),
    )
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
            // Pre-envelope (0.2.1 and earlier): (agent_name, ctx_seed) as two
            // payloads. Recognized only to produce a named error — SMA-484.
            2 => Err(reject_legacy(ACT_RENDER, 2)),
            _ => Err(PayloadConversionError::WrongEncoding),
        }
    }
}

/// Activity name used in decode diagnostics and pre-envelope rejections. Fully
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
            // Pre-envelope (0.2.1 and earlier): (agent_name, request) as two
            // payloads — this shape is unchanged since 0.1.x. Recognized only to
            // produce a named error — SMA-484.
            2 => Err(reject_legacy(ACT_CALL_MODEL, 2)),
            _ => Err(PayloadConversionError::WrongEncoding),
        }
    }
}

/// Activity name used in decode diagnostics and pre-envelope rejections. Fully
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
            // Pre-envelope (0.2.1 and earlier): (agent_name, call, ctx_seed) as
            // three payloads. Recognized only to produce a named error — SMA-484.
            3 => Err(reject_legacy(ACT_INVOKE_TOOL, 3)),
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

    /// The pre-envelope two-payload shape must now be **refused**, not decoded.
    ///
    /// Asserts the message's content, not merely that an error occurred: a
    /// variant-only assertion would still pass if the arm returned
    /// `WrongEncoding` (letting the composite silently fall through and losing
    /// the diagnostic in production), or if a copy-paste error passed the wrong
    /// `ACT_*` constant or arity into `reject_legacy`.
    #[test]
    fn render_instructions_rejects_legacy_two_payload_shape() {
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
            assert!(msg.contains("2 payloads"), "must name the count: {msg}");
            assert!(
                msg.contains("0.2.2"),
                "must name the recovery version: {msg}"
            );
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

    /// Arity 2 is deliberately absent here: it is `render_instructions`'
    /// former legacy arity and now yields `EncodingError` from `reject_legacy`,
    /// not `WrongEncoding`. Covered by
    /// `render_instructions_rejects_legacy_two_payload_shape`.
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
    ///
    /// Since SMA-484 the only **decodable** arity is 1, so this feeds a single
    /// payload. The bad-content case is a **missing required field**, kept
    /// deliberately distinct from `decode_diagnostics_never_leak_payload_bytes`
    /// (which feeds a bare JSON string at the same arity) so the two tests do
    /// not collapse into duplicates.
    #[test]
    fn render_instructions_content_failure_is_encoding_error() {
        with_ctx(|ctx| {
            // Envelope arity, but `agent_name` is absent and has no serde default.
            let bad = serde_json::json!({});
            let payload = ctx.converter.to_payload(ctx, &bad).expect("encode");
            let err = ctx
                .converter
                .from_payloads::<RenderInstructionsInput>(ctx, vec![payload])
                .expect_err("a missing agent_name must fail");
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
    /// Since SMA-484 the sibling `*_content_failure_is_encoding_error` tests
    /// also exercise the **envelope** arm's (arity 1) error path, but each
    /// feeds a struct with a missing or wrong-typed field; this test is the
    /// one that feeds a bare string instead, for the leak-surfacing reason
    /// above. It additionally asserts the error variant is `EncodingError`,
    /// not just sentinel-absent: if the envelope arm ever regressed to
    /// returning `WrongEncoding`, the composite converter would silently fall
    /// through to the `serde_json` arm (which also yields `WrongEncoding`),
    /// and production would lose this diagnostic — a bare "Wrong encoding" in
    /// Temporal history — while every other test here kept passing.
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

    /// Spec §7.3: the **rejection** diagnostic must be payload-free too.
    ///
    /// `decode_diagnostics_never_leak_payload_bytes` covers only the arity-1
    /// envelope arm. `reject_legacy` is a separate error path whose input
    /// carries real content, so without this test a later edit appending the
    /// offending payload's bytes to the message would ship silently into
    /// Temporal history.
    ///
    /// Also asserts the error variant, same as its arity-1 sibling: without
    /// it, deleting the `2 =>` rejection arm would leave this test passing
    /// against the fallback `WrongEncoding` case, whose `Display` ("Wrong
    /// encoding") is sentinel-free by construction — silently testing
    /// nothing about `reject_legacy` at all.
    #[test]
    fn rejection_diagnostics_never_leak_payload_bytes() {
        const SENTINEL: &str = "super-secret-tenant-token";
        with_ctx(|ctx| {
            let legacy = MultiArgs2(SENTINEL.to_owned(), Option::<serde_json::Value>::None);
            let payloads = ctx
                .converter
                .to_payloads(ctx, &legacy)
                .expect("encode legacy");
            let err = ctx
                .converter
                .from_payloads::<RenderInstructionsInput>(ctx, payloads)
                .expect_err("the pre-envelope shape must no longer decode");

            assert!(
                matches!(err, PayloadConversionError::EncodingError(_)),
                "expected EncodingError from reject_legacy, got {err:?}"
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

    /// The pre-envelope two-payload shape must now be refused — see
    /// `render_instructions_rejects_legacy_two_payload_shape` on why the
    /// message content is asserted rather than just the error variant.
    #[test]
    fn call_model_rejects_legacy_two_payload_shape() {
        with_ctx(|ctx| {
            let legacy = MultiArgs2("agent-1".to_owned(), ModelRequest::new());
            let payloads = ctx
                .converter
                .to_payloads(ctx, &legacy)
                .expect("encode legacy");
            assert_eq!(payloads.len(), 2, "legacy shape is two payloads");

            let err = ctx
                .converter
                .from_payloads::<CallModelInput>(ctx, payloads)
                .expect_err("the pre-envelope shape must no longer decode");
            assert!(
                matches!(err, PayloadConversionError::EncodingError(_)),
                "expected EncodingError, got {err:?}"
            );
            let msg = err.to_string();
            assert!(
                msg.contains(ACT_CALL_MODEL),
                "must name the activity: {msg}"
            );
            assert!(msg.contains("2 payloads"), "must name the count: {msg}");
            assert!(
                msg.contains("0.2.2"),
                "must name the recovery version: {msg}"
            );
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

    /// Arity 2 is deliberately absent here: it is `call_model`'s former legacy
    /// arity and now yields `EncodingError` from `reject_legacy`, not
    /// `WrongEncoding`. Covered by `call_model_rejects_legacy_two_payload_shape`.
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

    /// A recognized arity (1, the envelope) whose content is wrong must be
    /// `EncodingError` — see `render_instructions_content_failure_is_encoding_error`.
    ///
    /// Corrupts exactly one field of an otherwise-valid envelope, so the failure
    /// is unambiguously the **wrong type** on `agent_name` rather than a missing
    /// `request`.
    #[test]
    fn call_model_content_failure_is_encoding_error() {
        with_ctx(|ctx| {
            let mut bad = serde_json::to_value(call_model_args()).expect("to_value");
            bad["agent_name"] = serde_json::json!(42);
            let payload = ctx.converter.to_payload(ctx, &bad).expect("encode");
            let err = ctx
                .converter
                .from_payloads::<CallModelInput>(ctx, vec![payload])
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

    /// The pre-envelope three-payload shape must now be refused — see
    /// `render_instructions_rejects_legacy_two_payload_shape` on why the
    /// message content is asserted rather than just the error variant.
    #[test]
    fn invoke_tool_rejects_legacy_three_payload_shape() {
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

            let err = ctx
                .converter
                .from_payloads::<InvokeToolInput>(ctx, payloads)
                .expect_err("the pre-envelope shape must no longer decode");
            assert!(
                matches!(err, PayloadConversionError::EncodingError(_)),
                "expected EncodingError, got {err:?}"
            );
            let msg = err.to_string();
            assert!(
                msg.contains(ACT_INVOKE_TOOL),
                "must name the activity: {msg}"
            );
            assert!(msg.contains("3 payloads"), "must name the count: {msg}");
            assert!(
                msg.contains("0.2.2"),
                "must name the recovery version: {msg}"
            );
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

    /// Arity 3 is deliberately absent here: it is `invoke_tool`'s former legacy
    /// arity and now yields `EncodingError` from `reject_legacy`, not
    /// `WrongEncoding`. Covered by
    /// `invoke_tool_rejects_legacy_three_payload_shape`. The arity-2 probe below
    /// stays `WrongEncoding` — 2 is `invoke_tool`'s 0.1.x shape, outside the
    /// support window.
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

    /// A recognized arity (1, the envelope) whose content is wrong must be
    /// `EncodingError` — see `render_instructions_content_failure_is_encoding_error`.
    ///
    /// Corrupts exactly one field of an otherwise-valid envelope, so the failure
    /// is unambiguously the **wrong type** on `agent_name` rather than a missing
    /// `call`.
    #[test]
    fn invoke_tool_content_failure_is_encoding_error() {
        with_ctx(|ctx| {
            let mut bad = serde_json::to_value(invoke_tool_args()).expect("to_value");
            bad["agent_name"] = serde_json::json!(42);
            let payload = ctx.converter.to_payload(ctx, &bad).expect("encode");
            let err = ctx
                .converter
                .from_payloads::<InvokeToolInput>(ctx, vec![payload])
                .expect_err("a non-String agent_name must fail");
            assert!(
                matches!(err, PayloadConversionError::EncodingError(_)),
                "expected EncodingError, got {err:?}"
            );
        });
    }
}
