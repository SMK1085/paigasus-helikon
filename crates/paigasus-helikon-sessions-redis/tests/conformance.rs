//! Redis conformance — runs only when HELIKON_TEST_REDIS_URL is set.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use paigasus_helikon_core::Session;
use paigasus_helikon_sessions_redis::RedisSession;
use paigasus_helikon_sessions_testkit::run_all;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn redis_passes_conformance() {
    let Ok(url) = std::env::var("HELIKON_TEST_REDIS_URL") else {
        eprintln!("SKIP redis_passes_conformance: HELIKON_TEST_REDIS_URL unset");
        return;
    };
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let pid = std::process::id();
    run_all(|| {
        let url = url.clone();
        let id = COUNTER.fetch_add(1, Ordering::SeqCst);
        async move {
            let session = RedisSession::connect(&url, format!("conf-{pid}-{id}"))
                .await
                .expect("connect");
            Arc::new(session) as Arc<dyn Session>
        }
    })
    .await;
}
