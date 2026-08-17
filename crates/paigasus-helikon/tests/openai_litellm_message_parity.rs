//! Cross-crate parity: the OpenAI and LiteLLM providers must translate the
//! same conversation into byte-identical `messages`.
//!
//! The LiteLLM crate duplicates `to_chat_messages` (SMA-451 design §13.1).
//! Snapshot tests inside either crate pin only that crate's own shape, so both
//! suites would stay green while the two implementations diverged. This test
//! is what makes divergence visible.
//!
//! If this fails, decide deliberately: either the divergence is intentional
//! (LiteLLM fronts backends OpenAI does not) — in which case move the case to
//! a documented-divergence list here — or it is a drift bug.
//!
//! ## History: the `content: null` wire divergence (resolved)
//!
//! This test originally found a real, if cosmetic, wire divergence: an
//! assistant turn synthesized purely from pending tool calls (no preceding
//! text) sets `content: Value::Null` in `to_chat_messages`'s
//! `flush_pending`/`assistant_message` helpers — identically in both crates,
//! so this was never duplication drift. But the OpenAI provider's *wire*
//! output never actually showed that literal `null`:
//! `providers-openai/src/backend/chat.rs` round-trips the translated
//! `serde_json::Value` through `async-openai`'s typed
//! `ChatCompletionRequestAssistantMessage`, whose `content` field is
//! `Option<_>` with `#[serde(skip_serializing_if = "Option::is_none")]` —
//! deserializing `null` yields `None`, which is then omitted on
//! serialization. `providers-litellm` has no `async-openai` dependency by
//! design (SMA-451 design doc D2: "own reqwest + eventsource-stream client")
//! and, until this was fixed, serialized the translated `Value` directly, so
//! its literal `content: null` survived to the wire unchanged.
//!
//! The project owner decided to close the gap in the LiteLLM provider rather
//! than carry a permanent normalization step in this test: the shape had
//! never been exercised end-to-end against a real backend, and it was the
//! only reason this drift detector needed anything beyond a plain
//! `assert_eq!`. `providers-litellm::translate::build_request` now strips a
//! literal `content: null` from assistant messages as a post-processing step
//! (`translate/mod.rs::strip_null_assistant_content`) — the shared
//! `to_chat_messages` translator itself is untouched and stays
//! byte-identical to the OpenAI crate's copy, which is what this test and
//! the SMA-451 design's D6 duplication decision both rest on. With the wire
//! divergence gone, the comparison below is a plain `assert_eq!` on the raw
//! `messages` arrays — there is nothing left to mask.
#![cfg(all(feature = "openai", feature = "litellm"))]

use futures_util::StreamExt;
use paigasus_helikon_core::{
    CancellationToken, ContentPart, Item, MediaSource, Model, ModelRequest,
};

fn fixtures() -> Vec<(&'static str, Vec<Item>)> {
    vec![
        (
            "plain text",
            vec![Item::UserMessage {
                content: vec![ContentPart::Text { text: "hi".into() }],
            }],
        ),
        (
            "system + user",
            vec![
                Item::System {
                    content: vec![ContentPart::Text {
                        text: "be terse".into(),
                    }],
                },
                Item::UserMessage {
                    content: vec![ContentPart::Text { text: "hi".into() }],
                },
            ],
        ),
        (
            "tool call + result",
            vec![
                Item::ToolCall {
                    call_id: "c1".into(),
                    name: "f".into(),
                    args: serde_json::json!({"a": 1}),
                },
                Item::ToolResult {
                    call_id: "c1".into(),
                    content: vec![ContentPart::Text { text: "ok".into() }],
                },
            ],
        ),
        (
            "multimodal user",
            vec![Item::UserMessage {
                content: vec![
                    ContentPart::Text {
                        text: "what?".into(),
                    },
                    ContentPart::Image {
                        source: MediaSource::Base64 {
                            mime_type: "image/png".into(),
                            data: "AAAA".into(),
                        },
                    },
                ],
            }],
        ),
        (
            "assistant with nested tool_use",
            vec![Item::AssistantMessage {
                content: vec![ContentPart::ToolUse {
                    call_id: "c2".into(),
                    name: "g".into(),
                    args: serde_json::json!({}),
                }],
                agent: None,
            }],
        ),
        // Positive control: exercises `assistant_message`'s string-content
        // branch alongside `tool_calls`, so the comparison is demonstrably
        // still sensitive to a real `content` value — the other two
        // assistant-message fixtures both produce a tool-calls-only turn
        // (no text), so neither on its own proves that.
        (
            "assistant with text and nested tool_use",
            vec![Item::AssistantMessage {
                content: vec![
                    ContentPart::Text {
                        text: "let me check".into(),
                    },
                    ContentPart::ToolUse {
                        call_id: "c3".into(),
                        name: "h".into(),
                        args: serde_json::json!({"x": 2}),
                    },
                ],
                agent: None,
            }],
        ),
    ]
}

#[tokio::test]
async fn openai_and_litellm_translate_messages_identically() {
    use wiremock::matchers::method;
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let sse = |body: &'static str| {
        ResponseTemplate::new(200)
            .insert_header("content-type", "text/event-stream")
            .set_body_raw(body, "text/event-stream")
    };
    const DONE: &str =
        "data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n";

    let mut compared = 0usize;

    for (label, items) in fixtures() {
        // OpenAI provider: drive a request through it so the mock server
        // records the real body, then read the body back.
        let oa_server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(sse(DONE))
            .mount(&oa_server)
            .await;
        let oa = paigasus_helikon::openai::OpenAiModel::chat("gpt-4o")
            .api_key("sk-test")
            .base_url(format!("{}/v1", oa_server.uri()))
            .build()
            .unwrap();
        let mut oa_stream = oa
            .invoke(
                {
                    let mut req = ModelRequest::new();
                    req.messages = items.clone();
                    req
                },
                CancellationToken::new(),
            )
            .await
            .unwrap();
        while oa_stream.next().await.is_some() {}
        let oa_body: serde_json::Value =
            serde_json::from_slice(&oa_server.received_requests().await.unwrap()[0].body).unwrap();

        // LiteLLM provider: same fixture, same drive-and-read pattern.
        let ll_server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(sse(DONE))
            .mount(&ll_server)
            .await;
        let ll = paigasus_helikon::litellm::LiteLlmModel::chat("prod-fast")
            .base_url(ll_server.uri())
            .api_key("sk-test")
            .build()
            .unwrap();
        let mut ll_stream = ll
            .invoke(
                {
                    let mut req = ModelRequest::new();
                    req.messages = items.clone();
                    req
                },
                CancellationToken::new(),
            )
            .await
            .unwrap();
        while ll_stream.next().await.is_some() {}
        let ll_body: serde_json::Value =
            serde_json::from_slice(&ll_server.received_requests().await.unwrap()[0].body).unwrap();

        let oa_messages = &oa_body["messages"];
        let ll_messages = &ll_body["messages"];

        // Genuine invariant, not a precondition for normalizing anymore:
        // `providers-litellm::translate::build_request` now strips a
        // literal `content: null` from assistant messages
        // (`strip_null_assistant_content`) to match the OpenAI provider's
        // wire shape (see module docs). Both sides should therefore never
        // carry a literal null `content` at all; assert that on the OpenAI
        // side to catch a regression in the `async-openai` round-trip
        // assumption this test's history documents.
        assert!(
            oa_messages
                .as_array()
                .into_iter()
                .flatten()
                .all(|m| !m.get("content").is_some_and(|c| c.is_null())),
            "fixture `{label}`: openai body contained a literal null `content` \
             — the async-openai round-trip assumption in this file's module \
             docs no longer holds: {oa_messages}"
        );

        // Guard against a vacuous pass: `Value` indexing returns `Null` for
        // a missing key, so if `messages` ever disappeared from both bodies
        // (e.g. a body-shape change on both sides) this would silently
        // compare `Null == Null` and report green rather than failing on a
        // hollowed-out drift detector.
        assert!(
            oa_messages.as_array().is_some_and(|a| !a.is_empty()),
            "openai body had no messages array for fixture `{label}`: {oa_body}"
        );
        assert!(
            ll_messages.as_array().is_some_and(|a| !a.is_empty()),
            "litellm body had no messages array for fixture `{label}`: {ll_body}"
        );

        assert_eq!(
            oa_messages, ll_messages,
            "messages diverged for fixture `{label}`\n  openai:  {oa_messages}\n  litellm: {ll_messages}"
        );
        compared += 1;
    }

    assert_eq!(compared, fixtures().len(), "not every fixture was compared");
}
