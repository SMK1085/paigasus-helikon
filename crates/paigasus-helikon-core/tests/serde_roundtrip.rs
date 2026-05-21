//! Locks AC #1 — every serializable variant round-trips through JSON.
//!
//! Each test serializes a representative instance, snapshots the prettified
//! JSON, deserializes it back, and re-serializes to confirm round-trip
//! equality. The snapshot diff is the visual regression check; the
//! `assert_eq!` covers semantic equivalence.

use paigasus_helikon_core::*;

fn roundtrip<T>(value: &T) -> String
where
    T: serde::Serialize + serde::de::DeserializeOwned,
{
    let json = serde_json::to_string_pretty(value).unwrap();
    let parsed: T = serde_json::from_str(&json).unwrap();
    let json2 = serde_json::to_string_pretty(&parsed).unwrap();
    assert_eq!(json, json2, "round-trip mismatch");
    json
}

// --- Item ---

#[test]
fn item_user_message_roundtrip() {
    let item = Item::UserMessage {
        content: vec![ContentPart::Text {
            text: "hello".into(),
        }],
    };
    insta::assert_snapshot!(roundtrip(&item));
}

#[test]
fn item_assistant_message_roundtrip() {
    let item = Item::AssistantMessage {
        content: vec![
            ContentPart::Text {
                text: "let me check".into(),
            },
            ContentPart::Reasoning {
                text: "the user asked X".into(),
            },
        ],
        agent: Some("triage".into()),
    };
    insta::assert_snapshot!(roundtrip(&item));
}

#[test]
fn item_assistant_message_no_agent_roundtrip() {
    let item = Item::AssistantMessage {
        content: vec![ContentPart::Text {
            text: "anonymous reply".into(),
        }],
        agent: None,
    };
    insta::assert_snapshot!(roundtrip(&item));
}

#[test]
fn item_system_roundtrip() {
    let item = Item::System {
        content: vec![ContentPart::Text {
            text: "you are a helpful assistant".into(),
        }],
    };
    insta::assert_snapshot!(roundtrip(&item));
}

#[test]
fn item_tool_call_roundtrip() {
    let item = Item::ToolCall {
        call_id: "call_abc".into(),
        name: "calculator".into(),
        args: serde_json::json!({ "expr": "1+1" }),
    };
    insta::assert_snapshot!(roundtrip(&item));
}

#[test]
fn item_tool_result_roundtrip() {
    let item = Item::ToolResult {
        call_id: "call_abc".into(),
        content: vec![ContentPart::Text { text: "2".into() }],
    };
    insta::assert_snapshot!(roundtrip(&item));
}

// --- ContentPart ---

#[test]
fn content_part_text_roundtrip() {
    let part = ContentPart::Text { text: "hi".into() };
    insta::assert_snapshot!(roundtrip(&part));
}

#[test]
fn content_part_image_roundtrip() {
    let part = ContentPart::Image {
        source: MediaSource::Url {
            url: "https://example.com/cat.png".into(),
        },
    };
    insta::assert_snapshot!(roundtrip(&part));
}

#[test]
fn content_part_audio_roundtrip() {
    let part = ContentPart::Audio {
        source: MediaSource::Base64 {
            mime_type: "audio/wav".into(),
            data: "UklGRg==".into(),
        },
    };
    insta::assert_snapshot!(roundtrip(&part));
}

#[test]
fn content_part_tool_use_roundtrip() {
    let part = ContentPart::ToolUse {
        call_id: "call_xyz".into(),
        name: "search".into(),
        args: serde_json::json!({ "q": "rust" }),
    };
    insta::assert_snapshot!(roundtrip(&part));
}

#[test]
fn content_part_tool_result_roundtrip() {
    let part = ContentPart::ToolResult {
        call_id: "call_xyz".into(),
        content: vec![ContentPart::Text {
            text: "result".into(),
        }],
    };
    insta::assert_snapshot!(roundtrip(&part));
}

#[test]
fn content_part_reasoning_roundtrip() {
    let part = ContentPart::Reasoning {
        text: "considering...".into(),
    };
    insta::assert_snapshot!(roundtrip(&part));
}

// --- MediaSource ---

#[test]
fn media_source_url_roundtrip() {
    let src = MediaSource::Url {
        url: "https://example.com/img.png".into(),
    };
    insta::assert_snapshot!(roundtrip(&src));
}

#[test]
fn media_source_base64_roundtrip() {
    let src = MediaSource::Base64 {
        mime_type: "image/png".into(),
        data: "iVBORw0KGgo=".into(),
    };
    insta::assert_snapshot!(roundtrip(&src));
}
