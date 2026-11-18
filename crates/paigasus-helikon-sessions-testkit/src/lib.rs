//! Session conformance suite for Paigasus Helikon backends, including your own.
//!
//! Each function takes a factory that returns a session. The suite runs four
//! tests against that session:
//!
//! - `run_append_read`: events that you append come back in order, with the same content.
//! - `run_watermark_exclusive`: a read with a watermark returns only the events after it.
//! - `run_projection`: `snapshot()` equals `project(events())`.
//! - `run_concurrent_writers`: tasks append at the same time and every event survives once.
//!
//! `run_all` runs all four tests.
//!
//! # Factory contract
//!
//! Each call to the factory must return a fresh, empty session. If you return
//! the same session for two calls, the second test sees the events of the first
//! test and fails.
//!
//! # Tokio runtime
//!
//! `run_concurrent_writers` calls `tokio::spawn`. `run_all` panics if it runs
//! outside a Tokio runtime. Use the `multi_thread` flavor with
//! `worker_threads = 4`.
//!
//! # Example
//!
//! ```
//! use std::sync::Arc;
//! use paigasus_helikon_core::{MemorySession, Session};
//! use paigasus_helikon_sessions_testkit::run_all;
//!
//! #[tokio::main(flavor = "multi_thread", worker_threads = 4)]
//! async fn main() {
//!     run_all(|| async { Arc::new(MemorySession::new()) as Arc<dyn Session> }).await;
//! }
//! ```
//!
//! # Same core version
//!
//! The session type in your factory must come from the same
//! `paigasus-helikon-core` version as this crate. If two versions exist in your
//! dependency graph, the types do not match. Check with
//! `cargo tree -d -i paigasus-helikon-core`. The command can list other duplicate
//! crates. If `paigasus-helikon-core` does not appear at the start of a line,
//! you have one version.
//!
//! # Stability
//!
//! On 0.x versions, new sub-tests arrive in patch releases. A new sub-test can
//! fail a backend that passed before. To control the upgrade, pin the version
//! with `=0.1.x`.

use std::future::Future;
use std::sync::Arc;

use jiff::Timestamp;
use paigasus_helikon_core::{ContentPart, Item, SequenceId, Session, SessionEvent};

fn user(text: &str) -> SessionEvent {
    SessionEvent::UserMessage {
        content: vec![ContentPart::Text { text: text.into() }],
        ts: Timestamp::from_second(1_700_000_000).unwrap(),
    }
}

/// Extract the text of a `UserMessage` event, panicking on any other shape.
/// Used by the assertions below to compare exact content and order.
fn event_text(ev: &SessionEvent) -> &str {
    match ev {
        SessionEvent::UserMessage { content, .. } => match content.first() {
            Some(ContentPart::Text { text }) => text.as_str(),
            other => panic!("expected leading Text content, got {other:?}"),
        },
        other => panic!("expected UserMessage, got {other:?}"),
    }
}

/// Extract the text of a projected `Item::UserMessage`, panicking otherwise.
fn item_text(item: &Item) -> &str {
    match item {
        Item::UserMessage { content } => match content.first() {
            Some(ContentPart::Text { text }) => text.as_str(),
            other => panic!("expected leading Text content, got {other:?}"),
        },
        other => panic!("expected Item::UserMessage, got {other:?}"),
    }
}

/// Append several events, read them back, assert order and count are preserved.
pub async fn run_append_read<F, Fut>(make: F)
where
    F: Fn() -> Fut,
    Fut: Future<Output = Arc<dyn Session>>,
{
    let s = make().await;
    s.append(&[user("a"), user("b"), user("c")]).await.unwrap();
    let got = s.events(None).await.unwrap();
    assert_eq!(got.len(), 3, "all appended events read back");
    // Assert exact content AND order, not just count: a backend that reordered
    // or corrupted payloads must fail here.
    let texts: Vec<&str> = got.iter().map(event_text).collect();
    assert_eq!(
        texts,
        ["a", "b", "c"],
        "events read back in exact append order with exact content"
    );
}

/// `events(Some(SequenceId(n)))` is an exclusive watermark: returns positions > n.
pub async fn run_watermark_exclusive<F, Fut>(make: F)
where
    F: Fn() -> Fut,
    Fut: Future<Output = Arc<dyn Session>>,
{
    let s = make().await;
    for i in 0..5 {
        s.append(&[user(&format!("m{i}"))]).await.unwrap();
    }
    let after = s.events(Some(SequenceId(2))).await.unwrap();
    // Assert it returns exactly the right two events (positions 3 and 4) in
    // order — not merely "two events": a backend returning the wrong rows must fail.
    let texts: Vec<&str> = after.iter().map(event_text).collect();
    assert_eq!(
        texts,
        ["m3", "m4"],
        "exclusive watermark returns exactly positions 3 and 4, in order"
    );
}

/// `snapshot()` equals `project(events())`.
pub async fn run_projection<F, Fut>(make: F)
where
    F: Fn() -> Fut,
    Fut: Future<Output = Arc<dyn Session>>,
{
    let s = make().await;
    s.append(&[user("p1"), user("p2")]).await.unwrap();
    let snap = s.snapshot().await.unwrap();
    let events = s.events(None).await.unwrap();
    let expected = paigasus_helikon_core::project(&events);
    assert_eq!(snap.messages.len(), expected.messages.len());
    assert_eq!(snap.messages.len(), 2);
    // Assert the projected snapshot preserves exact content and order, not just
    // length: a snapshot with the right count but wrong messages must fail.
    let texts: Vec<&str> = snap.messages.iter().map(item_text).collect();
    assert_eq!(
        texts,
        ["p1", "p2"],
        "snapshot() projects messages with exact content in order"
    );
}

/// N tasks append concurrently to the same session; every event survives once.
pub async fn run_concurrent_writers<F, Fut>(make: F)
where
    F: Fn() -> Fut,
    Fut: Future<Output = Arc<dyn Session>>,
{
    const N_TASKS: usize = 16;
    const M_EVENTS: usize = 10;
    let session = make().await;
    let mut handles = Vec::new();
    for t in 0..N_TASKS {
        let s = session.clone();
        handles.push(tokio::spawn(async move {
            for j in 0..M_EVENTS {
                s.append(&[user(&format!("t{t}-m{j}"))]).await.unwrap();
            }
        }));
    }
    for h in handles {
        h.await.unwrap();
    }
    let all = session.events(None).await.unwrap();
    assert_eq!(
        all.len(),
        N_TASKS * M_EVENTS,
        "no lost or duplicated events"
    );
    let mut texts: Vec<String> = all
        .into_iter()
        .filter_map(|e| match e {
            SessionEvent::UserMessage { content, .. } => match content.into_iter().next() {
                Some(ContentPart::Text { text }) => Some(text),
                _ => None,
            },
            _ => None,
        })
        .collect();
    texts.sort();
    let mut expected: Vec<String> = (0..N_TASKS)
        .flat_map(|t| (0..M_EVENTS).map(move |j| format!("t{t}-m{j}")))
        .collect();
    expected.sort();
    assert_eq!(texts, expected, "every sent event present exactly once");
}

/// Run the full conformance suite against `make`. `make` is invoked once per
/// sub-test and MUST return a fresh, empty session each time.
pub async fn run_all<F, Fut>(make: F)
where
    F: Fn() -> Fut,
    Fut: Future<Output = Arc<dyn Session>>,
{
    run_append_read(&make).await;
    run_watermark_exclusive(&make).await;
    run_projection(&make).await;
    run_concurrent_writers(&make).await;
}
