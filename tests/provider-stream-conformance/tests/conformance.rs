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

/// The `Model::invoke` stream contract, checked against the OpenAI Chat
/// Completions backend.
///
/// This is the subject whose historical defect motivated the whole suite:
/// SMA-522 (PR #197), where this exact translator emitted `Finish` before
/// `Usage` on every streaming turn. It went undetected because the fixtures
/// in place at the time encoded a wire shape real OpenAI-compatible servers
/// do not send — `usage` arriving *before* `finish_reason` rather than after.
/// `CleanStop` below places `usage` after the `finish_reason` chunk for
/// exactly that reason.
///
/// # Fixture provenance
///
/// Every envelope shape below is transcribed from a committed fixture in this
/// workspace, cited on the helper that builds it:
///
/// - `crates/paigasus-helikon-providers-openai/tests/fixtures/chat_text_usage_trailing.txt`
///   — the SMA-522 capture: content deltas, the `finish_reason` chunk, the
///   trailing `usage` chunk, `[DONE]`.
/// - `crates/paigasus-helikon-providers-openai/tests/chat_wire.rs`'s
///   `happy_path_text_completion` fixture body — grounds the explicit
///   `"finish_reason":null"` OpenAI sends on every non-terminal chunk, which
///   is exactly the shape [`OpenAiChat::encodes_stop_reason`]'s marker must
///   not match.
/// - `crates/paigasus-helikon-providers-openai/tests/fixtures/chat_parallel_tool_calls.txt`
///   — the tool-call envelope: `id`/`name` arriving with empty `arguments`,
///   followed by args-only continuations with no `name` key.
/// - `crates/paigasus-helikon-providers-litellm/tests/fixtures/tool_call_stream_fragmented_name.txt`
///   — the committed capture of a name split across two deltas that BOTH
///   arrive after the `id`, which `FragmentedToolName` must use verbatim per
///   the design spec's §6 provenance note (the "id resolves late" variant has
///   no capture anywhere in the repo and is not built here).
///
/// `chat_content_filter.txt` and `chat_text_usage_trailing_empty_choices.txt`
/// were read but contribute no shape this module needs: this subject serves
/// only `Stop` and `ToolCalls` finish reasons, and the empty-`choices` usage
/// variant is a second encoding of the same trailing-usage envelope already
/// covered by `chat_text_usage_trailing.txt`.
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
/// even drained. So this subject's `FragmentedToolName` script is a *prefix*
/// of the committed capture: the three tool-call deltas, verbatim, with the
/// body ending cleanly right after them — no `finish_reason`, no `usage`, no
/// `[DONE]`. Assertion 7 (exactly one name-bearing delta per `call_id`) is
/// fully exercised by that prefix alone; nothing here shortens what the
/// scenario tests, only when its script stops.
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
    /// subject: OpenAI sends `"finish_reason":null` on every ordinary content
    /// or tool-call delta (see [`text_delta`]), so a bare substring scan would
    /// report a stop reason for scripts that encode none. Scanning for the
    /// opening quote of a *string* value excludes the null case: `null` has
    /// no quote after the colon, `"stop"`/`"tool_calls"` do.
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
    /// `chat_text_usage_trailing.txt`. The explicit `"finish_reason":null` is
    /// grounded in `tests/chat_wire.rs`'s `happy_path_text_completion`
    /// fixture body (itself captured from LiteLLM) — it is the frame
    /// `scan_finds_only_a_populated_finish_reason` uses to prove the marker
    /// does not fire on a null value.
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
    /// usage chunk exactly, including its token counts: an empty `delta`, no
    /// `finish_reason` key at all, and a top-level `usage` object.
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

    /// The scripted response for one scenario.
    ///
    /// Every scenario is served — `openai/chat` declines none — so unlike
    /// `bedrock::script` this returns a bare [`Script`], not an `Option`.
    fn script(scenario: Scenario) -> Script {
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
            // the real OpenAI wire order and the exact shape SMA-522 got
            // wrong — see the module doc.
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
            // The committed capture's shape, verbatim and in full — see the
            // module doc's "FragmentedToolName deliberately never completes"
            // section for why the body ends right here rather than
            // continuing on to the capture's finish_reason/usage/[DONE]
            // tail.
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
        };

        Script {
            content_type: "text/event-stream",
            chunks,
            gate_after,
            ending,
        }
    }

    /// The request driven through `Model::invoke`.
    ///
    /// Carries a tool definition for both tool scenarios so the exchange is
    /// coherent — a response returning `tool_calls` for a request that
    /// offered no tools is not a stream OpenAI would ever produce.
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
                .chunks
                .iter()
                .any(|chunk| contains(chunk, FINISH_REASON_MARKER))
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
                Scenario::ToolCallCleanStop => Some(FinishReason::ToolCalls),
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
                | Scenario::FragmentedToolName => None,
            }
        }

        async fn stream(&self, scenario: Scenario, cancel: CancellationToken) -> Outcome {
            let script = script(scenario);
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
    /// in an ordinary content delta, which is the OpenAI-specific pitfall the
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
/// helper that builds it:
///
/// - `text_then_trailing_usage.txt` — content deltas with no `finish_reason`
///   key at all (not even an explicit `null`), a chunk carrying a populated
///   `finish_reason`, a trailing usage-only chunk with no `finish_reason` key,
///   `[DONE]`.
/// - `tool_call_stream.txt` — the whole-tool-call envelope: `id` and the
///   complete function `name` arriving together with an empty-string
///   `arguments`, followed by two args-only continuations with no `name` key
///   at all (not even empty), then a `"tool_calls"` finish chunk and trailing
///   usage.
/// - `tool_call_stream_fragmented_name.txt` — captured against LiteLLM
///   1.98.0; the canonical fragmented-name shape, both fragments arriving
///   after the id (the first fragment arrives together with the id, the
///   second on a later delta once the id has already resolved). Per the
///   design spec's §6 provenance note the "id resolves late" variant has no
///   capture anywhere in the repo and is not built here — matching
///   `openai_chat`'s note on the same file.
/// - `truncated_no_finish.txt` — grounds the shape "content delta(s), then
///   the body simply ends" with no `finish_reason` chunk anywhere.
///
/// `unknown_finish_reason.txt` was read but contributes no shape this module
/// needs: this subject serves only `Stop` and `ToolCalls` finish reasons, and
/// the `"guardrail_intervened"` shape it captures is a second encoding of the
/// already-covered populated-`finish_reason` envelope.
///
/// [`FINISH_REASON_MARKER`]'s guard test needs one more shape that no
/// committed *litellm-crate* fixture happens to carry: a content delta with
/// an **explicit** `"finish_reason":null`. That shape is grounded exactly as
/// `openai_chat` grounds it — via
/// `crates/paigasus-helikon-providers-openai/tests/chat_wire.rs`'s
/// `happy_path_text_completion` fixture body, whose own comment records it as
/// "Captured from LiteLLM". This module's own `text_delta` stays faithful to
/// `text_then_trailing_usage.txt` (which omits the key rather than nulling
/// it), so that second shape is scripted only inside the guard test, not
/// served to the subject.
///
/// # `FragmentedToolName` is a verbatim prefix of the capture
///
/// Per `Scenario::expects_stop_reason` (`src/lib.rs`) and the design spec's
/// §6 table (`7, 1` only, no assertion 3), this scenario must not carry a
/// `finish_reason`. `tool_call_stream_fragmented_name.txt`'s capture
/// continues on to a `"tool_calls"` finish chunk and trailing usage, but this
/// subject's script stops right after the three tool-call deltas — see
/// `openai_chat`'s module doc for the full reasoning (same shape, same fix,
/// same file).
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
/// behaves correctly on the wire shapes LiteLLM is actually observed to
/// send, **not** as a standing regression guard for the `canonicalize` fix
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
    /// (see [`text_delta`]): OpenAI-compatible backends behind LiteLLM can
    /// still emit an explicit `"finish_reason":null` on an ordinary delta, so
    /// the marker has to exclude that shape structurally, not merely happen
    /// to avoid the one shape this module scripts. Scanning for the opening
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

    /// The scripted response for one scenario.
    ///
    /// Every scenario is served — `litellm` declines none — so this returns a
    /// bare [`Script`], not an `Option`.
    fn script(scenario: Scenario) -> Script {
        // The two-delta opening every text scenario shares, matching
        // `text_then_trailing_usage.txt`'s two content chunks.
        let opening = || vec![text_delta("Hel"), text_delta("lo")];
        // The opening plus a populated finish_reason chunk.
        let through_stop = || {
            let mut chunks = opening();
            chunks.push(finish_chunk("stop"));
            chunks
        };

        let (chunks, gate_after, ending) = match scenario {
            // Full stream: deltas, stop reason, then usage AFTER it, then
            // [DONE] and a clean end of body — the exact order
            // `text_then_trailing_usage.txt` captures.
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
            // A verbatim prefix of `tool_call_stream_fragmented_name.txt`:
            // the three tool-call deltas, and nothing more — no
            // finish_reason, no usage, no [DONE]. See the module doc's
            // "FragmentedToolName is a verbatim prefix of the capture"
            // section for why the body ends right here.
            Scenario::FragmentedToolName => {
                let chunks = vec![
                    tool_call_start(TOOL_CALL_ID, "get_"),
                    tool_call_name_fragment("weather"),
                    tool_call_args("{\"city\":\"Berlin\"}"),
                ];
                (chunks, None, Ending::Clean)
            }
            // `tool_call_stream.txt`, verbatim and in full: the whole name
            // arrives with the id (empty args), the arguments arrive split
            // across two continuations (so exactly one emitted
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
        };

        Script {
            content_type: "text/event-stream",
            chunks,
            gate_after,
            ending,
        }
    }

    /// The request driven through `Model::invoke`.
    ///
    /// Carries a tool definition for both tool scenarios so the exchange is
    /// coherent — a response returning `tool_calls` for a request that
    /// offered no tools is not a stream a real proxy would ever produce.
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
                .chunks
                .iter()
                .any(|chunk| contains(chunk, FINISH_REASON_MARKER))
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
                Scenario::ToolCallCleanStop => Some(FinishReason::ToolCalls),
                // Truncated, errored and cancelled streams must withhold
                // `Finish` entirely, and so must `FragmentedToolName`, whose
                // script ends before any finish_reason chunk arrives.
                Scenario::TruncatedMidGeneration
                | Scenario::ErrorMidGeneration
                | Scenario::ErrorAfterStopReason
                | Scenario::CancelMidGeneration
                | Scenario::CancelAfterStopReason
                | Scenario::FragmentedToolName => None,
            }
        }

        async fn stream(&self, scenario: Scenario, cancel: CancellationToken) -> Outcome {
            let script = script(scenario);
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
    /// explicit `null` (a shape no committed litellm-crate fixture happens to
    /// carry, but which OpenAI-compatible backends behind LiteLLM can send —
    /// grounded here via `providers-openai/tests/chat_wire.rs`'s
    /// `happy_path_text_completion` fixture, itself captured from LiteLLM;
    /// see `openai_chat`'s citation of the same file).
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
/// consumer got no terminal event at all. `TruncatedAfterStopReason` below is
/// that exact shape, transcribed verbatim from the fixture the SMA-531 fix
/// itself is tested against.
///
/// # Fixture provenance
///
/// Every envelope shape below is transcribed from a fixture committed in
/// `crates/paigasus-helikon-providers-anthropic/tests/fixtures/`, cited on the
/// helper or script arm that builds it:
///
/// - `text_only.txt` — the baseline: `message_start` (including its top-level
///   `"stop_reason":null`), a text `content_block_start`, two
///   `content_block_delta`s, `content_block_stop`, `message_delta`
///   (`stop_reason` + `usage` in one event), `message_stop`. Grounds
///   [`message_start`], [`content_block_start_text`], [`text_delta`],
///   [`content_block_stop`], [`message_delta`] and [`message_stop`], and
///   `CleanStop`'s full script.
/// - `eof_after_message_delta.txt` — the exact SMA-531 shape and the fixture
///   the crate's own `clean_eof_after_message_delta_emits_finish` regression
///   test reads: `text_only.txt`'s prefix through `message_delta`, then the
///   body simply ends — no `message_stop`. `TruncatedAfterStopReason`
///   transcribes this verbatim (via `through_stop()`, with no `message_stop`
///   appended).
/// - `eof_mid_content_block.txt` — grounds `TruncatedMidGeneration`'s
///   envelope shape (`message_start`, `content_block_start`, a
///   `content_block_delta`, then EOF with no `message_delta` at all). The
///   delta *count* is not grounded from this file — as with every other
///   subject in this module, `opening()`'s two deltas are arbitrary content
///   over a grounded envelope, not a literal transcription.
/// - `stream_error.txt` — grounds `ErrorMidGeneration`'s prefix envelope
///   (`message_start`, `content_block_start`, one `content_block_delta`)
///   *before* its own in-band `event: error` frame. See "Why `Ending::Abort`
///   and not the fixtures' own `error` event" below for why that frame itself
///   is not transcribed into this script.
/// - `error_after_message_delta.txt` — grounds `ErrorAfterStopReason`'s prefix
///   envelope, identical to `eof_after_message_delta.txt`'s through
///   `message_delta`, again *before* its own in-band `error` frame.
/// - `parallel_tool_use.txt` and `tool_use_then_continuation.txt` — the
///   `tool_use` envelope: a `content_block_start` carrying the whole `id` and
///   `name` together, followed by exactly one `input_json_delta` carrying the
///   whole argument JSON. Grounds [`tool_use_start`], [`input_json_delta`],
///   and `ToolCallCleanStop`'s script.
///
/// `body_cut_inside_message_stop.txt` and `thinking_then_text.txt` were read
/// but contribute no shape this module needs: the former is a byte-level cut
/// of `eof_after_message_delta.txt` that the SSE parser discards down to the
/// same events (already covered), and this suite has no reasoning-delta
/// scenario for any subject.
///
/// # Why `Ending::Abort` and not the fixtures' own `error` event
///
/// `stream_error.txt` and `error_after_message_delta.txt` both encode
/// Anthropic's real error mechanism: an in-band `event: error` SSE frame,
/// parsed by `AnthropicEvent::Error` and turned into `Err(ModelError::…)` by
/// `stream.rs`'s `consume`. That path is already exercised directly against
/// the translator by the crate's own
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
/// way across every subject, rather than re-deriving Anthropic's in-band error
/// shape a second time in a different harness. Functionally the two
/// mechanisms are indistinguishable to this driver either way: `model.rs`'s
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
/// **`message_start` contains a literal `"stop_reason":null`.** A naive scan
/// for the substring `stop_reason` would therefore match the very first event
/// of every script, whether or not a stop reason was ever encoded.
/// [`STOP_REASON_MARKER`] scans for the populated-string shape instead, pinned
/// against `message_start`'s `null` by `scan_finds_only_a_populated_stop_reason`
/// — the only detector for `ErrorAfterStopReason` degrading into
/// `ErrorMidGeneration`, whose observable events are otherwise byte-identical.
///
/// # `ToolCallCleanStop` never splits `input_json_delta`
///
/// Every committed tool-call fixture (`parallel_tool_use.txt`,
/// `tool_use_then_continuation.txt`) carries one tool call's whole argument
/// JSON in a single `input_json_delta`. Unlike `bedrock`/`openai_chat`/
/// `litellm`, this module does not split the arguments across two calls to its
/// delta helper to *additionally* prove "not more than one delta carries the
/// name" — no committed capture grounds Anthropic ever doing that, and
/// inventing the split would fabricate a shape per this file's provenance
/// rule. A single `input_json_delta` still fully exercises assertion 7's
/// "exactly one, not zero" on this shape: the translator's `name_emitted` flag
/// starts `false`, so the one delta this script sends is the one that must
/// carry the name.
mod anthropic {
    use super::*;
    use paigasus_helikon_providers_anthropic::AnthropicModel;

    /// The model id `build_model_against` requests. Matches the constructor
    /// example in the task brief (`AnthropicModel::messages("claude-3-5-sonnet-latest")`).
    const MODEL_ID: &str = "claude-3-5-sonnet-latest";

    /// The tool name every tool-call fixture declares.
    const TOOL_NAME: &str = "get_weather";

    /// `id` for the single tool call in the tool fixture. Not claimed to match
    /// any real Anthropic id format (`tu_weather`/`tu_a`/`tu_b` is the shape
    /// `parallel_tool_use.txt` and `tool_use_then_continuation.txt` use, but
    /// nothing under test inspects it) — fixed purely so failure output is
    /// stable and greppable.
    const TOOL_USE_ID: &str = "tu_conformance_0";

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
    /// carry an explicit event name — unlike the bare `data:` frames OpenAI
    /// and LiteLLM send — matching every fixture in this crate's own
    /// `tests/fixtures/`.
    ///
    /// The `event:` line is cosmetic to the translator: `model.rs` parses
    /// `event.data` alone via `AnthropicEvent`'s `#[serde(tag = "type")]`,
    /// never `event.event`. It is still written here because every real
    /// Anthropic response sends it, and a fixture that omitted it would not be
    /// the stream it claims to be.
    fn frame(event: &str, payload: serde_json::Value) -> Vec<u8> {
        format!("event: {event}\ndata: {payload}\n\n").into_bytes()
    }

    /// `message_start`, which opens every Anthropic stream and is the event
    /// the translator reads its initial `Usage` from
    /// (`stream.rs`'s `MessageStart` arm).
    ///
    /// Matches `text_only.txt`'s opening event, including the top-level
    /// `"stop_reason":null` on the `message` object — the shape
    /// [`STOP_REASON_MARKER`] must not match.
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
    /// no `ModelEvent` for this; it is included because a real stream opens
    /// every block it later closes.
    fn content_block_start_text() -> Vec<u8> {
        frame(
            "content_block_start",
            json!({
                "type": "content_block_start",
                "index": 0,
                "content_block": { "type": "text", "text": "" }
            }),
        )
    }

    /// One text delta. Envelope matches `text_only.txt`'s delta events; the
    /// content itself is arbitrary, as in every other subject in this file.
    fn text_delta(text: &str) -> Vec<u8> {
        frame(
            "content_block_delta",
            json!({
                "type": "content_block_delta",
                "index": 0,
                "delta": { "type": "text_delta", "text": text }
            }),
        )
    }

    /// `content_block_stop`, closing the block opened above. Matches
    /// `text_only.txt`. Ignored by the translator
    /// (`AnthropicEvent::ContentBlockStop { .. }` only logs); included because
    /// a real stream closes every block it opens.
    fn content_block_stop() -> Vec<u8> {
        frame(
            "content_block_stop",
            json!({ "type": "content_block_stop", "index": 0 }),
        )
    }

    /// `message_delta` — the one event that carries BOTH the stop reason and
    /// usage in a single frame. Matches `text_only.txt`'s and
    /// `eof_after_message_delta.txt`'s shared shape.
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
    /// tool-call events. The translator records `call_id`/`name` here but
    /// emits nothing — the name-carrying `ToolCallDelta` comes with the first
    /// `input_json_delta` — which is why `FragmentedToolName` is declined for
    /// this subject: the name arrives whole, in this one event.
    fn tool_use_start(call_id: &str, name: &str) -> Vec<u8> {
        frame(
            "content_block_start",
            json!({
                "type": "content_block_start",
                "index": 0,
                "content_block": { "type": "tool_use", "id": call_id, "name": name, "input": {} }
            }),
        )
    }

    /// One `input_json_delta`, carrying a whole argument JSON. See the module
    /// doc's "`ToolCallCleanStop` never splits `input_json_delta`" section for
    /// why this is never called twice for one call in this module, unlike its
    /// siblings' tool-argument helpers.
    fn input_json_delta(partial_json: &str) -> Vec<u8> {
        frame(
            "content_block_delta",
            json!({
                "type": "content_block_delta",
                "index": 0,
                "delta": { "type": "input_json_delta", "partial_json": partial_json }
            }),
        )
    }

    /// The scripted response for one scenario, or `None` for
    /// `FragmentedToolName`, which this subject declines.
    ///
    /// Returning an `Option` rather than panicking on the declined scenario is
    /// what lets [`Anthropic::encodes_stop_reason`] call this for *every*
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
            // one event), then message_stop, then a clean end of body.
            // Matches `text_only.txt` in full.
            Scenario::CleanStop => {
                let mut chunks = through_stop();
                chunks.push(message_stop());
                (chunks, None, Ending::Clean)
            }
            // The exact SMA-531 shape: `message_delta` is observed, then the
            // body simply ends — no `message_stop` — so the translator's
            // buffered stop reason has to be flushed by the EOF path
            // (`MessageTranslator::finish`, called from `model.rs`'s
            // `None =>` arm). Matches `eof_after_message_delta.txt` verbatim.
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
            // (`tool_use_start`), the whole argument JSON arrives on the one
            // `input_json_delta` Anthropic is ever observed to send per call
            // (see the module doc's "never splits" section), then a
            // `tool_use` stop reason and its usage, then `message_stop`.
            // Matches `parallel_tool_use.txt`'s single-call shape.
            Scenario::ToolCallCleanStop => (
                vec![
                    message_start(),
                    tool_use_start(TOOL_USE_ID, TOOL_NAME),
                    input_json_delta("{\"city\":\"Berlin\"}"),
                    content_block_stop(),
                    message_delta("tool_use", 18),
                    message_stop(),
                ],
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
    /// coherent — a response returning `tool_use` for a request that offered
    /// no tools is not a stream Anthropic would ever produce.
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
                Scenario::ToolCallCleanStop => Some(FinishReason::ToolCalls),
                // Truncated, errored and cancelled streams must withhold
                // `Finish` entirely.
                Scenario::TruncatedMidGeneration
                | Scenario::ErrorMidGeneration
                | Scenario::ErrorAfterStopReason
                | Scenario::CancelMidGeneration
                | Scenario::CancelAfterStopReason => None,
                // Declined; never reached.
                Scenario::FragmentedToolName => None,
            }
        }

        async fn stream(&self, scenario: Scenario, cancel: CancellationToken) -> Outcome {
            // The name is set once, whole, in the `content_block_start`
            // `tool_use` block; there is no second event it could be split
            // across.
            if scenario == Scenario::FragmentedToolName {
                return Outcome::Declined("name arrives whole in content_block_start");
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
    /// into `ErrorMidGeneration`: their observable events are byte-for-byte
    /// identical (`[TokenDelta, TokenDelta, Err]`), so if `through_stop()`
    /// ever lost its `message_delta`, this scan is what would notice.
    #[test]
    fn scan_finds_only_a_populated_stop_reason() {
        assert!(
            contains(&message_delta("end_turn", 5), STOP_REASON_MARKER),
            "the message_delta frame must carry the marker, or the scan measures nothing"
        );

        for (name, frame) in [
            ("message_start (stop_reason: null)", message_start()),
            ("content_block_start (text)", content_block_start_text()),
            ("content delta", text_delta("Hel")),
            ("content_block_stop", content_block_stop()),
            ("message_stop", message_stop()),
            ("tool_use start", tool_use_start(TOOL_USE_ID, TOOL_NAME)),
            (
                "input_json_delta",
                input_json_delta("{\"city\":\"Berlin\"}"),
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

    #[tokio::test]
    async fn conforms() {
        assert_conforms(&Anthropic).await;
    }
}
