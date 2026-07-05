//! SQLite runs the shared conformance suite (spec §5).

use std::sync::Arc;
use std::time::Duration;

use paigasus_helikon_core::Session;
use paigasus_helikon_sessions_sqlite::SqliteSession;
use paigasus_helikon_sessions_testkit::run_all;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous};

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sqlite_passes_conformance() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("conf.db");
    // Pool sizing rationale: see concurrent_writers.rs — the testkit's
    // run_concurrent_writers (via run_all) replays the same 16×10 workload
    // here (SMA-431).
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
        .unwrap();
    SqliteSession::migrate(&pool).await.unwrap();

    // Unique session id per make() call -> fresh empty session each time.
    let counter = std::sync::atomic::AtomicU64::new(0);
    run_all(|| {
        let pool = pool.clone();
        let id = counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        async move {
            Arc::new(SqliteSession::open_without_migrate(
                pool,
                format!("conf-{id}"),
            )) as Arc<dyn Session>
        }
    })
    .await;
    // keep `dir` alive until here
    drop(dir);
}
