//! Hand-built `application/vnd.amazon.eventstream` frames, so a Bedrock
//! fixture can be written as JSON instead of as opaque captured bytes.
//!
//! Bedrock is the only subject in this suite whose wire format is binary. Every
//! other provider streams SSE, which a fixture file can hold verbatim; an
//! eventstream frame carries two CRC-32s over a length-prefixed header block,
//! so a hand-typed `.bin` fixture is neither reviewable nor editable. Building
//! the frames with the same encoder the SDK itself uses keeps the fixture
//! readable *and* keeps the bytes on the wire real: `Model::invoke` runs its
//! production transport, the SDK's own decoder parses these frames, and the
//! production translator sees genuine `ConverseStreamOutput` values.
//!
//! ## The silent-drop hazard
//!
//! `crates/paigasus-helikon-providers-bedrock/src/stream.rs` ends its `match`
//! with a forward-compat catch-all that ignores unknown
//! `ConverseStreamOutput` variants. A frame whose `:event-type` does not match
//! a union member name exactly therefore produces **no error and no event** —
//! it simply vanishes. The failure presents as an empty stream and reads like a
//! translator bug, so the header names below are load-bearing and are pinned by
//! `frame_headers_are_exactly_the_three_the_decoder_requires`.

use aws_smithy_eventstream::frame::write_message_to;
use aws_smithy_types::event_stream::{Header, HeaderValue, Message};

/// Encode one Bedrock `ConverseStream` event as a complete
/// `vnd.amazon.eventstream` frame.
///
/// `event_type` is the union member name **exactly** as the Bedrock model
/// spells it — `messageStart`, `contentBlockStart`, `contentBlockDelta`,
/// `contentBlockStop`, `messageStop`, `metadata`. `payload` is the event's JSON
/// body, serialised as the frame payload.
///
/// The returned bytes are a self-contained frame (prelude, headers, payload,
/// message CRC) and can be handed to the paced server as one chunk, or split
/// across chunks to exercise a decoder that has to reassemble them.
///
/// # Panics
///
/// Panics if `payload` cannot be serialised (only reachable for a
/// `serde_json::Value` containing a map with non-string keys, which the `json!`
/// macro cannot build) or if the encoder rejects the message.
pub fn frame(event_type: &str, payload: &serde_json::Value) -> Vec<u8> {
    let body = serde_json::to_vec(payload).expect("frame payload should serialise");

    // All three headers are string headers, and all three are required: the
    // SDK's `parse_response_headers` errors without `:message-type`, dispatches
    // on `:event-type`, and the generated unmarshaller reads the payload as
    // JSON on the strength of `:content-type`.
    let message = Message::new_from_parts(
        vec![
            Header::new(":message-type", HeaderValue::String("event".into())),
            Header::new(
                ":event-type",
                // `StrBytes: From<&'static str>` only, so an ordinary `&str`
                // has to go through `String`.
                HeaderValue::String(event_type.to_owned().into()),
            ),
            Header::new(
                ":content-type",
                HeaderValue::String("application/json".into()),
            ),
        ],
        body,
    );

    let mut buf = Vec::new();
    write_message_to(&message, &mut buf).expect("eventstream message should encode");
    buf
}

/// Build a real `BedrockModel` pointed at a local plain-HTTP endpoint.
///
/// The `SdkConfig` is assembled by hand rather than through
/// `aws_config::defaults(..).load()` because that loader is `async` and would
/// also consult the ambient environment — a developer's real
/// `AWS_ENDPOINT_URL` or credential profile must not be able to change what
/// this suite talks to. Everything the SDK needs beyond these four settings
/// (HTTP client, sleep impl, time source, retry policy) comes from the
/// generated client's default runtime plugins.
///
/// The credentials are inert placeholders: SigV4 still signs the request, and
/// the paced server ignores the `Authorization` header entirely.
#[cfg(test)]
fn build_bedrock_model_against(base_url: &str) -> paigasus_helikon_providers_bedrock::BedrockModel {
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

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::StreamExt as _;
    use paigasus_helikon_core::{
        CancellationToken, ContentPart, Item, Model, ModelEvent, ModelRequest,
    };

    /// The AWS SDK must reach a local plain-HTTP endpoint, and hand-built
    /// eventstream frames must decode into real translator output.
    ///
    /// A failure here means the whole Bedrock registration needs the
    /// `StaticReplayClient` fallback in spec §11 — report it, do not paper over
    /// it. In particular, a stream of zero `TokenDelta`s means the frames hit
    /// the forward-compat catch-all in `bedrock/src/stream.rs` and were
    /// dropped, which looks like a translator bug but is not one.
    #[tokio::test]
    async fn bedrock_reads_hand_built_frames_over_local_http() {
        let script = crate::Script {
            content_type: "application/vnd.amazon.eventstream",
            chunks: vec![
                frame(
                    "contentBlockDelta",
                    &serde_json::json!({
                        "contentBlockIndex": 0,
                        "delta": { "text": "hi" }
                    }),
                ),
                frame(
                    "messageStop",
                    &serde_json::json!({ "stopReason": "end_turn" }),
                ),
                frame(
                    "metadata",
                    &serde_json::json!({
                        "usage": { "inputTokens": 3, "outputTokens": 1, "totalTokens": 4 }
                    }),
                ),
            ],
            gate_after: None,
            ending: crate::Ending::Clean,
        };
        let server = crate::PacedServer::start(script).await;

        let model = build_bedrock_model_against(&server.base_url());
        let mut req = ModelRequest::new();
        req.messages = vec![Item::UserMessage {
            content: vec![ContentPart::Text { text: "hi".into() }],
        }];

        let mut stream = model
            .invoke(req, CancellationToken::new())
            .await
            .expect("invoke should reach the local endpoint");

        let mut events = Vec::new();
        while let Some(ev) = stream.next().await {
            events.push(ev.expect("no error event expected on a clean script"));
        }

        assert!(
            events
                .iter()
                .any(|e| matches!(e, ModelEvent::TokenDelta { text } if text == "hi")),
            "expected a TokenDelta; zero means the frames were dropped as unknown, got {events:?}"
        );
        assert!(
            events
                .iter()
                .any(|e| matches!(e, ModelEvent::Finish { .. })),
            "expected a terminal Finish, got {events:?}"
        );
    }

    /// Pin the three headers by name and value.
    ///
    /// The transport test above can only tell you that *something* is wrong
    /// with a frame — a mistyped header name and a mistyped event name both
    /// present as an empty stream, because the SDK decodes the frame into
    /// `ConverseStreamOutput::Unknown` and the translator's forward-compat
    /// catch-all drops it without an error. This test separates the two: it
    /// fails on the encoder, so a green here plus a red above means the fault
    /// is the *event name*, not the framing.
    #[test]
    fn frame_headers_are_exactly_the_three_the_decoder_requires() {
        use aws_smithy_eventstream::frame::{DecodedFrame, MessageFrameDecoder};

        let bytes = frame("contentBlockDelta", &serde_json::json!({ "a": 1 }));
        let mut decoder = MessageFrameDecoder::new();
        let decoded = match decoder
            .decode_frame(bytes.as_slice())
            .expect("a frame this module wrote must decode")
        {
            DecodedFrame::Complete(message) => message,
            DecodedFrame::Incomplete => {
                panic!("`frame` must emit one complete, self-contained frame")
            }
        };

        let headers: Vec<(String, String)> = decoded
            .headers()
            .iter()
            .map(|h| {
                (
                    h.name().as_str().to_owned(),
                    h.value()
                        .as_string()
                        .expect("every eventstream header here is a string header")
                        .as_str()
                        .to_owned(),
                )
            })
            .collect();

        assert_eq!(
            headers,
            vec![
                (":message-type".to_owned(), "event".to_owned()),
                (":event-type".to_owned(), "contentBlockDelta".to_owned()),
                (":content-type".to_owned(), "application/json".to_owned()),
            ],
        );
        assert_eq!(
            decoded.payload().as_ref(),
            br#"{"a":1}"#,
            "the payload must be the JSON body, unwrapped"
        );
    }
}
