//! Anchor test: run the full conformance suite against `MemorySession`.
#![allow(missing_docs)]

use paigasus_helikon_core::{MemorySession, Session};
use paigasus_helikon_sessions_testkit::run_all;
use std::sync::Arc;

#[tokio::test]
async fn memory_session_passes_conformance() {
    run_all(|| async { Arc::new(MemorySession::new()) as Arc<dyn Session> }).await;
}
