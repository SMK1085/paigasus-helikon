//! Pins the contention failure mode documented in the crate docs (SMA-431):
//! an `append` that cannot acquire SQLite's database-level write lock within
//! `busy_timeout` fails with `SessionError::Backend` wrapping `SQLITE_BUSY`
//! ("database is locked"), persists nothing, and leaves the session usable.
//!
//! Deterministic by construction — no timing race:
//! 1. migrations run BEFORE any lock is held (so the only write that can
//!    collide with the held lock is the append under test);
//! 2. the blocking `BEGIN IMMEDIATE` transaction is provably held when the
//!    short-timeout append runs;
//! 3. it is released with an explicit awaited `rollback()` (sqlx's `Drop`
//!    enqueues ROLLBACK asynchronously — never rely on it for ordering)
//!    before the retry.

use std::time::Duration;

use jiff::Timestamp;
use paigasus_helikon_core::{ContentPart, Session, SessionError, SessionEvent};
use paigasus_helikon_sessions_sqlite::SqliteSession;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};

fn msg(text: &str) -> SessionEvent {
    SessionEvent::UserMessage {
        content: vec![ContentPart::Text { text: text.into() }],
        ts: Timestamp::from_second(1_700_000_000).unwrap(),
    }
}

#[tokio::test]
async fn busy_timeout_exhaustion_fails_cleanly_and_session_recovers() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("busy.db");

    // Pool A: default busy_timeout; used to migrate first, then to hold the
    // write lock.
    let opts_a = SqliteConnectOptions::new()
        .filename(&path)
        .create_if_missing(true)
        .journal_mode(SqliteJournalMode::Wal);
    let pool_a = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(opts_a)
        .await
        .expect("pool a");
    SqliteSession::migrate(&pool_a).await.expect("migrate");

    // Pool B: aggressively short busy_timeout, session under test. Built via
    // `open_without_migrate` — `open` would re-touch `_sqlx_migrations` and
    // is not the path under test.
    let opts_b = SqliteConnectOptions::new()
        .filename(&path)
        .journal_mode(SqliteJournalMode::Wal)
        .busy_timeout(Duration::from_millis(100));
    let pool_b = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(opts_b)
        .await
        .expect("pool b");
    let session = SqliteSession::open_without_migrate(pool_b, "busy-session");

    // Hold SQLite's write lock: BEGIN IMMEDIATE takes it up-front and keeps
    // it until commit/rollback.
    let tx_a = pool_a
        .begin_with("BEGIN IMMEDIATE")
        .await
        .expect("begin immediate on pool a");

    // The append must exhaust its 100ms busy_timeout and fail cleanly.
    let err = session
        .append(&[msg("blocked")])
        .await
        .expect_err("append must time out while the write lock is held");
    assert!(
        matches!(&err, SessionError::Backend(_)),
        "expected SessionError::Backend, got: {err:?}"
    );
    assert!(
        err.to_string().contains("database is locked"),
        "expected SQLITE_BUSY (\"database is locked\"), got: {err}"
    );

    // Release the lock deterministically, then the same session must work.
    tx_a.rollback().await.expect("rollback");
    session
        .append(&[msg("after-recovery")])
        .await
        .expect("append succeeds once the lock is released");

    // The failed append persisted nothing; exactly one event survives.
    let events = session.events(None).await.expect("events");
    assert_eq!(events.len(), 1, "failed append must not persist anything");
    match &events[0] {
        SessionEvent::UserMessage { content, .. } => match &content[0] {
            ContentPart::Text { text } => assert_eq!(text, "after-recovery"),
            other => panic!("unexpected content part: {other:?}"),
        },
        other => panic!("unexpected event: {other:?}"),
    }
}
