//! Behaviour tests for CompactingSession (spec §4.2, AC §11).

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use futures_core::stream::BoxStream;
use paigasus_helikon_core::{
    CancellationToken, CompactingSession, ContentPart, FinishReason, Item, MemorySession, Model,
    ModelCapabilities, ModelError, ModelEvent, ModelRequest, Session, SessionEvent, TokenCounter,
};

/// Fake model: returns a fixed summary, counting invocations.
#[derive(Clone)]
struct FakeModel {
    summary: String,
    calls: Arc<AtomicUsize>,
}
impl FakeModel {
    fn new(summary: &str) -> Self {
        Self {
            summary: summary.into(),
            calls: Arc::new(AtomicUsize::new(0)),
        }
    }
}
#[async_trait]
impl Model for FakeModel {
    async fn invoke(
        &self,
        _req: ModelRequest,
        _cancel: CancellationToken,
    ) -> Result<BoxStream<'static, Result<ModelEvent, ModelError>>, ModelError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let events = vec![
            Ok(ModelEvent::TokenDelta {
                text: self.summary.clone(),
            }),
            Ok(ModelEvent::Finish {
                reason: FinishReason::Stop,
            }),
        ];
        Ok(Box::pin(futures_util::stream::iter(events)))
    }
    fn capabilities(&self) -> ModelCapabilities {
        ModelCapabilities::default()
    }
}

/// Model that always errors on invoke.
struct ErrModel;
#[async_trait]
impl Model for ErrModel {
    async fn invoke(
        &self,
        _req: ModelRequest,
        _cancel: CancellationToken,
    ) -> Result<BoxStream<'static, Result<ModelEvent, ModelError>>, ModelError> {
        Err(ModelError::Unavailable)
    }
    fn capabilities(&self) -> ModelCapabilities {
        ModelCapabilities::default()
    }
}

fn user(text: &str) -> SessionEvent {
    SessionEvent::user_message(vec![ContentPart::Text { text: text.into() }])
}

// 1-token-per-char counter for exact threshold math in tests.
#[derive(Debug)]
struct CharCounter;
impl paigasus_helikon_core::TokenCounter for CharCounter {
    fn count(&self, items: &[Item]) -> usize {
        items
            .iter()
            .map(|i| match i {
                Item::UserMessage { content }
                | Item::System { content }
                | Item::AssistantMessage { content, .. } => content
                    .iter()
                    .map(|p| match p {
                        ContentPart::Text { text } => text.chars().count(),
                        _ => 0,
                    })
                    .sum(),
                _ => 0,
            })
            .sum()
    }
}

#[tokio::test]
async fn compacts_below_threshold_when_exceeded() {
    let model = FakeModel::new("S"); // 1-char summary
    let cs = CompactingSession::builder(MemorySession::new(), Arc::new(model.clone()))
        .token_counter(Arc::new(CharCounter))
        .threshold(10)
        .build()
        .unwrap();

    // Append > 10 chars of user text across two appends.
    cs.append(&[user("hello world")]).await.unwrap(); // 11 chars -> over threshold
    let snap = cs.snapshot().await.unwrap();
    assert_eq!(
        CharCounter.count(&snap.messages),
        1,
        "snapshot reduced to the 1-char summary"
    );
    assert!(matches!(snap.messages.as_slice(), [Item::System { .. }]));
    assert_eq!(
        model.calls.load(Ordering::SeqCst),
        1,
        "summarized exactly once"
    );
}

#[tokio::test]
async fn records_compacted_event_and_retains_raw_log() {
    let cs = CompactingSession::builder(MemorySession::new(), Arc::new(FakeModel::new("S")))
        .token_counter(Arc::new(CharCounter))
        .threshold(3)
        .build()
        .unwrap();
    cs.append(&[user("abcd")]).await.unwrap(); // 4 chars > 3
    let raw = cs.events(None).await.unwrap();
    // raw log: the user event + the appended Compacted marker
    assert_eq!(raw.len(), 2);
    assert!(matches!(
        raw[1],
        SessionEvent::Compacted {
            original_count: 1,
            ..
        }
    ));
}

#[tokio::test]
async fn llm_error_is_swallowed_and_no_marker_appended() {
    let cs = CompactingSession::builder(MemorySession::new(), Arc::new(ErrModel))
        .token_counter(Arc::new(CharCounter))
        .threshold(3)
        .build()
        .unwrap();
    cs.append(&[user("abcd")]).await.unwrap(); // append still Ok
    let raw = cs.events(None).await.unwrap();
    assert_eq!(raw.len(), 1, "no Compacted marker appended on LLM failure");
}

#[tokio::test]
async fn empty_summary_appends_no_marker() {
    let cs = CompactingSession::builder(MemorySession::new(), Arc::new(FakeModel::new("   ")))
        .token_counter(Arc::new(CharCounter))
        .threshold(3)
        .build()
        .unwrap();
    cs.append(&[user("abcd")]).await.unwrap();
    let raw = cs.events(None).await.unwrap();
    assert_eq!(raw.len(), 1, "whitespace-only summary => no marker");
}

#[tokio::test]
async fn resume_over_threshold_compacts_on_first_append() {
    // Pre-populate an inner session ABOVE threshold, THEN wrap it.
    let inner = MemorySession::new();
    inner.append(&[user("0123456789")]).await.unwrap(); // 10 chars
    let cs = CompactingSession::builder(inner, Arc::new(FakeModel::new("S")))
        .token_counter(Arc::new(CharCounter))
        .threshold(5)
        .build()
        .unwrap();
    cs.append(&[user("x")]).await.unwrap(); // first append must seed + compact
    let snap = cs.snapshot().await.unwrap();
    assert_eq!(
        CharCounter.count(&snap.messages),
        1,
        "resumed backlog compacted on first append"
    );
}

#[tokio::test]
async fn threshold_zero_is_rejected() {
    let err = CompactingSession::builder(MemorySession::new(), Arc::new(FakeModel::new("S")))
        .threshold(0)
        .build();
    assert!(err.is_err());
}

#[tokio::test]
async fn lone_summary_over_threshold_is_not_recompacted() {
    // Inner already projects to a single, over-threshold System summary.
    let inner = MemorySession::new();
    inner
        .append(&[SessionEvent::compacted("LONG SUMMARY OVER THRESHOLD", 1)])
        .await
        .unwrap();
    let model = FakeModel::new("X");
    let cs = CompactingSession::builder(inner, Arc::new(model.clone()))
        .token_counter(Arc::new(CharCounter))
        .threshold(3) // summary (26 chars) is far above threshold
        .build()
        .unwrap();
    // A handoff contributes 0 projected messages, so the snapshot stays a lone
    // System summary (messages.len() == 1) -> the guard MUST skip compaction.
    cs.append(&[SessionEvent::handoff_occurred("a", "b")])
        .await
        .unwrap();
    assert_eq!(
        model.calls.load(Ordering::SeqCst),
        0,
        "lone summary must not be re-compacted"
    );
    let raw = cs.events(None).await.unwrap();
    assert_eq!(
        raw.len(),
        2,
        "only the pre-seeded Compacted + the handoff; no new marker appended"
    );
}
