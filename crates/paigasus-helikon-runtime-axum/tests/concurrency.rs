//! Concurrency and error-path integration tests.
//!
//! - [`start_error_returns_500_not_hang`] — a runner whose `run_streamed` returns `Err`
//!   immediately causes the one-shot handler to return 500, not hang.
//! - [`async_run_survives_creator_disconnect`] — an async run outlives its creator's
//!   HTTP connection.
//! - [`concurrent_same_session_serialize`] — two concurrent one-shot requests sharing
//!   the same `X-Session-Id` serialize completely: ticks are `[start, end, start, end]`.

mod support;

use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use futures_util::StreamExt;
use paigasus_helikon_runtime_axum::AgentServer;

/// A [`Runner`] whose `run_streamed` returns `Err` immediately must cause the
/// one-shot handler to return `500 Internal Server Error` within a reasonable
/// timeout, not hang indefinitely.
#[tokio::test]
async fn start_error_returns_500_not_hang() {
    let server = AgentServer::<()>::builder()
        .with_default_context()
        .runner(Arc::new(support::FailingRunner))
        .agent(Arc::new(support::ScriptedAgent {
            name: "echo".into(),
            events: support::echo_script(),
        }))
        .build()
        .expect("server builds");

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind ephemeral port");
    let addr = listener.local_addr().expect("local addr");
    tokio::spawn(async move {
        server
            .serve_with_listener(listener)
            .await
            .expect("serve loop");
    });

    let resp = tokio::time::timeout(
        Duration::from_secs(5),
        reqwest::Client::new()
            .post(format!("http://{addr}/agents/echo/runs"))
            .header("content-type", "application/json")
            .body(r#"{"input":"test"}"#)
            .send(),
    )
    .await
    .expect("request completed within 5 s")
    .expect("HTTP request succeeded");

    assert_eq!(
        resp.status(),
        500,
        "a start-error must yield 500, not 200 or a hang"
    );
}

/// A writer task whose event stream panics mid-run must still record terminal
/// state (via the `TerminalGuard` drop), so a one-shot request returns instead
/// of hanging forever.
#[tokio::test]
async fn panicking_stream_still_returns_not_hangs() {
    let server = AgentServer::<()>::builder()
        .with_default_context()
        .runner(Arc::new(support::PanicStreamRunner))
        .agent(Arc::new(support::ScriptedAgent {
            name: "echo".into(),
            events: vec![],
        }))
        .build()
        .expect("server builds");

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind ephemeral port");
    let addr = listener.local_addr().expect("local addr");
    tokio::spawn(async move {
        server
            .serve_with_listener(listener)
            .await
            .expect("serve loop");
    });

    let resp = tokio::time::timeout(
        Duration::from_secs(5),
        reqwest::Client::new()
            .post(format!("http://{addr}/agents/echo/runs"))
            .header("content-type", "application/json")
            .body(r#"{"input":"test"}"#)
            .send(),
    )
    .await
    .expect("request must complete within 5s, not hang on a panicking stream")
    .expect("HTTP request succeeded");

    // The run panicked before any terminal event, so the aggregated envelope
    // defaults to a 200 with status=failed; the key property is that it RETURNED.
    assert_eq!(resp.status(), 200);
}

/// Dropping the HTTP connection that created an async run must not cancel the
/// run. The run continues independently and is fully replayable via WebSocket.
#[tokio::test]
async fn async_run_survives_creator_disconnect() {
    let addr = support::spawn_echo_server().await;

    // POST ?mode=async and immediately drop the response (the HTTP connection
    // closes after the 202 body is read). The run must continue independently.
    let run_id = support::create_async_run(addr, "echo").await;

    // Give the scripted agent (synchronous) a moment to finish before we
    // subscribe. The subscribe path handles in-progress runs too, but this
    // avoids a spurious race on slow CI machines.
    tokio::time::sleep(Duration::from_millis(30)).await;

    let url = format!("ws://{addr}/agents/echo/runs/{run_id}/events");
    let (mut ws, _) = tokio_tungstenite::connect_async(url)
        .await
        .expect("WS handshake succeeds for a completed async run");

    // Bound the drain so a regression fails fast instead of hanging the suite.
    let got = tokio::time::timeout(Duration::from_secs(5), async {
        let mut got = Vec::new();
        while let Some(Ok(msg)) = ws.next().await {
            if msg.is_text() {
                got.push(support::parse_event(msg.to_text().unwrap()));
            }
        }
        got
    })
    .await
    .expect("WS drain must complete within 5s, not hang");

    assert_eq!(
        serde_json::to_value(&got).unwrap(),
        serde_json::to_value(support::echo_script()).unwrap(),
        "full event sequence must be replayable after the creator disconnects"
    );
}

/// Two concurrent one-shot requests sharing the same `X-Session-Id` must
/// serialize: the second run must not start until the first has completed.
///
/// The [`support::OrderingAgent`] records `[TICK_START, TICK_END]` per run
/// under the session lock. If runs interleave, the tick sequence would be
/// `[start, start, end, end]`; correct serialization yields
/// `[start, end, start, end]`.
#[tokio::test]
async fn concurrent_same_session_serialize() {
    let ticks: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));

    let server = AgentServer::<()>::builder()
        .with_default_context()
        .agent(Arc::new(support::OrderingAgent {
            name: "ordering".into(),
            ticks: Arc::clone(&ticks),
        }))
        .build()
        .expect("server builds");

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind ephemeral port");
    let addr = listener.local_addr().expect("local addr");
    tokio::spawn(async move {
        server
            .serve_with_listener(listener)
            .await
            .expect("serve loop");
    });

    let client = reqwest::Client::new();
    let make_req = || {
        client
            .post(format!("http://{addr}/agents/ordering/runs"))
            .header("content-type", "application/json")
            .header("x-session-id", "s1")
            .body(r#"{"input":"test"}"#)
            .send()
    };

    // Fire both requests truly concurrently and wait for both responses. The
    // timeout fails fast if a serialization regression deadlocks the pair.
    let (r1, r2) = tokio::time::timeout(Duration::from_secs(10), async {
        tokio::join!(make_req(), make_req())
    })
    .await
    .expect("both same-session requests must complete within 10s");
    assert_eq!(r1.unwrap().status(), 200, "first run should succeed");
    assert_eq!(r2.unwrap().status(), 200, "second run should succeed");

    let t = ticks.lock().unwrap();
    assert_eq!(
        *t,
        vec![
            support::TICK_START,
            support::TICK_END,
            support::TICK_START,
            support::TICK_END,
        ],
        "same-session runs must not interleave"
    );
}
