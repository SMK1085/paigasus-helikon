//! Session conformance suite for Paigasus Helikon backends, including your own.
//!
//! Each function takes a factory that returns a session. The suite runs five
//! tests against that session:
//!
//! - `run_append_read`: events that you append come back in order, with the
//!   same content.
//! - `run_watermark_exclusive`: a read with a watermark returns only the
//!   events after it.
//! - `run_projection`: `snapshot()` equals `project(events())`.
//! - `run_concurrent_writers`: tasks append at the same time and every event
//!   survives once.
//! - `run_tool_deferred`: a log with deferred tool calls comes back exactly,
//!   `snapshot()` adds no item for a deferral, and `open_deferrals` finds the
//!   open deferrals.
//!
//! `run_all` runs all five tests.
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
//! outside a Tokio runtime. We recommend the `multi_thread` flavor with
//! `worker_threads = 4`. The default current-thread runtime works, but it does
//! not run the writers in parallel.
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
//! `paigasus-helikon-core` version as this crate. Run
//! `cargo tree -i paigasus-helikon-core`. If it fails with
//! "specification ... is ambiguous", your crate and the testkit resolve two
//! core versions. Align your `paigasus-helikon-core` requirement with the
//! requirement of the testkit.
//!
//! # Stability
//!
//! On 0.x versions, new sub-tests arrive in patch releases. A new sub-test can
//! fail a backend that passed before. To control the upgrade, pin an exact
//! version, for example `=0.1.1`. A requirement such as `=0.1` or `0.1` still
//! accepts every `0.1` patch release. A patch release can also move the core
//! requirement: 0.1.0 uses core 0.5, and 0.1.1 uses core 0.6. So an exact pin
//! must match your core version.

use std::future::Future;
use std::sync::Arc;

use jiff::Timestamp;
use paigasus_helikon_core::{
    open_deferrals, project, ContentPart, Item, PendingToolCall, SequenceId, Session, SessionEvent,
};

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

const D1: &str = "6f1c2a3b-4d5e-4f60-8a7b-9c0d1e2f3a4b";
const D2: &str = "0b8e7d6c-5a4f-4e3d-9c2b-1a0f9e8d7c6b";
const D3: &str = "d3a1b2c4-e5f6-4a7b-8c9d-0e1f2a3b4c5d";
// The trailing space is on purpose: a backend that trims text must fail.
const D1_PROMPT: &str = "Approve the transfer of 1250.75 EUR?\nPayee: Hausverwaltung Nord ";

/// A whole-second timestamp that is different for each event of the log.
fn fixed_ts(i: i64) -> Timestamp {
    Timestamp::from_second(1_700_000_000 + i).unwrap()
}

/// The effective arguments of `d1`. They cover the JSON round-trip edge cases.
fn effective_args() -> serde_json::Value {
    serde_json::json!({
        "memo": "Miete für März 💶",
        "note": "ref \"A-7\"\\rent\tmonthly",
        "amount": 1250.75,
        "ref_id": 9_007_199_254_740_993_u64,
        "adjust": -42,
        "recurring": true,
        "end_date": null,
        "tags": ["rent", "monthly"],
        "payee": { "name": "Hausverwaltung Nord", "iban_suffix": "0042" }
    })
}

/// The arguments that the model sent for `c1`. They differ from the effective
/// arguments: a different amount and no `memo` key.
fn model_args() -> serde_json::Value {
    let mut args = effective_args();
    let obj = args.as_object_mut().unwrap();
    obj.insert("amount".into(), serde_json::json!(1300.5));
    obj.remove("memo");
    args
}

fn text(t: &str) -> Vec<ContentPart> {
    vec![ContentPart::Text { text: t.into() }]
}

fn called(i: i64, call_id: &str, name: &str, args: serde_json::Value) -> SessionEvent {
    SessionEvent::ToolCalled {
        call_id: call_id.into(),
        name: name.into(),
        args,
        ts: fixed_ts(i),
    }
}

fn deferred(
    i: i64,
    deferral_id: &str,
    call_id: &str,
    name: &str,
    args: serde_json::Value,
    prompt: &str,
    handoff_path: &[&str],
) -> SessionEvent {
    SessionEvent::ToolDeferred {
        deferral_id: deferral_id.into(),
        call_id: call_id.into(),
        name: name.into(),
        args,
        prompt: prompt.into(),
        handoff_path: handoff_path.iter().map(|s| (*s).to_owned()).collect(),
        ts: fixed_ts(i),
    }
}

/// The log of `run_tool_deferred`: eight events with three deferrals.
fn deferral_log() -> Vec<SessionEvent> {
    let acct = || serde_json::json!({ "account": "acct-9" });
    vec![
        SessionEvent::UserMessage {
            content: text("pay my rent"),
            ts: fixed_ts(0),
        },
        called(1, "c1", "transfer_funds", model_args()),
        deferred(
            2,
            D1,
            "c1",
            "transfer_funds",
            effective_args(),
            D1_PROMPT,
            &["triage", "payments"],
        ),
        called(3, "c2", "close_account", acct()),
        deferred(
            4,
            D2,
            "c2",
            "close_account",
            acct(),
            "Close the account acct-9?",
            &[],
        ),
        called(5, "c3", "lookup_balance", serde_json::json!({})),
        deferred(
            6,
            D3,
            "c3",
            "lookup_balance",
            serde_json::json!({}),
            "Show the balance?",
            &["triage"],
        ),
        SessionEvent::ToolReturned {
            call_id: "c3".into(),
            content: text("balance: 40.00"),
            ts: fixed_ts(7),
        },
    ]
}

/// Compare every field of two `ToolDeferred` events. The message of a failure
/// names the deferral (`label`) and the field.
fn assert_deferral(got: &SessionEvent, want: &SessionEvent, label: &str) {
    let SessionEvent::ToolDeferred {
        deferral_id,
        call_id,
        name,
        args,
        prompt,
        handoff_path,
        ts,
    } = got
    else {
        panic!("run_tool_deferred: ToolDeferred {label}: not a ToolDeferred event, got {got:?}");
    };
    let SessionEvent::ToolDeferred {
        deferral_id: w_deferral_id,
        call_id: w_call_id,
        name: w_name,
        args: w_args,
        prompt: w_prompt,
        handoff_path: w_handoff_path,
        ts: w_ts,
    } = want
    else {
        panic!("run_tool_deferred: the expected event of {label} is not a ToolDeferred event");
    };
    assert_eq!(
        deferral_id, w_deferral_id,
        "run_tool_deferred: ToolDeferred {label}: deferral_id"
    );
    assert_eq!(
        call_id, w_call_id,
        "run_tool_deferred: ToolDeferred {label}: call_id"
    );
    assert_eq!(
        name, w_name,
        "run_tool_deferred: ToolDeferred {label}: name"
    );
    assert_eq!(
        args, w_args,
        "run_tool_deferred: ToolDeferred {label}: args"
    );
    assert_eq!(
        prompt, w_prompt,
        "run_tool_deferred: ToolDeferred {label}: prompt"
    );
    assert_eq!(
        handoff_path, w_handoff_path,
        "run_tool_deferred: ToolDeferred {label}: handoff_path"
    );
    assert_eq!(ts, w_ts, "run_tool_deferred: ToolDeferred {label}: ts");
}

fn kinds(events: &[SessionEvent]) -> Vec<&'static str> {
    events.iter().map(SessionEvent::kind).collect()
}

/// Deferred tool calls survive storage.
///
/// The test appends a log with three deferred tool calls in two batches. It
/// checks that every field comes back unchanged, that `snapshot()` equals the
/// projection of the log, that `open_deferrals` finds `d1` and `d2`, and that a
/// watermark inside a batch reads correctly. The test calls the factory once.
pub async fn run_tool_deferred<F, Fut>(make: F)
where
    F: Fn() -> Fut,
    Fut: Future<Output = Arc<dyn Session>>,
{
    let s = make().await;
    let log = deferral_log();
    s.append(&log[0..5])
        .await
        .expect("run_tool_deferred: append batch 1 (events 0-4)");
    s.append(&log[5..8])
        .await
        .expect("run_tool_deferred: append batch 2 (events 5-7)");

    // 1. Round trip.
    let got = s
        .events(None)
        .await
        .expect("run_tool_deferred: events(None)");
    assert_eq!(got.len(), 8, "run_tool_deferred: events(None): event count");
    assert_eq!(
        kinds(&got),
        kinds(&log),
        "run_tool_deferred: events(None): event kinds"
    );
    for (pos, label) in [(2, "d1"), (4, "d2"), (6, "d3")] {
        assert_deferral(&got[pos], &log[pos], label);
    }
    assert_eq!(
        serde_json::to_value(&got).unwrap(),
        serde_json::to_value(&log).unwrap(),
        "run_tool_deferred: events(None): JSON of all events"
    );

    // 2. Projection.
    let snap = s.snapshot().await.expect("run_tool_deferred: snapshot()");
    assert_eq!(
        snap.messages.len(),
        5,
        "run_tool_deferred: snapshot().messages: item count"
    );
    assert_eq!(
        serde_json::to_value(&snap.messages).unwrap(),
        serde_json::to_value(&project(&log).messages).unwrap(),
        "run_tool_deferred: snapshot().messages: JSON"
    );

    // 3. Open deferrals: d1 and d2 are open, d3 is closed by event 7.
    let open = open_deferrals(&got);
    let want_open = vec![
        PendingToolCall::new(
            D1,
            "c1",
            "transfer_funds",
            effective_args(),
            D1_PROMPT,
            vec!["triage".to_owned(), "payments".to_owned()],
        ),
        PendingToolCall::new(
            D2,
            "c2",
            "close_account",
            serde_json::json!({ "account": "acct-9" }),
            "Close the account acct-9?",
            Vec::new(),
        ),
    ];
    assert_eq!(
        open, want_open,
        "run_tool_deferred: open_deferrals: [d1, d2] in log order"
    );
    assert_eq!(
        open[0].agent(),
        Some("payments"),
        "run_tool_deferred: open_deferrals: d1.agent()"
    );
    assert_eq!(
        open[1].agent(),
        None,
        "run_tool_deferred: open_deferrals: d2.agent()"
    );
    assert!(
        open.iter().all(|p| p.deferral_id() != D3),
        "run_tool_deferred: open_deferrals: d3 is closed"
    );

    // 4. Watermark. SequenceId(5) is inside the second append batch.
    let after4 = s
        .events(Some(SequenceId(4)))
        .await
        .expect("run_tool_deferred: events(Some(SequenceId(4)))");
    assert_eq!(
        kinds(&after4),
        ["tool_called", "tool_deferred", "tool_returned"],
        "run_tool_deferred: events(Some(SequenceId(4))): event kinds"
    );
    assert_deferral(&after4[1], &log[6], "d3 after SequenceId(4)");
    assert_eq!(
        serde_json::to_value(&after4).unwrap(),
        serde_json::to_value(&log[5..8]).unwrap(),
        "run_tool_deferred: events(Some(SequenceId(4))): JSON"
    );
    let after5 = s
        .events(Some(SequenceId(5)))
        .await
        .expect("run_tool_deferred: events(Some(SequenceId(5)))");
    assert_eq!(
        kinds(&after5),
        ["tool_deferred", "tool_returned"],
        "run_tool_deferred: events(Some(SequenceId(5))): event kinds"
    );
    assert_deferral(&after5[0], &log[6], "d3 after SequenceId(5)");
    assert_eq!(
        serde_json::to_value(&after5).unwrap(),
        serde_json::to_value(&log[6..8]).unwrap(),
        "run_tool_deferred: events(Some(SequenceId(5))): JSON"
    );
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
    run_tool_deferred(&make).await;
}
