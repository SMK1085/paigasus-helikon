//! The registered conformance subjects.
//!
//! Each subject in this file serves its own wire bytes through the crate's
//! paced HTTP server and hands back the stream from its real `Model::invoke`,
//! so every assertion in `assert_conforms` is made about production driver and
//! production translator code — never about a reimplementation of either.
//!
//! Adding a subject means adding a `StreamUnderTest` impl and one
//! `#[tokio::test]` that calls `assert_conforms`. Scenarios a provider's wire
//! format cannot express are declined with a reason that must match the pinned
//! `DECLINED` table; the reasons below are literal strings copied from
//! `src/declines.rs` rather than looked up from it, because a subject that read
//! the table would make the table's own cross-check vacuous.

use paigasus_helikon_core::{
    CancellationToken, ContentPart, FinishReason, Item, Model, ModelRequest, ToolDef,
};
use paigasus_helikon_provider_stream_conformance::{
    assert_conforms, eventstream::frame, Ending, Outcome, PacedServer, Scenario, Script,
    StreamUnderTest,
};
use serde_json::json;

// ── bedrock ───────────────────────────────────────────────────────────────────

/// The tool name every Bedrock tool fixture declares.
const BEDROCK_TOOL_NAME: &str = "get_weather";

/// `toolUseId` for the single tool call in the Bedrock tool fixtures. Shaped
/// like a real Bedrock id (`tooluse_` plus an opaque token) but fixed, so the
/// failure output is stable.
const BEDROCK_TOOL_USE_ID: &str = "tooluse_conformance_0";

/// The `Model::invoke` stream contract, checked against the Bedrock
/// `ConverseStream` provider.
///
/// # Fixture provenance
///
/// Bedrock is the one subject in this suite whose wire format is binary, and
/// the repo holds no captured Bedrock traffic to transcribe. So — as the task
/// brief directs when no capture exists — every event shape below is derived
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
struct Bedrock;

/// `messageStart`, which opens every Converse stream.
///
/// The translator ignores it (`ConverseStreamOutput::MessageStart(_) => {}`);
/// it is here because a real stream always sends it first, and a fixture that
/// silently omitted the opening event would not be the stream it claims to be.
/// Key: `role` (`shape_message_start_event.rs`), value from `ConversationRole`
/// (`_conversation_role.rs`: `"assistant"`).
fn bedrock_message_start() -> Vec<u8> {
    frame("messageStart", &json!({ "role": "assistant" }))
}

/// One text delta. `stream.rs`'s `ContentBlockDelta::Text` arm turns this into
/// `ModelEvent::TokenDelta`.
///
/// Keys: `contentBlockIndex` and `delta`
/// (`shape_content_block_delta_event.rs`), with `text` the `ContentBlockDelta`
/// union member (`shape_content_block_delta.rs`).
fn bedrock_text_delta(text: &str) -> Vec<u8> {
    frame(
        "contentBlockDelta",
        &json!({ "contentBlockIndex": 0, "delta": { "text": text } }),
    )
}

/// `contentBlockStart` opening a tool-use block.
///
/// The translator records the block's `call_id` and `name` here but emits
/// nothing — the name-carrying `ToolCallDelta` comes with the *first* input
/// delta. That is why `FragmentedToolName` is declined for this subject: the
/// name arrives whole, in this one event.
///
/// Keys: `contentBlockIndex` and `start`
/// (`shape_content_block_start_event.rs`), `toolUse` the union member
/// (`shape_content_block_start.rs`), and `toolUseId` / `name` inside it
/// (`shape_tool_use_block_start.rs`).
fn bedrock_tool_use_start() -> Vec<u8> {
    frame(
        "contentBlockStart",
        &json!({
            "contentBlockIndex": 0,
            "start": {
                "toolUse": { "toolUseId": BEDROCK_TOOL_USE_ID, "name": BEDROCK_TOOL_NAME }
            }
        }),
    )
}

/// One tool-argument delta. Bedrock streams tool input as a partial JSON
/// string, so `input` is a string fragment and not an object.
///
/// Keys: `delta.toolUse` (`shape_content_block_delta.rs`) with `input`
/// (`shape_tool_use_block_delta.rs`).
fn bedrock_tool_use_delta(input: &str) -> Vec<u8> {
    frame(
        "contentBlockDelta",
        &json!({ "contentBlockIndex": 0, "delta": { "toolUse": { "input": input } } }),
    )
}

/// `contentBlockStop`, which closes a content block. Ignored by the translator
/// (`ConverseStreamOutput::ContentBlockStop(_) => {}`), included because a real
/// stream closes every block it opened. Key: `contentBlockIndex`
/// (`shape_content_block_stop_event.rs`).
fn bedrock_content_block_stop() -> Vec<u8> {
    frame("contentBlockStop", &json!({ "contentBlockIndex": 0 }))
}

/// `messageStop`, the only event carrying a stop reason.
///
/// `reason` is a `StopReason` wire string (`_stop_reason.rs`): `"end_turn"`
/// maps to `FinishReason::Stop` and `"tool_use"` to `FinishReason::ToolCalls`,
/// per `stream.rs`'s `finish_events`. Key: `stopReason`
/// (`shape_message_stop_event.rs`).
fn bedrock_message_stop(reason: &str) -> Vec<u8> {
    frame("messageStop", &json!({ "stopReason": reason }))
}

/// `metadata`, the only event carrying usage — and it arrives **after**
/// `messageStop`, which is the whole reason the translator buffers the stop
/// reason instead of emitting `Finish` on the spot.
///
/// **This ordering is not self-enforcing, so do not reorder it casually.**
/// `stream.rs` deliberately handles the reversed order too (`Metadata` before
/// `MessageStop` emits `Usage` at once and `Finish` on the stop), so a fixture
/// that put `metadata` first would still pass every assertion — while quietly
/// testing a wire shape Bedrock does not produce, and no longer covering the
/// buffering path that exists precisely because the real order is this one.
/// Verified by mutation while registering this subject: swapping the two left
/// the suite green.
///
/// Keys: `usage` and `metrics` (`shape_converse_stream_metadata_event.rs`),
/// `inputTokens` / `outputTokens` / `totalTokens` (`shape_token_usage.rs`), and
/// `latencyMs` (`shape_converse_stream_metrics.rs`).
fn bedrock_metadata() -> Vec<u8> {
    frame(
        "metadata",
        &json!({
            "usage": { "inputTokens": 11, "outputTokens": 4, "totalTokens": 15 },
            "metrics": { "latencyMs": 314 }
        }),
    )
}

/// The scripted response for one scenario.
///
/// Each script is the same Converse stream cut at a different point, so the
/// scenarios differ only in what the client gets to observe — which is exactly
/// the distinction the suite is checking.
fn bedrock_script(scenario: Scenario) -> Script {
    // The prefix every text script shares: the opening event and two deltas, so
    // a cancellation can be gated *between* two `TokenDelta`s rather than
    // before the first one.
    let opening = || {
        vec![
            bedrock_message_start(),
            bedrock_text_delta("Hel"),
            bedrock_text_delta("lo"),
        ]
    };
    // The opening plus a completed block and a stop reason.
    let through_stop = || {
        let mut chunks = opening();
        chunks.push(bedrock_content_block_stop());
        chunks.push(bedrock_message_stop("end_turn"));
        chunks
    };

    let (chunks, gate_after, ending) = match scenario {
        // Full stream: deltas, stop reason, then the usage-bearing metadata
        // event after it, then a clean end of body.
        Scenario::CleanStop => {
            let mut chunks = through_stop();
            chunks.push(bedrock_metadata());
            (chunks, None, Ending::Clean)
        }
        // The stop reason is observed, then the body simply ends — no
        // `metadata`, so the translator's buffered stop reason has to be
        // flushed by the EOF path in `model.rs` (`translator.finish()`).
        Scenario::TruncatedAfterStopReason => (through_stop(), None, Ending::Clean),
        // The body ends cleanly mid-generation: no `messageStop`, so no stop
        // reason is ever observed and there is nothing to flush.
        Scenario::TruncatedMidGeneration => (opening(), None, Ending::Clean),
        // Same prefix, but the connection is torn down without the terminating
        // chunk, so the SDK's receiver yields a transport error.
        Scenario::ErrorMidGeneration => (opening(), None, Ending::Abort),
        // The stop reason is buffered and *then* the body is aborted, so the
        // error path must not flush it as a `Finish`.
        Scenario::ErrorAfterStopReason => (through_stop(), None, Ending::Abort),
        // Two chunks go out — `messageStart` and the first delta — and the
        // server then parks, holding the second delta back. The harness sees
        // the stream fall quiet and cancels. No `messageStop` anywhere in this
        // script, so no stop reason exists even after the gate is released.
        Scenario::CancelMidGeneration => (opening(), Some(2), Ending::Clean),
        // A whole tool call: the name arrives with the block start, the
        // arguments arrive split across two deltas (so exactly one emitted
        // `ToolCallDelta` carries the name), then a `tool_use` stop reason and
        // the usage-bearing metadata event.
        Scenario::ToolCallCleanStop => (
            vec![
                bedrock_message_start(),
                bedrock_tool_use_start(),
                bedrock_tool_use_delta("{\"city\":"),
                bedrock_tool_use_delta("\"Berlin\"}"),
                bedrock_content_block_stop(),
                bedrock_message_stop("tool_use"),
                bedrock_metadata(),
            ],
            None,
            Ending::Clean,
        ),
        Scenario::CancelAfterStopReason | Scenario::FragmentedToolName => {
            unreachable!("declined scenarios never reach a script")
        }
    };

    Script {
        content_type: "application/vnd.amazon.eventstream",
        chunks,
        gate_after,
        ending,
    }
}

/// The request driven through `Model::invoke`.
///
/// The scripted response is fixed, so the request only has to be one the
/// provider will translate and send. It carries a tool definition for the tool
/// scenarios so the exchange is coherent — a Converse stream returning a
/// `toolUse` block for a request that offered no tools is not a stream Bedrock
/// would ever produce.
fn bedrock_request(scenario: Scenario) -> ModelRequest {
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
            name: BEDROCK_TOOL_NAME.to_owned(),
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
/// Copied from `eventstream::build_bedrock_model_against`, which is
/// `#[cfg(test)]` and so belongs to the lib's unit-test build only —
/// unreachable from an integration test. The alternative, making it a normal
/// `pub` item, would put the whole AWS SDK in the lib's dependency graph for
/// every subject in this suite.
///
/// The `SdkConfig` is assembled by hand rather than through
/// `aws_config::defaults(..).load()` because that loader is `async` and would
/// also consult the ambient environment — a developer's real `AWS_ENDPOINT_URL`
/// or credential profile must not be able to change what this suite talks to.
fn build_bedrock_model_against(base_url: &str) -> paigasus_helikon_providers_bedrock::BedrockModel {
    use aws_sdk_bedrockruntime::config::{Credentials, SharedCredentialsProvider};

    let sdk_config = aws_config::SdkConfig::builder()
        // Pinned, matching `BedrockModel::from_env`, so a Dependabot
        // `aws-config` bump cannot silently shift SDK behaviour under the suite.
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

#[async_trait::async_trait]
impl StreamUnderTest for Bedrock {
    fn name(&self) -> &'static str {
        "bedrock"
    }

    fn encodes_stop_reason(&self, scenario: Scenario) -> bool {
        // Every served fixture above encodes exactly what its scenario calls
        // for: `through_stop()` ends with a `messageStop`, `opening()` has
        // none, and the tool script carries a `tool_use` one.
        scenario.expects_stop_reason()
    }

    fn fixture_tool_name(&self) -> &'static str {
        BEDROCK_TOOL_NAME
    }

    fn fixture_finish_reason(&self, scenario: Scenario) -> Option<FinishReason> {
        match scenario {
            // `"end_turn"` maps to `StopReason::EndTurn` and then to
            // `FinishReason::Stop` (`stream.rs`'s `finish_events`). Both of
            // these scenarios reach a `Finish`: `CleanStop` through the
            // `Metadata` path, `TruncatedAfterStopReason` through the EOF
            // flush.
            Scenario::CleanStop | Scenario::TruncatedAfterStopReason => Some(FinishReason::Stop),
            // `"tool_use"` maps to `StopReason::ToolUse` and then to
            // `FinishReason::ToolCalls`, since this request is not a
            // structured-output synthesis.
            Scenario::ToolCallCleanStop => Some(FinishReason::ToolCalls),
            // Truncated, errored and cancelled streams must withhold `Finish`
            // entirely.
            _ => None,
        }
    }

    async fn stream(&self, scenario: Scenario, cancel: CancellationToken) -> Outcome {
        match scenario {
            // The name is set once, whole, in the `contentBlockStart` toolUse
            // block; there is no second event it could be split across.
            Scenario::FragmentedToolName => {
                return Outcome::Declined("name arrives whole in toolUse start")
            }
            // The translator buffers the stop reason at `MessageStop` and emits
            // nothing until `Metadata`, so between "stop reason observed" and
            // "Finish emitted" the stream is silent. With no event in that
            // window there is no edge to gate on, and any gate placed there
            // would degrade into `CancelMidGeneration`.
            Scenario::CancelAfterStopReason => {
                return Outcome::Declined("no observable event between MessageStop and Metadata")
            }
            _ => {}
        }

        let mut server = PacedServer::start(bedrock_script(scenario)).await;
        let gate = server.take_gate();
        let model = build_bedrock_model_against(&server.base_url());

        let stream = model
            .invoke(bedrock_request(scenario), cancel)
            .await
            .expect("bedrock invoke should reach the local paced server");

        Outcome::Served { stream, gate }
    }
}

#[tokio::test]
async fn bedrock_conforms() {
    assert_conforms(&Bedrock).await;
}
