//! Locks AC #1 — every serializable variant round-trips through JSON.
//!
//! Each test serializes a representative instance, snapshots the prettified
//! JSON, deserializes it back, and re-serializes to confirm round-trip
//! equality. The snapshot diff is the visual regression check; the
//! `assert_eq!` covers semantic equivalence.

use jiff::Timestamp;
use paigasus_helikon_core::*;

fn pinned_ts() -> Timestamp {
    // Fixed instant so insta snapshots are deterministic.
    Timestamp::from_second(0).expect("0 is a valid timestamp")
}

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

// --- AgentEvent ---

#[test]
fn agent_event_run_started_roundtrip() {
    let ev = AgentEvent::RunStarted {
        agent: "triage".into(),
    };
    insta::assert_snapshot!(roundtrip(&ev));
}

#[test]
fn agent_event_turn_started_roundtrip() {
    let ev = AgentEvent::TurnStarted { turn: 1 };
    insta::assert_snapshot!(roundtrip(&ev));
}

#[test]
fn agent_event_token_delta_roundtrip() {
    let ev = AgentEvent::TokenDelta { text: "hel".into() };
    insta::assert_snapshot!(roundtrip(&ev));
}

#[test]
fn agent_event_reasoning_delta_roundtrip() {
    let ev = AgentEvent::ReasoningDelta {
        text: "let me think".into(),
    };
    insta::assert_snapshot!(roundtrip(&ev));
}

#[test]
fn agent_event_tool_call_delta_roundtrip() {
    let ev = AgentEvent::ToolCallDelta {
        call_id: "call_1".into(),
        name: Some("calc".into()),
        args_delta: "{\"x\":".into(),
    };
    insta::assert_snapshot!(roundtrip(&ev));
}

#[test]
fn agent_event_tool_call_delta_no_name_roundtrip() {
    let ev = AgentEvent::ToolCallDelta {
        call_id: "call_1".into(),
        name: None,
        args_delta: "1+1}".into(),
    };
    insta::assert_snapshot!(roundtrip(&ev));
}

#[test]
fn agent_event_message_output_roundtrip() {
    let ev = AgentEvent::MessageOutput {
        item: Item::AssistantMessage {
            content: vec![ContentPart::Text {
                text: "hello".into(),
            }],
            agent: Some("triage".into()),
        },
    };
    insta::assert_snapshot!(roundtrip(&ev));
}

#[test]
fn agent_event_tool_call_item_roundtrip() {
    let ev = AgentEvent::ToolCallItem {
        item: Item::ToolCall {
            call_id: "call_1".into(),
            name: "calc".into(),
            args: serde_json::json!({ "expr": "1+1" }),
        },
    };
    insta::assert_snapshot!(roundtrip(&ev));
}

#[test]
fn agent_event_tool_output_item_roundtrip() {
    let ev = AgentEvent::ToolOutputItem {
        item: Item::ToolResult {
            call_id: "call_1".into(),
            content: vec![ContentPart::Text { text: "2".into() }],
        },
    };
    insta::assert_snapshot!(roundtrip(&ev));
}

#[test]
fn agent_event_handoff_item_roundtrip() {
    let ev = AgentEvent::HandoffItem {
        from: "triage".into(),
        to: "billing".into(),
    };
    insta::assert_snapshot!(roundtrip(&ev));
}

#[test]
fn agent_event_agent_updated_roundtrip() {
    let ev = AgentEvent::AgentUpdated {
        agent: "billing".into(),
    };
    insta::assert_snapshot!(roundtrip(&ev));
}

#[test]
fn agent_event_guardrail_triggered_roundtrip() {
    let ev = AgentEvent::GuardrailTriggered {
        kind: GuardrailKind::InputPolicy,
        info: serde_json::json!({ "score": 0.92 }),
    };
    insta::assert_snapshot!(roundtrip(&ev));
}

#[test]
fn agent_event_guardrail_triggered_other_roundtrip() {
    let ev = AgentEvent::GuardrailTriggered {
        kind: GuardrailKind::Other {
            reason: "custom policy".into(),
        },
        info: serde_json::json!({ "detail": "matched X" }),
    };
    insta::assert_snapshot!(roundtrip(&ev));
}

#[test]
fn agent_event_approval_requested_roundtrip() {
    let ev = AgentEvent::ApprovalRequested {
        call_id: "call_1".into(),
        tool: "delete_file".into(),
        args: serde_json::json!({ "path": "/etc/passwd" }),
    };
    insta::assert_snapshot!(roundtrip(&ev));
}

#[test]
fn agent_event_run_completed_roundtrip() {
    let mut usage = TokenUsage::default();
    usage.input_tokens = 100;
    usage.output_tokens = 50;
    usage.cached_input_tokens = 30;
    usage.reasoning_tokens = 10;
    usage.total_tokens = 160;
    let ev = AgentEvent::RunCompleted { usage };
    insta::assert_snapshot!(roundtrip(&ev));
}

#[test]
fn agent_event_run_failed_roundtrip() {
    let ev = AgentEvent::RunFailed {
        error: "model unavailable".into(),
    };
    insta::assert_snapshot!(roundtrip(&ev));
}

// --- SessionEvent ---

#[test]
fn session_event_user_message_roundtrip() {
    let ev = SessionEvent::UserMessage {
        content: vec![ContentPart::Text {
            text: "hello".into(),
        }],
        ts: pinned_ts(),
    };
    insta::assert_snapshot!(roundtrip(&ev));
}

#[test]
fn session_event_assistant_message_roundtrip() {
    let ev = SessionEvent::AssistantMessage {
        content: vec![ContentPart::Text {
            text: "hi back".into(),
        }],
        agent: "triage".into(),
        ts: pinned_ts(),
    };
    insta::assert_snapshot!(roundtrip(&ev));
}

#[test]
fn session_event_tool_called_roundtrip() {
    let ev = SessionEvent::ToolCalled {
        call_id: "call_1".into(),
        name: "calc".into(),
        args: serde_json::json!({ "expr": "1+1" }),
        ts: pinned_ts(),
    };
    insta::assert_snapshot!(roundtrip(&ev));
}

#[test]
fn session_event_tool_returned_roundtrip() {
    let ev = SessionEvent::ToolReturned {
        call_id: "call_1".into(),
        content: vec![ContentPart::Text { text: "2".into() }],
        ts: pinned_ts(),
    };
    insta::assert_snapshot!(roundtrip(&ev));
}

#[test]
fn session_event_handoff_occurred_roundtrip() {
    let ev = SessionEvent::HandoffOccurred {
        from: "triage".into(),
        to: "billing".into(),
        ts: pinned_ts(),
    };
    insta::assert_snapshot!(roundtrip(&ev));
}

#[test]
fn session_event_compacted_roundtrip() {
    let ev = SessionEvent::Compacted {
        summary: "user asked for a refund; assistant agreed".into(),
        original_count: 12,
        ts: pinned_ts(),
    };
    insta::assert_snapshot!(roundtrip(&ev));
}

// --- ConversationSnapshot ---

#[test]
fn conversation_snapshot_roundtrip() {
    let mut snapshot = ConversationSnapshot::default();
    snapshot.messages = vec![
        Item::UserMessage {
            content: vec![ContentPart::Text {
                text: "hello".into(),
            }],
        },
        Item::AssistantMessage {
            content: vec![ContentPart::Text {
                text: "hi back".into(),
            }],
            agent: Some("triage".into()),
        },
    ];
    insta::assert_snapshot!(roundtrip(&snapshot));
}
