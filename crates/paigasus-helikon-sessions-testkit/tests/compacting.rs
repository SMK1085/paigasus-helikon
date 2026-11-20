//! `CompactingSession` over `MemorySession` against `run_tool_deferred`.
//!
//! The threshold is 1 and the model panics when it is called. A pass proves
//! that no compaction starts while a deferral is open.
#![allow(missing_docs)]

use std::sync::Arc;

use async_trait::async_trait;
use futures_core::stream::BoxStream;
use paigasus_helikon_core::{
    CancellationToken, CompactingSession, ContentPart, MemorySession, Model, ModelCapabilities,
    ModelError, ModelEvent, ModelRequest, Session, SessionEvent,
};
use paigasus_helikon_sessions_testkit::run_tool_deferred;

/// A model that fails the test when `CompactingSession` calls it.
struct PanicModel;

#[async_trait]
impl Model for PanicModel {
    async fn invoke(
        &self,
        _request: ModelRequest,
        _cancel: CancellationToken,
    ) -> Result<BoxStream<'static, Result<ModelEvent, ModelError>>, ModelError> {
        panic!("PanicModel::invoke: CompactingSession started a compaction");
    }

    fn capabilities(&self) -> ModelCapabilities {
        ModelCapabilities::default()
    }
}

fn compacting() -> Arc<dyn Session> {
    Arc::new(
        CompactingSession::builder(MemorySession::new(), Arc::new(PanicModel))
            .threshold(1)
            .build()
            .expect("threshold 1 is valid"),
    )
}

#[tokio::test]
async fn compacting_session_passes_tool_deferred() {
    run_tool_deferred(|| async { compacting() }).await;
}

/// Control: threshold 1 reaches the model when no deferral is open.
#[tokio::test]
#[should_panic(expected = "PanicModel::invoke")]
async fn compacting_control_compacts_without_deferral() {
    compacting()
        .append(&[SessionEvent::user_message(vec![ContentPart::Text {
            text: "pay my rent".into(),
        }])])
        .await
        .expect("append");
}
