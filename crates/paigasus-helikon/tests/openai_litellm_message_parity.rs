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
//! ## Documented divergence: explicit `content: null` vs. an omitted key
//!
//! When the translated conversation puts `content: null` on an
//! assistant-role message (an assistant turn with tool calls but no text —
//! `to_chat_messages`'s `flush_pending`/`assistant_message` helpers, which
//! are byte-identical between the two crates, both do this deliberately),
//! the OpenAI provider's wire output never actually contains a literal
//! `null`: `openai/src/backend/chat.rs` round-trips the translated
//! `serde_json::Value` through `async-openai`'s typed
//! `ChatCompletionRequestAssistantMessage`, whose `content` field is
//! `Option<_>` with `#[serde(skip_serializing_if = "Option::is_none")]` —
//! deserializing `null` yields `None`, which is then omitted on
//! serialization. The LiteLLM provider does not depend on `async-openai` by
//! design (SMA-451 design doc D2: "own reqwest + eventsource-stream client")
//! and serializes the translated `Value` directly, so its literal
//! `content: null` survives to the wire unchanged.
//!
//! This is **not** duplication drift — the shared translation helpers agree
//! byte-for-byte — and it is semantically inert: the OpenAI Chat Completions
//! API documents `content` as nullable for the assistant role, so `null` and
//! an absent key mean the same thing to a conformant backend. `normalize`
//! below drops a literal JSON `null` `content` key from both sides
//! (symmetric, and a no-op for OpenAI's output, which never contains one) so
//! this one documented shape doesn't spuriously fail the test while any
//! other difference — including a *non-null* `content` mismatch, or any
//! divergence in `tool_calls`, roles, or ordering — still does.
#![cfg(all(feature = "openai", feature = "litellm"))]

use futures_util::StreamExt;
use paigasus_helikon_core::{
    CancellationToken, ContentPart, Item, MediaSource, Model, ModelRequest,
};

/// Drop a literal JSON `null` `content` key from every **assistant-role**
/// message object, so the documented `content: null` vs. omitted-key
/// divergence (see module docs) doesn't fail the comparison. Every other
/// field, including a present-but-different `content` value, is left
/// untouched.
///
/// Scoped to `role == "assistant"` — the only role either translator emits
/// a literal `content: null` for — rather than to "any message with a null
/// content", so the masking set stays exactly as wide as the documented
/// justification above it, not wider.
fn normalize(messages: &serde_json::Value) -> serde_json::Value {
    let mut messages = messages.clone();
    if let Some(arr) = messages.as_array_mut() {
        for msg in arr {
            if let Some(obj) = msg.as_object_mut() {
                let is_assistant = obj.get("role").and_then(|r| r.as_str()) == Some("assistant");
                if is_assistant && obj.get("content").is_some_and(|c| c.is_null()) {
                    obj.remove("content");
                }
            }
        }
    }
    messages
}

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
        // still sensitive to a real (non-null) `content` value — the other
        // two assistant-message fixtures both produce null content, which
        // `normalize` removes, so neither on its own proves that.
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

        // Self-check the module docs' load-bearing claim: the OpenAI
        // provider's wire output never contains a literal null `content`
        // (the `async-openai` typed round-trip drops it). If that ever
        // stops being true — e.g. `providers-openai` drops the round-trip —
        // `normalize` would start silently masking a real divergence rather
        // than the documented cosmetic one. Fail loudly here instead of
        // letting that drift unnoticed.
        assert!(
            oa_body["messages"]
                .as_array()
                .into_iter()
                .flatten()
                .all(|m| !m.get("content").is_some_and(|c| c.is_null())),
            "fixture `{label}`: openai body contained a literal null `content` \
             — the async-openai round-trip assumption in this file's module \
             docs no longer holds; re-derive `normalize`'s masking set: {}",
            oa_body["messages"]
        );

        let oa_messages = normalize(&oa_body["messages"]);
        let ll_messages = normalize(&ll_body["messages"]);

        // Guard against a vacuous pass: `Value` indexing returns `Null` for
        // a missing key, and `normalize` returns its input unchanged when
        // it is not an array, so if `messages` ever disappeared from both
        // bodies (e.g. a body-shape change on both sides) this would
        // silently compare `Null == Null` and report green rather than
        // failing on a hollowed-out drift detector.
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
            "messages diverged for fixture `{label}`\n  openai (raw):    {}\n  litellm (raw):   {}\n  openai (norm):   {oa_messages}\n  litellm (norm):  {ll_messages}",
            oa_body["messages"], ll_body["messages"]
        );
        compared += 1;
    }

    assert_eq!(compared, fixtures().len(), "not every fixture was compared");
}
