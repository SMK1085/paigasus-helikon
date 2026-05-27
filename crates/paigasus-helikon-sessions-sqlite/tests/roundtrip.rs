//! Append + read-back round-trip for every [`SessionEvent`] variant.
//!
//! **Pool note:** the in-memory test pool MUST use `max_connections = 1`.
//! `sqlite::memory:` creates a *separate* in-memory database per
//! connection, so a multi-connection pool would intermittently hit
//! "no such table: session_events" because some connections never saw
//! the migration. Don't parallelize this — it'll reintroduce the bug.

use jiff::Timestamp;
use paigasus_helikon_core::{ContentPart, Session, SessionEvent};
use paigasus_helikon_sessions_sqlite::SqliteSession;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

async fn fresh_session() -> SqliteSession {
    let opts = SqliteConnectOptions::new().in_memory(true);
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(opts)
        .await
        .expect("pool");
    SqliteSession::open(pool, "test-session")
        .await
        .expect("open")
}

fn pinned() -> Timestamp {
    Timestamp::from_second(1_700_000_000).expect("valid ts")
}

fn all_variants() -> Vec<SessionEvent> {
    vec![
        SessionEvent::UserMessage {
            content: vec![ContentPart::Text {
                text: "hello".into(),
            }],
            ts: pinned(),
        },
        SessionEvent::AssistantMessage {
            content: vec![ContentPart::Text {
                text: "hi back".into(),
            }],
            agent: "triage".into(),
            ts: pinned(),
        },
        SessionEvent::ToolCalled {
            call_id: "c1".into(),
            name: "calc".into(),
            args: serde_json::json!({"x": 1}),
            ts: pinned(),
        },
        SessionEvent::ToolReturned {
            call_id: "c1".into(),
            content: vec![ContentPart::Text { text: "2".into() }],
            ts: pinned(),
        },
        SessionEvent::HandoffOccurred {
            from: "triage".into(),
            to: "billing".into(),
            ts: pinned(),
        },
        SessionEvent::Compacted {
            summary: "previous turns summarized".into(),
            original_count: 5,
            ts: pinned(),
        },
    ]
}

#[tokio::test]
async fn roundtrip_preserves_every_variant_and_timestamps() {
    let session = fresh_session().await;
    let events = all_variants();

    session.append(&events).await.expect("append");
    let read_back = session.events(None).await.expect("events");

    assert_eq!(read_back.len(), events.len(), "event count");
    for (orig, got) in events.iter().zip(read_back.iter()) {
        let orig_json = serde_json::to_value(orig).unwrap();
        let got_json = serde_json::to_value(got).unwrap();
        assert_eq!(orig_json, got_json, "round-trip mismatch");
    }
}

#[tokio::test]
async fn events_since_is_exclusive_watermark() {
    let session = fresh_session().await;
    let events: Vec<SessionEvent> = (0..5)
        .map(|i| SessionEvent::UserMessage {
            content: vec![ContentPart::Text {
                text: format!("msg-{i}"),
            }],
            ts: pinned(),
        })
        .collect();
    session.append(&events).await.expect("append");

    // SequenceId(2) → strictly after index 2, so we get events 3, 4.
    let tail = session
        .events(Some(paigasus_helikon_core::SequenceId(2)))
        .await
        .expect("events");
    assert_eq!(tail.len(), 2);

    let head = session.events(None).await.expect("events");
    assert_eq!(head.len(), 5);
}

#[tokio::test]
async fn snapshot_projects_through_project_function() {
    let session = fresh_session().await;
    session
        .append(&[
            SessionEvent::UserMessage {
                content: vec![ContentPart::Text {
                    text: "hello".into(),
                }],
                ts: pinned(),
            },
            SessionEvent::AssistantMessage {
                content: vec![ContentPart::Text { text: "hi".into() }],
                agent: "triage".into(),
                ts: pinned(),
            },
        ])
        .await
        .expect("append");

    let snap = session.snapshot().await.expect("snapshot");
    assert_eq!(snap.messages.len(), 2);
}
