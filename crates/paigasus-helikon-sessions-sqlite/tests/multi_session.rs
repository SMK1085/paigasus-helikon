//! Two `SqliteSession`s with distinct `session_id`s in one pool must read
//! back only their own events.

use jiff::Timestamp;
use paigasus_helikon_core::{ContentPart, Session, SessionEvent};
use paigasus_helikon_sessions_sqlite::SqliteSession;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

fn msg(text: &str) -> SessionEvent {
    SessionEvent::UserMessage {
        content: vec![ContentPart::Text { text: text.into() }],
        ts: Timestamp::from_second(0).unwrap(),
    }
}

#[tokio::test]
async fn distinct_session_ids_are_isolated() {
    let opts = SqliteConnectOptions::new().in_memory(true);
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(opts)
        .await
        .expect("pool");
    SqliteSession::migrate(&pool).await.expect("migrate");

    let a = SqliteSession::open_unchecked(pool.clone(), "session-a");
    let b = SqliteSession::open_unchecked(pool, "session-b");

    a.append(&[msg("a1"), msg("a2"), msg("a3"), msg("a4"), msg("a5")])
        .await
        .expect("append a");
    b.append(&[msg("b1"), msg("b2"), msg("b3"), msg("b4"), msg("b5")])
        .await
        .expect("append b");

    let a_events = a.events(None).await.expect("events a");
    let b_events = b.events(None).await.expect("events b");

    assert_eq!(a_events.len(), 5);
    assert_eq!(b_events.len(), 5);

    // Each session sees only its own prefix.
    let extract_text = |ev: &SessionEvent| -> String {
        match ev {
            SessionEvent::UserMessage { content, .. } => match &content[0] {
                ContentPart::Text { text } => text.clone(),
                _ => unreachable!(),
            },
            _ => unreachable!(),
        }
    };
    let a_texts: Vec<String> = a_events.iter().map(extract_text).collect();
    let b_texts: Vec<String> = b_events.iter().map(extract_text).collect();

    assert_eq!(a_texts, vec!["a1", "a2", "a3", "a4", "a5"]);
    assert_eq!(b_texts, vec!["b1", "b2", "b3", "b4", "b5"]);
}
