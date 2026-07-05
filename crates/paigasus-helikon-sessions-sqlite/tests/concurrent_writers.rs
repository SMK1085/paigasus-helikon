//! Covers acceptance criterion #2 from SMA-318 (concurrency): N tasks
//! appending to the same `session_id` produce a contiguous sequence with
//! no gaps or duplicates.
//!
//! **Why not loom:** loom models pure-Rust concurrency primitives and
//! can't reason about SQLite's lock state machine. Using a real
//! `tokio::test` with a file-backed pool exercises the actual write-lock
//! path.
//!
//! **Pool sizing (SMA-431):** `busy_timeout` caps each writer's wait for
//! SQLite's single write lock, and the worst-placed writer waits out the
//! whole backlog — all 160 transactions here. A healthy Windows runner
//! finishes that backlog in ~1.3s, but a degraded one (CI run 27703633580)
//! was still incomplete at 33.6s and blew through the previous 30s timeout.
//! 120s plus `synchronous=NORMAL` (no per-commit fsync) gives a ≥3.5×
//! guaranteed floor over the worst observed backlog, ~40× if fsync
//! dominates. See docs/superpowers/specs/2026-07-05-sma-431-sqlite-busy-flake-design.md.

use std::sync::Arc;
use std::time::Duration;

use jiff::Timestamp;
use paigasus_helikon_core::{ContentPart, Session, SessionEvent};
use paigasus_helikon_sessions_sqlite::SqliteSession;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous};

const N_TASKS: usize = 16;
const M_EVENTS_PER_TASK: usize = 10;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_appends_produce_contiguous_sequence() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("concurrent.db");

    let opts = SqliteConnectOptions::new()
        .filename(&path)
        .create_if_missing(true)
        .journal_mode(SqliteJournalMode::Wal)
        .synchronous(SqliteSynchronous::Normal)
        .busy_timeout(Duration::from_secs(120));
    let pool = SqlitePoolOptions::new()
        .max_connections(8)
        .connect_with(opts)
        .await
        .expect("pool");
    SqliteSession::migrate(&pool).await.expect("migrate");

    let session = Arc::new(SqliteSession::open_without_migrate(pool, "shared"));

    let handles = (0..N_TASKS)
        .map(|task_idx| {
            let s = session.clone();
            tokio::spawn(async move {
                for j in 0..M_EVENTS_PER_TASK {
                    let ev = SessionEvent::UserMessage {
                        content: vec![ContentPart::Text {
                            text: format!("task-{task_idx}-msg-{j}"),
                        }],
                        ts: Timestamp::from_second(1_700_000_000).unwrap(),
                    };
                    s.append(&[ev]).await.expect("append");
                }
            })
        })
        .collect::<Vec<_>>();

    for h in handles {
        h.await.expect("task panicked");
    }

    let all = session.events(None).await.expect("events");
    let expected = N_TASKS * M_EVENTS_PER_TASK;
    assert_eq!(all.len(), expected, "total event count");

    // The sequence column is internal; we observe it indirectly: every
    // event we read back must be one of the ones we sent.
    let mut texts: Vec<String> = all
        .into_iter()
        .filter_map(|ev| match ev {
            SessionEvent::UserMessage { content, .. } => match content.into_iter().next() {
                Some(ContentPart::Text { text }) => Some(text),
                _ => None,
            },
            _ => None,
        })
        .collect();
    texts.sort();

    let mut expected_texts: Vec<String> = (0..N_TASKS)
        .flat_map(|t| (0..M_EVENTS_PER_TASK).map(move |j| format!("task-{t}-msg-{j}")))
        .collect();
    expected_texts.sort();

    assert_eq!(
        texts, expected_texts,
        "every sent event is present exactly once"
    );
}
