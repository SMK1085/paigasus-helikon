//! Behavior tests for [`MemorySession`].

use jiff::Timestamp;
use paigasus_helikon_core::{ContentPart, MemorySession, SequenceId, Session, SessionEvent};

fn epoch() -> Timestamp {
    Timestamp::from_second(0).expect("0 is a valid timestamp")
}

fn user_msg(text: &str) -> SessionEvent {
    SessionEvent::UserMessage {
        content: vec![ContentPart::Text { text: text.into() }],
        ts: epoch(),
    }
}

#[tokio::test]
async fn append_and_read_back_preserves_order_and_timestamps() {
    let session = MemorySession::new();
    let events = vec![user_msg("first"), user_msg("second"), user_msg("third")];

    session.append(&events).await.expect("append");
    let read_back = session.events(None).await.expect("events");

    assert_eq!(read_back.len(), 3);
    for (orig, got) in events.iter().zip(read_back.iter()) {
        let orig_json = serde_json::to_value(orig).unwrap();
        let got_json = serde_json::to_value(got).unwrap();
        assert_eq!(orig_json, got_json);
    }
}

#[tokio::test]
async fn events_since_returns_strictly_after_watermark() {
    let session = MemorySession::new();
    let events = (0..5)
        .map(|i| user_msg(&format!("msg-{i}")))
        .collect::<Vec<_>>();
    session.append(&events).await.unwrap();

    // since = SequenceId(2) should return events at indices 3, 4 (exclusive).
    let tail = session.events(Some(SequenceId(2))).await.expect("events");
    assert_eq!(tail.len(), 2);

    let tail_first = serde_json::to_value(&tail[0]).unwrap();
    let expected_first = serde_json::to_value(&events[3]).unwrap();
    assert_eq!(tail_first, expected_first);
}

#[tokio::test]
async fn events_since_past_end_returns_empty() {
    let session = MemorySession::new();
    session.append(&[user_msg("only")]).await.unwrap();

    let tail = session.events(Some(SequenceId(100))).await.unwrap();
    assert!(tail.is_empty());
}

#[tokio::test]
async fn concurrent_appends_preserve_total_count() {
    use std::sync::Arc;

    let session = Arc::new(MemorySession::new());
    let tasks = (0..8)
        .map(|i| {
            let s = session.clone();
            tokio::spawn(async move {
                for _ in 0..100 {
                    s.append(&[user_msg(&format!("task-{i}"))]).await.unwrap();
                }
            })
        })
        .collect::<Vec<_>>();

    for t in tasks {
        t.await.unwrap();
    }

    let all = session.events(None).await.unwrap();
    assert_eq!(all.len(), 800);
}
