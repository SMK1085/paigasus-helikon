//! The registered conformance subjects.
//!
//! Each subject in this file serves its own wire bytes through the crate's
//! paced HTTP server and hands back the stream from its real `Model::invoke`,
//! so every assertion in `assert_conforms` is made about production driver and
//! production translator code — never about a reimplementation of either.
//!
//! ## Adding a subject
//!
//! One `mod <subject> { … }` per subject, holding its fixture helpers, its
//! `StreamUnderTest` impl and one `#[tokio::test]` that calls
//! `assert_conforms`. The module scoping is not cosmetic: it is what lets every
//! subject call its helpers `text_delta`, `script`, `request` and so on without
//! a per-subject prefix, and it keeps six subjects' worth of near-identical
//! names from colliding in one flat namespace.
//!
//! Two rules apply to every subject, and both exist because breaking them makes
//! a check pass without measuring anything:
//!
//! 1. **Decline and uncaptured reasons are literal strings**, copied by hand
//!    from `src/declines.rs`. A subject that looked its reasons up from
//!    `DECLINED` or `UNCAPTURED` would satisfy that table's two-directional
//!    cross-check by construction, which is exactly the drift the table exists
//!    to catch.
//! 2. **`encodes_stop_reason` is measured from the bytes about to be served**,
//!    never restated from `Scenario::expects_stop_reason`. Restating it
//!    compares the harness's expectation with itself. See the Bedrock impl for
//!    what it catches.
//!
//! ## Key order
//!
//! Each wire helper that builds a JSON payload uses `json!` and serializes the
//! `serde_json::Value`. (The `done()` helpers are byte literals.) The
//! `serde_json` 1.0.154 in `Cargo.lock` has no `preserve_order` feature, so
//! the keys come out in alphabetical order. That order can differ from the
//! order in the cited sources. No translator depends on key order: each one
//! decodes the payload as a JSON object.

use paigasus_helikon_core::{
    CancellationToken, ContentPart, FinishReason, Item, Model, ModelRequest, ToolChoice, ToolDef,
};
use paigasus_helikon_provider_stream_conformance::{
    assert_conforms,
    eventstream::{decode_frames, frame, frame_raw},
    Channel, Ending, ExpectedCall, Outcome, PacedServer, Scenario, Script, StreamUnderTest,
};
use serde_json::json;

/// The `Model::invoke` stream contract, checked against the Bedrock
/// `ConverseStream` provider.
///
/// # Fixture provenance
///
/// Label: the `ZeroArgToolCallCleanStop` scenario is CAPTURED. Every other
/// scenario is still HAND-AUTHORED, from the vendored `aws-sdk-bedrockruntime`
/// types and the translator's match arms. One captured stream now exists, for
/// the zero-argument scenario only. It is
/// `fixtures/bedrock/zero_arg_tool_call.raw` (see [`ZERO_ARG_EVENTS`]). No
/// helper function below is a capture.
///
/// Bedrock is the one subject in this suite whose wire format is binary. The
/// repo holds no captured Bedrock traffic for the other scenarios. So — as the
/// task brief directs when no capture exists — every event shape of those
/// scenarios is derived from two sources inside the workspace, and from
/// nothing else:
///
/// 1. **The translator's own match arms**, `providers-bedrock/src/stream.rs`.
///    Which `ConverseStreamOutput` variants matter, and what each one must
///    carry to produce a `ModelEvent`, is read off the arms that consume them:
///    `ContentBlockDelta`/`Text` to `TokenDelta`, `ContentBlockStart`/`ToolUse`
///    plus `ContentBlockDelta`/`ToolUse` to `ToolCallDelta`, `MessageStop` to
///    the buffered stop reason, `Metadata`/`usage` to `Usage` then `Finish`.
/// 2. **The vendored AWS Converse types** in `aws-sdk-bedrockruntime` 1.140.0.
///    Every JSON key below is the key that crate's own deserializer reads, and
///    every `:event-type` is the union member name its
///    `ConverseStreamOutputUnmarshaller` dispatches on
///    (`src/event_stream_serde.rs`). Per-shape references sit on each helper.
///
/// Nothing here is transcribed from vendor documentation. Each shape of the
/// hand-authored scenarios comes from those two sources. The shapes of the
/// captured scenario come from the wire. They have keys that no hand-authored
/// helper uses: `p`, `toolUse.type` and `usage.serverToolUsage`. For the
/// hand-authored scenarios: a frame the SDK's decoder does not recognise is
/// dropped silently by the translator's forward-compat catch-all rather than
/// rejected, so a shape from outside them would present as a translator bug
/// and not as a bad fixture.
///
/// The two sources give the keys and the event names. They do not give the
/// event order or the values. The one capture records the order of the events
/// for a zero-argument tool call: `metadata` comes after `messageStop`. No
/// capture records the order for the other scenarios. The numeric values in
/// `metadata()` are arbitrary.
///
/// # Why the hand-authored scripts have no `.bin` fixture files
///
/// A `vnd.amazon.eventstream` frame is a length-prefixed header block wrapped
/// in two CRC-32s, so a checked-in `.bin` would be neither reviewable nor
/// editable, and any byte edited by hand would fail its CRC rather than test
/// anything. `eventstream::frame` exists precisely to avoid that: it encodes
/// each fixture with the same encoder the SDK itself uses, so the frame bytes
/// come from the SDK's own encoder while the fixture stays readable as JSON.
/// The captured stream is the one exception. Its `.raw` file holds the wire
/// bytes. The script encodes [`ZERO_ARG_EVENTS`] with
/// `eventstream::frame_raw`. The guard test
/// `zero_arg_events_are_the_capture_frames` decodes the file and compares the
/// payload bytes.
mod bedrock {
    use super::*;

    /// The tool name every Bedrock tool fixture declares.
    const TOOL_NAME: &str = "get_weather";

    /// `toolUseId` for the single tool call in the tool fixtures.
    ///
    /// Deliberately *not* claimed to match any real Bedrock id format: the SDK
    /// types this field as a plain `String` with no format constraint
    /// (`_tool_use_block_start.rs`: `pub tool_use_id: String`), and the
    /// translator copies it verbatim into `ModelEvent::ToolCallDelta.call_id`
    /// (`stream.rs:116`), so nothing under test inspects its shape. The value
    /// is fixed and self-describing purely so failure output is stable and
    /// greppable.
    const TOOL_USE_ID: &str = "tooluse_conformance_0";

    /// The `:event-type` value carried by the one event that encodes a stop
    /// reason. Used by [`Bedrock::encodes_stop_reason`] to measure a script.
    const MESSAGE_STOP_EVENT_TYPE: &str = "messageStop";

    /// `messageStart`, which opens every Converse stream.
    ///
    /// The translator ignores it (`ConverseStreamOutput::MessageStart(_) => {}`);
    /// it is here because the script models a Converse stream that opens with
    /// `messageStart`. The zero-argument capture confirms that a real stream
    /// sends it first.
    /// Key: `role` (`shape_message_start_event.rs`), value from
    /// `ConversationRole` (`_conversation_role.rs`: `"assistant"`).
    fn message_start() -> Vec<u8> {
        frame("messageStart", &json!({ "role": "assistant" }))
    }

    /// One text delta. `stream.rs`'s `ContentBlockDelta::Text` arm turns this
    /// into `ModelEvent::TokenDelta`.
    ///
    /// Keys: `contentBlockIndex` and `delta`
    /// (`shape_content_block_delta_event.rs`), with `text` the
    /// `ContentBlockDelta` union member (`shape_content_block_delta.rs`).
    fn text_delta(text: &str) -> Vec<u8> {
        frame(
            "contentBlockDelta",
            &json!({ "contentBlockIndex": 0, "delta": { "text": text } }),
        )
    }

    /// `contentBlockStart` opening a tool-use block.
    ///
    /// The translator records the block's `call_id` and `name` here but emits
    /// nothing — the name-carrying `ToolCallDelta` comes with the *first* input
    /// delta. That is why `FragmentedToolName` is declined for this subject:
    /// the name arrives whole, in this one event.
    ///
    /// Keys: `contentBlockIndex` and `start`
    /// (`shape_content_block_start_event.rs`), `toolUse` the union member
    /// (`shape_content_block_start.rs`), and `toolUseId` / `name` inside it
    /// (`shape_tool_use_block_start.rs`).
    fn tool_use_start() -> Vec<u8> {
        frame(
            "contentBlockStart",
            &json!({
                "contentBlockIndex": 0,
                "start": { "toolUse": { "toolUseId": TOOL_USE_ID, "name": TOOL_NAME } }
            }),
        )
    }

    /// One tool-argument delta. Bedrock streams tool input as a partial JSON
    /// string, so `input` is a string fragment and not an object.
    ///
    /// Keys: `delta.toolUse` (`shape_content_block_delta.rs`) with `input`
    /// (`shape_tool_use_block_delta.rs`).
    fn tool_use_delta(input: &str) -> Vec<u8> {
        frame(
            "contentBlockDelta",
            &json!({ "contentBlockIndex": 0, "delta": { "toolUse": { "input": input } } }),
        )
    }

    /// `contentBlockStop`, which closes a content block. Ignored by the
    /// translator (`ConverseStreamOutput::ContentBlockStop(_) => {}`), included
    /// so that the script closes each block it opens. The zero-argument
    /// capture confirms that a real stream closes its block. Key: `contentBlockIndex`
    /// (`shape_content_block_stop_event.rs`).
    fn content_block_stop() -> Vec<u8> {
        frame("contentBlockStop", &json!({ "contentBlockIndex": 0 }))
    }

    /// `messageStop`, the only event carrying a stop reason.
    ///
    /// `reason` is a `StopReason` wire string (`_stop_reason.rs`): `"end_turn"`
    /// maps to `FinishReason::Stop` and `"tool_use"` to
    /// `FinishReason::ToolCalls`, per `stream.rs`'s `finish_events`. Key:
    /// `stopReason` (`shape_message_stop_event.rs`).
    fn message_stop(reason: &str) -> Vec<u8> {
        frame(MESSAGE_STOP_EVENT_TYPE, &json!({ "stopReason": reason }))
    }

    /// `metadata`, the only event carrying usage. The scripts send it **after**
    /// `messageStop`. The translator buffers the stop reason instead of
    /// emitting `Finish` on the spot so that it can handle this order. The
    /// zero-argument capture records that a real Bedrock stream sends
    /// `metadata` after `messageStop`.
    ///
    /// **This ordering is not self-enforced by the translator.**
    /// `assert_message_stop_before_metadata` enforces it for every script.
    /// `stream.rs` deliberately handles the reversed order too (`Metadata`
    /// before `MessageStop` emits `Usage` at once and `Finish` on the stop), so
    /// a fixture that put `metadata` first would still pass every other
    /// assertion — while no longer covering the buffering path, which exists
    /// for the order `messageStop` then `metadata`. Verified by mutation while
    /// registering this subject: swapping the two left the suite green.
    ///
    /// The values (11/4/15 tokens, `latencyMs` 314) are arbitrary. No capture
    /// supplies them.
    ///
    /// Keys: `usage` and `metrics` (`shape_converse_stream_metadata_event.rs`),
    /// `inputTokens` / `outputTokens` / `totalTokens` (`shape_token_usage.rs`),
    /// and `latencyMs` (`shape_converse_stream_metrics.rs`).
    fn metadata() -> Vec<u8> {
        frame(
            "metadata",
            &json!({
                "usage": { "inputTokens": 11, "outputTokens": 4, "totalTokens": 15 },
                "metrics": { "latencyMs": 314 }
            }),
        )
    }

    /// The six events of `fixtures/bedrock/zero_arg_tool_call.raw`, as
    /// (`:event-type`, payload) pairs, byte for byte, in file order. See the
    /// `ZeroArgToolCallCleanStop` arm of [`script`] for the provenance. Each
    /// payload keeps its `"p"` padding field.
    const ZERO_ARG_EVENTS: [(&str, &str); 6] = [
        (
            "messageStart",
            r##"{"p":"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXY","role":"assistant"}"##,
        ),
        (
            "contentBlockStart",
            r##"{"contentBlockIndex":0,"p":"abcde","start":{"toolUse":{"name":"get_current_time","toolUseId":"tooluse_kiuoEhT6WJIVsi1t2fq6PV","type":"tool_use"}}}"##,
        ),
        (
            "contentBlockDelta",
            r##"{"contentBlockIndex":0,"delta":{"toolUse":{"input":""}},"p":"abcdefghijklmnopqrstuvwxyzABCDEFGH"}"##,
        ),
        (
            "contentBlockStop",
            r##"{"contentBlockIndex":0,"p":"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123"}"##,
        ),
        (
            "messageStop",
            r##"{"p":"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQ","stopReason":"tool_use"}"##,
        ),
        (
            "metadata",
            r##"{"metrics":{"latencyMs":493},"p":"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWX","usage":{"inputTokens":485,"outputTokens":11,"serverToolUsage":{},"totalTokens":496}}"##,
        ),
    ];

    /// The one call of [`ZERO_ARG_EVENTS`]: the tool name and empty arguments.
    /// The wire sends one `toolUse.input` delta with the empty string.
    const ZERO_ARG_CALLS: &[ExpectedCall] = &[ExpectedCall {
        name: "get_current_time",
        args: "{}",
    }];

    /// The whole capture file, to check the script against.
    const ZERO_ARG_RAW: &[u8] = include_bytes!("../fixtures/bedrock/zero_arg_tool_call.raw");

    /// The scripted response for one scenario, or `None` for a scenario this
    /// subject declines or has no capture for (an uncaptured scenario).
    ///
    /// Each hand-authored script is the same Converse stream cut at a
    /// different point, so those scenarios differ only in what the client gets
    /// to observe — which is exactly the distinction the suite is checking.
    /// `ZeroArgToolCallCleanStop` is the exception. It is a different,
    /// captured stream.
    ///
    /// Returning an `Option` rather than panicking on the declined or uncaptured
    /// scenarios is what lets [`Bedrock::encodes_stop_reason`] call this for *every*
    /// scenario and measure the bytes.
    fn script(scenario: Scenario) -> Option<Script> {
        // The prefix every text script shares: the opening event and two
        // deltas, so a cancellation can be gated *between* two `TokenDelta`s
        // rather than before the first one.
        let opening = || vec![message_start(), text_delta("Hel"), text_delta("lo")];
        // The opening plus a completed block and a stop reason.
        let through_stop = || {
            let mut chunks = opening();
            chunks.push(content_block_stop());
            chunks.push(message_stop("end_turn"));
            chunks
        };

        let (chunks, gate_after, ending) = match scenario {
            // Full stream: deltas, stop reason, then the usage-bearing metadata
            // event after it, then a clean end of body.
            Scenario::CleanStop => {
                let mut chunks = through_stop();
                chunks.push(metadata());
                (chunks, None, Ending::Clean)
            }
            // The stop reason is observed, then the body simply ends — no
            // `metadata`, so the translator's buffered stop reason has to be
            // flushed by the EOF path in `model.rs` (`translator.finish()`).
            Scenario::TruncatedAfterStopReason => (through_stop(), None, Ending::Clean),
            // The body ends cleanly mid-generation: no `messageStop`, so no
            // stop reason is ever observed and there is nothing to flush.
            Scenario::TruncatedMidGeneration => (opening(), None, Ending::Clean),
            // Same prefix, but the connection is torn down without the
            // terminating chunk, so the SDK's receiver yields a transport error.
            Scenario::ErrorMidGeneration => (opening(), None, Ending::Abort),
            // The stop reason is buffered and *then* the body is aborted, so
            // the error path must not flush it as a `Finish`.
            Scenario::ErrorAfterStopReason => (through_stop(), None, Ending::Abort),
            // Two chunks go out — `messageStart` and the first delta — and the
            // server then parks, holding the second delta back. The harness
            // sees the stream fall quiet and cancels. No `messageStop` anywhere
            // in this script, so no stop reason exists even after the gate is
            // released.
            Scenario::CancelMidGeneration => (opening(), Some(2), Ending::Clean),
            // A whole tool call: the name arrives with the block start, the
            // arguments arrive split across two deltas (so exactly one emitted
            // `ToolCallDelta` carries the name), then a `tool_use` stop reason
            // and the usage-bearing metadata event.
            Scenario::ToolCallCleanStop => (
                vec![
                    message_start(),
                    tool_use_start(),
                    tool_use_delta("{\"city\":"),
                    tool_use_delta("\"Berlin\"}"),
                    content_block_stop(),
                    message_stop("tool_use"),
                    metadata(),
                ],
                None,
                Ending::Clean,
            ),
            // Declined — see the reasons in `Bedrock::stream`.
            Scenario::CancelAfterStopReason | Scenario::FragmentedToolName => return None,
            // Declined — the scenario serves an anthropic thinking block.
            Scenario::ThinkingToolCallCleanStop => return None,
            // Not served — see the reason in `stream`.
            Scenario::ReasoningCleanStop => return None,
            // Not served — see the reason in `stream`.
            Scenario::ParallelToolCallCleanStop => return None,
            // `fixtures/bedrock/zero_arg_tool_call.raw`, CAPTURED from the
            // Bedrock Runtime `converse-stream` endpoint (inference profile
            // `eu.anthropic.claude-haiku-5-5`, `eu-central-1`) on 2026-11-24
            // at 20:39:45 UTC, with one zero-argument tool and a forced
            // `tool_choice`. The stream has six events: `messageStart`,
            // `contentBlockStart`, one `contentBlockDelta` with
            // `toolUse.input` set to the empty string, `contentBlockStop`,
            // `messageStop` and `metadata`. `metadata` comes after
            // `messageStop`, so this capture records that order.
            //
            // Edits made here: the suite encodes the prelude, the header block
            // and the CRCs again. The header order is `:message-type`,
            // `:event-type`, `:content-type`. The capture has `:event-type`,
            // `:content-type`, `:message-type`. The payload bytes are
            // unchanged ([`ZERO_ARG_EVENTS`]).
            Scenario::ZeroArgToolCallCleanStop => (
                ZERO_ARG_EVENTS
                    .iter()
                    .map(|(event, payload)| frame_raw(event, payload.as_bytes()))
                    .collect(),
                None,
                Ending::Clean,
            ),
        };

        Some(Script {
            content_type: "application/vnd.amazon.eventstream",
            chunks,
            gate_after,
            ending,
        })
    }

    /// The request driven through `Model::invoke`.
    ///
    /// The scripted response is fixed, so the request only has to be one the
    /// provider will translate and send. It carries a tool definition for the
    /// tool scenario so the exchange is coherent — a Converse stream returning
    /// a `toolUse` block for a request that offered no tools would not be a
    /// coherent exchange.
    fn request(scenario: Scenario) -> ModelRequest {
        let mut req = ModelRequest::new();
        req.messages = vec![Item::UserMessage {
            content: vec![ContentPart::Text {
                text: "What is the weather in Berlin?".into(),
            }],
        }];
        if scenario == Scenario::ZeroArgToolCallCleanStop {
            // The tool and the forced choice of the capture.
            req.tools = vec![ToolDef::new(
                "get_current_time",
                "Return the current server time. Takes no arguments.",
                json!({ "type": "object", "properties": {} }),
            )];
            req.model_settings.tool_choice = Some(ToolChoice::Tool {
                name: "get_current_time".into(),
            });
        }
        // `ToolCallCleanStop` only: `FragmentedToolName` is the subject's other
        // tool scenario, but it is declined and so never reaches this function.
        if matches!(scenario, Scenario::ToolCallCleanStop) {
            req.tools = vec![ToolDef::new(
                TOOL_NAME,
                "Look up the current weather for a city.",
                json!({
                    "type": "object",
                    "properties": { "city": { "type": "string" } },
                    "required": ["city"]
                }),
            )];
        }
        req
    }

    /// Build a real `BedrockModel` pointed at a local plain-HTTP endpoint.
    ///
    /// **Kept in sync by hand with `eventstream::build_bedrock_model_against`
    /// in `src/eventstream.rs`.** That copy is `#[cfg(test)]`, so it belongs to
    /// the lib's unit-test build only and is unreachable from an integration
    /// test; making it `pub` instead would pull the whole AWS SDK into the
    /// lib's dependency graph for every subject in this suite. The duplication
    /// has one consequence worth knowing: the SigV4 assertion in
    /// `bedrock_signs_the_local_request_with_sigv4` exercises the *lib* copy,
    /// not this one, so a region or credential-scope change made here alone
    /// would go unasserted. Change both, or neither.
    ///
    /// The `SdkConfig` is assembled by hand rather than through
    /// `aws_config::defaults(..).load()` because that loader is `async` and
    /// would also consult the ambient environment — a developer's real
    /// `AWS_ENDPOINT_URL` or credential profile must not be able to change what
    /// this suite talks to.
    fn build_model_against(base_url: &str) -> paigasus_helikon_providers_bedrock::BedrockModel {
        use aws_sdk_bedrockruntime::config::{Credentials, SharedCredentialsProvider};

        let sdk_config = aws_config::SdkConfig::builder()
            // Pinned, matching `BedrockModel::from_env`, so a Dependabot
            // `aws-config` bump cannot silently shift SDK behaviour under the
            // suite.
            .behavior_version(aws_config::BehaviorVersion::v2026_11_17())
            .region(aws_config::Region::new("us-east-1"))
            .endpoint_url(base_url)
            .credentials_provider(SharedCredentialsProvider::new(Credentials::new(
                "AKIDTEST",
                "secret",
                None,
                None,
                "conformance",
            )))
            .build();

        paigasus_helikon_providers_bedrock::BedrockModel::converse("anthropic.claude-sonnet-4-6")
            .sdk_config(&sdk_config)
            .build()
            .expect("bedrock model should build from a static SdkConfig")
    }

    /// See the module-level docs on this file for the fixture provenance rules
    /// this subject follows.
    struct Bedrock;

    #[async_trait::async_trait]
    impl StreamUnderTest for Bedrock {
        fn name(&self) -> &'static str {
            "bedrock"
        }

        fn encodes_stop_reason(&self, scenario: Scenario) -> bool {
            // MEASURED from the bytes about to be served — deliberately not
            // `scenario.expects_stop_reason()`, which would restate the
            // harness's own expectation and make the cross-check dead code.
            //
            // It has to measure something, because one scenario is otherwise
            // undetectable if its fixture drifts: `ErrorAfterStopReason`'s
            // observable events are `[TokenDelta, TokenDelta, Err]`, byte-for-
            // byte identical to `ErrorMidGeneration`'s. If `through_stop()`
            // ever lost its `messageStop`, that scenario would silently degrade
            // into its sibling with every check in the suite still passing —
            // assertion 3 skips itself when an `Err` is present, and assertion 6
            // holds either way. This is the only guard that notices.
            //
            // A substring scan is sound here. In a `vnd.amazon.eventstream`
            // frame the `:event-type` header value is stored as raw ASCII, so
            // `"messageStop"` appears literally in any frame that carries it.
            // It cannot appear anywhere else in these fixtures: no other event
            // name this subject emits contains it (`messageStart` and
            // `contentBlockStop` are the near misses, and neither does), and no
            // payload above contains the text. `scan_finds_only_the_stop_event`
            // is the test that keeps that true.
            script(scenario)
                .map(|script| {
                    script
                        .chunks
                        .iter()
                        .any(|chunk| contains(chunk, MESSAGE_STOP_EVENT_TYPE.as_bytes()))
                })
                .unwrap_or(false)
        }

        fn fixture_tool_name(&self) -> &'static str {
            TOOL_NAME
        }

        fn fixture_finish_reason(&self, scenario: Scenario) -> Option<FinishReason> {
            // Exhaustive on purpose, with no `_` arm: a newly added `Scenario`
            // must break this match at the same time it breaks `script`'s, so
            // the two cannot drift. A catch-all here would silently declare
            // `None` for the new scenario while the build failed elsewhere.
            match scenario {
                // `"end_turn"` maps to `StopReason::EndTurn` and then to
                // `FinishReason::Stop` (`stream.rs`'s `finish_events`). Both of
                // these scenarios reach a `Finish`: `CleanStop` through the
                // `Metadata` path, `TruncatedAfterStopReason` through the EOF
                // flush.
                Scenario::CleanStop | Scenario::TruncatedAfterStopReason => {
                    Some(FinishReason::Stop)
                }
                // `"tool_use"` maps to `StopReason::ToolUse` and then to
                // `FinishReason::ToolCalls`, since this request is not a
                // structured-output synthesis.
                // The zero-argument capture's `messageStop` carries `"tool_use"`.
                Scenario::ToolCallCleanStop | Scenario::ZeroArgToolCallCleanStop => {
                    Some(FinishReason::ToolCalls)
                }
                // Truncated, errored and cancelled streams must withhold
                // `Finish` entirely.
                Scenario::TruncatedMidGeneration
                | Scenario::ErrorMidGeneration
                | Scenario::ErrorAfterStopReason
                | Scenario::CancelMidGeneration => None,
                // Declined; never reached.
                Scenario::CancelAfterStopReason
                | Scenario::FragmentedToolName
                | Scenario::ThinkingToolCallCleanStop
                | Scenario::ReasoningCleanStop
                | Scenario::ParallelToolCallCleanStop => None,
            }
        }

        fn fixture_channel_order(&self, _scenario: Scenario) -> Option<&'static [Channel]> {
            // This subject serves no scenario whose channel order the harness
            // checks, so it makes no claim.
            None
        }

        fn fixture_calls(&self, scenario: Scenario) -> &'static [ExpectedCall] {
            match scenario {
                // The one call of the zero-argument capture.
                Scenario::ZeroArgToolCallCleanStop => ZERO_ARG_CALLS,
                // No other scenario this subject serves declares calls.
                _ => &[],
            }
        }

        async fn stream(&self, scenario: Scenario, cancel: CancellationToken) -> Outcome {
            match scenario {
                // The name is set once, whole, in the `contentBlockStart`
                // toolUse block; there is no second event it could be split
                // across.
                Scenario::FragmentedToolName => {
                    return Outcome::Declined("name arrives whole in toolUse start")
                }
                // The translator buffers the stop reason at `MessageStop` and
                // emits nothing until `Metadata`, so between "stop reason
                // observed" and "Finish emitted" the stream is silent. With no
                // event in that window there is no edge to gate on, and any
                // gate placed there would degrade into `CancelMidGeneration`.
                Scenario::CancelAfterStopReason => {
                    return Outcome::Declined(
                        "no observable event between MessageStop and Metadata",
                    )
                }
                Scenario::ThinkingToolCallCleanStop => {
                    return Outcome::Declined("the scenario serves an anthropic thinking block")
                }
                // A reasoning stream can occur on this wire, but no capture
                // of one exists (SMA-858).
                Scenario::ReasoningCleanStop => {
                    return Outcome::Uncaptured("no captured reasoning stream")
                }
                // Parallel tool calls can occur on this wire, but no capture
                // of them exists (SMA-859).
                Scenario::ParallelToolCallCleanStop => {
                    return Outcome::Uncaptured("no captured parallel tool-call stream")
                }
                _ => {}
            }

            let script =
                script(scenario).expect("a scenario this subject serves must have a script");
            let mut server = PacedServer::start(script).await;
            let gate = server.take_gate();
            let model = build_model_against(&server.base_url());

            let stream = model
                .invoke(request(scenario), cancel)
                .await
                .expect("bedrock invoke should reach the local paced server");

            Outcome::Served { stream, gate }
        }
    }

    /// The `:event-type` of each frame, in order.
    ///
    /// Each chunk is decoded with the SDK's own frame decoder, so the check
    /// reads what the client reads. A chunk that does not decode as one
    /// complete frame panics.
    fn event_types(chunks: &[Vec<u8>]) -> Vec<String> {
        use aws_smithy_eventstream::frame::{DecodedFrame, MessageFrameDecoder};

        chunks
            .iter()
            .map(|chunk| {
                let mut decoder = MessageFrameDecoder::new();
                let message = match decoder
                    .decode_frame(chunk.as_slice())
                    .expect("a frame this module wrote must decode")
                {
                    DecodedFrame::Complete(message) => message,
                    DecodedFrame::Incomplete => panic!("each chunk must be one complete frame"),
                };
                message
                    .headers()
                    .iter()
                    .find(|h| h.name().as_str() == ":event-type")
                    .expect("every frame carries an :event-type header")
                    .value()
                    .as_string()
                    .expect(":event-type is a string header")
                    .as_str()
                    .to_owned()
            })
            .collect()
    }

    /// Panic when a `metadata` frame comes before a `messageStop` frame.
    ///
    /// Bedrock sends `messageStop` first. `stream.rs` accepts both orders, so
    /// no other assertion notices a fixture in the wrong order.
    fn assert_message_stop_before_metadata(chunks: &[Vec<u8>]) {
        let types = event_types(chunks);
        let stop = types.iter().position(|t| t == MESSAGE_STOP_EVENT_TYPE);
        let meta = types.iter().position(|t| t == "metadata");
        if let (Some(stop), Some(meta)) = (stop, meta) {
            assert!(
                meta > stop,
                "metadata before messageStop: metadata at {meta}, messageStop at {stop} in {types:?}"
            );
        }
    }

    /// Whether `haystack` contains `needle` as a contiguous byte run.
    ///
    /// `[u8]` has no `contains(&[u8])`, and pulling in a dependency for one
    /// window scan over a fixture measured in hundreds of bytes is not worth it.
    fn contains(haystack: &[u8], needle: &[u8]) -> bool {
        haystack.windows(needle.len()).any(|w| w == needle)
    }

    /// The soundness claim behind [`Bedrock::encodes_stop_reason`]'s substring
    /// scan: `messageStop` occurs in the `messageStop` frame and in no other
    /// frame this subject builds.
    ///
    /// Without this, a future helper whose event name or payload happened to
    /// contain the marker would turn `encodes_stop_reason` into a function that
    /// answers `true` for everything — which is the same dead cross-check the
    /// method was rewritten to avoid, only harder to spot.
    #[test]
    fn scan_finds_only_the_stop_event() {
        let marker = MESSAGE_STOP_EVENT_TYPE.as_bytes();

        assert!(
            contains(&message_stop("end_turn"), marker),
            "the messageStop frame must carry the marker, or the scan measures nothing"
        );

        for (name, frame) in [
            ("messageStart", message_start()),
            ("contentBlockDelta/text", text_delta("Hel")),
            ("contentBlockStart/toolUse", tool_use_start()),
            ("contentBlockDelta/toolUse", tool_use_delta("{\"city\":")),
            ("contentBlockStop", content_block_stop()),
            ("metadata", metadata()),
            (
                "zero-arg messageStart",
                frame_raw(ZERO_ARG_EVENTS[0].0, ZERO_ARG_EVENTS[0].1.as_bytes()),
            ),
            (
                "zero-arg contentBlockStart",
                frame_raw(ZERO_ARG_EVENTS[1].0, ZERO_ARG_EVENTS[1].1.as_bytes()),
            ),
            (
                "zero-arg contentBlockDelta",
                frame_raw(ZERO_ARG_EVENTS[2].0, ZERO_ARG_EVENTS[2].1.as_bytes()),
            ),
            (
                "zero-arg contentBlockStop",
                frame_raw(ZERO_ARG_EVENTS[3].0, ZERO_ARG_EVENTS[3].1.as_bytes()),
            ),
            (
                "zero-arg metadata",
                frame_raw(ZERO_ARG_EVENTS[5].0, ZERO_ARG_EVENTS[5].1.as_bytes()),
            ),
        ] {
            assert!(
                !contains(&frame, marker),
                "{name} contains {MESSAGE_STOP_EVENT_TYPE:?}, so the scan would report a stop \
                 reason for scripts that encode none"
            );
        }
    }

    /// The check must fail on the reversed order, or it guards nothing.
    #[test]
    #[should_panic(expected = "metadata before messageStop")]
    fn a_swapped_metadata_and_message_stop_is_caught() {
        let mut chunks = vec![message_stop("end_turn"), metadata()];
        chunks.reverse();
        assert_message_stop_before_metadata(&chunks);
    }

    /// No Bedrock script may put `metadata` before `messageStop`.
    #[test]
    fn every_bedrock_script_keeps_message_stop_before_metadata() {
        let mut checked = Vec::new();
        for &scenario in Scenario::ALL {
            let Some(script) = script(scenario) else {
                continue;
            };
            let types = event_types(&script.chunks);
            if types.iter().any(|t| t == MESSAGE_STOP_EVENT_TYPE)
                && types.iter().any(|t| t == "metadata")
            {
                assert_message_stop_before_metadata(&script.chunks);
                checked.push(scenario);
            }
        }
        assert!(
            checked.len() >= 2,
            "expected at least two scripts with both frames, checked {checked:?}"
        );
        assert!(checked.contains(&Scenario::CleanStop), "{checked:?}");
        assert!(
            checked.contains(&Scenario::ToolCallCleanStop),
            "{checked:?}"
        );
    }

    /// The script is the capture: the same events, with the same payload
    /// bytes.
    ///
    /// Provenance: CAPTURED from the Bedrock Runtime endpoint
    /// `https://bedrock-runtime.eu-central-1.amazonaws.com/model/eu.anthropic.claude-haiku-5-5/converse-stream`
    /// with `curl --aws-sigv4`. The model is the inference profile
    /// `eu.anthropic.claude-haiku-5-5`. The capture ran on 2026-11-24 at
    /// 20:39:45 UTC. The request is
    /// `tests/provider-stream-conformance/fixtures/bedrock/zero_arg_tool_call.request.json`.
    /// The file is `fixtures/bedrock/zero_arg_tool_call.raw`. It
    /// is 1250 bytes. Its SHA-256 is
    /// `9196618e83783e261b5a3997d81a8cb343ff58bb1c560bff611a53c0d6a98b04`.
    /// Finding: the tool call has one `toolUse.input` delta with the empty
    /// string `""`.
    ///
    /// The suite encodes the frames again, so the test compares the decoded
    /// events (type and payload bytes) and not the raw bytes.
    ///
    /// Removed: nothing. Added: nothing. Changed: nothing.
    #[test]
    fn zero_arg_events_are_the_capture_frames() {
        let captured = decode_frames(ZERO_ARG_RAW);
        let copied: Vec<(String, Vec<u8>)> = ZERO_ARG_EVENTS
            .iter()
            .map(|(e, p)| ((*e).to_owned(), p.as_bytes().to_vec()))
            .collect();
        assert_eq!(captured.len(), 6);
        assert_eq!(copied, captured);

        let script = script(Scenario::ZeroArgToolCallCleanStop).expect("served");
        assert_eq!(decode_frames(&script.chunks.concat()), captured);
    }

    #[tokio::test]
    async fn conforms() {
        assert_conforms(&Bedrock).await;
    }
}

/// The `Model::invoke` stream contract, checked against the OpenAI Chat
/// Completions backend.
///
/// This is the subject whose historical defect motivated the whole suite:
/// SMA-522 (PR #197), where this exact translator emitted `Finish` before
/// `Usage` on every streaming turn. It went undetected because the fixtures
/// in place at the time put `usage` *before* `finish_reason`. The SMA-522
/// LiteLLM proxy capture (`chat_text_usage_trailing.txt`) shows `usage`
/// after `finish_reason`. `CleanStop` below places `usage` after the
/// `finish_reason` chunk for exactly that reason.
///
/// # Fixture provenance
///
/// Every envelope shape below is transcribed from a committed fixture in this
/// workspace, cited on the helper that builds it. Each source is either a
/// capture through a LiteLLM proxy, a hand-authored file, or the recorded
/// `api.openai.com` Chat Completions stream for the zero-argument call
/// (SMA-860, `fixtures/openai_chat/zero_arg_tool_call.raw`).
///
/// - `crates/paigasus-helikon-providers-openai/tests/fixtures/chat_text_usage_trailing.txt`
///   — CAPTURED with named reconstructed parts, through a LiteLLM proxy with
///   a keyless `mock_response` (SMA-522, PR #197): content deltas, the
///   `finish_reason` chunk, the trailing `usage` chunk, `[DONE]`.
/// - `crates/paigasus-helikon-providers-openai/tests/chat_wire.rs`'s
///   `happy_path_text_completion` fixture body — HAND-AUTHORED. It is the
///   source of the explicit `"finish_reason":null` that [`text_delta`] sends
///   on each content delta. The recorded `api.openai.com` capture
///   (SMA-860) sends that key on its tool-call chunks. It has no text
///   content delta. The LiteLLM captures omit it. The null is exactly the shape
///   [`OpenAiChat::encodes_stop_reason`]'s marker must not match.
/// - `crates/paigasus-helikon-providers-openai/tests/fixtures/chat_parallel_tool_calls.txt`
///   — HAND-AUTHORED (SMA-316, PR #30). The tool-call envelope: `id`/`name`
///   arriving with empty `arguments`, followed by args-only continuations
///   with no `name` key.
/// - `crates/paigasus-helikon-providers-litellm/tests/fixtures/tool_call_stream_fragmented_name.txt`
///   — CAPTURED from LiteLLM 1.98.0 over a hand-written fake upstream
///   (SMA-547, PR #204). Only the LiteLLM serialization is captured; the ids,
///   the name split and the argument split are authored input. It holds a
///   name split across two deltas that BOTH arrive after the `id`. Per the
///   design spec's §6 provenance note, this is the capture
///   `FragmentedToolName`'s shape must be drawn from — the note requires the
///   committed **shape**, not byte-identical values; see
///   "`FragmentedToolName` deliberately never completes" below for exactly
///   what this module substitutes (the "id resolves late" variant has no
///   capture anywhere in the repo and is not built here).
///
/// `chat_content_filter.txt` and `chat_text_usage_trailing_empty_choices.txt`
/// (both HAND-AUTHORED) were read but contribute no shape this module needs:
/// this subject serves only `Stop` and `ToolCalls` finish reasons, and the
/// empty-`choices` usage variant is a second encoding of the same
/// trailing-usage envelope already covered by `chat_text_usage_trailing.txt`.
///
/// # `FragmentedToolName` deliberately never completes
///
/// The task brief's scenario table describes `FragmentedToolName` as ending
/// with `finish_reason: "tool_calls"` → `usage` → `[DONE]`, mirroring the
/// litellm capture's full trace. That shape was tried first and reverted: the
/// design spec's §6 table asserts only **7, 1** for this scenario (`S6`), not
/// 3/4, and `Scenario::expects_stop_reason` (`src/lib.rs`) excludes
/// `FragmentedToolName` from the set of scenarios that may observe a stop
/// reason. Completing the stream — which the real translator does, buffering
/// `finish_reason` and flushing it as a genuine `Finish` at end-of-stream —
/// makes `encodes_stop_reason` measure `true` against an `expects_stop_reason`
/// of `false`, failing on `StopReasonDeclarationMismatch` before the stream is
/// even drained. So this subject's `FragmentedToolName` script draws its
/// event sequence and key structure from that capture's first three frames —
/// not a byte-identical copy: `id`/`created`/`model` are this module's fixed
/// constants (`RESPONSE_ID`/`MODEL_ID`) rather than the capture's literal
/// values, `TOOL_CALL_ID` stands in for the capture's literal tool-call id,
/// and the first frame omits the capture's `"role":"assistant"` key
/// (`ChatTranslator` does not read it) — see the `FragmentedToolName` match
/// arm in `script`, below, for the exact substitutions. The body ends
/// cleanly right after those three frames — no `finish_reason`, no `usage`,
/// no `[DONE]`. Assertion 7 (exactly one name-bearing delta per `call_id`) is
/// fully exercised by that prefix alone; nothing here shortens what the
/// scenario tests, only when its script stops.
///
/// # SMA-566's alias regression coverage lives in the crate's own unit tests
///
/// Every tool-call delta this module scripts carries an explicit `"index":0`,
/// and no delta ever introduces a second `id` — the continuation frames
/// (`tool_call_name_fragment`, `tool_call_args`) omit the key entirely. Only
/// one index value is ever in play, so the two-index-one-id shape that
/// `ChatTranslator::canonicalize` exists to merge never arises from these
/// bytes, and assertion 7 passes here without exercising it. Under
/// this suite's fixture-provenance rule the shape has no capture anywhere in
/// the repo and must not be invented, so it is not registered as a scenario —
/// exactly as `litellm` records for its own `canonicalize`.
///
/// Read this subject's `conforms` test as confirming the translator behaves
/// correctly on the wire shapes this module scripts from the sources above,
/// **not** as a standing regression guard for the alias fix. That guard is
/// `two_indexes_with_one_id_merge_into_a_single_call` and its eleven siblings
/// in `crates/paigasus-helikon-providers-openai/src/backend/chat.rs`. If a
/// future capture ever shows a backend reusing one `id` across two indexes,
/// that would be the fixture to add here; none currently committed does.
mod openai_chat {
    use super::*;
    use paigasus_helikon_providers_openai::OpenAiModel;

    /// `id` on every scripted chunk. Not claimed to match any real OpenAI id
    /// format — nothing under test inspects it — fixed purely so failure
    /// output is stable and greppable.
    const RESPONSE_ID: &str = "chatcmpl-conformance";

    /// `model` on every scripted chunk, and the model id `build_model_against`
    /// requests. Matches the identifier the task brief specifies.
    const MODEL_ID: &str = "gpt-4o-mini";

    /// The tool name every tool-call fixture declares.
    const TOOL_NAME: &str = "get_weather";

    /// `id` for the single tool call in the tool fixtures. Fixed for the same
    /// reason as [`RESPONSE_ID`].
    const TOOL_CALL_ID: &str = "call_conformance_0";

    /// The substring that appears in a Chat Completions chunk if and only if
    /// that chunk carries a *populated* `finish_reason`.
    ///
    /// The obvious marker — `finish_reason` alone — is wrong for this
    /// subject: [`text_delta`] sends `"finish_reason":null` on every content
    /// delta (no recorded capture does; see its doc), so a bare substring
    /// scan would report a stop reason for scripts that encode none. Scanning
    /// for the opening quote of a *string* value excludes the null case:
    /// `null` has no quote after the colon, `"stop"`/`"tool_calls"` do.
    /// `scan_finds_only_a_populated_finish_reason` is the guard that keeps
    /// this true as helpers are added or edited.
    const FINISH_REASON_MARKER: &[u8] = b"\"finish_reason\":\"";

    /// One SSE frame: `data: {payload}\n\n`. `serde_json::Value`'s `Display`
    /// impl serialises compactly (no extra whitespace), which is what keeps
    /// [`FINISH_REASON_MARKER`] a literal substring of the bytes on the wire.
    fn frame(payload: serde_json::Value) -> Vec<u8> {
        format!("data: {payload}\n\n").into_bytes()
    }

    /// One text delta.
    ///
    /// Envelope shape (`id`/`object`/`created`/`model`/`choices[].index`/
    /// `choices[].delta.content`) matches the content-delta chunks in
    /// `chat_text_usage_trailing.txt`. The explicit `"finish_reason":null`
    /// comes from `tests/chat_wire.rs`'s `happy_path_text_completion` body,
    /// which is HAND-AUTHORED. The recorded `api.openai.com` capture
    /// (SMA-860) sends this key on its tool-call chunks. It has no text
    /// content delta. `chat_text_usage_trailing.txt` and the LiteLLM
    /// captures omit it.
    /// The null stays on purpose. It is the frame
    /// `scan_finds_only_a_populated_finish_reason` uses to prove the marker
    /// does not fire on a null value.
    ///
    /// Changed from the capture: the first delta drops `"role":"assistant"`.
    fn text_delta(text: &str) -> Vec<u8> {
        frame(json!({
            "id": RESPONSE_ID,
            "object": "chat.completion.chunk",
            "created": 1,
            "model": MODEL_ID,
            "choices": [{
                "index": 0,
                "delta": { "content": text },
                "finish_reason": null
            }]
        }))
    }

    /// The chunk carrying a populated `finish_reason`. Matches
    /// `chat_text_usage_trailing.txt`'s finish chunk
    /// (`"delta":{},"finish_reason":"stop"`).
    ///
    /// `ChatTranslator` buffers this and does **not** emit `Finish` inline —
    /// it is released only by `finish()` at end-of-stream
    /// (`backend/chat.rs`'s `consume`/`finish` doc comments) — which is the
    /// whole reason `CleanStop` must place [`usage_chunk`] *after* this one:
    /// that ordering is the exact shape SMA-522 got wrong.
    fn finish_chunk(reason: &str) -> Vec<u8> {
        frame(json!({
            "id": RESPONSE_ID,
            "object": "chat.completion.chunk",
            "created": 1,
            "model": MODEL_ID,
            "choices": [{ "index": 0, "delta": {}, "finish_reason": reason }]
        }))
    }

    /// The trailing usage-only chunk. Matches `chat_text_usage_trailing.txt`'s
    /// usage chunk's shape: an empty `delta`, no `finish_reason` key at all,
    /// and a top-level `usage` object.
    ///
    /// Changed from the capture: drops `completion_tokens_details`.
    fn usage_chunk() -> Vec<u8> {
        frame(json!({
            "id": RESPONSE_ID,
            "object": "chat.completion.chunk",
            "created": 1,
            "model": MODEL_ID,
            "choices": [{ "index": 0, "delta": {} }],
            "usage": { "prompt_tokens": 8, "completion_tokens": 6, "total_tokens": 14 }
        }))
    }

    /// The stream terminator every committed fixture ends with.
    /// `async-openai`'s `create_stream` consumes `[DONE]` internally and ends
    /// iteration on it (`backend/chat.rs`'s `invoke` doc comment), so it never
    /// reaches the translator as an event — it only has to be present for a
    /// script to be the stream it claims to be.
    fn done() -> Vec<u8> {
        b"data: [DONE]\n\n".to_vec()
    }

    /// `id` plus a name fragment, empty `arguments`. Matches
    /// `tool_call_stream_fragmented_name.txt`'s first tool-call line
    /// (`"id":"call_abc","function":{"arguments":"","name":"get_"}`).
    fn tool_call_start(id: &str, name_frag: &str) -> Vec<u8> {
        frame(json!({
            "id": RESPONSE_ID,
            "object": "chat.completion.chunk",
            "created": 1,
            "model": MODEL_ID,
            "choices": [{
                "index": 0,
                "delta": {
                    "tool_calls": [{
                        "index": 0,
                        "id": id,
                        "type": "function",
                        "function": { "name": name_frag, "arguments": "" }
                    }]
                }
            }]
        }))
    }

    /// A later name fragment, no `id` key. Matches
    /// `tool_call_stream_fragmented_name.txt`'s second tool-call line
    /// (`"function":{"arguments":"","name":"weather"}`, no `id`) — the shape
    /// that proves both fragments arrive AFTER the id, the SMA-547 defect
    /// this capture exists to demonstrate.
    fn tool_call_name_fragment(name_frag: &str) -> Vec<u8> {
        frame(json!({
            "id": RESPONSE_ID,
            "object": "chat.completion.chunk",
            "created": 1,
            "model": MODEL_ID,
            "choices": [{
                "index": 0,
                "delta": {
                    "tool_calls": [{
                        "index": 0,
                        "type": "function",
                        "function": { "name": name_frag, "arguments": "" }
                    }]
                }
            }]
        }))
    }

    /// An args-only continuation: no `id`, no `name` key at all. Matches
    /// `tool_call_stream_fragmented_name.txt`'s third tool-call line and
    /// `chat_parallel_tool_calls.txt`'s argument-continuation lines.
    fn tool_call_args(args_frag: &str) -> Vec<u8> {
        frame(json!({
            "id": RESPONSE_ID,
            "object": "chat.completion.chunk",
            "created": 1,
            "model": MODEL_ID,
            "choices": [{
                "index": 0,
                "delta": {
                    "tool_calls": [{
                        "index": 0,
                        "type": "function",
                        "function": { "arguments": args_frag }
                    }]
                }
            }]
        }))
    }

    /// The five `data:` values of `fixtures/openai_chat/zero_arg_tool_call.raw`,
    /// byte for byte, in file order. The last is `[DONE]`. See the
    /// `ZeroArgToolCallCleanStop` arm of [`script`] for the provenance.
    const ZERO_ARG_CHUNKS: [&str; 5] = [
        r##"{"id":"chatcmpl-EnraJW4db12yTP2Ifkbz1kycatHzr","object":"chat.completion.chunk","created":1795552759,"model":"gpt-5-mini-2025-08-07","service_tier":"default","system_fingerprint":null,"choices":[{"index":0,"delta":{"role":"assistant","content":null,"tool_calls":[{"index":0,"id":"call_XHHS5dpxrpSbtNSmMIlKra2i","type":"function","function":{"name":"get_current_time","arguments":""}}],"refusal":null},"finish_reason":null}],"usage":null,"obfuscation":"ljR0UWZNjpG"}"##,
        r##"{"id":"chatcmpl-EnraJW4db12yTP2Ifkbz1kycatHzr","object":"chat.completion.chunk","created":1795552759,"model":"gpt-5-mini-2025-08-07","service_tier":"default","system_fingerprint":null,"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{}"}}]},"finish_reason":null}],"usage":null,"obfuscation":"e4rivpQ9NuI"}"##,
        r##"{"id":"chatcmpl-EnraJW4db12yTP2Ifkbz1kycatHzr","object":"chat.completion.chunk","created":1795552759,"model":"gpt-5-mini-2025-08-07","service_tier":"default","system_fingerprint":null,"choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}],"usage":null,"obfuscation":"2HaYtdPdrxy"}"##,
        r##"{"id":"chatcmpl-EnraJW4db12yTP2Ifkbz1kycatHzr","object":"chat.completion.chunk","created":1795552759,"model":"gpt-5-mini-2025-08-07","service_tier":"default","system_fingerprint":null,"choices":[],"usage":{"prompt_tokens":128,"completion_tokens":148,"total_tokens":276,"prompt_tokens_details":{"cached_tokens":0,"audio_tokens":0},"completion_tokens_details":{"reasoning_tokens":128,"audio_tokens":0,"accepted_prediction_tokens":0,"rejected_prediction_tokens":0}},"obfuscation":"T"}"##,
        r##"[DONE]"##,
    ];

    /// The one call of [`ZERO_ARG_CHUNKS`]: the tool name and the literal `{}`
    /// that the API streams as its arguments.
    const ZERO_ARG_CALLS: &[ExpectedCall] = &[ExpectedCall {
        name: "get_current_time",
        args: "{}",
    }];

    /// The whole capture file, to check the script against.
    const ZERO_ARG_RAW: &[u8] = include_bytes!("../fixtures/openai_chat/zero_arg_tool_call.raw");

    /// One captured SSE frame: `data: {payload}\n\n`.
    fn captured_frame(payload: &str) -> Vec<u8> {
        format!("data: {payload}\n\n").into_bytes()
    }

    /// The scripted response for one scenario.
    ///
    /// Returns `None` for `ThinkingToolCallCleanStop`, which this subject
    /// declines, like `bedrock::script`, and for the scenarios that it has no
    /// capture for (uncaptured).
    fn script(scenario: Scenario) -> Option<Script> {
        // The two-delta opening every text scenario shares. The content is
        // arbitrary (as in `bedrock::opening`); the envelope is what is
        // grounded — see `text_delta`.
        let opening = || vec![text_delta("Hel"), text_delta("lo")];
        // The opening plus a populated finish_reason chunk.
        let through_stop = || {
            let mut chunks = opening();
            chunks.push(finish_chunk("stop"));
            chunks
        };

        let (chunks, gate_after, ending) = match scenario {
            // Full stream: deltas, stop reason, then usage AFTER it, then
            // [DONE] and a clean end of body. Usage-after-finish_reason is
            // the order the SMA-522 LiteLLM proxy capture shows
            // (`chat_text_usage_trailing.txt`) and the exact shape SMA-522
            // got wrong — see the module doc.
            Scenario::CleanStop => {
                let mut chunks = through_stop();
                chunks.push(usage_chunk());
                chunks.push(done());
                (chunks, None, Ending::Clean)
            }
            // The stop reason is observed, then the body simply ends — no
            // usage, no [DONE] — so the translator's buffered stop reason has
            // to be flushed by the EOF path (`ChatTranslator::finish`).
            Scenario::TruncatedAfterStopReason => (through_stop(), None, Ending::Clean),
            // The body ends cleanly mid-generation: no finish_reason chunk
            // ever arrives, so no stop reason is ever observed.
            Scenario::TruncatedMidGeneration => (opening(), None, Ending::Clean),
            // Same prefix, but the connection is torn down without a
            // terminating chunk, so the client observes a transport error.
            Scenario::ErrorMidGeneration => (opening(), None, Ending::Abort),
            // The stop reason is buffered and *then* the body is aborted, so
            // the error path must not flush it as a Finish.
            Scenario::ErrorAfterStopReason => (through_stop(), None, Ending::Abort),
            // One content delta goes out, then the server parks holding the
            // second back (`gate_after: Some(1)` pauses *before* sending the
            // chunk at index 1, per `server.rs`'s `feed` loop) — no
            // finish_reason chunk anywhere in this script, so no stop reason
            // exists even after the gate releases.
            Scenario::CancelMidGeneration => {
                let chunks = opening();
                (chunks, Some(1), Ending::Clean)
            }
            // The stop reason AND the usage chunk after it are both observed
            // before the gate. `usage` is the harness's required edge here:
            // `ChatTranslator` buffers `finish_reason` silently
            // (`backend/chat.rs`'s `consume` doc comment), so the stop-reason
            // chunk itself is not an observable event — usage is the first
            // thing the client sees after it, and the floor in
            // `assert_conforms` requires at least one `Usage` for this exact
            // reason (see its `CancelAfterStopReason` comment).
            Scenario::CancelAfterStopReason => {
                let mut chunks = through_stop();
                chunks.push(usage_chunk());
                let gate_after = chunks.len();
                (chunks, Some(gate_after), Ending::Clean)
            }
            // The committed capture's event sequence and key structure for
            // its first three frames, not a byte-identical copy: `id`/
            // `created`/`model` are this module's fixed constants
            // (`RESPONSE_ID`/`MODEL_ID`) rather than the capture's
            // `"chatcmpl-fake"`/`1786999074`/`"shape-fragment"` values,
            // `TOOL_CALL_ID` stands in for the capture's literal
            // `"call_abc"`, and [`tool_call_start`]'s frame omits the
            // capture's `"role":"assistant"` key — `ChatTranslator` does not
            // read it, the same reason every other omitted-but-unread field
            // in this module is dropped. See the module doc's
            // "FragmentedToolName deliberately never completes" section for
            // why the body ends right here rather than continuing on to the
            // capture's finish_reason/usage/[DONE] tail.
            Scenario::FragmentedToolName => {
                let chunks = vec![
                    tool_call_start(TOOL_CALL_ID, "get_"),
                    tool_call_name_fragment("weather"),
                    tool_call_args("{\"city\":\"Berlin\"}"),
                ];
                (chunks, None, Ending::Clean)
            }
            // A whole tool call: the name arrives with the first delta
            // (empty args), the arguments arrive on the next delta (so
            // exactly one emitted `ToolCallDelta` carries the name), then a
            // tool_calls stop reason and the usage-bearing trailing chunk.
            Scenario::ToolCallCleanStop => {
                let chunks = vec![
                    tool_call_start(TOOL_CALL_ID, TOOL_NAME),
                    tool_call_args("{\"city\":\"Berlin\"}"),
                    finish_chunk("tool_calls"),
                    usage_chunk(),
                    done(),
                ];
                (chunks, None, Ending::Clean)
            }
            // Declined — the scenario serves an anthropic thinking block.
            Scenario::ThinkingToolCallCleanStop => return None,
            // Not served — see the reason in `stream`.
            Scenario::ReasoningCleanStop => return None,
            // Not served — see the reason in `stream`.
            Scenario::ParallelToolCallCleanStop => return None,
            // `fixtures/openai_chat/zero_arg_tool_call.raw`, CAPTURED against
            // the real OpenAI Chat Completions API (`gpt-5-mini`, wire model
            // `gpt-5-mini-2025-08-07`, 2026-11-24 20:39:21 UTC) with one
            // zero-argument tool and a forced `tool_choice` (request:
            // `fixtures/openai_chat/zero_arg_tool_call.request.json`). The API
            // streams the arguments as `""` and then as the literal `{}`.
            // Five frames: the tool-call start, the `{}` delta, the finish
            // chunk, the usage chunk and `[DONE]`.
            //
            // Edits made here: none to any payload. The script sends the five
            // `data:` values byte for byte ([`ZERO_ARG_CHUNKS`]). Each one
            // becomes one `data:` frame that ends with a blank line.
            Scenario::ZeroArgToolCallCleanStop => {
                let chunks = ZERO_ARG_CHUNKS.iter().map(|p| captured_frame(p)).collect();
                (chunks, None, Ending::Clean)
            }
        };

        Some(Script {
            content_type: "text/event-stream",
            chunks,
            gate_after,
            ending,
        })
    }

    /// The request driven through `Model::invoke`.
    ///
    /// Carries a tool definition for both tool scenarios so the exchange is
    /// coherent — a response returning `tool_calls` for a request that
    /// offered no tools would not be a coherent exchange.
    fn request(scenario: Scenario) -> ModelRequest {
        let mut req = ModelRequest::new();
        req.messages = vec![Item::UserMessage {
            content: vec![ContentPart::Text {
                text: "What is the weather in Berlin?".into(),
            }],
        }];
        if matches!(
            scenario,
            Scenario::ToolCallCleanStop | Scenario::FragmentedToolName
        ) {
            req.tools = vec![ToolDef::new(
                TOOL_NAME,
                "Look up the current weather for a city.",
                json!({
                    "type": "object",
                    "properties": { "city": { "type": "string" } },
                    "required": ["city"]
                }),
            )];
        }
        if scenario == Scenario::ZeroArgToolCallCleanStop {
            // The tool and the forced choice of the capture.
            req.tools = vec![ToolDef::new(
                "get_current_time",
                "Return the current server time. Takes no arguments.",
                json!({ "type": "object", "properties": {} }),
            )];
            req.model_settings.tool_choice = Some(ToolChoice::Tool {
                name: "get_current_time".into(),
            });
        }
        req
    }

    /// Build a real `OpenAiModel` (Chat Completions backend) pointed at a
    /// local plain-HTTP endpoint.
    fn build_model_against(base_url: &str) -> OpenAiModel {
        OpenAiModel::chat(MODEL_ID)
            .api_key("sk-conformance-test")
            .base_url(base_url)
            .build()
            .expect("openai/chat model should build with an explicit api key and base url")
    }

    /// See the module-level docs on this file for the fixture provenance
    /// rules this subject follows.
    struct OpenAiChat;

    #[async_trait::async_trait]
    impl StreamUnderTest for OpenAiChat {
        fn name(&self) -> &'static str {
            "openai/chat"
        }

        fn encodes_stop_reason(&self, scenario: Scenario) -> bool {
            // MEASURED from the bytes about to be served — deliberately not
            // `scenario.expects_stop_reason()`. See the crate's `declines`
            // module doc and `bedrock::encodes_stop_reason` for why that
            // restatement would make the harness's cross-check dead code.
            script(scenario)
                .map(|script| {
                    script
                        .chunks
                        .iter()
                        .any(|chunk| contains(chunk, FINISH_REASON_MARKER))
                })
                .unwrap_or(false)
        }

        fn fixture_tool_name(&self) -> &'static str {
            TOOL_NAME
        }

        fn fixture_finish_reason(&self, scenario: Scenario) -> Option<FinishReason> {
            // Exhaustive on purpose, with no `_` arm — see
            // `bedrock::fixture_finish_reason` for why.
            match scenario {
                Scenario::CleanStop | Scenario::TruncatedAfterStopReason => {
                    Some(FinishReason::Stop)
                }
                Scenario::ToolCallCleanStop | Scenario::ZeroArgToolCallCleanStop => {
                    Some(FinishReason::ToolCalls)
                }
                // Truncated, errored and cancelled streams must withhold
                // `Finish` entirely, and so — per the module doc's
                // "FragmentedToolName deliberately never completes" section —
                // must this subject's `FragmentedToolName` script, which ends
                // before any finish_reason chunk arrives.
                Scenario::TruncatedMidGeneration
                | Scenario::ErrorMidGeneration
                | Scenario::ErrorAfterStopReason
                | Scenario::CancelMidGeneration
                | Scenario::CancelAfterStopReason
                | Scenario::FragmentedToolName
                | Scenario::ThinkingToolCallCleanStop
                | Scenario::ReasoningCleanStop
                | Scenario::ParallelToolCallCleanStop => None,
            }
        }

        fn fixture_channel_order(&self, _scenario: Scenario) -> Option<&'static [Channel]> {
            // This subject serves no scenario whose channel order the harness
            // checks, so it makes no claim.
            None
        }

        fn fixture_calls(&self, scenario: Scenario) -> &'static [ExpectedCall] {
            match scenario {
                // The one call of the zero-argument capture.
                Scenario::ZeroArgToolCallCleanStop => ZERO_ARG_CALLS,
                _ => &[],
            }
        }

        async fn stream(&self, scenario: Scenario, cancel: CancellationToken) -> Outcome {
            if scenario == Scenario::ThinkingToolCallCleanStop {
                return Outcome::Declined("the scenario serves an anthropic thinking block");
            }
            // Chat Completions has no field that carries reasoning text.
            if scenario == Scenario::ReasoningCleanStop {
                return Outcome::Declined("the Chat Completions wire carries no reasoning text");
            }
            // Parallel tool calls can occur on this wire, but the repo
            // fixture of them is hand-authored, not captured (SMA-859).
            if scenario == Scenario::ParallelToolCallCleanStop {
                return Outcome::Uncaptured("no captured parallel tool-call stream");
            }

            let script =
                script(scenario).expect("a scenario this subject serves must have a script");
            let mut server = PacedServer::start(script).await;
            let gate = server.take_gate();
            let model = build_model_against(&server.base_url());

            let stream = model
                .invoke(request(scenario), cancel)
                .await
                .expect("openai/chat invoke should reach the local paced server");

            Outcome::Served { stream, gate }
        }
    }

    /// Whether `haystack` contains `needle` as a contiguous byte run.
    /// Duplicated per subject module by design — see `bedrock::contains`.
    fn contains(haystack: &[u8], needle: &[u8]) -> bool {
        haystack.windows(needle.len()).any(|w| w == needle)
    }

    /// The soundness claim behind [`OpenAiChat::encodes_stop_reason`]'s
    /// substring scan: [`FINISH_REASON_MARKER`] occurs in the finish_reason
    /// chunk and in no other frame this subject builds — in particular, not
    /// in an ordinary content delta, which is the explicit-null pitfall the
    /// task brief calls out (`"finish_reason":null` contains `finish_reason`
    /// but not the marker).
    #[test]
    fn scan_finds_only_a_populated_finish_reason() {
        assert!(
            contains(&finish_chunk("stop"), FINISH_REASON_MARKER),
            "the finish_reason chunk must carry the marker, or the scan measures nothing"
        );

        for (name, frame) in [
            ("content delta (finish_reason: null)", text_delta("Hel")),
            ("usage chunk", usage_chunk()),
            ("[DONE]", done()),
            ("tool_call start", tool_call_start(TOOL_CALL_ID, "get_")),
            (
                "tool_call name fragment",
                tool_call_name_fragment("weather"),
            ),
            ("tool_call args", tool_call_args("{\"city\":\"Berlin\"}")),
            ("zero-arg tool chunk", captured_frame(ZERO_ARG_CHUNKS[1])),
        ] {
            assert!(
                !contains(&frame, FINISH_REASON_MARKER),
                "{name} contains the populated-finish_reason marker, so the scan would report a \
                 stop reason for scripts that encode none. In particular, an ordinary content \
                 delta carries \"finish_reason\":null, and the marker must not match that null \
                 value."
            );
        }
    }

    /// The script is the capture, byte for byte.
    ///
    /// Provenance: CAPTURED against `https://api.openai.com/v1/chat/completions`
    /// with `gpt-5-mini` (wire model `gpt-5-mini-2025-08-07`) on 2026-11-24
    /// at 20:39:21 UTC. The request is
    /// `fixtures/openai_chat/zero_arg_tool_call.request.json`. The file is
    /// 1618 bytes. Its SHA-256 is
    /// `5c5e74ac372aa0f954e89801c0addb62592a1ae02ad3c36b427b4d24f56d8cf2`.
    /// Finding: the API streams the arguments as the delta `""` and then as
    /// the delta `"{}"`.
    ///
    /// Removed: nothing. Added: nothing. Changed: nothing.
    #[test]
    fn zero_arg_script_is_the_capture_byte_for_byte() {
        let script = script(Scenario::ZeroArgToolCallCleanStop).expect("served");
        assert_eq!(script.chunks.len(), 5);
        assert_eq!(script.chunks.concat(), ZERO_ARG_RAW);
    }

    #[tokio::test]
    async fn conforms() {
        assert_conforms(&OpenAiChat).await;
    }
}

/// The `Model::invoke` stream contract, checked against the LiteLLM proxy's
/// Chat Completions backend.
///
/// LiteLLM's wire shape is the OpenAI Chat Completions envelope
/// (`openai_chat` above is its closest sibling in this suite), but SMA-550
/// fixed a real correlation bug in this translator specifically:
/// `providers-litellm/src/stream.rs` used to key buffered tool-call fragments
/// by their *wire* key (`Index`/`Id`) rather than canonicalizing on the
/// resolved `call_id`, so one call could own two state entries and emit two
/// name-bearing deltas. Since the fix, every delta for one call is
/// re-keyed onto a single canonical `Key::Id(call_id)` slot
/// (`ChatTranslator::canonicalize`), which is what makes assertion 7 —
/// "exactly one name-carrying `ToolCallDelta` per `call_id`" — structural
/// for the translator in general. **It is not, however, probed by this
/// module's own fixtures** — see the section below on where that coverage
/// actually lives.
///
/// # Fixture provenance
///
/// Every envelope shape below is transcribed from a fixture committed in
/// `crates/paigasus-helikon-providers-litellm/tests/fixtures/`, cited on the
/// helper that builds it. The one exception is the last bullet. It names a
/// capture that this test crate owns:
///
/// - `text_then_trailing_usage.txt` — CAPTURED with named reconstructed
///   parts, from LiteLLM 1.97.0 with a keyless `mock_response` (SMA-451,
///   PR #199). The second content delta is reconstructed, and the record does
///   not show the envelope values. Content deltas with no `finish_reason`
///   key at all (not even an explicit `null`), a chunk carrying a populated
///   `finish_reason`, a trailing usage-only chunk with no `finish_reason` key,
///   `[DONE]`.
/// - `tool_call_stream.txt` — CAPTURED from LiteLLM 1.98.0 over a
///   hand-written fake upstream (SMA-547, PR #204). Only the LiteLLM
///   serialization is captured; the ids and the splits are authored input.
///   The whole-tool-call envelope: `id` and the
///   complete function `name` arriving together with an empty-string
///   `arguments`, followed by two args-only continuations with no `name` key
///   at all (not even empty), then a `"tool_calls"` finish chunk and trailing
///   usage.
/// - `tool_call_stream_fragmented_name.txt` — CAPTURED from LiteLLM 1.98.0
///   over the same kind of hand-written fake upstream (SMA-547, PR #204);
///   the canonical fragmented-name shape, both fragments arriving
///   after the id (the first fragment arrives together with the id, the
///   second on a later delta once the id has already resolved). Per the
///   design spec's §6 provenance note the "id resolves late" variant has no
///   capture anywhere in the repo and is not built here — matching
///   `openai_chat`'s note on the same file.
/// - `truncated_no_finish.txt` — HAND-AUTHORED (SMA-451, PR #199). Grounds
///   the shape "content delta(s), then the body simply ends" with no
///   `finish_reason` chunk anywhere. No capture of this case is recorded.
/// - `tests/provider-stream-conformance/fixtures/litellm/zero_arg_tool_call.raw`
///   — CAPTURED (LiteLLM 1.98.0 proxy, 2026-11-24). `ZeroArgToolCallCleanStop`
///   sends all six `data:` values byte for byte ([`ZERO_ARG_CHUNKS`]). The
///   request and the proxy config sit next to it.
///
/// `unknown_finish_reason.txt` (HAND-AUTHORED) was read but contributes no
/// shape this module needs: this subject serves only `Stop` and `ToolCalls`
/// finish reasons, and the `"guardrail_intervened"` value it carries is a
/// second encoding of the already-covered populated-`finish_reason`
/// envelope. `unparseable_frame.txt` (HAND-AUTHORED) is not cited here.
///
/// [`FINISH_REASON_MARKER`]'s guard test needs one more shape that no
/// committed *litellm-crate* fixture happens to carry: a content delta with
/// an **explicit** `"finish_reason":null`. That shape comes from the same
/// source as in `openai_chat`:
/// `crates/paigasus-helikon-providers-openai/tests/chat_wire.rs`'s
/// `happy_path_text_completion` body, which is HAND-AUTHORED. No recorded
/// capture sends that null on a content delta. This module's own
/// `text_delta` stays faithful to `text_then_trailing_usage.txt` (which omits
/// the key rather than nulling it), so that second shape is scripted only
/// inside the guard test, not served to the subject.
///
/// # `FragmentedToolName` draws its shape, not its literal values, from the
/// capture
///
/// Per `Scenario::expects_stop_reason` (`src/lib.rs`) and the design spec's
/// §6 table (`7, 1` only, no assertion 3), this scenario must not carry a
/// `finish_reason`. `tool_call_stream_fragmented_name.txt`'s capture
/// continues on to a `"tool_calls"` finish chunk and trailing usage, but this
/// subject's script stops right after the three tool-call deltas — see
/// `openai_chat`'s module doc for the full reasoning (same shape, same fix,
/// same file). As in every other scenario in this module, `id`/`created`/
/// `model` are this module's fixed constants rather than the capture's
/// literal values, and `TOOL_CALL_ID` stands in for the capture's literal
/// tool-call id; unlike `openai_chat`'s version of this same scenario,
/// this module's `tool_call_start` keeps the capture's `"role":"assistant"`
/// key.
///
/// # `canonicalize`'s SMA-550 regression coverage lives in the crate's own
/// unit tests, not here
///
/// Both `tool_call_stream.txt` and `tool_call_stream_fragmented_name.txt`
/// carry an explicit `"index":0` on *every* tool-call delta. Because of
/// that, `handle_tool_call` (`stream.rs:373-448`) already resolves every
/// delta for one call to the same `call_id` via
/// `self.tool_calls.get(&Key::Index(0))` alone — the `Key::Index`/`Key::Id`
/// boundary that `ChatTranslator::canonicalize` exists to unify never arises
/// from these specific bytes.
///
/// Confirmed by mutation while registering this subject: temporarily
/// stubbing `canonicalize` into an identity function left `litellm::conforms`
/// green, while the same change failed **11 tests** in
/// `crates/paigasus-helikon-providers-litellm/src/stream.rs`'s own test
/// module — most directly
/// `stream::tests::dual_key_call_emits_at_most_one_name_mid_stream`, which
/// observes exactly the pre-SMA-550 shape (two name-carrying deltas for one
/// `call_id`) that neither of this subject's tool scenarios can reproduce.
///
/// So: read this subject's `conforms` test as confirming the translator
/// behaves correctly on the wire shapes this module scripts from the sources
/// above, **not** as a standing regression guard for the `canonicalize` fix
/// itself — that guard is `dual_key_call_emits_at_most_one_name_mid_stream`
/// and its ten siblings, in the crate under test. If a future litellm
/// capture ever shows a backend whose `index`/`id` correlation key changes
/// mid-call, that would be the fixture to add here; none currently
/// committed does.
mod litellm {
    use super::*;
    use paigasus_helikon_providers_litellm::LiteLlmModel;

    /// `id` on every scripted chunk. Not claimed to match any real LiteLLM id
    /// format — nothing under test inspects it — fixed purely so failure
    /// output is stable and greppable.
    const RESPONSE_ID: &str = "chatcmpl-conformance";

    /// `model` on every scripted chunk, and the model alias `build_model_against`
    /// requests. Matches the alias the task brief specifies.
    const MODEL_ID: &str = "prod-fast";

    /// The tool name every tool-call fixture declares.
    const TOOL_NAME: &str = "get_weather";

    /// `id` for the single tool call in the tool fixtures. Fixed for the same
    /// reason as [`RESPONSE_ID`].
    const TOOL_CALL_ID: &str = "call_conformance_0";

    /// The substring that appears in a chunk if and only if it carries a
    /// *populated* `finish_reason`.
    ///
    /// The obvious marker — `finish_reason` alone — would be wrong even
    /// though this subject's own `text_delta` chunks omit the key entirely
    /// (see [`text_delta`]): the marker has to exclude an explicit
    /// `"finish_reason":null` structurally, not merely happen to avoid the
    /// one shape this module scripts. No recorded LiteLLM capture sends that
    /// null; the hand-authored `chat_wire.rs` body does, and the task brief
    /// names the shape. Scanning for the opening
    /// quote of a *string* value does that: `null` has no quote after the
    /// colon, `"stop"`/`"tool_calls"` do.
    /// `scan_finds_only_a_populated_finish_reason` is the guard that keeps
    /// this true, including against a literal `"finish_reason":null` chunk.
    const FINISH_REASON_MARKER: &[u8] = b"\"finish_reason\":\"";

    /// One SSE frame: `data: {payload}\n\n`. `serde_json::Value`'s `Display`
    /// impl serialises compactly (no extra whitespace), which is what keeps
    /// [`FINISH_REASON_MARKER`] a literal substring of the bytes on the wire.
    fn frame(payload: serde_json::Value) -> Vec<u8> {
        format!("data: {payload}\n\n").into_bytes()
    }

    /// One text delta. Matches `text_then_trailing_usage.txt`'s content
    /// chunks: no `finish_reason` key at all, not even `null`.
    ///
    /// Changed from the capture: the first delta drops `"role":"assistant"`.
    fn text_delta(text: &str) -> Vec<u8> {
        frame(json!({
            "id": RESPONSE_ID,
            "object": "chat.completion.chunk",
            "created": 1,
            "model": MODEL_ID,
            "choices": [{
                "index": 0,
                "delta": { "content": text }
            }]
        }))
    }

    /// The chunk carrying a populated `finish_reason`. Matches
    /// `text_then_trailing_usage.txt`'s finish chunk
    /// (`"delta":{},"finish_reason":"stop"`).
    ///
    /// `ChatTranslator` buffers this and does **not** emit `Finish` inline —
    /// it is released only by `finish()` at end-of-stream (`stream.rs`'s
    /// module doc, invariant 1) — which is why `CleanStop` below must place
    /// [`usage_chunk`] *after* this one.
    fn finish_chunk(reason: &str) -> Vec<u8> {
        frame(json!({
            "id": RESPONSE_ID,
            "object": "chat.completion.chunk",
            "created": 1,
            "model": MODEL_ID,
            "choices": [{ "index": 0, "delta": {}, "finish_reason": reason }]
        }))
    }

    /// The trailing usage-only chunk. Matches `text_then_trailing_usage.txt`'s
    /// usage chunk's shape: an empty `delta`, no `finish_reason` key at all,
    /// and a top-level `usage` object.
    ///
    /// Changed from the capture: drops `completion_tokens_details`.
    fn usage_chunk() -> Vec<u8> {
        frame(json!({
            "id": RESPONSE_ID,
            "object": "chat.completion.chunk",
            "created": 1,
            "model": MODEL_ID,
            "choices": [{ "index": 0, "delta": {} }],
            "usage": { "prompt_tokens": 8, "completion_tokens": 6, "total_tokens": 14 }
        }))
    }

    /// The stream terminator every committed fixture ends with.
    fn done() -> Vec<u8> {
        b"data: [DONE]\n\n".to_vec()
    }

    /// `id` plus the whole function name, empty `arguments`. Matches
    /// `tool_call_stream.txt`'s first tool-call line
    /// (`"id":"call_abc","function":{"arguments":"","name":"get_weather"}`) —
    /// also the shape of `tool_call_stream_fragmented_name.txt`'s first line
    /// with a fragment instead of the whole name.
    fn tool_call_start(id: &str, name_frag: &str) -> Vec<u8> {
        frame(json!({
            "id": RESPONSE_ID,
            "created": 1,
            "model": MODEL_ID,
            "object": "chat.completion.chunk",
            "choices": [{
                "index": 0,
                "delta": {
                    "role": "assistant",
                    "tool_calls": [{
                        "id": id,
                        "function": { "arguments": "", "name": name_frag },
                        "type": "function",
                        "index": 0
                    }]
                }
            }]
        }))
    }

    /// A later name fragment, no `id` key, empty `arguments`. Matches
    /// `tool_call_stream_fragmented_name.txt`'s second tool-call line
    /// (`"function":{"arguments":"","name":"weather"}`, no `id`) — the shape
    /// that proves the second fragment arrives once the id has already
    /// resolved, the SMA-547 defect this capture exists to demonstrate.
    fn tool_call_name_fragment(name_frag: &str) -> Vec<u8> {
        frame(json!({
            "id": RESPONSE_ID,
            "created": 1,
            "model": MODEL_ID,
            "object": "chat.completion.chunk",
            "choices": [{
                "index": 0,
                "delta": {
                    "tool_calls": [{
                        "function": { "arguments": "", "name": name_frag },
                        "type": "function",
                        "index": 0
                    }]
                }
            }]
        }))
    }

    /// An args-only continuation: no `id`, no `name` key at all. Matches
    /// `tool_call_stream.txt`'s and `tool_call_stream_fragmented_name.txt`'s
    /// argument-continuation lines.
    fn tool_call_args(args_frag: &str) -> Vec<u8> {
        frame(json!({
            "id": RESPONSE_ID,
            "created": 1,
            "model": MODEL_ID,
            "object": "chat.completion.chunk",
            "choices": [{
                "index": 0,
                "delta": {
                    "tool_calls": [{
                        "function": { "arguments": args_frag },
                        "type": "function",
                        "index": 0
                    }]
                }
            }]
        }))
    }

    /// The six `data:` values of `fixtures/litellm/zero_arg_tool_call.raw`,
    /// byte for byte, in file order. The last is `[DONE]`. See the
    /// `ZeroArgToolCallCleanStop` arm of [`script`] for the provenance.
    const ZERO_ARG_CHUNKS: [&str; 6] = [
        r##"{"id":"chatcmpl-8dcb0076-1d0c-4623-a467-a321ea324374","created":1795553946,"model":"zero-arg-anthropic","object":"chat.completion.chunk","choices":[{"index":0,"delta":{"content":"","role":"assistant","tool_calls":[{"id":"toolu_01Ph5aTG6RG9RAhP9Q6F3Hmg","function":{"arguments":"","name":"get_current_time"},"type":"function","index":0,"caller":{"type":"direct"}}]}}]}"##,
        r##"{"id":"chatcmpl-8dcb0076-1d0c-4623-a467-a321ea324374","created":1795553946,"model":"zero-arg-anthropic","object":"chat.completion.chunk","choices":[{"index":0,"delta":{"content":"","tool_calls":[{"function":{"arguments":""},"type":"function","index":0}]}}]}"##,
        r##"{"id":"chatcmpl-8dcb0076-1d0c-4623-a467-a321ea324374","created":1795553946,"model":"zero-arg-anthropic","object":"chat.completion.chunk","choices":[{"index":0,"delta":{"content":"","tool_calls":[{"function":{"arguments":"{}"},"type":"function","index":0}]}}]}"##,
        r##"{"id":"chatcmpl-8dcb0076-1d0c-4623-a467-a321ea324374","object":"chat.completion.chunk","created":1795553946,"model":"zero-arg-anthropic","choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}"##,
        r##"{"id":"chatcmpl-8dcb0076-1d0c-4623-a467-a321ea324374","created":1795553946,"model":"zero-arg-anthropic","object":"chat.completion.chunk","choices":[{"index":0,"delta":{}}],"usage":{"completion_tokens":11,"prompt_tokens":485,"total_tokens":496,"completion_tokens_details":{"reasoning_tokens":0,"text_tokens":11},"prompt_tokens_details":{"cached_tokens":0,"text_tokens":485,"cache_write_tokens":0,"cache_creation_tokens":0,"cache_creation_token_details":{"ephemeral_5m_input_tokens":0,"ephemeral_1h_input_tokens":0}},"cache_creation_input_tokens":0,"cache_read_input_tokens":0,"inference_geo":"global"}}"##,
        r##"[DONE]"##,
    ];

    /// The one call of [`ZERO_ARG_CHUNKS`]: the tool name and the literal `{}`
    /// that the proxy streams as its arguments.
    const ZERO_ARG_CALLS: &[ExpectedCall] = &[ExpectedCall {
        name: "get_current_time",
        args: "{}",
    }];

    /// The whole capture file, to check the script against.
    const ZERO_ARG_RAW: &[u8] = include_bytes!("../fixtures/litellm/zero_arg_tool_call.raw");

    /// One captured SSE frame: `data: {payload}\n\n`.
    fn captured_frame(payload: &str) -> Vec<u8> {
        format!("data: {payload}\n\n").into_bytes()
    }

    /// The scripted response for one scenario.
    ///
    /// Returns `None` for `ThinkingToolCallCleanStop`, which this subject
    /// declines, and for the scenarios that it has no capture for (uncaptured).
    fn script(scenario: Scenario) -> Option<Script> {
        // The two-delta opening every text scenario shares, matching
        // `text_then_trailing_usage.txt`'s two content chunks (the second
        // is a reconstructed part; see that file's header).
        let opening = || vec![text_delta("Hel"), text_delta("lo")];
        // The opening plus a populated finish_reason chunk.
        let through_stop = || {
            let mut chunks = opening();
            chunks.push(finish_chunk("stop"));
            chunks
        };

        let (chunks, gate_after, ending) = match scenario {
            // Full stream: deltas, stop reason, then usage AFTER it, then
            // [DONE] and a clean end of body — the order the
            // `text_then_trailing_usage.txt` capture shows (SMA-451 design,
            // Appendix B, probe P1).
            Scenario::CleanStop => {
                let mut chunks = through_stop();
                chunks.push(usage_chunk());
                chunks.push(done());
                (chunks, None, Ending::Clean)
            }
            // The stop reason is observed, then the body simply ends — no
            // usage, no [DONE] — so the translator's buffered stop reason has
            // to be flushed by the EOF path (`ChatTranslator::finish`).
            Scenario::TruncatedAfterStopReason => (through_stop(), None, Ending::Clean),
            // The body ends cleanly mid-generation: no finish_reason chunk
            // ever arrives, so no stop reason is ever observed. Grounded in
            // `truncated_no_finish.txt`'s shape (content delta(s), then EOF).
            Scenario::TruncatedMidGeneration => (opening(), None, Ending::Clean),
            // Same prefix, but the connection is torn down without a
            // terminating chunk, so the client observes a transport error.
            Scenario::ErrorMidGeneration => (opening(), None, Ending::Abort),
            // The stop reason is buffered and *then* the body is aborted, so
            // the error path must not flush it as a Finish. This is the one
            // scenario byte-identical to ErrorMidGeneration's observable
            // events, which is exactly what `encodes_stop_reason` exists to
            // keep distinguishable — see its doc.
            Scenario::ErrorAfterStopReason => (through_stop(), None, Ending::Abort),
            // One content delta goes out, then the server parks holding the
            // second back (`gate_after: Some(1)` pauses *before* sending the
            // chunk at index 1, per `server.rs`'s `feed` loop) — no
            // finish_reason chunk anywhere in this script, so no stop reason
            // exists even after the gate releases.
            Scenario::CancelMidGeneration => {
                let chunks = opening();
                (chunks, Some(1), Ending::Clean)
            }
            // The stop reason AND the usage chunk after it are both observed
            // before the gate. `usage` is the harness's required edge here:
            // `ChatTranslator` buffers `finish_reason` silently, so the
            // stop-reason chunk itself is not an observable event — usage is
            // the first thing the client sees after it.
            Scenario::CancelAfterStopReason => {
                let mut chunks = through_stop();
                chunks.push(usage_chunk());
                let gate_after = chunks.len();
                (chunks, Some(gate_after), Ending::Clean)
            }
            // `tool_call_stream_fragmented_name.txt`'s event sequence and key
            // structure for its first three frames, not a byte-identical
            // copy (see the module doc's "`FragmentedToolName` draws its
            // shape, not its literal values, from the capture" section for
            // the substitutions) — and nothing more: no finish_reason, no
            // usage, no [DONE]. See that same section for why the body ends
            // right here.
            Scenario::FragmentedToolName => {
                let chunks = vec![
                    tool_call_start(TOOL_CALL_ID, "get_"),
                    tool_call_name_fragment("weather"),
                    tool_call_args("{\"city\":\"Berlin\"}"),
                ];
                (chunks, None, Ending::Clean)
            }
            // `tool_call_stream.txt`'s event sequence and key structure, not
            // its literal values: `id`/`created`/`model` are this module's
            // fixed constants (`RESPONSE_ID`/`MODEL_ID`) rather than the
            // capture's `"chatcmpl-fake"`/`1786999072`/`"shape-normal"`, and
            // `TOOL_CALL_ID` stands in for the capture's literal
            // `"call_abc"` (same substitution convention as every other
            // scenario in this module). What the fixture actually grounds:
            // the whole name arrives with the id (empty args), the arguments
            // arrive split across two continuations (so exactly one emitted
            // `ToolCallDelta` carries the name), then a tool_calls stop
            // reason and the usage-bearing trailing chunk.
            Scenario::ToolCallCleanStop => {
                let chunks = vec![
                    tool_call_start(TOOL_CALL_ID, TOOL_NAME),
                    tool_call_args("{\"city\":"),
                    tool_call_args("\"Berlin\"}"),
                    finish_chunk("tool_calls"),
                    usage_chunk(),
                    done(),
                ];
                (chunks, None, Ending::Clean)
            }
            // Declined — the scenario serves an anthropic thinking block.
            Scenario::ThinkingToolCallCleanStop => return None,
            // Not served — see the reason in `stream`.
            Scenario::ReasoningCleanStop => return None,
            // Not served — see the reason in `stream`.
            Scenario::ParallelToolCallCleanStop => return None,
            // `fixtures/litellm/zero_arg_tool_call.raw`, CAPTURED from a
            // LiteLLM 1.98.0 proxy (`ghcr.io/berriai/litellm@sha256:3a1980074ff2ac13883e0ffee1bebec345d8addd004be251079cee173b0e8305`)
            // on `/v1/chat/completions`, 2026-11-24 20:59:06 UTC. The wire
            // `model` is the proxy alias `zero-arg-anthropic`. It routes to
            // the upstream `anthropic/claude-haiku-5-5`. The request and the
            // proxy config are in `fixtures/litellm/`. The proxy streams the
            // arguments as `""`, `""` and then the literal `{}`. Six frames:
            // the tool-call start, an empty argument delta, the `{}` delta,
            // the finish chunk, the usage chunk and `[DONE]`.
            //
            // Edits made here: none to any payload. The script sends the six
            // `data:` values byte for byte ([`ZERO_ARG_CHUNKS`]). Each one
            // becomes one `data:` frame that ends with a blank line.
            Scenario::ZeroArgToolCallCleanStop => {
                let chunks = ZERO_ARG_CHUNKS
                    .iter()
                    .copied()
                    .map(captured_frame)
                    .collect();
                (chunks, None, Ending::Clean)
            }
        };

        Some(Script {
            content_type: "text/event-stream",
            chunks,
            gate_after,
            ending,
        })
    }

    /// The request driven through `Model::invoke`.
    ///
    /// Carries a tool definition for both tool scenarios so the exchange is
    /// coherent — a response returning `tool_calls` for a request that
    /// offered no tools would not be a coherent exchange.
    fn request(scenario: Scenario) -> ModelRequest {
        let mut req = ModelRequest::new();
        req.messages = vec![Item::UserMessage {
            content: vec![ContentPart::Text {
                text: "What is the weather in Berlin?".into(),
            }],
        }];
        if matches!(
            scenario,
            Scenario::ToolCallCleanStop | Scenario::FragmentedToolName
        ) {
            req.tools = vec![ToolDef::new(
                TOOL_NAME,
                "Look up the current weather for a city.",
                json!({
                    "type": "object",
                    "properties": { "city": { "type": "string" } },
                    "required": ["city"]
                }),
            )];
        }
        if scenario == Scenario::ZeroArgToolCallCleanStop {
            // The tool and the forced choice of the capture.
            req.tools = vec![ToolDef::new(
                "get_current_time",
                "Return the current server time. Takes no arguments.",
                json!({ "type": "object", "properties": {} }),
            )];
            req.model_settings.tool_choice = Some(ToolChoice::Tool {
                name: "get_current_time".into(),
            });
        }
        req
    }

    /// Build a real `LiteLlmModel` pointed at a local plain-HTTP endpoint.
    fn build_model_against(base_url: &str) -> LiteLlmModel {
        LiteLlmModel::chat(MODEL_ID)
            .base_url(base_url)
            .build()
            .expect("litellm model should build with an explicit base url")
    }

    /// See the module-level docs on this file for the fixture provenance
    /// rules this subject follows.
    struct LiteLlm;

    #[async_trait::async_trait]
    impl StreamUnderTest for LiteLlm {
        fn name(&self) -> &'static str {
            "litellm"
        }

        fn encodes_stop_reason(&self, scenario: Scenario) -> bool {
            // MEASURED from the bytes about to be served — deliberately not
            // `scenario.expects_stop_reason()`. See the crate's `declines`
            // module doc and `bedrock::encodes_stop_reason` for why that
            // restatement would make the harness's cross-check dead code.
            script(scenario)
                .map(|script| {
                    script
                        .chunks
                        .iter()
                        .any(|chunk| contains(chunk, FINISH_REASON_MARKER))
                })
                .unwrap_or(false)
        }

        fn fixture_tool_name(&self) -> &'static str {
            TOOL_NAME
        }

        fn fixture_finish_reason(&self, scenario: Scenario) -> Option<FinishReason> {
            // Exhaustive on purpose, with no `_` arm — see
            // `bedrock::fixture_finish_reason` for why.
            match scenario {
                Scenario::CleanStop | Scenario::TruncatedAfterStopReason => {
                    Some(FinishReason::Stop)
                }
                Scenario::ToolCallCleanStop | Scenario::ZeroArgToolCallCleanStop => {
                    Some(FinishReason::ToolCalls)
                }
                // Truncated, errored and cancelled streams must withhold
                // `Finish` entirely, and so must `FragmentedToolName`, whose
                // script ends before any finish_reason chunk arrives.
                Scenario::TruncatedMidGeneration
                | Scenario::ErrorMidGeneration
                | Scenario::ErrorAfterStopReason
                | Scenario::CancelMidGeneration
                | Scenario::CancelAfterStopReason
                | Scenario::FragmentedToolName
                | Scenario::ThinkingToolCallCleanStop
                | Scenario::ReasoningCleanStop
                | Scenario::ParallelToolCallCleanStop => None,
            }
        }

        fn fixture_channel_order(&self, _scenario: Scenario) -> Option<&'static [Channel]> {
            // This subject serves no scenario whose channel order the harness
            // checks, so it makes no claim.
            None
        }

        fn fixture_calls(&self, scenario: Scenario) -> &'static [ExpectedCall] {
            match scenario {
                // The one call of the zero-argument capture.
                Scenario::ZeroArgToolCallCleanStop => ZERO_ARG_CALLS,
                _ => &[],
            }
        }

        async fn stream(&self, scenario: Scenario, cancel: CancellationToken) -> Outcome {
            if scenario == Scenario::ThinkingToolCallCleanStop {
                return Outcome::Declined("the scenario serves an anthropic thinking block");
            }
            // A reasoning stream can occur on this wire, but no capture of one
            // exists (SMA-858).
            if scenario == Scenario::ReasoningCleanStop {
                return Outcome::Uncaptured("no captured reasoning stream");
            }
            // Parallel tool calls can occur on this wire, but no capture of
            // them exists (SMA-859).
            if scenario == Scenario::ParallelToolCallCleanStop {
                return Outcome::Uncaptured("no captured parallel tool-call stream");
            }

            let script =
                script(scenario).expect("a scenario this subject serves must have a script");
            let mut server = PacedServer::start(script).await;
            let gate = server.take_gate();
            let model = build_model_against(&server.base_url());

            let stream = model
                .invoke(request(scenario), cancel)
                .await
                .expect("litellm invoke should reach the local paced server");

            Outcome::Served { stream, gate }
        }
    }

    /// Whether `haystack` contains `needle` as a contiguous byte run.
    /// Duplicated per subject module by design — see `bedrock::contains`.
    fn contains(haystack: &[u8], needle: &[u8]) -> bool {
        haystack.windows(needle.len()).any(|w| w == needle)
    }

    /// The soundness claim behind [`LiteLlm::encodes_stop_reason`]'s
    /// substring scan: [`FINISH_REASON_MARKER`] occurs in the finish_reason
    /// chunk and in no other frame this subject builds — in particular, not
    /// in an ordinary content delta, whether that delta omits the key
    /// entirely (this module's own `text_delta`) or carries it as an
    /// explicit `null` (a shape no committed litellm-crate fixture carries
    /// and no recorded capture sends — taken from
    /// `providers-openai/tests/chat_wire.rs`'s `happy_path_text_completion`
    /// body, which is HAND-AUTHORED; see `openai_chat`'s citation of the
    /// same file).
    #[test]
    fn scan_finds_only_a_populated_finish_reason() {
        assert!(
            contains(&finish_chunk("stop"), FINISH_REASON_MARKER),
            "the finish_reason chunk must carry the marker, or the scan measures nothing"
        );

        // Not scripted by this module — `text_delta` stays faithful to
        // `text_then_trailing_usage.txt`, which omits the key rather than
        // nulling it. Included only so the marker is proven safe against the
        // shape the task brief calls out, grounded per the doc comment above.
        let explicit_null_delta = frame(json!({
            "id": RESPONSE_ID,
            "object": "chat.completion.chunk",
            "created": 1,
            "model": MODEL_ID,
            "choices": [{
                "index": 0,
                "delta": { "content": "hello" },
                "finish_reason": null
            }]
        }));

        for (name, frame) in [
            ("content delta (no finish_reason key)", text_delta("Hel")),
            (
                "content delta (explicit finish_reason: null)",
                explicit_null_delta,
            ),
            ("usage chunk", usage_chunk()),
            ("[DONE]", done()),
            ("tool_call start", tool_call_start(TOOL_CALL_ID, "get_")),
            (
                "tool_call name fragment",
                tool_call_name_fragment("weather"),
            ),
            ("tool_call args", tool_call_args("{\"city\":\"Berlin\"}")),
            ("zero-arg tool chunk", captured_frame(ZERO_ARG_CHUNKS[2])),
        ] {
            assert!(
                !contains(&frame, FINISH_REASON_MARKER),
                "{name} contains the populated-finish_reason marker, so the scan would report a \
                 stop reason for scripts that encode none. In particular, an ordinary content \
                 delta carries at most \"finish_reason\":null, and the marker must not match \
                 that null value."
            );
        }
    }

    /// The script is the capture, byte for byte.
    ///
    /// Provenance: CAPTURED from the LiteLLM 1.98.0 proxy
    /// `ghcr.io/berriai/litellm@sha256:3a1980074ff2ac13883e0ffee1bebec345d8addd004be251079cee173b0e8305`
    /// at `/v1/chat/completions`. The wire `model` is the proxy alias
    /// `zero-arg-anthropic`. The upstream model is `anthropic/claude-haiku-5-5`.
    /// The capture ran on 2026-11-24 at 20:59:06 UTC. The request is
    /// `tests/provider-stream-conformance/fixtures/litellm/zero_arg_tool_call.request.json`.
    /// The proxy config is
    /// `tests/provider-stream-conformance/fixtures/litellm/zero_arg_tool_call.litellm_config.yaml`.
    /// The file is 1739 bytes. Its
    /// SHA-256 is
    /// `b81bc790cdcf6a93e1ef6e8391a353a344d95549ef08a412ee0f7c1f241552c7`.
    /// Finding: the proxy streams the arguments as the deltas `""`, `""` and
    /// `"{}"`. The upstream call was not captured. So the source of the `{}`
    /// is an inference (spec section 4, finding 5).
    ///
    /// Removed: nothing. Added: nothing. Changed: nothing.
    #[test]
    fn zero_arg_script_is_the_capture_byte_for_byte() {
        let script = script(Scenario::ZeroArgToolCallCleanStop).expect("served");
        assert_eq!(script.chunks.len(), 6);
        assert_eq!(script.chunks.concat(), ZERO_ARG_RAW);
    }

    #[tokio::test]
    async fn conforms() {
        assert_conforms(&LiteLlm).await;
    }
}

/// The `Model::invoke` stream contract, checked against the Anthropic Messages
/// API.
///
/// This is the provider whose truncation defect is half the reason this suite
/// exists: SMA-531 (PR #200), where a stream that ended cleanly between
/// `message_delta` and `message_stop` emitted `Usage` and then nothing —
/// `MessageTranslator`'s buffered stop reason was never flushed, so the
/// consumer got no terminal event at all. `TruncatedAfterStopReason` below
/// reproduces that shape — the same event sequence, through `message_delta`'s
/// `stop_reason`/`usage` values — from the fixture the SMA-531 fix itself is
/// tested against; see the Fixture provenance section below for what this
/// module substitutes rather than transcribes.
///
/// # Fixture provenance
///
/// Every envelope shape below is transcribed from a fixture committed in
/// `crates/paigasus-helikon-providers-anthropic/tests/fixtures/`, cited on the
/// helper or script arm that builds it. The last bullet is the one exception.
/// It names a capture that this test crate owns. Each fixture header states
/// its label.
/// The CAPTURED fixtures that this module copies from are the three SMA-840
/// captures (`thinking_text_tool_use.txt`, `empty_thinking_tool_use.txt` and
/// `redacted_thinking_tool_use.txt`) and the SMA-860 capture
/// `fixtures/anthropic/zero_arg_tool_call.raw` in this test crate. All the
/// others are HAND-AUTHORED. The
/// `input_json_delta` values ([`CAPTURED_ARGS`]) come from all three
/// captures. The thinking helpers come from `thinking_text_tool_use.txt`. The
/// `message_start`, `message_delta` and `tool_use_start` helpers copy
/// HAND-AUTHORED shapes. The header of `text_only.txt` lists the keys that the
/// SMA-840 captures carry and those shapes do not.
///
/// - `text_only.txt` — HAND-AUTHORED (SMA-317, PR #33). The source of its
///   shapes is not recorded. The baseline: `message_start` (including its
///   top-level `"stop_reason":null`), a text `content_block_start`, two
///   `content_block_delta`s, `content_block_stop`, `message_delta`
///   (`stop_reason` + `usage` in one event), `message_stop`. Grounds
///   [`message_start`], [`content_block_start_text`], [`text_delta`],
///   [`content_block_stop`], [`message_delta`] and [`message_stop`], and
///   `CleanStop`'s full script.
/// - `eof_after_message_delta.txt` — HAND-AUTHORED (SMA-531, PR #200), a
///   prefix of `text_only.txt`. The exact SMA-531 shape and the fixture
///   the crate's own `clean_eof_after_message_delta_emits_finish` regression
///   test reads: `text_only.txt`'s prefix through `message_delta`, then the
///   body simply ends — no `message_stop`. `TruncatedAfterStopReason` draws
///   its event sequence and `message_delta`'s `stop_reason`/`usage` values
///   from this fixture (via `through_stop()`, with no `message_stop`
///   appended); `message_start`'s `id`/`model` and the delta text are this
///   module's own constants, not the fixture's literal values (see
///   [`message_start`]'s doc).
/// - `eof_mid_content_block.txt` — HAND-AUTHORED (SMA-531, PR #200), a
///   prefix of `text_only.txt`. Grounds `TruncatedMidGeneration`'s
///   envelope shape (`message_start`, `content_block_start`, a
///   `content_block_delta`, then EOF with no `message_delta` at all). The
///   delta *count* is not grounded from this file — as with every other
///   subject in this module, `opening()`'s two deltas are arbitrary content
///   over a grounded envelope, not a literal transcription.
/// - `stream_error.txt` — HAND-AUTHORED (SMA-317, PR #33). Grounds
///   `ErrorMidGeneration`'s prefix envelope
///   (`message_start`, `content_block_start`, one `content_block_delta`)
///   *before* its own in-band `event: error` frame. See "Why `Ending::Abort`
///   and not the fixtures' own `error` event" below for why that frame itself
///   is not transcribed into this script.
/// - `error_after_message_delta.txt` — HAND-AUTHORED (SMA-531, PR #200).
///   Grounds `ErrorAfterStopReason`'s
///   prefix envelope: the same event-sequence shape as
///   `eof_after_message_delta.txt`'s through `message_delta` (message_start,
///   content_block_start, delta(s), content_block_stop, message_delta),
///   though the two fixtures are not byte-identical to each other — this one
///   carries a different `message_start` id and one delta where the other
///   has two — again *before* its own in-band `error` frame.
/// - `parallel_tool_use.txt` and `tool_use_then_continuation.txt` — both
///   HAND-AUTHORED (SMA-317, PR #33). The `tool_use` block start: a
///   `content_block_start` carrying the whole `id` and `name` together.
///   Grounds [`tool_use_start`]. These files send the whole argument JSON in
///   one `input_json_delta`. The tool scripts do not copy that part. They
///   send the captured split instead (see "Tool scripts send the captured
///   split `input_json_delta`" below). The SMA-840 captures also differ in
///   the block start (the headers of both files): their tool
///   `content_block_start` also carries `"caller":{"type":"direct"}`.
/// - `thinking_text_tool_use.txt` — CAPTURED (transcribed, SMA-840, PR #387):
///   probe run C, turn 1, `claude-sonnet-4-5`, on 2026-11-21. Its header
///   records one change: the real signature is replaced with `SIG_FAKE_1`. A
///   signed thinking block before a `tool_use` block. Grounds
///   [`thinking_start`] (`"thinking":"","signature":""`), [`thinking_delta`],
///   [`signature_delta`] (fake value `SIG_FAKE_1`), [`CAPTURED_ARGS`] (lines
///   143-153) and `ThinkingToolCallCleanStop`'s script. The fixture holds a
///   text block
///   between the thinking block and the tool call. That script leaves it out,
///   so the tool call sits at index 1. [`tool_use_start_at`],
///   [`input_json_delta_at`] and [`content_block_stop_at`] carry that index.
///   The script also leaves out other parts of the capture. See the
///   `ThinkingToolCallCleanStop` arm in [`script`] for the list.
///   The same file, all of it (run C), is `ReasoningCleanStop`'s script: the
///   thinking block at index 0, the text block at index 1 and the `tool_use`
///   block at index 2. The script arm lists every edit. It also grounds
///   [`message_start_run_c`], [`message_delta_run_c`],
///   [`tool_use_start_with_caller_at`], [`content_block_start_text_at`] and
///   [`text_delta_at`].
/// - `empty_thinking_tool_use.txt` and `redacted_thinking_tool_use.txt` —
///   both CAPTURED (transcribed, SMA-840, PR #387). Each sends the same four
///   `input_json_delta` values as `thinking_text_tool_use.txt`
///   (`empty_thinking_tool_use.txt` lines 94-104,
///   `redacted_thinking_tool_use.txt` lines 95-105). Grounds
///   [`CAPTURED_ARGS`] together with `thinking_text_tool_use.txt`. No other
///   shape in this module comes from these two files.
/// - `tests/provider-stream-conformance/fixtures/anthropic/zero_arg_tool_call.raw`
///   — CAPTURED (`claude-haiku-5-5`, 2026-11-24). `ZeroArgToolCallCleanStop`
///   sends the whole file byte for byte ([`ZERO_ARG_EVENTS`] lists its seven
///   events). The request sits next to it.
///
/// `body_cut_inside_message_stop.txt` (HAND-AUTHORED, SMA-531, PR #200) and
/// `thinking_then_text.txt` (HAND-AUTHORED, SMA-317, PR #33) were read
/// but contribute no shape this module needs: the former is `text_only.txt` cut
/// inside `message_stop` (its header). The SSE parser discards the partial last
/// event, so it parses to the same events as `eof_after_message_delta.txt`
/// (already covered). For the latter, the two thinking scenarios here,
/// `ThinkingToolCallCleanStop` and `ReasoningCleanStop`, use the CAPTURED
/// `thinking_text_tool_use.txt` instead.
///
/// SMA-840 (PR #387) also added two fixtures that no helper in this module
/// copies a shape from: `interleaved_thinking.txt` and
/// `unsigned_thinking_tool_use.txt` (both HAND-AUTHORED). They split the
/// argument JSON differently from the captures, so they are not a source for
/// [`CAPTURED_ARGS`].
///
/// # Why `Ending::Abort` and not the fixtures' own `error` event
///
/// `stream_error.txt` and `error_after_message_delta.txt` both encode an
/// in-band `event: error` SSE frame. Both files are HAND-AUTHORED. None of
/// the three SMA-840 captures has an `error` event, and the headers of both
/// files say that no capture supports it. The translator parses that frame as
/// `AnthropicEvent::Error`, and `stream.rs`'s `consume` turns it into
/// `Err(ModelError::…)`. That path is already exercised directly against the
/// translator by the crate's own
/// `stream_error_overloaded_terminates_with_unavailable` and
/// `error_after_buffered_stop_reason_emits_no_finish` tests.
///
/// This module instead scripts `ErrorMidGeneration` and `ErrorAfterStopReason`
/// with the harness's `Ending::Abort` — a raw transport-level disconnect, no
/// `error` frame on the wire at all — matching exactly how `bedrock`,
/// `openai_chat` and `litellm` script the same two scenarios. That keeps this
/// suite testing the property it exists to test (the driver's `tokio::select!`
/// cancel/error arms in `model.rs`, and specifically that a buffered stop
/// reason is discarded rather than flushed when the transport fails) the same
/// way across every subject, rather than re-deriving the HAND-AUTHORED
/// in-band error shape a second time in a different harness. Functionally the
/// two mechanisms are indistinguishable to this driver either way: `model.rs`'s
/// `Some(Err(e)) => { yield Err(...); return; }` arm returns as soon as it
/// observes *any* transport-level error, before it would ever get to parse a
/// subsequent `error` frame.
///
/// # Three things specific to this provider
///
/// **`message_delta` carries the stop reason AND usage in the same event**
/// (see [`message_delta`]'s doc). That is what gives `CancelAfterStopReason` an
/// observable edge to gate on at all — unlike `openai_chat`/`litellm`, whose
/// finish-reason chunk is silent and needs a synthetic *following* usage chunk
/// for the gate to sit after.
///
/// **`message_start` also emits a `Usage`** (`stream.rs`'s `MessageStart` arm),
/// which makes the harness's own "at least one `Usage` observed" floor for
/// `CancelAfterStopReason` vacuous for this subject — it is satisfied by
/// `message_start` alone, whether or not the gate sits in the right place. The
/// harness's `floor_violation` doc records this narrowing explicitly. The only
/// guard left for this scenario on this subject is the provenance comment on
/// its `script` match arm below, which states in prose where `gate_after`
/// counts to and why — treat that comment as load-bearing, not decoration.
///
/// **`message_start` contains a literal `"stop_reason":null`.** The CAPTURED
/// `thinking_text_tool_use.txt` also sends it in `message_start`. A naive scan
/// for the substring `stop_reason` would therefore match the very first event
/// of every script, whether or not a stop reason was ever encoded.
/// [`STOP_REASON_MARKER`] scans for the populated-string shape instead, pinned
/// against `message_start`'s `null` by `scan_finds_only_a_populated_stop_reason`
/// — the only detector for `ErrorAfterStopReason` degrading into
/// `ErrorMidGeneration`: `classify` and `floor_violation` don't distinguish
/// the two by `Usage` count, only by `Finish`/`Err` presence and ordering,
/// which already match for both (see `scan_finds_only_a_populated_stop_reason`'s
/// own doc for the full argument).
///
/// # Tool scripts send the captured split `input_json_delta`
///
/// The two HAND-AUTHORED SMA-317 tool-call fixtures (`parallel_tool_use.txt`,
/// `tool_use_then_continuation.txt`) carry one tool call's whole argument
/// JSON in a single `input_json_delta`. The three SMA-840 captures do not.
/// Each sends an empty first `input_json_delta` (`"partial_json":""`), then
/// the JSON in three fragments. See `thinking_text_tool_use.txt` lines
/// 143-153, `empty_thinking_tool_use.txt` lines 94-104 and
/// `redacted_thinking_tool_use.txt` lines 95-105.
///
/// The three tool scripts (`ToolCallCleanStop`, `ThinkingToolCallCleanStop`
/// and `ReasoningCleanStop`) send the captured shape: the four
/// [`CAPTURED_ARGS`] values, in order, one `input_json_delta` each. Like `bedrock`/`openai_chat`/`litellm`, this
/// module therefore sends more than one argument delta for one call. The
/// fourth tool script, `ZeroArgToolCallCleanStop`, is the exception. It sends
/// the capture byte for byte: one `input_json_delta` with `partial_json: ""`.
///
/// The translator gives the name to the first `input_json_delta` of a call,
/// also when its `partial_json` is empty (`stream.rs`'s `InputJsonDelta`
/// arm). The three fragments after it carry no name. Two checks guard this
/// shape. The harness floor "no ToolCallDelta carried the tool name" fails
/// when no delta carries the name. Assertion 7 fails when more than one delta
/// carries it: a translator that names every delta reports a count of 4. The
/// suite does not check WHICH delta carries the name. A translator that
/// skipped the empty delta and named the first fragment would still pass.
mod anthropic {
    use super::*;
    use paigasus_helikon_providers_anthropic::AnthropicModel;

    /// Uses a current id from `KNOWN_MODELS`. The suite reads neither the
    /// model capabilities nor the request body, so this id only has to be
    /// one the crate accepts.
    const MODEL_ID: &str = "claude-sonnet-4-6";

    /// The tool name that `tool_use_then_continuation.txt` declares. The
    /// other tool fixtures use other names: `a` and `b` in
    /// `parallel_tool_use.txt`, and `lookup_city_code` or `weather_by_code`
    /// in the SMA-840 fixtures.
    const TOOL_NAME: &str = "get_weather";

    /// `id` for the single tool call in the tool fixture. Not claimed to match
    /// any real Anthropic id format (`tu_weather`/`tu_a`/`tu_b` is the shape
    /// the HAND-AUTHORED `parallel_tool_use.txt` and
    /// `tool_use_then_continuation.txt` use, but nothing under test inspects
    /// it) — fixed purely so failure output is stable and greppable. The
    /// CAPTURED `thinking_text_tool_use.txt` has an id with the prefix
    /// `toolu_`.
    const TOOL_USE_ID: &str = "tu_conformance_0";

    /// The four `input_json_delta` values that each of the three CAPTURED
    /// SMA-840 fixtures sends for its tool call, in order. The first one is
    /// empty. All three captures send the same bytes:
    /// `thinking_text_tool_use.txt` lines 143-153,
    /// `empty_thinking_tool_use.txt` lines 94-104 and
    /// `redacted_thinking_tool_use.txt` lines 95-105. Joined, they are
    /// `{"city": "Paris"}`. The two HAND-AUTHORED SMA-840 fixtures split the
    /// arguments differently and are not a source.
    const CAPTURED_ARGS: &[&str] = &["", "{\"city\"", ": \"Pari", "s\"}"];

    /// The substring that appears in a chunk if and only if it carries a
    /// *populated* `stop_reason`.
    ///
    /// The obvious marker — `stop_reason` alone — is wrong for this subject:
    /// `message_start` carries an explicit `"stop_reason":null` at the top
    /// level of its `message` object (see [`message_start`], grounded in
    /// `text_only.txt`), so a bare substring scan would report a stop reason
    /// on the very first event of every script, whether or not one was ever
    /// encoded. Scanning for the opening quote of a *string* value excludes
    /// the null case: `null` has no quote after the colon, `"end_turn"`/
    /// `"tool_use"` do. `scan_finds_only_a_populated_stop_reason` is the guard
    /// that keeps this true, including against `message_start`'s literal
    /// `null`.
    const STOP_REASON_MARKER: &[u8] = b"\"stop_reason\":\"";

    /// One SSE frame: `event: {event}\ndata: {payload}\n\n`. Anthropic frames
    /// carry an explicit event name — unlike the bare `data:` frames in the
    /// `openai_chat` and `litellm` sources — matching every fixture in the
    /// Anthropic crate's own `tests/fixtures/`.
    ///
    /// The `event:` line is cosmetic to the translator: `model.rs` parses
    /// `event.data` alone via `AnthropicEvent`'s `#[serde(tag = "type")]`,
    /// never `event.event`. It is still written here because each fixture
    /// sends it. That includes the three SMA-840 captures, which have an
    /// `event:` line before each `data:` line.
    fn frame(event: &str, payload: serde_json::Value) -> Vec<u8> {
        format!("event: {event}\ndata: {payload}\n\n").into_bytes()
    }

    /// `message_start`, which opens each fixture in this crate, including
    /// the three SMA-840 captures. It is the event the translator reads its
    /// initial `Usage` from
    /// (`stream.rs`'s `MessageStart` arm).
    ///
    /// Matches `text_only.txt`'s opening event (HAND-AUTHORED), including the
    /// top-level `"stop_reason":null` on the `message` object — the shape
    /// [`STOP_REASON_MARKER`] must not match. The CAPTURED
    /// `thinking_text_tool_use.txt` also sends that null. Its `message_start`
    /// carries more keys than this one; the header of `text_only.txt` lists
    /// them. The token counts are the same as in `text_only.txt` (line 49).
    fn message_start() -> Vec<u8> {
        frame(
            "message_start",
            json!({
                "type": "message_start",
                "message": {
                    "id": "msg_conformance",
                    "type": "message",
                    "role": "assistant",
                    "content": [],
                    "model": MODEL_ID,
                    "stop_reason": null,
                    "usage": {
                        "input_tokens": 12,
                        "cache_read_input_tokens": 0,
                        "cache_creation_input_tokens": 0,
                        "output_tokens": 0
                    }
                }
            }),
        )
    }

    /// `content_block_start` opening a text block. Matches `text_only.txt`'s
    /// second event. The translator's `ContentBlockStart`/`Text` arm records
    /// no `ModelEvent` for this; it is included so that the script opens each
    /// block it later closes. In the CAPTURED `thinking_text_tool_use.txt`,
    /// each `content_block_stop` follows a `content_block_start` for the
    /// same index.
    fn content_block_start_text() -> Vec<u8> {
        content_block_start_text_at(0)
    }

    /// [`content_block_start_text`] for the block at `index`. In run C the
    /// text block sits at index 1, after the thinking block.
    fn content_block_start_text_at(index: u32) -> Vec<u8> {
        frame(
            "content_block_start",
            json!({
                "type": "content_block_start",
                "index": index,
                "content_block": { "type": "text", "text": "" }
            }),
        )
    }

    /// One text delta. Envelope matches `text_only.txt`'s delta events; the
    /// content itself is arbitrary, as in every other subject in this file.
    fn text_delta(text: &str) -> Vec<u8> {
        text_delta_at(0, text)
    }

    /// [`text_delta`] for the block at `index`.
    fn text_delta_at(index: u32, text: &str) -> Vec<u8> {
        frame(
            "content_block_delta",
            json!({
                "type": "content_block_delta",
                "index": index,
                "delta": { "type": "text_delta", "text": text }
            }),
        )
    }

    /// `content_block_stop`, closing the block opened above. Matches
    /// `text_only.txt`. Ignored by the translator
    /// (`AnthropicEvent::ContentBlockStop { .. }` only logs); included so
    /// that the script closes each block it opens, as the CAPTURED
    /// `thinking_text_tool_use.txt` does for each of its three blocks.
    fn content_block_stop() -> Vec<u8> {
        content_block_stop_at(0)
    }

    /// `content_block_stop` for the block at `index`. The thinking scenario
    /// has two blocks, so its second block closes at index 1.
    fn content_block_stop_at(index: u32) -> Vec<u8> {
        frame(
            "content_block_stop",
            json!({ "type": "content_block_stop", "index": index }),
        )
    }

    /// `content_block_start` opening a thinking block, with the empty
    /// `thinking` and empty `signature` the fixture
    /// `thinking_text_tool_use.txt` (CAPTURED) sends at line 42. The keys
    /// and the values are the same as in that line. The signature arrives
    /// later, in a `signature_delta`.
    fn thinking_start() -> Vec<u8> {
        frame(
            "content_block_start",
            json!({
                "type": "content_block_start",
                "index": 0,
                "content_block": { "type": "thinking", "thinking": "", "signature": "" }
            }),
        )
    }

    /// One `thinking_delta`. Envelope matches `thinking_text_tool_use.txt`
    /// (CAPTURED), for example line 48. The `ReasoningCleanStop` script sends
    /// the captured run C text, all 15 deltas. The text of
    /// `ThinkingToolCallCleanStop` is arbitrary, and that script sends one
    /// delta.
    fn thinking_delta(thinking: &str) -> Vec<u8> {
        frame(
            "content_block_delta",
            json!({
                "type": "content_block_delta",
                "index": 0,
                "delta": { "type": "thinking_delta", "thinking": thinking }
            }),
        )
    }

    /// The `signature_delta` that closes a thinking block's content. Envelope
    /// matches `thinking_text_tool_use.txt` (CAPTURED), line 93. The fixture
    /// carries the fake value `SIG_FAKE_1`; so does this script. That value
    /// is not wire bytes: the fixture header records that it replaces the
    /// real signature.
    fn signature_delta(signature: &str) -> Vec<u8> {
        frame(
            "content_block_delta",
            json!({
                "type": "content_block_delta",
                "index": 0,
                "delta": { "type": "signature_delta", "signature": signature }
            }),
        )
    }

    /// `message_delta` — the one event that carries BOTH the stop reason and
    /// usage in a single frame. Matches `text_only.txt`'s and
    /// `eof_after_message_delta.txt`'s shared shape (both HAND-AUTHORED).
    /// The CAPTURED `message_delta` carries more keys; the header of
    /// `text_only.txt` lists them.
    ///
    /// This is the load-bearing difference from every other subject in this
    /// file: `stream.rs`'s `MessageDelta` arm emits `Usage` from the same
    /// event that buffers `stop_reason`, so this event is itself an
    /// observable edge a cancellation can gate on — unlike `openai_chat`/
    /// `litellm`, whose finish-reason chunk is silent and needs a *following*
    /// usage chunk to give `CancelAfterStopReason` something to gate on.
    fn message_delta(stop_reason: &str, output_tokens: u32) -> Vec<u8> {
        frame(
            "message_delta",
            json!({
                "type": "message_delta",
                "delta": { "stop_reason": stop_reason, "stop_sequence": null },
                "usage": { "output_tokens": output_tokens }
            }),
        )
    }

    /// `message_stop`, which flushes the buffered stop reason into `Finish`
    /// (`stream.rs`'s `MessageStop` arm). Matches `text_only.txt`.
    fn message_stop() -> Vec<u8> {
        frame("message_stop", json!({ "type": "message_stop" }))
    }

    /// `content_block_start` opening a `tool_use` block. Matches
    /// `parallel_tool_use.txt`'s and `tool_use_then_continuation.txt`'s
    /// tool-call events (both HAND-AUTHORED). The CAPTURED
    /// `thinking_text_tool_use.txt` (line 141) also carries
    /// `"caller":{"type":"direct"}`; this helper does not send it. The
    /// translator records `call_id`/`name` here but
    /// emits nothing — the name-carrying `ToolCallDelta` comes with the first
    /// `input_json_delta` — which is why `FragmentedToolName` is declined for
    /// this subject: the name arrives whole, in this one event.
    fn tool_use_start(call_id: &str, name: &str) -> Vec<u8> {
        tool_use_start_at(0, call_id, name)
    }

    /// [`tool_use_start`] for the block at `index`. In the thinking scenario
    /// the thinking block holds index 0, so the tool call sits at index 1.
    /// In the capture the tool call sits at index 2, after a text block
    /// (`thinking_text_tool_use.txt`, line 141). The keys are those of
    /// [`tool_use_start`], so this helper also drops `caller`.
    fn tool_use_start_at(index: u32, call_id: &str, name: &str) -> Vec<u8> {
        frame(
            "content_block_start",
            json!({
                "type": "content_block_start",
                "index": index,
                "content_block": { "type": "tool_use", "id": call_id, "name": name, "input": {} }
            }),
        )
    }

    /// The `content_block_start` of run C's `tool_use` block. It differs from
    /// [`tool_use_start_at`] in one key: run C also sends
    /// `"caller":{"type":"direct"}`.
    fn tool_use_start_with_caller_at(index: u32, call_id: &str, name: &str) -> Vec<u8> {
        frame(
            "content_block_start",
            json!({
                "type": "content_block_start",
                "index": index,
                "content_block": {
                    "type": "tool_use",
                    "id": call_id,
                    "name": name,
                    "input": {},
                    "caller": { "type": "direct" }
                }
            }),
        )
    }

    /// Run C's `message_start`, with every key and value of
    /// `thinking_text_tool_use.txt` line 39. [`message_start`] is not reused,
    /// because its `id`, `model` and `usage` are this module's own values.
    fn message_start_run_c() -> Vec<u8> {
        frame(
            "message_start",
            json!({
                "type": "message_start",
                "message": {
                    "model": "claude-sonnet-4-5-20250929",
                    "id": "msg_011ChGZ7F3NWW9KiPZPYPzWL",
                    "type": "message",
                    "role": "assistant",
                    "content": [],
                    "container": null,
                    "stop_reason": null,
                    "stop_sequence": null,
                    "stop_details": null,
                    "usage": {
                        "input_tokens": 687,
                        "cache_creation_input_tokens": 0,
                        "cache_read_input_tokens": 0,
                        "cache_creation": {
                            "ephemeral_5m_input_tokens": 0,
                            "ephemeral_1h_input_tokens": 0
                        },
                        "output_tokens": 3,
                        "service_tier": "standard",
                        "inference_geo": "not_available",
                        "speed": "standard"
                    },
                    "diagnostics": null
                }
            }),
        )
    }

    /// Run C's `message_delta`, with every key and value of
    /// `thinking_text_tool_use.txt` line 159. [`message_delta`] is not
    /// reused, because run C also sends `stop_details`, `container`, the
    /// input and cache counts, and `output_tokens_details`.
    fn message_delta_run_c() -> Vec<u8> {
        frame(
            "message_delta",
            json!({
                "type": "message_delta",
                "delta": {
                    "stop_reason": "tool_use",
                    "stop_sequence": null,
                    "stop_details": null,
                    "container": null
                },
                "usage": {
                    "input_tokens": 687,
                    "cache_creation_input_tokens": 0,
                    "cache_read_input_tokens": 0,
                    "output_tokens": 182,
                    "output_tokens_details": { "thinking_tokens": 68 }
                }
            }),
        )
    }

    /// The fifteen `thinking_delta` texts of run C, lines 48-90, in order.
    const RUN_C_THINKING: &[&str] = &[
        "The user wants",
        " to",
        " know the weather in Paris.",
        " They've",
        " specifically",
        " asked me to:\n1. First",
        " look up the city code for Paris",
        "\n2. Then get the weather using",
        " that code",
        "\n3. Think briefly before each step",
        "\n\nLet me start",
        " with",
        " step",
        " 1:",
        " looking up the city code for Paris.",
    ];

    /// The twelve `text_delta` texts of run C, lines 102-135, in order.
    const RUN_C_TEXT: &[&str] = &[
        "I'll help you get the weather for",
        " Paris. Let me start",
        " by looking up the city code.",
        "\n\n**",
        "Step 1: Looking up the city",
        " code for Paris**",
        "\nI",
        " need",
        " to use",
        " the lookup",
        "_city_code function with \"",
        "Paris\" as the city parameter.",
    ];

    /// The `tool_use` id of run C, line 141.
    const RUN_C_TOOL_USE_ID: &str = "toolu_017EB2o3zrXW8R73aqBPNiZi";

    /// The tool name of run C's call, line 141.
    const RUN_C_TOOL_NAME: &str = "lookup_city_code";

    /// Run C's one call: [`RUN_C_TOOL_NAME`] and the [`CAPTURED_ARGS`]
    /// values (lines 143-153) concatenated.
    const RUN_C_CALLS: &[ExpectedCall] = &[ExpectedCall {
        name: RUN_C_TOOL_NAME,
        args: "{\"city\": \"Paris\"}",
    }];

    /// One `input_json_delta`, carrying one fragment of a call's argument
    /// JSON. The tool scripts call it once for each [`CAPTURED_ARGS`] value,
    /// as the SMA-840 captures send them. See the module doc's "Tool scripts
    /// send the captured split `input_json_delta`" section.
    fn input_json_delta(partial_json: &str) -> Vec<u8> {
        input_json_delta_at(0, partial_json)
    }

    /// [`input_json_delta`] for the block at `index`.
    fn input_json_delta_at(index: u32, partial_json: &str) -> Vec<u8> {
        frame(
            "content_block_delta",
            json!({
                "type": "content_block_delta",
                "index": index,
                "delta": { "type": "input_json_delta", "partial_json": partial_json }
            }),
        )
    }

    /// The seven events of `fixtures/anthropic/zero_arg_tool_call.raw`, as
    /// `(event name, data payload)` pairs, byte for byte, in file order. Each
    /// payload keeps the spaces before its closing `}`. See the
    /// `ZeroArgToolCallCleanStop` arm of [`script`] for the provenance.
    const ZERO_ARG_EVENTS: [(&str, &str); 7] = [
        (
            "message_start",
            r##"{"type":"message_start","message":{"model":"claude-haiku-5-5","id":"msg_011ChNQ3Tog5cJ6yjFuLDgND","type":"message","role":"assistant","content":[],"container":null,"stop_reason":null,"stop_sequence":null,"stop_details":null,"usage":{"input_tokens":485,"cache_creation_input_tokens":0,"cache_read_input_tokens":0,"cache_creation":{"ephemeral_5m_input_tokens":0,"ephemeral_1h_input_tokens":0},"output_tokens":11,"service_tier":"standard","inference_geo":"global","speed":"standard"},"diagnostics":null}      }"##,
        ),
        (
            "content_block_start",
            r##"{"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"toolu_01RJqBrxUZ8pUUbgZAKYM8T5","name":"get_current_time","input":{},"caller":{"type":"direct"}}     }"##,
        ),
        ("ping", r##"{"type": "ping"}"##),
        (
            "content_block_delta",
            r##"{"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":""}    }"##,
        ),
        (
            "content_block_stop",
            r##"{"type":"content_block_stop","index":0}"##,
        ),
        (
            "message_delta",
            r##"{"type":"message_delta","delta":{"stop_reason":"tool_use","stop_sequence":null,"stop_details":null,"container":null},"usage":{"input_tokens":485,"cache_creation_input_tokens":0,"cache_read_input_tokens":0,"output_tokens":11,"output_tokens_details":{"thinking_tokens":0}}               }"##,
        ),
        ("message_stop", r##"{"type":"message_stop"  }"##),
    ];

    /// The one call of [`ZERO_ARG_EVENTS`]: the tool name and empty arguments.
    /// The wire sends one `partial_json` of `""`, which the floors read as
    /// `{}`.
    const ZERO_ARG_CALLS: &[ExpectedCall] = &[ExpectedCall {
        name: "get_current_time",
        args: "{}",
    }];

    /// The whole capture file, to check the script against.
    const ZERO_ARG_RAW: &[u8] = include_bytes!("../fixtures/anthropic/zero_arg_tool_call.raw");

    /// One captured SSE frame: `event: {event}\ndata: {payload}\n\n`. The
    /// `event:` line is kept, as in the file.
    fn captured_frame(event: &str, payload: &str) -> Vec<u8> {
        format!("event: {event}\ndata: {payload}\n\n").into_bytes()
    }

    /// The scripted response for one scenario, or `None` for
    /// `FragmentedToolName`, which this subject declines, or for a scenario
    /// that it has no capture for (uncaptured).
    ///
    /// Returning an `Option` rather than panicking on the declined or
    /// uncaptured scenarios is what lets [`Anthropic::encodes_stop_reason`] call this for *every*
    /// scenario and measure the bytes — see `bedrock::script`'s doc for the
    /// same pattern.
    fn script(scenario: Scenario) -> Option<Script> {
        // The four-event opening every text scenario shares: message_start,
        // the text block's content_block_start, and two deltas — so a
        // cancellation can be gated *between* two `TokenDelta`s rather than
        // before the first one.
        let opening = || {
            vec![
                message_start(),
                content_block_start_text(),
                text_delta("Hel"),
                text_delta("lo"),
            ]
        };
        // The opening plus a completed block and a populated message_delta.
        // Deliberately stops short of `message_stop` — callers that want it
        // append it themselves — which is what lets `CancelAfterStopReason`
        // gate right after this prefix without ever having to include the
        // event it is gating in front of.
        let through_stop = || {
            let mut chunks = opening();
            chunks.push(content_block_stop());
            chunks.push(message_delta("end_turn", 5));
            chunks
        };

        let (chunks, gate_after, ending) = match scenario {
            // Full stream: deltas, then message_delta (stop reason + usage in
            // one event), then message_stop, then a clean end of body — every
            // event `text_only.txt` sends, not a prefix of it. `stop_reason`
            // and `usage.output_tokens` are pulled from the fixture
            // (`message_delta("end_turn", 5)`); `message_start`'s `id`/
            // `model` and the delta text are this module's own values, not
            // the fixture's `"msg_01"`/`"claude-sonnet-4-6"`/`"Hello"`/
            // `" world"`.
            Scenario::CleanStop => {
                let mut chunks = through_stop();
                chunks.push(message_stop());
                (chunks, None, Ending::Clean)
            }
            // The exact SMA-531 shape: `message_delta` is observed, then the
            // body simply ends — no `message_stop` — so the translator's
            // buffered stop reason has to be flushed by the EOF path
            // (`MessageTranslator::finish`, called from `model.rs`'s
            // `None =>` arm). Matches `eof_after_message_delta.txt`'s event
            // sequence and `stop_reason`/`usage` values exactly; `id`/`model`
            // and delta text are this module's own values, same substitution
            // as `CleanStop` above.
            Scenario::TruncatedAfterStopReason => (through_stop(), None, Ending::Clean),
            // The body ends cleanly mid-generation: no `message_delta` ever
            // arrives, so no stop reason is ever observed. Envelope grounded
            // in `eof_mid_content_block.txt` (see the module doc's provenance
            // section for why the delta count itself is not transcribed).
            Scenario::TruncatedMidGeneration => (opening(), None, Ending::Clean),
            // Same prefix, but the connection is torn down without a
            // terminating chunk, so the client observes a transport error.
            // Prefix envelope grounded in `stream_error.txt`; see the module
            // doc's "Why `Ending::Abort`" section for why the fixture's own
            // in-band `error` frame is not transcribed into this script.
            Scenario::ErrorMidGeneration => (opening(), None, Ending::Abort),
            // The stop reason is buffered and *then* the body is aborted, so
            // the error path must not flush it as a `Finish`. Prefix envelope
            // grounded in `error_after_message_delta.txt`, through its own
            // `message_delta`; same note on `Ending::Abort` as above.
            Scenario::ErrorAfterStopReason => (through_stop(), None, Ending::Abort),
            // Three chunks go out — message_start, the text block's
            // content_block_start, and the first delta — and the server then
            // parks, holding the second delta back (`gate_after: Some(3)`
            // pauses *before* the chunk at index 3, per `server.rs`'s `feed`
            // loop). No `message_delta` anywhere in this script, so no stop
            // reason exists even after the gate releases.
            Scenario::CancelMidGeneration => (opening(), Some(3), Ending::Clean),
            // The stop reason AND its usage are both observed before the gate
            // — `message_delta` carries both in one event (see its doc), so
            // unlike `openai_chat`/`litellm` this scenario needs no *extra*
            // usage chunk appended for the gate to sit after.
            //
            // **This comment is the only guard for this scenario on this
            // subject.** The harness's own "at least one `Usage` observed"
            // floor (`assert_conforms`'s `floor_violation`) is VACUOUS here:
            // `message_start` above already emits a `Usage`
            // (`stream.rs`'s `MessageStart` arm), so that floor is satisfied
            // whether or not the gate sits in the right place — even a gate
            // placed before `message_delta` would still show a `Usage` from
            // `message_start`, and pass. `floor_violation`'s own doc records
            // this narrowing for `anthropic` explicitly.
            //
            // So the only thing standing between this scenario and silently
            // degrading into `CancelMidGeneration` under another name is
            // this: `gate_after` is a COUNT — `chunks.len()`, not a
            // hand-typed literal — read *after* `through_stop()` has pushed
            // `message_delta` onto `chunks`. That parks the server with
            // `message_delta` already sent and `message_stop` withheld
            // indefinitely, so cancellation truncates the stream strictly
            // after the stop reason was buffered.
            Scenario::CancelAfterStopReason => {
                let chunks = through_stop();
                let gate_after = chunks.len();
                (chunks, Some(gate_after), Ending::Clean)
            }
            // Declined — see `Anthropic::stream`.
            Scenario::FragmentedToolName => return None,
            // A whole tool call: the name arrives with the block start
            // (`tool_use_start`, the HAND-AUTHORED `parallel_tool_use.txt`
            // shape). The arguments arrive as the four `CAPTURED_ARGS`
            // values, one `input_json_delta` each, as the SMA-840 captures
            // send them (see the module doc's "Tool scripts send the captured
            // split `input_json_delta`" section). Then a `tool_use` stop
            // reason and its usage, then `message_stop`.
            Scenario::ToolCallCleanStop => {
                let mut chunks = vec![message_start(), tool_use_start(TOOL_USE_ID, TOOL_NAME)];
                chunks.extend(CAPTURED_ARGS.iter().map(|a| input_json_delta(a)));
                chunks.extend([
                    content_block_stop(),
                    message_delta("tool_use", 18),
                    message_stop(),
                ]);
                (chunks, None, Ending::Clean)
            }
            // A signed thinking block at index 0, then one tool call at
            // index 1, then a `tool_use` stop reason. Event shapes are
            // transcribed from `thinking_text_tool_use.txt`: the thinking
            // `content_block_start` with `"thinking":"","signature":""`, a
            // `thinking_delta`, the `signature_delta` with the fake value
            // `SIG_FAKE_1`, and the `content_block_stop`. The fixture also
            // holds a text block between the thinking block and the tool call.
            // This script leaves that block out, so the tool call takes the
            // next index (1). The `tool_use` events are the same as in
            // `ToolCallCleanStop`. The script also differs from the capture
            // in these ways: no `ping` event; one `thinking_delta`, not 15;
            // no `caller` key on the tool block. The four `input_json_delta`
            // values are the capture's own (`CAPTURED_ARGS`, lines 143-153),
            // at index 1 in place of 2. `message_start` and
            // `message_delta` use the HAND-AUTHORED `text_only.txt` keys,
            // not the capture's. The translator then emits a call-scoped
            // `ProviderData` position for the tool call, and assertion 8
            // checks it against the streamed call.
            Scenario::ThinkingToolCallCleanStop => {
                let mut chunks = vec![
                    message_start(),
                    thinking_start(),
                    thinking_delta("I need the weather."),
                    signature_delta("SIG_FAKE_1"),
                    content_block_stop(),
                    tool_use_start_at(1, TOOL_USE_ID, TOOL_NAME),
                ];
                chunks.extend(CAPTURED_ARGS.iter().map(|a| input_json_delta_at(1, a)));
                chunks.extend([
                    content_block_stop_at(1),
                    message_delta("tool_use", 18),
                    message_stop(),
                ]);
                (chunks, None, Ending::Clean)
            }
            // All of run C, `thinking_text_tool_use.txt` (the SMA-840 probe,
            // claude-sonnet-4-5; its signature is already replaced with
            // `SIG_FAKE_1` in the file). In wire order: `message_start`, the
            // thinking block at index 0 (start, 15 `thinking_delta`s, the
            // `signature_delta`, stop), the text block at index 1 (start, 12
            // `text_delta`s, stop), the `tool_use` block at index 2 (start, 4
            // `input_json_delta`s, the `CAPTURED_ARGS` values of lines 143-153,
            // stop), `message_delta` (`tool_use`),
            // `message_stop`. No delta is merged. The edits, all of them:
            //
            // 1. The `ping` event (lines 44-45) is dropped. The translator
            //    ignores it (`AnthropicEvent::Ping => {}`).
            // 2. Each payload is built with `json!`, so its key order and
            //    spacing can differ from the source. The keys and values are
            //    unchanged.
            // 3. The provenance header (lines 1-36) of the source file is not
            //    sent.
            Scenario::ReasoningCleanStop => {
                let mut chunks = vec![message_start_run_c(), thinking_start()];
                chunks.extend(RUN_C_THINKING.iter().map(|t| thinking_delta(t)));
                chunks.push(signature_delta("SIG_FAKE_1"));
                chunks.push(content_block_stop_at(0));
                chunks.push(content_block_start_text_at(1));
                chunks.extend(RUN_C_TEXT.iter().map(|t| text_delta_at(1, t)));
                chunks.push(content_block_stop_at(1));
                chunks.push(tool_use_start_with_caller_at(
                    2,
                    RUN_C_TOOL_USE_ID,
                    RUN_C_TOOL_NAME,
                ));
                chunks.extend(CAPTURED_ARGS.iter().map(|a| input_json_delta_at(2, a)));
                chunks.push(content_block_stop_at(2));
                chunks.push(message_delta_run_c());
                chunks.push(message_stop());
                (chunks, None, Ending::Clean)
            }
            // Not served — see the reason in `Anthropic::stream`.
            Scenario::ParallelToolCallCleanStop => return None,
            // `fixtures/anthropic/zero_arg_tool_call.raw`, CAPTURED from the
            // Anthropic Messages API (`https://api.anthropic.com/v1/messages`,
            // `anthropic-version: 2023-06-01`, `claude-haiku-5-5`) on
            // 2026-11-24 at 20:38:59 UTC, with one zero-argument tool and a
            // forced `tool_choice`. The wire sent one `input_json_delta` with
            // `partial_json: ""`. The `content_block_start` carries
            // `"input":{}`, which the translator ignores. The floors read the
            // blank arguments as `{}`. Seven events: `message_start`,
            // `content_block_start`, `ping`, `content_block_delta`,
            // `content_block_stop`, `message_delta` and `message_stop`.
            //
            // Edits made here: none to any payload. This script sends the
            // seven `event:` and `data:` payloads byte for byte
            // ([`ZERO_ARG_EVENTS`]). Each pair becomes one frame that ends
            // with a blank line.
            Scenario::ZeroArgToolCallCleanStop => (
                ZERO_ARG_EVENTS
                    .iter()
                    .map(|(event, payload)| captured_frame(event, payload))
                    .collect(),
                None,
                Ending::Clean,
            ),
        };

        Some(Script {
            content_type: "text/event-stream",
            chunks,
            gate_after,
            ending,
        })
    }

    /// The request driven through `Model::invoke`.
    ///
    /// Carries a tool definition for `ToolCallCleanStop`,
    /// `ThinkingToolCallCleanStop` and `ReasoningCleanStop` so the exchange is
    /// coherent — a response returning `tool_use` for a request that offered
    /// no tools would not be a coherent exchange. For
    /// `ReasoningCleanStop` the offered tool is the one that the script calls:
    /// run C's `lookup_city_code`. The request of run C is not in the repo, so
    /// only the tool name is taken from the capture.
    /// `FragmentedToolName` is this subject's other tool scenario, but it is
    /// declined and so never reaches this function.
    fn request(scenario: Scenario) -> ModelRequest {
        let mut req = ModelRequest::new();
        req.messages = vec![Item::UserMessage {
            content: vec![ContentPart::Text {
                text: "What is the weather in Paris?".into(),
            }],
        }];
        if scenario == Scenario::ReasoningCleanStop {
            req.tools = vec![ToolDef::new(
                RUN_C_TOOL_NAME,
                "Return the internal code for a city.",
                json!({
                    "type": "object",
                    "properties": { "city": { "type": "string" } },
                    "required": ["city"]
                }),
            )];
        }
        if matches!(
            scenario,
            Scenario::ToolCallCleanStop | Scenario::ThinkingToolCallCleanStop
        ) {
            req.tools = vec![ToolDef::new(
                TOOL_NAME,
                "Look up the current weather for a city.",
                json!({
                    "type": "object",
                    "properties": { "city": { "type": "string" } },
                    "required": ["city"]
                }),
            )];
        }
        if scenario == Scenario::ZeroArgToolCallCleanStop {
            // The tool and the forced choice of the capture.
            req.tools = vec![ToolDef::new(
                "get_current_time",
                "Return the current server time. Takes no arguments.",
                json!({ "type": "object", "properties": {} }),
            )];
            req.model_settings.tool_choice = Some(ToolChoice::Tool {
                name: "get_current_time".into(),
            });
        }
        req
    }

    /// Build a real `AnthropicModel` (Messages API) pointed at a local
    /// plain-HTTP endpoint.
    fn build_model_against(base_url: &str) -> AnthropicModel {
        AnthropicModel::messages(MODEL_ID)
            .api_key("sk-ant-conformance-test")
            .base_url(base_url)
            .build()
            .expect("anthropic model should build with an explicit api key and base url")
    }

    /// See the module-level docs on this file for the fixture provenance
    /// rules this subject follows.
    struct Anthropic;

    #[async_trait::async_trait]
    impl StreamUnderTest for Anthropic {
        fn name(&self) -> &'static str {
            "anthropic"
        }

        fn encodes_stop_reason(&self, scenario: Scenario) -> bool {
            // MEASURED from the bytes about to be served — deliberately not
            // `scenario.expects_stop_reason()`. See the crate's `declines`
            // module doc and `bedrock::encodes_stop_reason` for why that
            // restatement would make the harness's cross-check dead code.
            script(scenario)
                .map(|script| {
                    script
                        .chunks
                        .iter()
                        .any(|chunk| contains(chunk, STOP_REASON_MARKER))
                })
                .unwrap_or(false)
        }

        fn fixture_tool_name(&self) -> &'static str {
            TOOL_NAME
        }

        fn fixture_finish_reason(&self, scenario: Scenario) -> Option<FinishReason> {
            // Exhaustive on purpose, with no `_` arm — see
            // `bedrock::fixture_finish_reason` for why.
            match scenario {
                // `"end_turn"` maps to `FinishReason::Stop`
                // (`stream.rs`'s `finish_or_error`). Both of these scenarios
                // reach a `Finish`: `CleanStop` through `message_stop`,
                // `TruncatedAfterStopReason` through the EOF flush.
                Scenario::CleanStop | Scenario::TruncatedAfterStopReason => {
                    Some(FinishReason::Stop)
                }
                // `"tool_use"` maps to `FinishReason::ToolCalls` when no
                // structured-output synthesis is in play.
                // Run C's `message_delta` carries `"tool_use"` too.
                Scenario::ToolCallCleanStop
                | Scenario::ThinkingToolCallCleanStop
                | Scenario::ReasoningCleanStop
                | Scenario::ZeroArgToolCallCleanStop => Some(FinishReason::ToolCalls),
                // Truncated, errored and cancelled streams must withhold
                // `Finish` entirely.
                Scenario::TruncatedMidGeneration
                | Scenario::ErrorMidGeneration
                | Scenario::ErrorAfterStopReason
                | Scenario::CancelMidGeneration
                | Scenario::CancelAfterStopReason => None,
                // Declined or uncaptured; never reached.
                Scenario::FragmentedToolName | Scenario::ParallelToolCallCleanStop => None,
            }
        }

        fn fixture_channel_order(&self, scenario: Scenario) -> Option<&'static [Channel]> {
            // Exhaustive on purpose, with no `_` arm — see
            // `bedrock::fixture_finish_reason` for why.
            match scenario {
                // Read from run C (`thinking_text_tool_use.txt`): the thinking
                // block at index 0, the text block at index 1, the `tool_use`
                // block at index 2.
                Scenario::ReasoningCleanStop => {
                    Some(&[Channel::Reasoning, Channel::Text, Channel::Tool])
                }
                Scenario::CleanStop
                | Scenario::TruncatedAfterStopReason
                | Scenario::TruncatedMidGeneration
                | Scenario::ErrorMidGeneration
                | Scenario::ErrorAfterStopReason
                | Scenario::CancelMidGeneration
                | Scenario::CancelAfterStopReason
                | Scenario::FragmentedToolName
                | Scenario::ToolCallCleanStop
                | Scenario::ThinkingToolCallCleanStop
                | Scenario::ParallelToolCallCleanStop
                | Scenario::ZeroArgToolCallCleanStop => None,
            }
        }

        fn fixture_calls(&self, scenario: Scenario) -> &'static [ExpectedCall] {
            // Exhaustive on purpose, with no `_` arm — see
            // `bedrock::fixture_finish_reason` for why.
            match scenario {
                // Run C's one call (`thinking_text_tool_use.txt`, lines
                // 106-118).
                Scenario::ReasoningCleanStop => RUN_C_CALLS,
                // The one call of the zero-argument capture.
                Scenario::ZeroArgToolCallCleanStop => ZERO_ARG_CALLS,
                Scenario::CleanStop
                | Scenario::TruncatedAfterStopReason
                | Scenario::TruncatedMidGeneration
                | Scenario::ErrorMidGeneration
                | Scenario::ErrorAfterStopReason
                | Scenario::CancelMidGeneration
                | Scenario::CancelAfterStopReason
                | Scenario::FragmentedToolName
                | Scenario::ToolCallCleanStop
                | Scenario::ThinkingToolCallCleanStop
                | Scenario::ParallelToolCallCleanStop => &[],
            }
        }

        async fn stream(&self, scenario: Scenario, cancel: CancellationToken) -> Outcome {
            // The name is set once, whole, in the `content_block_start`
            // `tool_use` block. That is so in each committed fixture with a
            // `tool_use` block, including the three SMA-840 captures. No
            // `ContentBlockDelta` variant (`sse.rs`) has a name field.
            if scenario == Scenario::FragmentedToolName {
                return Outcome::Declined("name arrives whole in content_block_start");
            }
            // Parallel tool calls can occur on this wire, but
            // `parallel_tool_use.txt` has no capture behind it (SMA-859).
            if scenario == Scenario::ParallelToolCallCleanStop {
                return Outcome::Uncaptured("no captured parallel tool-call stream");
            }

            let script =
                script(scenario).expect("a scenario this subject serves must have a script");
            let mut server = PacedServer::start(script).await;
            let gate = server.take_gate();
            let model = build_model_against(&server.base_url());

            let stream = model
                .invoke(request(scenario), cancel)
                .await
                .expect("anthropic invoke should reach the local paced server");

            Outcome::Served { stream, gate }
        }
    }

    /// Whether `haystack` contains `needle` as a contiguous byte run.
    /// Duplicated per subject module by design — see `bedrock::contains`.
    fn contains(haystack: &[u8], needle: &[u8]) -> bool {
        haystack.windows(needle.len()).any(|w| w == needle)
    }

    /// The soundness claim behind [`Anthropic::encodes_stop_reason`]'s
    /// substring scan: [`STOP_REASON_MARKER`] occurs in `message_delta`'s
    /// populated `stop_reason` and in no other frame this subject builds —
    /// in particular, not in `message_start`, which carries a literal
    /// `"stop_reason":null` (see its doc comment), the Anthropic-specific
    /// pitfall the task brief calls out.
    ///
    /// This is also the only detector for `ErrorAfterStopReason` degrading
    /// into `ErrorMidGeneration`. Their `ModelEvent` streams are not
    /// literally identical today — both open with `message_start`, which
    /// always emits a `Usage` (`stream.rs`'s `MessageStart` arm), and
    /// `ErrorAfterStopReason` additionally observes a second `Usage` from
    /// `message_delta` that `ErrorMidGeneration` never sends — but neither
    /// `classify` nor `floor_violation` (`assert_conforms`'s two event-level
    /// checks) counts `Usage` events or otherwise distinguishes the two:
    /// both see "no `Finish`, one `Err`" and pass identically either way. So
    /// if `through_stop()` ever lost its `message_delta`, `ErrorAfterStopReason`
    /// would become indistinguishable from `ErrorMidGeneration` in every way
    /// the suite's event-level checks can see — this byte-level scan on the
    /// *script*, not the translated events, is what would still notice.
    #[test]
    fn scan_finds_only_a_populated_stop_reason() {
        assert!(
            contains(&message_delta("end_turn", 5), STOP_REASON_MARKER),
            "the message_delta frame must carry the marker, or the scan measures nothing"
        );
        assert!(
            contains(&message_delta_run_c(), STOP_REASON_MARKER),
            "run C's message_delta frame must carry the marker, or the scan measures nothing"
        );

        for (name, frame) in [
            ("message_start (stop_reason: null)", message_start()),
            ("content_block_start (text)", content_block_start_text()),
            ("content delta", text_delta("Hel")),
            ("content_block_stop", content_block_stop()),
            ("message_stop", message_stop()),
            ("tool_use start", tool_use_start(TOOL_USE_ID, TOOL_NAME)),
            ("input_json_delta", input_json_delta(CAPTURED_ARGS[1])),
            (
                "zero-arg input_json_delta",
                captured_frame(ZERO_ARG_EVENTS[3].0, ZERO_ARG_EVENTS[3].1),
            ),
            ("thinking start", thinking_start()),
            ("thinking delta", thinking_delta("I need the weather.")),
            ("signature delta", signature_delta("SIG_FAKE_1")),
            (
                "run C message_start (stop_reason: null)",
                message_start_run_c(),
            ),
            ("text block start at 1", content_block_start_text_at(1)),
            ("text delta at 1", text_delta_at(1, "Paris")),
            (
                "run C tool_use start",
                tool_use_start_with_caller_at(2, RUN_C_TOOL_USE_ID, RUN_C_TOOL_NAME),
            ),
        ] {
            assert!(
                !contains(&frame, STOP_REASON_MARKER),
                "{name} contains the populated-stop_reason marker, so the scan would report a \
                 stop reason for scripts that encode none. In particular, message_start carries \
                 a literal \"stop_reason\":null, and the marker must not match that null value."
            );
        }
    }

    /// The script is the capture, byte for byte.
    ///
    /// Provenance: CAPTURED from the Anthropic Messages API at
    /// `https://api.anthropic.com/v1/messages` with the header
    /// `anthropic-version: 2023-06-01`. The model is `claude-haiku-5-5`. The
    /// capture ran on 2026-11-24 at 20:38:59 UTC. The request is
    /// `tests/provider-stream-conformance/fixtures/anthropic/zero_arg_tool_call.request.json`.
    /// The file is `fixtures/anthropic/zero_arg_tool_call.raw`. It is 1364 bytes. Its
    /// SHA-256 is
    /// `b6114d956d7d80a6b315fcd2f6866fff8c776e9ebbed846c8ab7ccf9093dd86e`.
    /// Finding: the wire sends one `input_json_delta` with `partial_json: ""`.
    /// The `content_block_start` carries `"input":{}`.
    ///
    /// Removed: nothing. Added: nothing. Changed: nothing.
    #[test]
    fn zero_arg_script_is_the_capture_byte_for_byte() {
        let script = script(Scenario::ZeroArgToolCallCleanStop).expect("served");
        assert_eq!(script.chunks.len(), 7);
        assert_eq!(script.chunks.concat(), ZERO_ARG_RAW);
    }

    #[tokio::test]
    async fn conforms() {
        assert_conforms(&Anthropic).await;
    }
}

/// The `Model::invoke` stream contract, checked against the Gemini
/// `streamGenerateContent` (SSE) backend.
///
/// This is the one subject in this suite whose driver has **two distinct
/// terminal arms**: `model.rs`'s event loop flushes `translator.finish()` from
/// both a `[DONE]` sentinel arm and a bare-EOF arm
/// (`crates/paigasus-helikon-providers-gemini/src/model.rs:149-158`). `S2`
/// (`TruncatedAfterStopReason`) is what makes that non-vacuous: `CleanStop`
/// below ends with `[DONE]`, `TruncatedAfterStopReason` ends at bare EOF with
/// no `[DONE]` anywhere in its script, and Step 4 of this subject's
/// registration proved by mutation that disabling one arm fails exactly one of
/// the two scenarios while leaving the other green — see the task report for
/// the recorded result.
///
/// # Fixture provenance
///
/// The SSE this module draws from lives in two places: inline in
/// `crates/paigasus-helikon-providers-gemini/tests/gemini_streaming.rs`, and,
/// for the tool-call shape, in this module's own doc comments (see "Live
/// capture" below). Every envelope shape is transcribed from one of these,
/// cited on the helper that builds it. The Gemini crate also has a fixture
/// directory since SMA-827 (PR #373): `tests/fixtures/parallel_signed.sse`,
/// CAPTURED (gemini-3.8-flash, 2026-11-20). No helper here is drawn from it.
///
/// - `crates/paigasus-helikon-providers-gemini/tests/gemini_streaming.rs:67-69`
///   (`truncated_stream_no_finish`) — HAND-AUTHORED, source not recorded
///   (SMA-449, PR #122). Grounds the SCRIPT shape of
///   `TruncatedMidGeneration` (one delta, then EOF, no `finishReason`).
/// - `crates/paigasus-helikon-providers-gemini/tests/gemini_streaming.rs:90-95`
///   (`multi_chunk_usage_emits_usage_per_chunk`) — HAND-AUTHORED, source not
///   recorded (SMA-449, PR #122). Both of its two chunks
///   carry `usageMetadata`, including the first, which has no `finishReason`.
///   Grounds [`text_delta`]'s per-chunk usage and, together with `:42`
///   (`text_then_finish`, also HAND-AUTHORED), [`stop_chunk`]'s combined
///   text+`finishReason`+`usageMetadata` event.
/// - `crates/paigasus-helikon-providers-gemini/tests/fixtures/parallel_signed.sse`
///   — "Capture 1" of the SMA-827 design
///   (`docs/superpowers/specs/2026-11-20-sma-827-gemini-thought-signatures-design.md:299`).
///   `ParallelToolCallCleanStop` sends all four of its payloads byte for byte
///   ([`PARALLEL_SIGNED`]); the script arm lists the edits.
/// - `tests/provider-stream-conformance/fixtures/gemini/zero_arg_tool_call.raw`
///   — CAPTURED (gemini-3.8-flash, 2026-11-24). `ZeroArgToolCallCleanStop`
///   sends both of its payloads byte for byte ([`ZERO_ARG_CHUNKS`]), with CRLF
///   frame endings.
///
/// # Live capture — `ToolCallCleanStop`
///
/// Label: CAPTURED (gemini-3.6-flash, 2026-08-20). This is not the capture in
/// `parallel_signed.sse`.
///
/// When this subject was registered, the `functionCall` shape had no
/// `tests/`-level capture anywhere else in the repo (a subject-registration
/// review confirmed this and ruled that the
/// `bedrock`-style "derive from the translator's own unit tests" fallback
/// this module originally used does not transfer here — see the task report's
/// appended fix-report section for the full account of that ruling). Sven
/// captured a real streaming turn to settle it:
///
/// ```text
/// CAPTURED against the real Gemini Developer API, `streamGenerateContent?alt=sse`,
/// model `gemini-3.6-flash`, 2026-08-20. Prompt offered one `get_weather(city)`
/// tool; the model called it. Two SSE events, verbatim structure (the
/// `thoughtSignature` value is a ~700-character opaque base64 blob, elided
/// below to `<elided>`; every other key and value is exactly as captured):
///
/// data: {"candidates":[{"content":{"parts":[{"functionCall":{"name":"get_weather",
///   "args":{"city":"Berlin"},"id":"call_1285087"},"thoughtSignature":"<elided>"}],
///   "role":"model"},"index":0}],"usageMetadata":{"promptTokenCount":58,
///   "candidatesTokenCount":16,"totalTokenCount":150,"promptTokensDetails":
///   [{"modality":"TEXT","tokenCount":58}],"thoughtsTokenCount":76,
///   "serviceTier":"standard"},"modelVersion":"gemini-3.6-flash",
///   "responseId":"tR-HasqFGurtxN8PrpzvwAw"}
///
/// data: {"candidates":[{"content":{"parts":[{"text":""}],"role":"model"},
///   "finishReason":"STOP","index":0}],"usageMetadata":{"promptTokenCount":58,
///   "candidatesTokenCount":16,"totalTokenCount":150,"promptTokensDetails":
///   [{"modality":"TEXT","tokenCount":58}],"thoughtsTokenCount":76,
///   "serviceTier":"standard"},"modelVersion":"gemini-3.6-flash",
///   "responseId":"tR-HasqFGurtxN8PrpzvwAw"}
///
/// Body ended at bare EOF — no `[DONE]` sentinel in this capture.
/// ```
///
/// SMA-827 (PR #373) later added a second capture with `functionCall` parts,
/// `parallel_signed.sse`. It agrees with points 1-3 below: each call has an
/// `id`, `"STOP"` arrives on a separate last chunk with
/// `"parts":[{"text":""}]`, and `thoughtSignature` is a sibling of
/// `functionCall`.
///
/// This settles the question the module previously flagged as open:
///
/// 1. **`functionCall.id` IS present in real traffic** —
///    `"id":"call_1285087"`. `stream.rs`'s `fc_{index}` synthesis
///    (`stream.rs:42-46`) is a fallback for the case a chunk omits it, not the
///    common path for this model. [`tool_call_chunk`] scripts `id` as present,
///    now capture-verified rather than merely plausible.
/// 2. **The `functionCall` chunk carries no `finishReason`.** `"STOP"` arrives
///    on a *separate, later* chunk, alongside an empty text part
///    (`"parts":[{"text":""}]`) — not the empty-`content` shape this module
///    guessed at before the capture existed. [`stop_chunk`] (already used for
///    the text scenarios) reproduces this part shape when called with
///    `text: ""` (it drops `role`, `index`, `modelVersion` and `responseId`,
///    and trims `usageMetadata`), so `ToolCallCleanStop`'s second chunk
///    reuses it directly rather than needing its own helper — see
///    [`stop_chunk`]'s doc.
/// 3. **`thoughtSignature` is a real field**, a sibling of `functionCall`
///    inside the `Part` object (not nested inside `functionCall` itself).
///    `sse.rs`'s `Part` struct has no `deny_unknown_fields` attribute (checked:
///    none of `sse.rs`'s structs do), so an unrecognised key is silently
///    ignored by serde rather than rejected — [`tool_call_chunk`] keeps this
///    field, both for fidelity to the capture and because its presence is a
///    small, free regression check that a real-world extra field does not
///    break parsing. Its value is a short synthetic placeholder, not the
///    captured ~700-character blob — nothing under test inspects its content
///    or length, only that the key is present with some string value,
///    matching this module's established "arbitrary content over a grounded
///    envelope" convention (e.g. [`text_delta`]'s "Hel"/"lo").
///
/// # `usageMetadata` appears on every chunk — corrected from an earlier draft
///
/// **This module previously confined `usageMetadata` to the stop-reason
/// chunk alone**, specifically so the harness's "at least one `Usage`
/// observed" floor for `CancelAfterStopReason` would be non-vacuous. The live
/// capture above contradicts that shape directly: its first chunk carries
/// `usageMetadata` despite having no `finishReason` at all.
/// `parallel_signed.sse` shows the same: each of its four chunks carries
/// `usageMetadata`. So do both chunks of `zero_arg_tool_call.raw`. In all three recorded captures, usage is on every chunk, not
/// only the terminal one. `multi_chunk_usage_emits_usage_per_chunk`'s first
/// chunk (already cited above) has the same shape, but it is HAND-AUTHORED
/// and is not evidence of wire behaviour. [`text_delta`] now sets
/// `usageMetadata` on every call, matching the captures.
///
/// **Consequence, accepted rather than fought:** the `CancelAfterStopReason`
/// floor is now vacuous for this subject, the same way it already is for
/// `anthropic` (whose `message_start` emits `Usage` unconditionally) — a gate
/// placed anywhere in this script, even before `stop_chunk`, would still show
/// a `Usage` from the first `text_delta` and pass the floor trivially. The
/// guarantee that this scenario is genuinely testing cancellation *after* the
/// stop reason was buffered therefore comes entirely from the fixture's own
/// provenance — `gate_after` being a *count* read after `through_stop()` has
/// pushed `stop_chunk` — documented at the scenario site in `script`, not
/// from the harness floor. See that match arm's comment, and `anthropic`'s
/// `CancelAfterStopReason` arm for the same gate-placement technique against
/// the same shape of floor-vacuity narrowing (message-level, not chunk-level,
/// since anthropic's unconditional `Usage` comes from one event —
/// `message_start` — rather than every chunk).
///
/// # Why `FragmentedToolName` is declined
///
/// `StreamTranslator::consume`'s `functionCall` arm (`stream.rs:40-52`) reads
/// the whole `name` out of one `Part` in one event — there is no second event
/// a name fragment could arrive on, the same reason `bedrock` and `anthropic`
/// decline this scenario. The pinned reason,
/// `"name arrives whole in functionCall"`, is asserted verbatim against
/// `DECLINED` in `declines.rs`.
mod gemini {
    use super::*;
    use paigasus_helikon_providers_gemini::GeminiModel;

    /// The model id `build_model_against` requests. Uses the id the crate
    /// docs name. `PacedServer` ignores the request, so this id only has to be
    /// one the crate accepts. It is independent of the model the live capture
    /// happened to be taken against (`gemini-3.6-flash` — see the module doc's
    /// "Live capture" section).
    const MODEL_ID: &str = "gemini-3.8-flash";

    /// The tool name every tool-call fixture declares.
    const TOOL_NAME: &str = "get_weather";

    /// `id` for the single tool call in the tool fixture. The live capture
    /// (module doc, "Live capture") confirms real traffic sends a
    /// `functionCall.id`, settling that this key belongs in the fixture at
    /// all — but this specific value is not claimed to match any real id
    /// format (the capture's was `"call_1285087"`), and nothing under test
    /// inspects it. Fixed purely so failure output is stable and greppable.
    const TOOL_CALL_ID: &str = "fc_conformance_0";

    /// Synthetic stand-in for the real `thoughtSignature` value. See the
    /// module doc's "Live capture" point 3 for why the field is kept but its
    /// content is not.
    const THOUGHT_SIGNATURE: &str = "thoughtsig_conformance_0";

    /// The substrings of which one appears in a chunk if and only if it
    /// carries a *populated* `finishReason`. There are two forms: the compact
    /// one that `json!` writes, and the spaced one that Capture 1 writes
    /// (`"finishReason": "STOP"`), which `ParallelToolCallCleanStop` sends
    /// byte for byte.
    ///
    /// Unlike `openai_chat`/`litellm`, Gemini has no explicit-`null` pitfall to
    /// exclude: `Candidate::finish_reason` (`sse.rs:23`) is `#[serde(default)]`
    /// `Option<String>`, so an ordinary content chunk omits the key entirely
    /// rather than nulling it (see [`text_delta`]). The marker still scans for
    /// the opening quote of a *string* value, matching the task brief's
    /// instruction and the sibling subjects' convention.
    /// `scan_finds_only_a_populated_finish_reason` is the guard that keeps
    /// this true against an ordinary content-only chunk — the only detector
    /// for `ErrorAfterStopReason` degrading into `ErrorMidGeneration` should
    /// `stop_chunk` ever be dropped from its script; see that scenario's own
    /// comment in `script` for exactly how their observable events differ as
    /// currently scripted.
    const FINISH_REASON_MARKERS: [&[u8]; 2] = [b"\"finishReason\":\"", b"\"finishReason\": \""];

    /// Whether `chunk` contains one of [`FINISH_REASON_MARKERS`].
    fn has_finish_reason(chunk: &[u8]) -> bool {
        FINISH_REASON_MARKERS
            .iter()
            .any(|marker| contains(chunk, marker))
    }

    /// The four `data:` payloads of `parallel_signed.sse`
    /// (`crates/paigasus-helikon-providers-gemini/tests/fixtures/`, lines 1,
    /// 3, 5 and 7), byte for byte. See the `ParallelToolCallCleanStop` arm of
    /// [`script`] for the provenance and the edits.
    const PARALLEL_SIGNED: [&str; 4] = [
        r#"{"candidates": [{"content": {"parts": [{"functionCall": {"name": "search","args": {"q": "cats"},"id": "call_16945647804962170957"},"thoughtSignature":"SIG_A"}],"role": "model"},"index": 0}],"usageMetadata": {"promptTokenCount": 109,"candidatesTokenCount": 14,"totalTokenCount": 291,"promptTokensDetails": [{"modality": "TEXT","tokenCount": 109}],"thoughtsTokenCount": 168,"serviceTier": "standard"},"modelVersion": "gemini-3.8-flash","responseId": "YM4Aa9-2PNSckdUP3pjG4AY"}"#,
        r#"{"candidates": [{"content": {"parts": [{"functionCall": {"name": "weather","args": {"city": "Paris"},"id": "call_16945647804962169654"}}],"role": "model"},"index": 0}],"usageMetadata": {"promptTokenCount": 109,"candidatesTokenCount": 28,"totalTokenCount": 305,"promptTokensDetails": [{"modality": "TEXT","tokenCount": 109}],"thoughtsTokenCount": 168,"serviceTier": "standard"},"modelVersion": "gemini-3.8-flash","responseId": "YM4Aa9-2PNSckdUP3pjG4AY"}"#,
        r#"{"candidates": [{"content": {"parts": [{"functionCall": {"name": "weather","args": {"city": "Rome"},"id": "call_16945647804962169841"}}],"role": "model"},"index": 0}],"usageMetadata": {"promptTokenCount": 109,"candidatesTokenCount": 42,"totalTokenCount": 319,"promptTokensDetails": [{"modality": "TEXT","tokenCount": 109}],"thoughtsTokenCount": 168,"serviceTier": "standard"},"modelVersion": "gemini-3.8-flash","responseId": "YM4Aa9-2PNSckdUP3pjG4AY"}"#,
        r#"{"candidates": [{"content": {"parts": [{"text": ""}],"role": "model"},"finishReason": "STOP","index": 0}],"usageMetadata": {"promptTokenCount": 109,"candidatesTokenCount": 42,"totalTokenCount": 319,"promptTokensDetails": [{"modality": "TEXT","tokenCount": 109}],"thoughtsTokenCount": 168,"serviceTier": "standard"},"modelVersion": "gemini-3.8-flash","responseId": "YM4Aa9-2PNSckdUP3pjG4AY"}"#,
    ];

    /// The calls of [`PARALLEL_SIGNED`], in wire order, with the arguments
    /// exactly as captured.
    const PARALLEL_CALLS: &[ExpectedCall] = &[
        ExpectedCall {
            name: "search",
            args: r#"{"q": "cats"}"#,
        },
        ExpectedCall {
            name: "weather",
            args: r#"{"city": "Paris"}"#,
        },
        ExpectedCall {
            name: "weather",
            args: r#"{"city": "Rome"}"#,
        },
    ];

    /// One SSE frame around a captured payload, sent byte for byte:
    /// `data: {payload}\n\n`.
    fn captured_frame(payload: &str) -> Vec<u8> {
        format!("data: {payload}\n\n").into_bytes()
    }

    /// One SSE frame around a payload of `zero_arg_tool_call.raw`, sent byte
    /// for byte: `data: {payload}\r\n\r\n`. That capture has CRLF line
    /// endings on the wire.
    fn captured_frame_crlf(payload: &str) -> Vec<u8> {
        format!("data: {payload}\r\n\r\n").into_bytes()
    }

    /// The two `data:` payloads of `fixtures/gemini/zero_arg_tool_call.raw`,
    /// byte for byte, in file order. The first keeps its real
    /// `thoughtSignature`. See the `ZeroArgToolCallCleanStop` arm of [`script`]
    /// for the provenance.
    const ZERO_ARG_CHUNKS: [&str; 2] = [
        r##"{"candidates": [{"content": {"parts": [{"functionCall": {"name": "get_current_time","args": {},"id": "call_15312425021673810944"},"thoughtSignature": "EvsCCvgCAWkUfRN4KWAsTtHg7IyaigsGASIrvbzyixovA+E9Y8pnO095GcTFcaNlKEXY5BqWZ0c1jBr+pb8s9TD0MQPcyPGSfn9SmnEVVO4e7Vul2wJRYPsEvBDxs6YjgcUAeeolbaBfk2He9U3aworlN6hxNnxCq+1ZcTJMIF0EbMd9W1sDIc95A9GAKAr0Av0BNwAX8RS9xvsv2sXv8mwSx2Rns2Oj7tNfnwp0C7Jk6OT8Y6x7yeZA9F0aL6FK4mJG2tN4Ft8jAHAJkj1/TOiTcvIdfc7sJc4H864lYnW88k+jrTwYkKzM3L9aJ7bfyhdc9taOyUTNMK+AL14GxlPvDTMWgi2Ez6IVURMUmgzS7sMcgwd8HJnDeFQT+7yFwjDRt+yFkJryNa3PalvCvvIPBCJWEqF4TjuSuQoK4LSio634kCp41xZZjkkZN0NJ1BPCeko8yjeW390rJyfJBs1ceiFIMAD/ek+ralcNR3wg47AZEhM01lkq+cwuLw=="}],"role": "model"},"index": 0}],"usageMetadata": {"promptTokenCount": 39,"candidatesTokenCount": 12,"totalTokenCount": 99,"promptTokensDetails": [{"modality": "TEXT","tokenCount": 39}],"thoughtsTokenCount": 48,"serviceTier": "standard"},"modelVersion": "gemini-3.8-flash","responseId": "P_4Fa_fCD-qG28oPjou6gAQ"}"##,
        r##"{"candidates": [{"content": {"parts": [{"text": ""}],"role": "model"},"finishReason": "STOP","index": 0}],"usageMetadata": {"promptTokenCount": 39,"candidatesTokenCount": 12,"totalTokenCount": 99,"promptTokensDetails": [{"modality": "TEXT","tokenCount": 39}],"thoughtsTokenCount": 48,"serviceTier": "standard"},"modelVersion": "gemini-3.8-flash","responseId": "P_4Fa_fCD-qG28oPjou6gAQ"}"##,
    ];

    /// The one call of [`ZERO_ARG_CHUNKS`]: the tool name and empty arguments.
    /// The wire sends `"args": {}`, not a missing `args`.
    const ZERO_ARG_CALLS: &[ExpectedCall] = &[ExpectedCall {
        name: "get_current_time",
        args: "{}",
    }];

    /// The whole capture file, to check the script against.
    const ZERO_ARG_RAW: &[u8] = include_bytes!("../fixtures/gemini/zero_arg_tool_call.raw");

    /// One SSE frame: `data: {payload}\n\n`. All three recorded Gemini captures
    /// (the module doc's "Live capture", `parallel_signed.sse` and
    /// `zero_arg_tool_call.raw`) have bare
    /// `data:` frames with no `event:` line, and so does every inline body
    /// cited above.
    fn frame(payload: serde_json::Value) -> Vec<u8> {
        format!("data: {payload}\n\n").into_bytes()
    }

    /// One text delta, carrying `usageMetadata`. Matches
    /// `multi_chunk_usage_emits_usage_per_chunk`'s first chunk
    /// (`crates/paigasus-helikon-providers-gemini/tests/gemini_streaming.rs:91-92`,
    /// HAND-AUTHORED): a content-only candidate (no `finishReason`) with
    /// `usageMetadata` as a top-level sibling of `candidates` — see the module
    /// doc's "`usageMetadata`
    /// appears on every chunk" section for why this replaced an earlier,
    /// usage-free version of this helper.
    fn text_delta(text: &str, prompt_tokens: u32, candidates_tokens: u32) -> Vec<u8> {
        frame(json!({
            "candidates": [{
                "content": { "parts": [ { "text": text } ] }
            }],
            "usageMetadata": {
                "promptTokenCount": prompt_tokens,
                "candidatesTokenCount": candidates_tokens
            }
        }))
    }

    /// A chunk carrying a `content.parts` text part, a populated
    /// `finishReason`, AND `usageMetadata` in the SAME event — Gemini's
    /// defining shape (task brief point 2): there is no separate trailing
    /// usage event the way there is for `openai_chat`/`litellm`. Both
    /// recorded captures show the same: usage rides on each chunk (module doc,
    /// "`usageMetadata` appears on every chunk"). Matches
    /// `gemini_streaming.rs`'s `text_then_finish` fixture (`:42`) and
    /// `multi_chunk_usage_emits_usage_per_chunk`'s second chunk (`:93-94`)
    /// when called with non-empty `text`. Both bodies are HAND-AUTHORED.
    ///
    /// Called with `text: ""` this is ALSO the shape `ToolCallCleanStop`'s
    /// terminal chunk needs — the live capture (module doc, "Live capture"
    /// point 2) shows the stop-reason chunk after a tool call carries an
    /// *empty* text part (`"parts":[{"text":""}]`), not an absent `content`
    /// key, which is exactly what this helper already produces for
    /// `text: ""`. Reusing it here rather than adding a second, near-identical
    /// helper is itself grounded by the capture, not merely convenient.
    fn stop_chunk(text: &str, reason: &str, prompt_tokens: u32, candidates_tokens: u32) -> Vec<u8> {
        frame(json!({
            "candidates": [{
                "content": { "parts": [ { "text": text } ] },
                "finishReason": reason
            }],
            "usageMetadata": {
                "promptTokenCount": prompt_tokens,
                "candidatesTokenCount": candidates_tokens
            }
        }))
    }

    /// The `[DONE]` sentinel `model.rs`'s driver compares against verbatim
    /// (`event.data == "[DONE]"`,
    /// `crates/paigasus-helikon-providers-gemini/src/model.rs:155`).
    /// Synthetic, not captured.
    ///
    /// No committed fixture anywhere in the repo exercises this arm — a
    /// repo-wide grep for the literal string `[DONE]` under
    /// `crates/paigasus-helikon-providers-gemini/` turns up only the driver's
    /// own comparison and a header note in `parallel_signed.sse` (which says
    /// the file has no `[DONE]` line), never a captured or scripted SSE
    /// frame, and the live capture (module doc, "Live capture") itself ended
    /// at bare EOF with no `[DONE]` either. `CleanStop` below is the first
    /// thing in this codebase that ever sends Gemini a `[DONE]` sentinel and
    /// checks what the driver does with it. Grounded directly in the driver's
    /// own string comparison rather than a capture; the bytes are the same
    /// `data: [DONE]\n\n`
    /// terminator `openai_chat` and `litellm` send, since `model.rs` checks
    /// nothing about the frame besides its `data` field.
    fn done() -> Vec<u8> {
        b"data: [DONE]\n\n".to_vec()
    }

    /// `id`, the whole function `name`, the whole argument JSON, and
    /// `thoughtSignature`, all in one `functionCall` part, plus
    /// `usageMetadata`. In all three recorded captures each `functionCall` part
    /// arrives whole in one chunk, and the translator reads it from one part
    /// (see the module doc's "why `FragmentedToolName` is declined" section).
    ///
    /// # Provenance
    ///
    /// Transcribed from the live capture (module doc, "Live capture"
    /// section) — `id`/`name`/`args`/`thoughtSignature` keys and their
    /// nesting match the captured bytes exactly (`thoughtSignature`'s VALUE
    /// is a short synthetic placeholder, not the captured blob — see
    /// [`THOUGHT_SIGNATURE`]). No `finishReason` key, matching the capture's
    /// first chunk. `sse.rs`'s `Part`/`FunctionCall` structs
    /// (`#[serde(rename_all = "camelCase")]`) independently confirm the wire
    /// key is `functionCall` and its modelled fields are `id`/`name`/`args`.
    ///
    /// Changed from the capture: drops `index`, `role`, `modelVersion` and
    /// `responseId`, and trims `usageMetadata` to two counts.
    fn tool_call_chunk(
        call_id: &str,
        name: &str,
        args: serde_json::Value,
        prompt_tokens: u32,
        candidates_tokens: u32,
    ) -> Vec<u8> {
        frame(json!({
            "candidates": [{
                "content": { "parts": [
                    {
                        "functionCall": { "name": name, "args": args, "id": call_id },
                        "thoughtSignature": THOUGHT_SIGNATURE
                    }
                ] }
            }],
            "usageMetadata": {
                "promptTokenCount": prompt_tokens,
                "candidatesTokenCount": candidates_tokens
            }
        }))
    }

    /// The scripted response for one scenario, or `None` for
    /// `FragmentedToolName`, which this subject declines, or for a scenario
    /// that it has no capture for (uncaptured).
    ///
    /// Returning an `Option` rather than panicking on the declined or
    /// uncaptured scenarios is what lets [`Gemini::encodes_stop_reason`] call this for *every*
    /// scenario and measure the bytes — see `bedrock::script`'s doc for the
    /// same pattern.
    fn script(scenario: Scenario) -> Option<Script> {
        // The two-delta opening every text scenario shares. Both deltas carry
        // `usageMetadata` — see the module doc's "`usageMetadata` appears on
        // every chunk" section for why. Numbers match
        // `multi_chunk_usage_emits_usage_per_chunk`'s two chunks verbatim.
        let opening = || vec![text_delta("Hel", 1, 1), text_delta("lo", 1, 2)];
        // The opening plus one chunk carrying BOTH the stop reason and usage
        // — see the module doc for why those two are never split across two
        // chunks in this subject.
        let through_stop = || {
            let mut chunks = opening();
            chunks.push(stop_chunk("!", "STOP", 1, 3));
            chunks
        };

        let (chunks, gate_after, ending) = match scenario {
            // Full stream: deltas, then the combined stop-reason/usage chunk,
            // then the `[DONE]` sentinel and a clean end of body. This is the
            // arm that exercises `model.rs`'s `[DONE]` terminal path
            // (`model.rs:155-158`) — see the module doc's two-terminal-arms
            // section.
            Scenario::CleanStop => {
                let mut chunks = through_stop();
                chunks.push(done());
                (chunks, None, Ending::Clean)
            }
            // The stop reason and usage are observed, then the body simply
            // ends — no `[DONE]` anywhere in this script — so the buffered
            // stop reason has to be flushed by the bare-EOF arm
            // (`model.rs:149-152`), the OTHER terminal path. Together with
            // `CleanStop` this is what makes S2 non-vacuous for this subject:
            // each scenario exercises a different arm of `model.rs`'s event
            // loop, not the same one twice.
            Scenario::TruncatedAfterStopReason => (through_stop(), None, Ending::Clean),
            // The body ends cleanly mid-generation: no `finishReason` chunk
            // ever arrives, so no stop reason is ever observed. The SCRIPT
            // shape (a delta, then EOF, no `finishReason`) matches
            // `truncated_stream_no_finish`'s envelope
            // (`gemini_streaming.rs:67-69`, HAND-AUTHORED) — `opening()`
            // alone.
            Scenario::TruncatedMidGeneration => (opening(), None, Ending::Clean),
            // Same prefix, but the connection is torn down without a
            // terminating chunk, so the client observes a transport error
            // (`model.rs`'s `Some(Err(e))` arm) rather than an end-of-stream.
            Scenario::ErrorMidGeneration => (opening(), None, Ending::Abort),
            // The stop reason and usage are buffered/observed and *then* the
            // body is aborted, so the error path must not flush a `Finish`.
            // If `stop_chunk` were ever dropped from this script by mistake,
            // this scenario's observable events would collapse to exactly
            // `ErrorMidGeneration`'s (`[TokenDelta, Usage, TokenDelta, Usage,
            // Err]`, from `opening()`'s two `usageMetadata`-carrying deltas).
            // As scripted, `stop_chunk("!", ...)` itself carries a non-empty
            // text part alongside its own `usageMetadata`, so it adds one
            // more `TokenDelta`/`Usage` pair before the `Err`, not merely a
            // trailing `Usage`. [`Gemini::encodes_stop_reason`]'s measured
            // scan is what keeps the two distinguishable at the fixture
            // level regardless of which of those it is.
            Scenario::ErrorAfterStopReason => (through_stop(), None, Ending::Abort),
            // One content delta goes out, then the server parks holding the
            // second back (`gate_after: Some(1)` pauses *before* the chunk at
            // index 1, per `server.rs`'s `feed` loop) — no `finishReason`
            // chunk anywhere in this script, so no stop reason exists even
            // after the gate releases.
            Scenario::CancelMidGeneration => (opening(), Some(1), Ending::Clean),
            // The stop reason AND its usage are both observed before the gate
            // — [`stop_chunk`] carries both in one event, so this scenario
            // needs no *extra* usage chunk appended for the gate to sit
            // after, the same shape as `anthropic`'s `message_delta`.
            //
            // **This comment is the only guard for this scenario on this
            // subject.** The harness's own "at least one `Usage` observed"
            // floor (`assert_conforms`'s `floor_violation`) is VACUOUS here,
            // the same way it is for `anthropic`: `text_delta` (module doc,
            // "`usageMetadata` appears on every chunk") already emits a
            // `Usage` on the very first chunk of `opening()`, so that floor
            // is satisfied whether or not the gate sits in the right place —
            // even a gate placed before `stop_chunk` would still show a
            // `Usage` from the first `text_delta`, and pass. An earlier draft
            // of this module confined `usageMetadata` to the stop-reason
            // chunk specifically to keep this floor non-vacuous; the live
            // capture (module doc) does not have that shape (its first chunk
            // carries usage too), so it was removed — fidelity over a
            // convenient floor.
            //
            // So the only thing standing between this scenario and silently
            // degrading into `CancelMidGeneration` under another name is
            // this: `gate_after` is a COUNT — `chunks.len()`, not a
            // hand-typed literal — read *after* `through_stop()` has pushed
            // `stop_chunk` onto `chunks`. That parks the server with
            // `stop_chunk` already sent and the body still open, so
            // cancellation truncates the stream strictly after the stop
            // reason was buffered.
            Scenario::CancelAfterStopReason => {
                let chunks = through_stop();
                let gate_after = chunks.len();
                (chunks, Some(gate_after), Ending::Clean)
            }
            // Declined — see the module doc's "why `FragmentedToolName` is
            // declined" section and `Gemini::stream`.
            Scenario::FragmentedToolName => return None,
            // A whole tool call, transcribed from the live capture (module
            // doc, "Live capture"): the name, arguments, id and
            // `thoughtSignature` arrive together in one `functionCall` part
            // alongside `usageMetadata` and no `finishReason` (see
            // [`tool_call_chunk`]'s doc), then a separate chunk carries an
            // empty text part, `finishReason: "STOP"`, and `usageMetadata`
            // again (see [`stop_chunk`]'s doc on its `text: ""` reuse), then
            // the `[DONE]` sentinel — not itself part of the capture, which
            // ended at bare EOF (see [`done`]'s doc).
            Scenario::ToolCallCleanStop => (
                vec![
                    tool_call_chunk(TOOL_CALL_ID, TOOL_NAME, json!({ "city": "Berlin" }), 58, 16),
                    stop_chunk("", "STOP", 58, 16),
                    done(),
                ],
                None,
                Ending::Clean,
            ),
            // Declined — the scenario serves an anthropic thinking block.
            Scenario::ThinkingToolCallCleanStop => return None,
            // Not served — see the reason in `stream`.
            Scenario::ReasoningCleanStop => return None,
            // Three parallel calls, then the stop reason: all of
            // `parallel_signed.sse`, which is "Capture 1" of the SMA-827
            // design (`docs/superpowers/specs/2026-11-20-sma-827-gemini-
            // thought-signatures-design.md:299`; the file has no provenance
            // header of its own). Streamed against the Developer API on
            // `gemini-3.8-flash`. Chunks 0 to 2 each hold one `functionCall`
            // (`search`, `weather` Paris, `weather` Rome) with a native id;
            // only chunk 0 carries a `thoughtSignature`. Chunk 3 holds an
            // empty text part with `"finishReason": "STOP"`. The body ended at
            // bare EOF, so no `[DONE]` is sent, and the buffered stop reason
            // is flushed by the bare-EOF arm.
            //
            // No edits: the body is the file, byte for byte. The file already
            // holds the placeholder `SIG_A`.
            //
            // The stop chunk writes `"finishReason": "STOP"` with a space, and
            // `FINISH_REASON_MARKERS` accepts that form.
            Scenario::ParallelToolCallCleanStop => (
                PARALLEL_SIGNED.iter().map(|p| captured_frame(p)).collect(),
                None,
                Ending::Clean,
            ),
            // `fixtures/gemini/zero_arg_tool_call.raw`, CAPTURED from the
            // Gemini Developer API (`streamGenerateContent?alt=sse`,
            // `gemini-3.8-flash`) on 2026-11-24 at 21:14:44 UTC, with one
            // zero-argument tool and a forced `tool_choice`. Chunk 0 holds one
            // `functionCall` part with `"args": {}` and a real
            // `thoughtSignature`. Chunk 1 holds an empty text part with
            // `"finishReason": "STOP"`. The body ended at bare EOF, so no
            // `[DONE]` is sent. The wire uses CRLF line endings.
            //
            // Edits made here: none to any payload. This script sends the two
            // `data:` payloads byte for byte ([`ZERO_ARG_CHUNKS`]). Each one
            // becomes one frame that ends with a CRLF blank line.
            Scenario::ZeroArgToolCallCleanStop => (
                ZERO_ARG_CHUNKS
                    .iter()
                    .map(|p| captured_frame_crlf(p))
                    .collect(),
                None,
                Ending::Clean,
            ),
        };

        Some(Script {
            content_type: "text/event-stream",
            chunks,
            gate_after,
            ending,
        })
    }

    /// The request driven through `Model::invoke`.
    ///
    /// Carries a tool definition for `ToolCallCleanStop` so the exchange is
    /// coherent — a response returning a `functionCall` for a request that
    /// offered no tools would not be a coherent exchange. For
    /// `ParallelToolCallCleanStop` it offers the two tools that Capture 1
    /// calls, `search` and `weather`. The SMA-827 probe request is not in the
    /// repo, so only the names and argument keys are taken from the capture.
    /// `FragmentedToolName` is this subject's other tool scenario, but it is
    /// declined and so never reaches this function.
    fn request(scenario: Scenario) -> ModelRequest {
        let mut req = ModelRequest::new();
        req.messages = vec![Item::UserMessage {
            content: vec![ContentPart::Text {
                text: "What is the weather in Berlin?".into(),
            }],
        }];
        if matches!(scenario, Scenario::ToolCallCleanStop) {
            req.tools = vec![ToolDef::new(
                TOOL_NAME,
                "Look up the current weather for a city.",
                json!({
                    "type": "object",
                    "properties": { "city": { "type": "string" } },
                    "required": ["city"]
                }),
            )];
        }
        if scenario == Scenario::ZeroArgToolCallCleanStop {
            // The tool and the forced choice of the capture.
            req.tools = vec![ToolDef::new(
                "get_current_time",
                "Return the current server time. Takes no arguments.",
                json!({ "type": "object", "properties": {} }),
            )];
            req.model_settings.tool_choice = Some(ToolChoice::Tool {
                name: "get_current_time".into(),
            });
        }
        if scenario == Scenario::ParallelToolCallCleanStop {
            req.tools = vec![
                ToolDef::new(
                    "search",
                    "Search the web.",
                    json!({
                        "type": "object",
                        "properties": { "q": { "type": "string" } },
                        "required": ["q"]
                    }),
                ),
                ToolDef::new(
                    "weather",
                    "Look up the current weather for a city.",
                    json!({
                        "type": "object",
                        "properties": { "city": { "type": "string" } },
                        "required": ["city"]
                    }),
                ),
            ];
        }
        req
    }

    /// Build a real `GeminiModel` (Developer-API transport) pointed at a
    /// local plain-HTTP endpoint.
    fn build_model_against(base_url: &str) -> GeminiModel {
        GeminiModel::developer(MODEL_ID)
            .api_key("gm-conformance-test")
            .base_url(base_url)
            .build()
            .expect("gemini model should build with an explicit api key and base url")
    }

    /// See the module-level docs on this file for the fixture provenance
    /// rules this subject follows.
    struct Gemini;

    #[async_trait::async_trait]
    impl StreamUnderTest for Gemini {
        fn name(&self) -> &'static str {
            "gemini"
        }

        fn encodes_stop_reason(&self, scenario: Scenario) -> bool {
            // MEASURED from the bytes about to be served — deliberately not
            // `scenario.expects_stop_reason()`. See the crate's `declines`
            // module doc and `bedrock::encodes_stop_reason` for why that
            // restatement would make the harness's cross-check dead code.
            script(scenario)
                .map(|script| script.chunks.iter().any(|chunk| has_finish_reason(chunk)))
                .unwrap_or(false)
        }

        fn fixture_tool_name(&self) -> &'static str {
            TOOL_NAME
        }

        fn fixture_finish_reason(&self, scenario: Scenario) -> Option<FinishReason> {
            // Exhaustive on purpose, with no `_` arm — see
            // `bedrock::fixture_finish_reason` for why.
            match scenario {
                // `"STOP"` maps to `FinishReason::Stop` when no `functionCall`
                // has been seen (`stream.rs`'s `finish` method). Both of these
                // scenarios reach a `Finish`: `CleanStop` through the
                // `[DONE]` arm, `TruncatedAfterStopReason` through the
                // bare-EOF arm.
                Scenario::CleanStop | Scenario::TruncatedAfterStopReason => {
                    Some(FinishReason::Stop)
                }
                // `"STOP"` maps to `FinishReason::ToolCalls` once
                // `saw_function_call` is set, which `tool_call_chunk` sets.
                // In all three recorded captures the finish chunk after a tool
                // call carries `"STOP"`, not a tool-specific value. The
                // translator derives `ToolCalls` purely from having seen a
                // `functionCall` part earlier in the stream, not from a
                // distinct `finishReason` string.
                // Capture 1's stop chunk carries the same `"STOP"` after three
                // `functionCall` parts.
                // The zero-argument capture's stop chunk carries `"STOP"` after one
                // `functionCall` part.
                Scenario::ToolCallCleanStop
                | Scenario::ParallelToolCallCleanStop
                | Scenario::ZeroArgToolCallCleanStop => Some(FinishReason::ToolCalls),
                // Truncated, errored and cancelled streams must withhold
                // `Finish` entirely.
                Scenario::TruncatedMidGeneration
                | Scenario::ErrorMidGeneration
                | Scenario::ErrorAfterStopReason
                | Scenario::CancelMidGeneration
                | Scenario::CancelAfterStopReason => None,
                // Declined or uncaptured; never reached.
                Scenario::FragmentedToolName
                | Scenario::ThinkingToolCallCleanStop
                | Scenario::ReasoningCleanStop => None,
            }
        }

        fn fixture_channel_order(&self, _scenario: Scenario) -> Option<&'static [Channel]> {
            // This subject serves no scenario whose channel order the harness
            // checks, so it makes no claim.
            None
        }

        fn fixture_calls(&self, scenario: Scenario) -> &'static [ExpectedCall] {
            // Exhaustive on purpose, with no `_` arm — see
            // `bedrock::fixture_finish_reason` for why.
            match scenario {
                // Capture 1's three calls, in wire order, with their captured
                // arguments ([`PARALLEL_SIGNED`]).
                Scenario::ParallelToolCallCleanStop => PARALLEL_CALLS,
                // The one call of the zero-argument capture.
                Scenario::ZeroArgToolCallCleanStop => ZERO_ARG_CALLS,
                Scenario::CleanStop
                | Scenario::TruncatedAfterStopReason
                | Scenario::TruncatedMidGeneration
                | Scenario::ErrorMidGeneration
                | Scenario::ErrorAfterStopReason
                | Scenario::CancelMidGeneration
                | Scenario::CancelAfterStopReason
                | Scenario::FragmentedToolName
                | Scenario::ToolCallCleanStop
                | Scenario::ThinkingToolCallCleanStop
                | Scenario::ReasoningCleanStop => &[],
            }
        }

        async fn stream(&self, scenario: Scenario, cancel: CancellationToken) -> Outcome {
            // The name and full argument JSON are set once, whole, in the one
            // `functionCall` part; there is no second event either could be
            // split across.
            if scenario == Scenario::FragmentedToolName {
                return Outcome::Declined("name arrives whole in functionCall");
            }
            if scenario == Scenario::ThinkingToolCallCleanStop {
                return Outcome::Declined("the scenario serves an anthropic thinking block");
            }
            // `request` never sets a thinking configuration, so no thought
            // parts arrive on the wire.
            if scenario == Scenario::ReasoningCleanStop {
                return Outcome::Declined(
                    "the request never enables thought parts, so none arrive on the wire",
                );
            }

            let script =
                script(scenario).expect("a scenario this subject serves must have a script");
            let mut server = PacedServer::start(script).await;
            let gate = server.take_gate();
            let model = build_model_against(&server.base_url());

            let stream = model
                .invoke(request(scenario), cancel)
                .await
                .expect("gemini invoke should reach the local paced server");

            Outcome::Served { stream, gate }
        }
    }

    /// Whether `haystack` contains `needle` as a contiguous byte run.
    /// Duplicated per subject module by design — see `bedrock::contains`.
    fn contains(haystack: &[u8], needle: &[u8]) -> bool {
        haystack.windows(needle.len()).any(|w| w == needle)
    }

    /// The soundness claim behind [`Gemini::encodes_stop_reason`]'s substring
    /// scan: a [`FINISH_REASON_MARKERS`] form occurs in [`stop_chunk`]'s populated
    /// `finishReason` and in no other frame this subject builds — in
    /// particular, not in an ordinary content-only chunk, which has no
    /// `finishReason` key at all (see [`text_delta`]'s doc), and not in a
    /// `functionCall`-only chunk either.
    #[test]
    fn scan_finds_only_a_populated_finish_reason() {
        assert!(
            has_finish_reason(&stop_chunk("!", "STOP", 1, 3)),
            "stop_chunk must carry the marker, or the scan measures nothing"
        );
        assert!(
            has_finish_reason(&captured_frame(PARALLEL_SIGNED[3])),
            "Capture 1's stop chunk must carry the spaced marker, or the scan measures nothing"
        );

        for (name, frame) in [
            (
                "content delta (no finishReason key)",
                text_delta("Hel", 1, 1),
            ),
            ("[DONE]", done()),
            (
                "functionCall chunk (no finishReason key)",
                tool_call_chunk(TOOL_CALL_ID, TOOL_NAME, json!({ "city": "Berlin" }), 58, 16),
            ),
            (
                "Capture 1 signed functionCall chunk",
                captured_frame(PARALLEL_SIGNED[0]),
            ),
            (
                "Capture 1 unsigned functionCall chunk (Paris)",
                captured_frame(PARALLEL_SIGNED[1]),
            ),
            (
                "Capture 1 unsigned functionCall chunk (Rome)",
                captured_frame(PARALLEL_SIGNED[2]),
            ),
            (
                "zero-arg functionCall chunk",
                captured_frame_crlf(ZERO_ARG_CHUNKS[0]),
            ),
        ] {
            assert!(
                !has_finish_reason(&frame),
                "{name} contains the populated-finishReason marker, so the scan would report a \
                 stop reason for scripts that encode none."
            );
        }
    }

    /// The parallel script is sent byte for byte, and its stop chunk writes
    /// `"finishReason": "STOP"` with a space. The scan must still find it.
    #[test]
    fn gemini_parallel_script_encodes_its_stop_reason() {
        assert!(Gemini.encodes_stop_reason(Scenario::ParallelToolCallCleanStop));
    }

    /// The script is the capture, byte for byte.
    ///
    /// Provenance: CAPTURED from the Gemini Developer API at
    /// `https://generativelanguage.googleapis.com/v1beta/models/gemini-3.8-flash:streamGenerateContent?alt=sse`.
    /// The model is `gemini-3.8-flash`. The capture ran on 2026-11-24 at
    /// 21:14:44 UTC. The request is
    /// `tests/provider-stream-conformance/fixtures/gemini/zero_arg_tool_call.request.json`.
    /// The file is `fixtures/gemini/zero_arg_tool_call.raw`. It
    /// is 1383 bytes. Its SHA-256 is
    /// `45271e2e2c96d08f183bec5251811498fee18b74c3cbfc92f9a2fc7c35542624`.
    /// Findings: the wire uses CRLF line endings. The call has `"args": {}`,
    /// not a missing `args`. An earlier capture without `parameters`
    /// (20:39:23 UTC) is not committed.
    ///
    /// Removed: nothing. Added: nothing. Changed: nothing.
    #[test]
    fn zero_arg_script_is_the_capture_byte_for_byte() {
        let script = script(Scenario::ZeroArgToolCallCleanStop).expect("served");
        assert_eq!(script.chunks.len(), 2);
        assert_eq!(script.chunks.concat(), ZERO_ARG_RAW);
    }

    #[tokio::test]
    async fn conforms() {
        assert_conforms(&Gemini).await;
    }
}

/// The `Model::invoke` stream contract, checked against the OpenAI Responses
/// backend (`ResponsesTranslator`, `crates/paigasus-helikon-providers-openai/
/// src/backend/responses.rs`). This is the last subject registered.
///
/// # Why this subject declines four scenarios
///
/// `terminal_events` (`backend/responses.rs`) builds `Usage` and `Finish`
/// together from a single upstream event — `response.completed` or
/// `response.incomplete` — and the driver (`backend/responses.rs`'s `invoke`,
/// the `None => return` arm) has no flush-on-EOF path at all: there is no
/// `finish()` method the way `ChatTranslator` has one. So there is never a
/// window in which a stop reason has been buffered but `Finish` has not yet
/// been emitted, which makes three scenarios unreachable:
/// `TruncatedAfterStopReason`, `ErrorAfterStopReason`,
/// `CancelAfterStopReason`. `FragmentedToolName` is declined for the same
/// reason every other subject with a whole-name tool-call shape declines it:
/// `response.output_item.added` carries the complete function `name` in one
/// event, so there is no second event a name fragment could split across.
///
/// The exact strings below are copied by hand from `src/declines.rs` — see
/// this file's top-level module doc for why that duplication is load-bearing.
///
/// # SMA-617's alias regression coverage lives in the crate's own unit tests
///
/// Every function call this module scripts has its own `call_id`, and no two
/// items share one. The two-items-one-`call_id` shape that
/// `ResponsesTranslator` gates on the call (SMA-617) therefore never arises
/// from these bytes, and assertion 7 passes here without exercising it. Under
/// this suite's fixture-provenance rule the shape has no capture anywhere in
/// the repo and must not be invented, so it is not registered as a scenario.
///
/// Read this subject's `conforms` test as confirming the translator behaves
/// correctly on the wire shapes this module scripts (see "Fixture
/// provenance" below: the text shapes come from HAND-AUTHORED fixtures, the
/// tool-call shape from a capture), **not** as a standing regression guard
/// for the SMA-617 fix. That guard is
/// `two_items_one_call_id_emit_one_name` and its siblings in
/// `crates/paigasus-helikon-providers-openai/src/backend/responses.rs`.
///
/// # Fixture provenance
///
/// Every non-tool-call shape below (`response.output_text.delta`,
/// `response.completed` with a plain `status`/`usage`) is transcribed from
/// the shapes already committed under
/// `crates/paigasus-helikon-providers-openai/tests/fixtures/`, in particular
/// `responses_reasoning_then_text.txt`, whose `response.output_text.delta`
/// event this module's [`text_delta`] matches field-for-field.
/// `responses_reasoning_then_text.txt` is HAND-AUTHORED (SMA-316), not a
/// capture. The Responses wire has no `[DONE]` sentinel (SMA-585), so no
/// script below sends one. [`completed`] matches that fixture's
/// `response.completed` event too, with one addition:
/// `incomplete_details: null`, which `responses_reasoning_then_text.txt`
/// predates and does not carry. That field is grounded in the newer
/// `responses_tool_call.txt` capture instead (CAPTURED; see its own
/// provenance header), which does send it.
///
/// Every helper sets `sequence_number: 0`. Each cited fixture numbers its
/// frames instead.
///
/// The tool-call shape is different: no committed fixture anywhere in the
/// repo grounded it, so Task 12 was originally reported BLOCKED rather than
/// derived from this crate's own unit tests — deriving an ungrounded shape
/// from unit tests instead of escalating is exactly how the Gemini
/// `functionCall` fixture went wrong under SMA-522 (see `gemini`'s module
/// doc). A real capture (label: CAPTURED) against
/// `https://api.openai.com/v1/responses` (`gpt-4o-mini-2024-07-18`, a single
/// `get_weather` tool, 2026-08-20) has since been transcribed into
/// `crates/paigasus-helikon-providers-openai/tests/fixtures/responses_tool_call.txt`
/// (see that file's own provenance header for the full detail on what is
/// kept versus trimmed from the raw capture, and why).
/// `ToolCallCleanStop`'s script below reconstructs that capture's event
/// sequence and argument content — the same five argument fragments, the
/// same terminal usage (`input_tokens: 51`, `output_tokens: 15`) — using
/// this module's own item id and call id ([`TOOL_ITEM_ID`]/[`TOOL_CALL_ID`])
/// rather than the capture's literal `fc_0bb8...`/`call_D3Tp...` values (see
/// those constants' own docs for why), and every helper that builds a
/// tool-call frame cites the `sequence_number` of the fixture frame it
/// matches. The same capture also grounds a
/// crate-level regression test,
/// `tool_call_turn_finishes_with_tool_calls` in
/// `crates/paigasus-helikon-providers-openai/tests/responses_streaming.rs`,
/// which runs the fixture through the real translator end to end.
///
/// # Correlation: `item.id` vs `item.call_id`
///
/// `ResponsesTranslator::item_to_call` (`backend/responses.rs`) is keyed on
/// `item.id` (the `fc_...` value), not `item.call_id` (the `call_...`
/// value) — the capture is what confirms these are genuinely two different
/// identifiers on the wire rather than an assumption read off the
/// translator's own code: `response.output_item.added` carries both,
/// separately, and `response.function_call_arguments.delta` carries only
/// `item_id` (never `call_id`). [`output_item_added`] and
/// [`function_call_arguments_delta`] use distinct constants,
/// [`TOOL_ITEM_ID`] and [`TOOL_CALL_ID`], for exactly this reason — using one
/// constant for both would silently stop exercising the correlation this
/// subject exists to check.
mod openai_responses {
    use super::*;
    use paigasus_helikon_providers_openai::OpenAiModel;

    /// The model id `build_model_against` requests, and every scripted
    /// `response.completed`'s `model` field.
    const MODEL_ID: &str = "gpt-4o-mini";

    /// The tool name every tool-call fixture declares.
    const TOOL_NAME: &str = "get_weather";

    /// The internal item id (`fc_...`) — the correlator
    /// `function_call_arguments.delta.item_id` matches, per
    /// `responses_tool_call.txt`'s `fc_0bb8...` value. Deliberately distinct
    /// from [`TOOL_CALL_ID`]; see the module doc's "Correlation" section.
    const TOOL_ITEM_ID: &str = "fc_conformance_0";

    /// The stable call id (`call_...`) downstream tool execution must use,
    /// per `responses_tool_call.txt`'s `call_D3Tp...` value. Deliberately
    /// distinct from [`TOOL_ITEM_ID`]; see the module doc's "Correlation"
    /// section.
    const TOOL_CALL_ID: &str = "call_conformance_0";

    /// The substring that appears in a chunk if and only if it carries the
    /// terminal `response.completed` event.
    ///
    /// Unlike `openai_chat`/`litellm`/`gemini`, this subject's stop reason is
    /// not a field threaded through an otherwise-ordinary chunk — it *is* the
    /// event type. Scanning for the `type` key's value is therefore the
    /// direct analogue of those subjects' finish-reason-field scan, not a
    /// looser approximation of it. `scan_finds_only_the_terminal_event` is
    /// the guard that keeps this true against an ordinary delta or tool-call
    /// frame.
    const COMPLETED_MARKER: &[u8] = b"\"type\":\"response.completed\"";

    /// One SSE frame: `data: {payload}\n\n`. Matches four of the six
    /// `responses_*.txt` fixtures already committed under
    /// `crates/paigasus-helikon-providers-openai/tests/fixtures/` (the four
    /// HAND-AUTHORED ones). The other two, both CAPTURED,
    /// `responses_tool_call.txt` and
    /// `responses_tool_call_zero_args.txt`, keep their `event:` lines (see
    /// their own provenance headers). async-openai's
    /// `ResponseStreamEvent` is `#[serde(tag = "type")]`, so the parser
    /// dispatches off the JSON `type` key inside `data:` alone either way,
    /// which is why this helper omits the `event:` line even when building
    /// the `ToolCallCleanStop` script below, whose shape is otherwise drawn
    /// from `responses_tool_call.txt`.
    fn frame(payload: serde_json::Value) -> Vec<u8> {
        format!("data: {payload}\n\n").into_bytes()
    }

    /// One text delta. Matches `responses_reasoning_then_text.txt`'s
    /// `response.output_text.delta` event shape (`item_id`/`output_index`/
    /// `content_index`/`delta`, no `logprobs` key — the translator does not
    /// read it and every committed fixture omits it).
    ///
    /// Changed from the fixture: `sequence_number` is 0, not the frame's
    /// own number.
    fn text_delta(text: &str) -> Vec<u8> {
        frame(json!({
            "type": "response.output_text.delta",
            "sequence_number": 0,
            "item_id": "msg_conformance",
            "output_index": 0,
            "content_index": 0,
            "delta": text
        }))
    }

    /// The terminal `response.completed` event: a `status`, an `output`
    /// array, and a `usage` object, all siblings under `response`. Matches
    /// `responses_reasoning_then_text.txt`'s `response.completed` event
    /// shape.
    ///
    /// Changed from the fixture: `sequence_number` is 0, not the frame's
    /// own number.
    fn completed(output: serde_json::Value, input_tokens: u32, output_tokens: u32) -> Vec<u8> {
        frame(json!({
            "type": "response.completed",
            "sequence_number": 0,
            "response": {
                "id": "resp_conformance",
                "object": "response",
                "created_at": 1,
                "model": MODEL_ID,
                "output": output,
                "status": "completed",
                "incomplete_details": null,
                "usage": {
                    "input_tokens": input_tokens,
                    "input_tokens_details": { "cached_tokens": 0 },
                    "output_tokens": output_tokens,
                    "output_tokens_details": { "reasoning_tokens": 0 },
                    "total_tokens": input_tokens + output_tokens
                }
            }
        }))
    }

    /// `response.output_item.added` for a function-call item: `item.id`,
    /// `item.call_id` and the whole `name`, all present together. Matches
    /// `responses_tool_call.txt`'s `sequence_number: 2` frame (the
    /// `event: response.output_item.added` frame) field-for-field — this is
    /// what registers `ResponsesTranslator::item_to_call`.
    ///
    /// Changed from the fixture: `sequence_number` is 0, not the frame's
    /// own number.
    fn output_item_added(item_id: &str, call_id: &str, name: &str) -> Vec<u8> {
        frame(json!({
            "type": "response.output_item.added",
            "sequence_number": 0,
            "output_index": 0,
            "item": {
                "type": "function_call",
                "id": item_id,
                "status": "in_progress",
                "arguments": "",
                "call_id": call_id,
                "name": name
            }
        }))
    }

    /// One `response.function_call_arguments.delta`, carrying `item_id` —
    /// never `call_id` — per `responses_tool_call.txt`'s `sequence_number: 3`
    /// to `7` frames (the five `event: response.function_call_arguments.delta`
    /// frames). `delta` is a small fragment, matching the capture's `{"`,
    /// `city`, `":"`, `Berlin`, `"}` split rather than one whole-argument
    /// frame.
    ///
    /// Changed from the fixture: `sequence_number` is 0, not the frame's
    /// own number.
    fn function_call_arguments_delta(item_id: &str, delta: &str) -> Vec<u8> {
        frame(json!({
            "type": "response.function_call_arguments.delta",
            "sequence_number": 0,
            "item_id": item_id,
            "output_index": 0,
            "delta": delta
        }))
    }

    /// `response.function_call_arguments.done`, carrying the reassembled
    /// whole argument string. Matches `responses_tool_call.txt`'s
    /// `sequence_number: 8` frame — present in the capture but a no-op to
    /// the translator (`consume`'s `other` arm), included here purely so the
    /// script is the stream the capture claims it is.
    ///
    /// Changed from the fixture: `sequence_number` is 0, not the frame's
    /// own number.
    fn function_call_arguments_done(item_id: &str, arguments: &str) -> Vec<u8> {
        frame(json!({
            "type": "response.function_call_arguments.done",
            "sequence_number": 0,
            "item_id": item_id,
            "output_index": 0,
            "arguments": arguments
        }))
    }

    /// `response.output_item.done`, the completed mirror of
    /// [`output_item_added`]. Matches `responses_tool_call.txt`'s
    /// `sequence_number: 9` frame.
    ///
    /// No longer a no-op to the translator: since SMA-562 this event emits a
    /// `ToolCallDelta` carrying the item's complete `arguments` when no
    /// argument delta has already been emitted for it. In THIS scenario the
    /// deltas always arrive first, so it stays a no-op here specifically —
    /// which is the property the double-emit dedup guarantees, and worth
    /// noticing if this suite ever goes red on a duplicated tool call.
    ///
    /// Changed from the fixture: `sequence_number` is 0, not the frame's
    /// own number.
    fn output_item_done(item_id: &str, call_id: &str, name: &str, arguments: &str) -> Vec<u8> {
        frame(json!({
            "type": "response.output_item.done",
            "sequence_number": 0,
            "output_index": 0,
            "item": {
                "type": "function_call",
                "id": item_id,
                "status": "completed",
                "arguments": arguments,
                "call_id": call_id,
                "name": name
            }
        }))
    }

    /// The five events of `responses_tool_call_zero_args.txt`
    /// (`crates/paigasus-helikon-providers-openai/tests/fixtures/`), as
    /// `(event name, data payload)` pairs, byte for byte, in file order. See
    /// the `ZeroArgToolCallCleanStop` arm of [`script`] for the provenance.
    const ZERO_ARG_EVENTS: [(&str, &str); 5] = [
        (
            "response.output_item.added",
            r##"{"type":"response.output_item.added","item":{"id":"fc_0ce0b75b3b84617f006a89d169347087d288c7d58999a8f93d","type":"function_call","status":"in_progress","arguments":"","call_id":"call_8xWY1dceitU93bp87yBo1ocG","name":"get_current_time"},"output_index":0,"sequence_number":2}"##,
        ),
        (
            "response.function_call_arguments.delta",
            r##"{"type":"response.function_call_arguments.delta","delta":"{}","item_id":"fc_0ce0b75b3b84617f006a89d169347087d288c7d58999a8f93d","obfuscation":"EzdgyuuAsmTPs2","output_index":0,"sequence_number":3}"##,
        ),
        (
            "response.function_call_arguments.done",
            r##"{"type":"response.function_call_arguments.done","arguments":"{}","item_id":"fc_0ce0b75b3b84617f006a89d169347087d288c7d58999a8f93d","output_index":0,"sequence_number":4}"##,
        ),
        (
            "response.output_item.done",
            r##"{"type":"response.output_item.done","item":{"id":"fc_0ce0b75b3b84617f006a89d169347087d288c7d58999a8f93d","type":"function_call","status":"completed","arguments":"{}","call_id":"call_8xWY1dceitU93bp87yBo1ocG","name":"get_current_time"},"output_index":0,"sequence_number":5}"##,
        ),
        (
            "response.completed",
            r##"{"type":"response.completed","response":{"id":"resp_0ce0b75b3b84617f006a89d167d25487d28d2a332550e69d61","object":"response","created_at":1787416935,"status":"completed","background":false,"completed_at":1787416937,"error":null,"frequency_penalty":0.0,"incomplete_details":null,"instructions":null,"max_output_tokens":null,"max_tool_calls":null,"model":"gpt-4o-mini-2024-07-18","moderation":null,"output":[{"id":"fc_0ce0b75b3b84617f006a89d169347087d288c7d58999a8f93d","type":"function_call","status":"completed","arguments":"{}","call_id":"call_8xWY1dceitU93bp87yBo1ocG","name":"get_current_time"}],"parallel_tool_calls":true,"presence_penalty":0.0,"previous_response_id":null,"prompt_cache_key":null,"prompt_cache_retention":"in_memory","reasoning":{"context":null,"effort":null,"summary":null},"safety_identifier":null,"service_tier":"default","store":true,"temperature":1.0,"text":{"format":{"type":"text"},"verbosity":"medium"},"tool_choice":"required","tool_usage":{"image_gen":{"input_tokens":0,"input_tokens_details":{"image_tokens":0,"text_tokens":0},"output_tokens":0,"output_tokens_details":{"image_tokens":0,"text_tokens":0},"total_tokens":0},"web_search":{"num_requests":0}},"tools":[{"type":"function","description":"Return the current server time. Takes no arguments.","name":"get_current_time","output_schema":null,"parameters":{"type":"object","properties":{},"required":[],"additionalProperties":false},"strict":true}],"top_logprobs":0,"top_p":1.0,"truncation":"disabled","usage":{"input_tokens":57,"input_tokens_details":{"cache_write_tokens":0,"cached_tokens":0},"output_tokens":2,"output_tokens_details":{"reasoning_tokens":0},"total_tokens":59},"user":null,"metadata":{}},"sequence_number":6}"##,
        ),
    ];

    /// The one call of [`ZERO_ARG_EVENTS`]: the tool name and the literal `{}`
    /// that the API streams as its arguments.
    const ZERO_ARG_CALLS: &[ExpectedCall] = &[ExpectedCall {
        name: "get_current_time",
        args: "{}",
    }];

    /// One captured SSE frame: `event: {event}\ndata: {payload}\n\n`. The
    /// `event:` line is kept, as in the file.
    fn captured_frame(event: &str, payload: &str) -> Vec<u8> {
        format!("event: {event}\ndata: {payload}\n\n").into_bytes()
    }

    /// The scripted response for one scenario, or `None` for the four this
    /// subject declines or for a scenario that it has no capture for
    /// (uncaptured) — see the module doc's "Why this subject declines
    /// four scenarios" section. Returning an `Option` rather than panicking
    /// on a declined scenario is what lets
    /// [`OpenAiResponses::encodes_stop_reason`] call this for *every*
    /// scenario and measure the bytes — see `bedrock::script`'s doc for the
    /// same pattern.
    fn script(scenario: Scenario) -> Option<Script> {
        // The two-delta opening every text scenario shares. The content is
        // arbitrary, as in every other subject in this file.
        let opening = || vec![text_delta("Hel"), text_delta("lo")];

        let (chunks, gate_after, ending) = match scenario {
            // Full stream: deltas, then the one event that carries the stop
            // reason and usage, then a clean end of body. On this wire
            // `response.completed` is both the stop reason and the
            // terminator. There is no separate sentinel.
            Scenario::CleanStop => {
                let mut chunks = opening();
                chunks.push(completed(json!([]), 8, 6));
                (chunks, None, Ending::Clean)
            }
            // Declined — `Usage`/`Finish` are built from one upstream event
            // with no earlier point at which a stop reason is buffered but
            // unflushed, and `invoke`'s driver has no flush-on-EOF path at
            // all. See the module doc.
            Scenario::TruncatedAfterStopReason => return None,
            // The body ends cleanly mid-generation: no `response.completed`/
            // `response.incomplete` event ever arrives, so no stop reason is
            // ever observed. `opening()` alone, matching every sibling
            // subject's shape for this scenario.
            Scenario::TruncatedMidGeneration => (opening(), None, Ending::Clean),
            // Same prefix, but the connection is torn down without a
            // terminating chunk, so the client observes a transport error.
            Scenario::ErrorMidGeneration => (opening(), None, Ending::Abort),
            // Declined for the same reason as `TruncatedAfterStopReason`.
            Scenario::ErrorAfterStopReason => return None,
            // One content delta goes out, then the server parks holding the
            // second back (`gate_after: Some(1)` pauses *before* the chunk at
            // index 1) — no terminal event anywhere in this script, so no
            // stop reason exists even after the gate releases.
            Scenario::CancelMidGeneration => (opening(), Some(1), Ending::Clean),
            // Declined for the same reason as `TruncatedAfterStopReason`.
            Scenario::CancelAfterStopReason => return None,
            // Declined — the whole function `name` arrives in
            // `output_item.added`; there is no second event a fragment could
            // split across. See the module doc.
            Scenario::FragmentedToolName => return None,
            // The capture's event sequence and payload shape, minus two
            // substitutions: `frame` (see its own doc) drops the `event:`
            // line every capture frame carried, and the capture's
            // `fc_0bb8...`/`call_D3Tp...` ids are replaced with
            // `TOOL_ITEM_ID`/`TOOL_CALL_ID` so the scenario isn't pinned to
            // an opaque copy-pasted string. Otherwise: `output_item.added`
            // registers `item_to_call`, five small argument fragments
            // reassemble to `{"city":"Berlin"}`, `function_call_arguments.done`
            // and `output_item.done` precede the terminal event exactly as
            // the capture ordered them, and `response.completed` carries
            // both the tool-call output and its usage. This is what proves
            // `terminal_events` maps a tool-only turn's `status: "completed"`
            // to `FinishReason::ToolCalls`, not `Stop` — the SMA-533 defect
            // this subject's registration surfaced.
            Scenario::ToolCallCleanStop => {
                let args = "{\"city\":\"Berlin\"}";
                let chunks = vec![
                    output_item_added(TOOL_ITEM_ID, TOOL_CALL_ID, TOOL_NAME),
                    function_call_arguments_delta(TOOL_ITEM_ID, "{\""),
                    function_call_arguments_delta(TOOL_ITEM_ID, "city"),
                    function_call_arguments_delta(TOOL_ITEM_ID, "\":\""),
                    function_call_arguments_delta(TOOL_ITEM_ID, "Berlin"),
                    function_call_arguments_delta(TOOL_ITEM_ID, "\"}"),
                    function_call_arguments_done(TOOL_ITEM_ID, args),
                    output_item_done(TOOL_ITEM_ID, TOOL_CALL_ID, TOOL_NAME, args),
                    completed(
                        json!([{
                            "type": "function_call",
                            "id": TOOL_ITEM_ID,
                            "status": "completed",
                            "arguments": args,
                            "call_id": TOOL_CALL_ID,
                            "name": TOOL_NAME
                        }]),
                        51,
                        15,
                    ),
                ];
                (chunks, None, Ending::Clean)
            }
            // Declined — the scenario serves an anthropic thinking block.
            Scenario::ThinkingToolCallCleanStop => return None,
            // Not served — see the reason in `stream`.
            Scenario::ReasoningCleanStop => return None,
            // Not served — see the reason in `stream`.
            Scenario::ParallelToolCallCleanStop => return None,
            // `responses_tool_call_zero_args.txt`, CAPTURED against the real
            // OpenAI Responses API (`gpt-4o-mini-2024-07-18`, 2026-08-22) with
            // one zero-argument strict tool and `tool_choice: "required"`.
            // The file is a regression pin for SMA-562: the API streams exactly
            // one `function_call_arguments.delta`, and it carries the literal
            // `{}`. Five events: `output_item.added`, the `{}` delta,
            // `function_call_arguments.done`, `output_item.done` and
            // `response.completed`. The file's own edits from the raw capture
            // were: it drops `response.created` and `response.in_progress`,
            // and it adds no `data: [DONE]`.
            //
            // Edits made here: none to any payload. This script sends the five
            // `event:` and `data:` payloads byte for byte
            // ([`ZERO_ARG_EVENTS`]). The only change is the framing: the
            // file's header comment lines are not sent, and each pair becomes
            // one `event:`/`data:` frame that ends with a blank line.
            Scenario::ZeroArgToolCallCleanStop => (
                ZERO_ARG_EVENTS
                    .iter()
                    .map(|(event, payload)| captured_frame(event, payload))
                    .collect(),
                None,
                Ending::Clean,
            ),
        };

        Some(Script {
            content_type: "text/event-stream",
            chunks,
            gate_after,
            ending,
        })
    }

    /// The request driven through `Model::invoke`.
    ///
    /// Carries a tool definition for `ToolCallCleanStop` so the exchange is
    /// coherent — a response returning a function call for a request that
    /// offered no tools would not be a coherent exchange.
    /// `FragmentedToolName` is this subject's other tool scenario,
    /// but it is declined and so never reaches this function.
    fn request(scenario: Scenario) -> ModelRequest {
        let mut req = ModelRequest::new();
        req.messages = vec![Item::UserMessage {
            content: vec![ContentPart::Text {
                text: "What is the weather in Berlin?".into(),
            }],
        }];
        if matches!(scenario, Scenario::ToolCallCleanStop) {
            req.tools = vec![ToolDef::new(
                TOOL_NAME,
                "Look up the current weather for a city.",
                json!({
                    "type": "object",
                    "properties": { "city": { "type": "string" } },
                    "required": ["city"]
                }),
            )];
        }
        if scenario == Scenario::ZeroArgToolCallCleanStop {
            // The tool of the capture: no arguments, as its schema says.
            req.tools = vec![ToolDef::new(
                "get_current_time",
                "Return the current server time. Takes no arguments.",
                json!({
                    "type": "object",
                    "properties": {},
                    "required": [],
                    "additionalProperties": false
                }),
            )];
        }
        req
    }

    /// Build a real `OpenAiModel` (Responses backend) pointed at a local
    /// plain-HTTP endpoint.
    fn build_model_against(base_url: &str) -> OpenAiModel {
        OpenAiModel::responses(MODEL_ID)
            .api_key("sk-conformance-test")
            .base_url(base_url)
            .build()
            .expect("openai/responses model should build with an explicit api key and base url")
    }

    /// See the module-level docs on this file for the fixture provenance
    /// rules this subject follows.
    struct OpenAiResponses;

    #[async_trait::async_trait]
    impl StreamUnderTest for OpenAiResponses {
        fn name(&self) -> &'static str {
            "openai/responses"
        }

        fn encodes_stop_reason(&self, scenario: Scenario) -> bool {
            // MEASURED from the bytes about to be served — deliberately not
            // `scenario.expects_stop_reason()`. See the crate's `declines`
            // module doc and `bedrock::encodes_stop_reason` for why that
            // restatement would make the harness's cross-check dead code.
            script(scenario)
                .map(|script| {
                    script
                        .chunks
                        .iter()
                        .any(|chunk| contains(chunk, COMPLETED_MARKER))
                })
                .unwrap_or(false)
        }

        fn fixture_tool_name(&self) -> &'static str {
            TOOL_NAME
        }

        fn fixture_finish_reason(&self, scenario: Scenario) -> Option<FinishReason> {
            // Exhaustive on purpose, with no `_` arm — see
            // `bedrock::fixture_finish_reason` for why.
            match scenario {
                Scenario::CleanStop => Some(FinishReason::Stop),
                Scenario::ToolCallCleanStop | Scenario::ZeroArgToolCallCleanStop => {
                    Some(FinishReason::ToolCalls)
                }
                Scenario::TruncatedMidGeneration
                | Scenario::ErrorMidGeneration
                | Scenario::CancelMidGeneration => None,
                // Declined; never reached.
                Scenario::TruncatedAfterStopReason
                | Scenario::ErrorAfterStopReason
                | Scenario::CancelAfterStopReason
                | Scenario::FragmentedToolName
                | Scenario::ThinkingToolCallCleanStop
                | Scenario::ReasoningCleanStop
                | Scenario::ParallelToolCallCleanStop => None,
            }
        }

        fn fixture_channel_order(&self, _scenario: Scenario) -> Option<&'static [Channel]> {
            // This subject serves no scenario whose channel order the harness
            // checks, so it makes no claim.
            None
        }

        fn fixture_calls(&self, scenario: Scenario) -> &'static [ExpectedCall] {
            // Exhaustive on purpose, with no `_` arm — see
            // `bedrock::fixture_finish_reason` for why.
            match scenario {
                // The one call of the zero-argument capture.
                Scenario::ZeroArgToolCallCleanStop => ZERO_ARG_CALLS,
                Scenario::CleanStop
                | Scenario::TruncatedAfterStopReason
                | Scenario::TruncatedMidGeneration
                | Scenario::ErrorMidGeneration
                | Scenario::ErrorAfterStopReason
                | Scenario::CancelMidGeneration
                | Scenario::CancelAfterStopReason
                | Scenario::FragmentedToolName
                | Scenario::ToolCallCleanStop
                | Scenario::ThinkingToolCallCleanStop
                | Scenario::ReasoningCleanStop
                | Scenario::ParallelToolCallCleanStop => &[],
            }
        }

        async fn stream(&self, scenario: Scenario, cancel: CancellationToken) -> Outcome {
            match scenario {
                // `Usage`/`Finish` are built from one upstream event with no
                // earlier point at which a stop reason is buffered but
                // unflushed, and there is no flush-on-EOF path either. See
                // the module doc's "Why this subject declines four
                // scenarios" section.
                Scenario::TruncatedAfterStopReason
                | Scenario::ErrorAfterStopReason
                | Scenario::CancelAfterStopReason => {
                    return Outcome::Declined("stop reason and Finish are the same event")
                }
                // The whole function `name` arrives in `output_item.added`;
                // there is no second event a fragment could split across.
                Scenario::FragmentedToolName => {
                    return Outcome::Declined("name arrives whole in output_item.added")
                }
                Scenario::ThinkingToolCallCleanStop => {
                    return Outcome::Declined("the scenario serves an anthropic thinking block")
                }
                // A reasoning stream can occur on this wire, but no capture
                // of one exists (SMA-858).
                Scenario::ReasoningCleanStop => {
                    return Outcome::Uncaptured("no captured reasoning stream")
                }
                // Parallel tool calls can occur on this wire, but no capture
                // of them exists (SMA-859).
                Scenario::ParallelToolCallCleanStop => {
                    return Outcome::Uncaptured("no captured parallel tool-call stream")
                }
                _ => {}
            }

            let script =
                script(scenario).expect("a scenario this subject serves must have a script");
            let mut server = PacedServer::start(script).await;
            let gate = server.take_gate();
            let model = build_model_against(&server.base_url());

            let stream = model
                .invoke(request(scenario), cancel)
                .await
                .expect("openai/responses invoke should reach the local paced server");

            Outcome::Served { stream, gate }
        }
    }

    /// Whether `haystack` contains `needle` as a contiguous byte run.
    /// Duplicated per subject module by design — see `bedrock::contains`.
    fn contains(haystack: &[u8], needle: &[u8]) -> bool {
        haystack.windows(needle.len()).any(|w| w == needle)
    }

    /// The soundness claim behind
    /// [`OpenAiResponses::encodes_stop_reason`]'s substring scan:
    /// [`COMPLETED_MARKER`] occurs in [`completed`]'s output and in no other
    /// frame this subject builds — in particular, not in an ordinary content
    /// delta or any of the tool-call frames, none of which carry a `type`
    /// value starting with `response.completed`.
    #[test]
    fn scan_finds_only_the_terminal_event() {
        assert!(
            contains(&completed(json!([]), 8, 6), COMPLETED_MARKER),
            "the completed event must carry the marker, or the scan measures nothing"
        );

        for (name, frame) in [
            ("content delta", text_delta("Hel")),
            (
                "output_item.added",
                output_item_added(TOOL_ITEM_ID, TOOL_CALL_ID, TOOL_NAME),
            ),
            (
                "function_call_arguments.delta",
                function_call_arguments_delta(TOOL_ITEM_ID, "{\""),
            ),
            (
                "function_call_arguments.done",
                function_call_arguments_done(TOOL_ITEM_ID, "{\"city\":\"Berlin\"}"),
            ),
            (
                "output_item.done",
                output_item_done(
                    TOOL_ITEM_ID,
                    TOOL_CALL_ID,
                    TOOL_NAME,
                    "{\"city\":\"Berlin\"}",
                ),
            ),
        ] {
            assert!(
                !contains(&frame, COMPLETED_MARKER),
                "{name} contains the response.completed marker, so the scan would report a stop \
                 reason for scripts that encode none."
            );
        }
    }

    #[tokio::test]
    async fn conforms() {
        assert_conforms(&OpenAiResponses).await;
    }
}
