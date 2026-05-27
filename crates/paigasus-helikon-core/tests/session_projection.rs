//! Unit tests for [`project`].

use jiff::Timestamp;
use paigasus_helikon_core::{project, ContentPart, Item, SessionEvent};

fn epoch() -> Timestamp {
    Timestamp::from_second(0).expect("0 is a valid timestamp")
}

fn user(text: &str) -> SessionEvent {
    SessionEvent::UserMessage {
        content: vec![ContentPart::Text { text: text.into() }],
        ts: epoch(),
    }
}

fn assistant(text: &str, agent: &str) -> SessionEvent {
    SessionEvent::AssistantMessage {
        content: vec![ContentPart::Text { text: text.into() }],
        agent: agent.into(),
        ts: epoch(),
    }
}

fn tool_called(call_id: &str) -> SessionEvent {
    SessionEvent::ToolCalled {
        call_id: call_id.into(),
        name: "calc".into(),
        args: serde_json::json!({"x": 1}),
        ts: epoch(),
    }
}

fn tool_returned(call_id: &str) -> SessionEvent {
    SessionEvent::ToolReturned {
        call_id: call_id.into(),
        content: vec![ContentPart::Text {
            text: "result".into(),
        }],
        ts: epoch(),
    }
}

fn handoff(from: &str, to: &str) -> SessionEvent {
    SessionEvent::HandoffOccurred {
        from: from.into(),
        to: to.into(),
        ts: epoch(),
    }
}

fn compacted(summary: &str, n: u64) -> SessionEvent {
    SessionEvent::Compacted {
        summary: summary.into(),
        original_count: n,
        ts: epoch(),
    }
}

#[test]
fn empty_log_projects_to_empty_snapshot() {
    let snap = project(&[]);
    assert!(snap.messages.is_empty());
}

#[test]
fn user_and_assistant_turns_project_in_order_with_agent() {
    let events = vec![user("hi"), assistant("hello", "triage")];
    let snap = project(&events);
    assert_eq!(snap.messages.len(), 2);
    match &snap.messages[0] {
        Item::UserMessage { content } => {
            assert_eq!(content.len(), 1);
        }
        other => panic!("expected UserMessage, got {other:?}"),
    }
    match &snap.messages[1] {
        Item::AssistantMessage { content, agent } => {
            assert_eq!(content.len(), 1);
            assert_eq!(agent.as_deref(), Some("triage"));
        }
        other => panic!("expected AssistantMessage, got {other:?}"),
    }
}

#[test]
fn tool_call_and_return_project_as_pair() {
    let events = vec![tool_called("c1"), tool_returned("c1")];
    let snap = project(&events);
    assert_eq!(snap.messages.len(), 2);
    assert!(matches!(snap.messages[0], Item::ToolCall { .. }));
    assert!(matches!(snap.messages[1], Item::ToolResult { .. }));
}

#[test]
fn handoff_produces_no_message() {
    let events = vec![
        assistant("first", "a"),
        handoff("a", "b"),
        assistant("second", "b"),
    ];
    let snap = project(&events);
    assert_eq!(snap.messages.len(), 2);
    // Second assistant message carries the new agent name.
    match &snap.messages[1] {
        Item::AssistantMessage { agent, .. } => {
            assert_eq!(agent.as_deref(), Some("b"));
        }
        other => panic!("expected AssistantMessage, got {other:?}"),
    }
}

#[test]
fn compaction_replaces_window_with_single_system_message() {
    // 7 events: 4 keep, 3 get compacted into one System message, then more after.
    let events = vec![
        user("u1"),
        assistant("a1", "x"),
        user("u2"),
        compacted("summary of last 3", 3),
        assistant("a2", "x"),
        user("u3"),
    ];
    let snap = project(&events);
    // u1, a1, u2 are dropped; one System (summary) replaces them; a2, u3 follow.
    assert_eq!(snap.messages.len(), 3);
    match &snap.messages[0] {
        Item::System { content } => match &content[0] {
            ContentPart::Text { text } => assert_eq!(text, "summary of last 3"),
            _ => panic!("expected Text"),
        },
        other => panic!("expected System, got {other:?}"),
    }
    assert!(matches!(snap.messages[1], Item::AssistantMessage { .. }));
    assert!(matches!(snap.messages[2], Item::UserMessage { .. }));
}

#[test]
fn compaction_over_window_with_handoff_does_not_break_math() {
    // 4-event window includes one Handoff (0 messages produced).
    let events = vec![
        user("u1"),
        assistant("a1", "x"),
        handoff("x", "y"),
        assistant("a2", "y"),
        compacted("summary", 4),
        user("u3"),
    ];
    let snap = project(&events);
    // u1, a1, (no handoff msg), a2 → 3 messages dropped; one System replaces.
    assert_eq!(snap.messages.len(), 2);
    assert!(matches!(snap.messages[0], Item::System { .. }));
    assert!(matches!(snap.messages[1], Item::UserMessage { .. }));
}

#[test]
fn compaction_with_oversized_count_clamps_to_zero() {
    let events = vec![user("u1"), compacted("summary", 999)];
    let snap = project(&events);
    // u1 dropped; one System replaces.
    assert_eq!(snap.messages.len(), 1);
    assert!(matches!(snap.messages[0], Item::System { .. }));
}

#[test]
fn two_consecutive_compactions_chain() {
    let events = vec![
        user("u1"),
        assistant("a1", "x"),
        compacted("first summary", 2),
        compacted("second summary", 1),
    ];
    let snap = project(&events);
    // After first compact: [System("first summary")].
    // After second compact: [System("second summary")] (replaces the first).
    assert_eq!(snap.messages.len(), 1);
    match &snap.messages[0] {
        Item::System { content } => match &content[0] {
            ContentPart::Text { text } => assert_eq!(text, "second summary"),
            _ => panic!("expected Text"),
        },
        other => panic!("expected System, got {other:?}"),
    }
}
