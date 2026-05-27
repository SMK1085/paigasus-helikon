//! Covers acceptance criterion #2 from SMA-318: a `SqliteSession` opens a
//! file, survives a process restart, and reads back what was written.

use jiff::Timestamp;
use paigasus_helikon_core::{ContentPart, Session, SessionEvent};
use paigasus_helikon_sessions_sqlite::SqliteSession;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

#[tokio::test]
async fn file_backed_session_survives_pool_drop() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("sessions.db");

    // First "process": write some events, then drop the session and pool.
    {
        let opts = SqliteConnectOptions::new()
            .filename(&path)
            .create_if_missing(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(opts)
            .await
            .expect("pool");
        let session = SqliteSession::open(pool, "convo-1").await.expect("open");
        session
            .append(&[
                SessionEvent::UserMessage {
                    content: vec![ContentPart::Text {
                        text: "before restart".into(),
                    }],
                    ts: Timestamp::from_second(1_700_000_000).unwrap(),
                },
                SessionEvent::AssistantMessage {
                    content: vec![ContentPart::Text { text: "ack".into() }],
                    agent: "triage".into(),
                    ts: Timestamp::from_second(1_700_000_001).unwrap(),
                },
            ])
            .await
            .expect("append");
        // Drop pool + session by leaving scope.
    }

    // Second "process": open fresh pool on the same file, expect events.
    let opts = SqliteConnectOptions::new()
        .filename(&path)
        .create_if_missing(false);
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(opts)
        .await
        .expect("pool");
    let session = SqliteSession::open(pool, "convo-1").await.expect("open");
    let read_back = session.events(None).await.expect("events");

    assert_eq!(read_back.len(), 2);
    match &read_back[0] {
        SessionEvent::UserMessage { content, .. } => match &content[0] {
            ContentPart::Text { text } => assert_eq!(text, "before restart"),
            _ => panic!("expected Text"),
        },
        other => panic!("expected UserMessage, got {other:?}"),
    }
}
