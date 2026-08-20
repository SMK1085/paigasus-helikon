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
//! 1. **Decline reasons are literal strings**, copied by hand from
//!    `src/declines.rs`. A subject that looked its reasons up from `DECLINED`
//!    would satisfy that table's two-directional cross-check by construction,
//!    which is exactly the drift the table exists to catch.
//! 2. **`encodes_stop_reason` is measured from the bytes about to be served**,
//!    never restated from `Scenario::expects_stop_reason`. Restating it
//!    compares the harness's expectation with itself. See the Bedrock impl for
//!    what it catches.

use paigasus_helikon_core::{
    CancellationToken, ContentPart, FinishReason, Item, Model, ModelRequest, ToolDef,
};
use paigasus_helikon_provider_stream_conformance::{
    assert_conforms, eventstream::frame, Ending, Outcome, PacedServer, Scenario, Script,
    StreamUnderTest,
};
use serde_json::json;

/// The `Model::invoke` stream contract, checked against the Bedrock
/// `ConverseStream` provider.
///
/// # Fixture provenance
///
/// Bedrock is the one subject in this suite whose wire format is binary, and
/// the repo holds no captured Bedrock traffic to transcribe. So — as the task
/// brief directs when no capture exists — every event shape here is derived
/// from two sources inside the workspace, and from nothing else:
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
/// Nothing here is transcribed from vendor documentation, and no shape is
/// invented: a frame the SDK's decoder does not recognise is dropped silently
/// by the translator's forward-compat catch-all rather than rejected, so an
/// invented shape would present as a translator bug and not as a bad fixture.
///
/// # Why there are no `.bin` fixture files
///
/// A `vnd.amazon.eventstream` frame is a length-prefixed header block wrapped
/// in two CRC-32s, so a checked-in `.bin` would be neither reviewable nor
/// editable, and any byte edited by hand would fail its CRC rather than test
/// anything. `eventstream::frame` exists precisely to avoid that: it encodes
/// each fixture with the same encoder the SDK itself uses, so the bytes that
/// reach the wire are real while the fixture stays readable as JSON.
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
    /// it is here because a real stream always sends it first, and a fixture
    /// that silently omitted the opening event would not be the stream it
    /// claims to be. Key: `role` (`shape_message_start_event.rs`), value from
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
    /// because a real stream closes every block it opened. Key:
    /// `contentBlockIndex` (`shape_content_block_stop_event.rs`).
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

    /// `metadata`, the only event carrying usage — and it arrives **after**
    /// `messageStop`, which is the whole reason the translator buffers the stop
    /// reason instead of emitting `Finish` on the spot.
    ///
    /// **This ordering is not self-enforcing, so do not reorder it casually.**
    /// `stream.rs` deliberately handles the reversed order too (`Metadata`
    /// before `MessageStop` emits `Usage` at once and `Finish` on the stop), so
    /// a fixture that put `metadata` first would still pass every assertion —
    /// while quietly testing a wire shape Bedrock does not produce, and no
    /// longer covering the buffering path that exists precisely because the
    /// real order is this one. Verified by mutation while registering this
    /// subject: swapping the two left the suite green.
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

    /// The scripted response for one scenario, or `None` for a scenario this
    /// subject declines.
    ///
    /// Each script is the same Converse stream cut at a different point, so the
    /// scenarios differ only in what the client gets to observe — which is
    /// exactly the distinction the suite is checking.
    ///
    /// Returning an `Option` rather than panicking on the declined scenarios is
    /// what lets [`Bedrock::encodes_stop_reason`] call this for *every*
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
    /// a `toolUse` block for a request that offered no tools is not a stream
    /// Bedrock would ever produce.
    fn request(scenario: Scenario) -> ModelRequest {
        let mut req = ModelRequest::new();
        req.messages = vec![Item::UserMessage {
            content: vec![ContentPart::Text {
                text: "What is the weather in Berlin?".into(),
            }],
        }];
        // `ToolCallCleanStop` only: `FragmentedToolName` is the subject's other
        // tool scenario, but it is declined and so never reaches this function.
        if matches!(scenario, Scenario::ToolCallCleanStop) {
            req.tools = vec![ToolDef {
                name: TOOL_NAME.to_owned(),
                description: "Look up the current weather for a city.".to_owned(),
                schema: json!({
                    "type": "object",
                    "properties": { "city": { "type": "string" } },
                    "required": ["city"]
                }),
            }];
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
            .behavior_version(aws_config::BehaviorVersion::v2026_01_12())
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

        paigasus_helikon_providers_bedrock::BedrockModel::converse(
            "anthropic.claude-3-5-sonnet-20240620-v1:0",
        )
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
                Scenario::ToolCallCleanStop => Some(FinishReason::ToolCalls),
                // Truncated, errored and cancelled streams must withhold
                // `Finish` entirely.
                Scenario::TruncatedMidGeneration
                | Scenario::ErrorMidGeneration
                | Scenario::ErrorAfterStopReason
                | Scenario::CancelMidGeneration => None,
                // Declined; never reached.
                Scenario::CancelAfterStopReason | Scenario::FragmentedToolName => None,
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
        ] {
            assert!(
                !contains(&frame, marker),
                "{name} contains {MESSAGE_STOP_EVENT_TYPE:?}, so the scan would report a stop \
                 reason for scripts that encode none"
            );
        }
    }

    #[tokio::test]
    async fn conforms() {
        assert_conforms(&Bedrock).await;
    }
}
