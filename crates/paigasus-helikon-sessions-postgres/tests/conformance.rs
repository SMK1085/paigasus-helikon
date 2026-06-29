//! Postgres conformance — runs only when HELIKON_TEST_POSTGRES_URL is set
//! (loud-skips otherwise, like forkd_live). spec §9.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use paigasus_helikon_core::Session;
use paigasus_helikon_sessions_postgres::PostgresSession;
use paigasus_helikon_sessions_testkit::run_all;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn postgres_passes_conformance() {
    let Ok(url) = std::env::var("HELIKON_TEST_POSTGRES_URL") else {
        eprintln!("SKIP postgres_passes_conformance: HELIKON_TEST_POSTGRES_URL unset");
        return;
    };
    let pool = sqlx::PgPool::connect(&url).await.expect("connect");
    PostgresSession::migrate(&pool).await.expect("migrate"); // migrate ONCE up front

    // Process-unique prefix so reruns against a reused database never collide
    // with a prior run's rows for the same session id.
    let pid = std::process::id();
    let counter = AtomicU64::new(0);
    run_all(|| {
        let pool = pool.clone();
        let id = counter.fetch_add(1, Ordering::SeqCst);
        async move {
            Arc::new(PostgresSession::open_without_migrate(
                pool,
                format!("conf-{pid}-{id}"),
            )) as Arc<dyn Session>
        }
    })
    .await;
}
