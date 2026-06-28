//! Shared `Session` conformance suite (spec §5). Each backend supplies a
//! factory that yields a fresh, empty session; these functions exercise the
//! append/read/projection/concurrency contract every backend must uphold.

use std::future::Future;
use std::sync::Arc;

use jiff::Timestamp;
use paigasus_helikon_core::{ContentPart, SequenceId, Session, SessionEvent};

fn user(text: &str) -> SessionEvent {
    SessionEvent::UserMessage {
        content: vec![ContentPart::Text { text: text.into() }],
        ts: Timestamp::from_second(1_700_000_000).unwrap(),
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
    assert!(matches!(&got[0], SessionEvent::UserMessage { content, .. }
        if matches!(&content[0], ContentPart::Text { text } if text == "a")));
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
    assert_eq!(after.len(), 2, "positions 3 and 4 only (exclusive of 2)");
}

/// `snapshot()` equals `project(events())`.
pub async fn run_projection<F, Fut>(make: F)
where
    F: Fn() -> Fut,
    Fut: Future<Output = Arc<dyn Session>>,
{
    let s = make().await;
    s.append(&[user("hi")]).await.unwrap();
    let snap = s.snapshot().await.unwrap();
    let events = s.events(None).await.unwrap();
    let expected = paigasus_helikon_core::project(&events);
    assert_eq!(snap.messages.len(), expected.messages.len());
    assert_eq!(snap.messages.len(), 1);
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
