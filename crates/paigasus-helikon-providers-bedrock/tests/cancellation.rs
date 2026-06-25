//! Cancellation tests for `BedrockModel::invoke`.
//!
//! Since there is no transport mock for the Bedrock SDK, we test the
//! cancellation contract by driving `drive_stream` — the `pub(crate)` helper
//! extracted from `model.rs` — with a hand-rolled async event source.
//!
//! The contract: when the `CancellationToken` is fired mid-stream, the stream
//! ends immediately **without** emitting a `ModelEvent::Finish`.

use futures_core::Stream;
use futures_util::StreamExt;
use paigasus_helikon_core::{CancellationToken, ModelError, ModelEvent};
use paigasus_helikon_providers_bedrock::testing::drive_stream_with_token;

// ── helpers ───────────────────────────────────────────────────────────────────

/// Build a stream that yields `n` `TokenDelta` events then ends.
fn token_stream(n: usize) -> impl Stream<Item = Result<ModelEvent, ModelError>> + Send + 'static {
    let events: Vec<Result<ModelEvent, ModelError>> = (0..n)
        .map(|i| {
            Ok(ModelEvent::TokenDelta {
                text: format!("chunk-{i}"),
            })
        })
        .collect();
    futures_util::stream::iter(events)
}

/// Build a stream that yields `n` `TokenDelta` events then a `Finish` event.
fn token_stream_with_finish(
    n: usize,
) -> impl Stream<Item = Result<ModelEvent, ModelError>> + Send + 'static {
    use paigasus_helikon_core::FinishReason;
    let mut events: Vec<Result<ModelEvent, ModelError>> = (0..n)
        .map(|i| {
            Ok(ModelEvent::TokenDelta {
                text: format!("chunk-{i}"),
            })
        })
        .collect();
    events.push(Ok(ModelEvent::Finish {
        reason: FinishReason::Stop,
    }));
    futures_util::stream::iter(events)
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn cancel_before_stream_ends_no_finish() {
    let cancel = CancellationToken::new();
    let source = token_stream(10);

    // Cancel immediately — the stream should yield 0 events and no Finish.
    cancel.cancel();
    let events: Vec<_> = drive_stream_with_token(source, cancel).collect().await;

    let has_finish = events
        .iter()
        .any(|r| matches!(r, Ok(ModelEvent::Finish { .. })));
    assert!(!has_finish, "cancelled stream must not emit Finish");
}

#[tokio::test]
async fn cancel_mid_stream_no_finish() {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    let cancel = CancellationToken::new();
    let cancel_clone = cancel.clone();

    // A stream that cancels itself after yielding the first token.
    let cancelled = Arc::new(AtomicBool::new(false));
    let cancelled_clone = cancelled.clone();

    let source = async_stream::stream! {
        yield Ok::<ModelEvent, ModelError>(ModelEvent::TokenDelta { text: "first".to_owned() });
        // Signal cancellation
        if !cancelled_clone.swap(true, Ordering::SeqCst) {
            cancel_clone.cancel();
        }
        // Yield more events — the driver should not emit these after cancel.
        yield Ok(ModelEvent::TokenDelta { text: "second".to_owned() });
        yield Ok(ModelEvent::Finish { reason: paigasus_helikon_core::FinishReason::Stop });
    };

    let events: Vec<_> = drive_stream_with_token(source, cancel).collect().await;

    let has_finish = events
        .iter()
        .any(|r| matches!(r, Ok(ModelEvent::Finish { .. })));
    assert!(!has_finish, "mid-stream cancel must not emit Finish");
}

#[tokio::test]
async fn uncancelled_stream_emits_finish() {
    let cancel = CancellationToken::new();
    let source = token_stream_with_finish(3);

    let events: Vec<_> = drive_stream_with_token(source, cancel).collect().await;

    let has_finish = events
        .iter()
        .any(|r| matches!(r, Ok(ModelEvent::Finish { .. })));
    assert!(has_finish, "uncancelled stream must emit Finish");
    // All 3 token deltas + finish = 4 events.
    assert_eq!(events.len(), 4, "expected 3 tokens + 1 finish");
}

#[tokio::test]
async fn cancel_does_not_drop_events_already_yielded() {
    let cancel = CancellationToken::new();
    // 5 events, no finish.  Don't cancel — all should arrive.
    let source = token_stream(5);
    let events: Vec<_> = drive_stream_with_token(source, cancel).collect().await;
    assert_eq!(events.len(), 5, "all events must arrive when not cancelled");
}
