//! SQLite runs the shared conformance suite (spec §5).

use std::sync::Arc;
use std::time::Duration;

use paigasus_helikon_core::Session;
use paigasus_helikon_sessions_sqlite::SqliteSession;
use paigasus_helikon_sessions_testkit::run_all;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sqlite_passes_conformance() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("conf.db");
    let opts = SqliteConnectOptions::new()
        .filename(&path)
        .create_if_missing(true)
        .journal_mode(SqliteJournalMode::Wal)
        .busy_timeout(Duration::from_secs(30));
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
