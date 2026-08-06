//! Multi-worker de-risking spike for the shared-runtime design (SMA-343 Task 7).
//!
//! This is the GO/NO-GO checkpoint for decision 6: the writer task runs on the
//! process-wide runtime while the response subscriber runs on a per-worker
//! `actix-rt` runtime, so every one-shot run already exercises the cross-runtime
//! `EventLog` `Notify` handoff.
//!
//! - [`multi_worker_oneshot_correctness`] — 4 concurrent one-shot runs against the
//!   default (multi-worker) server all return `completed` (GO gate #1).
//! - [`concurrent_same_session_serialize`] — two concurrent same-`X-Session-Id`
//!   requests serialize via the shared [`SessionLocks`]: ticks are
//!   `[start, end, start, end]` (GO gate #2).
//! - [`oneshot_client_disconnect_behavior`] — INVESTIGATION (not a gate): does
//!   actix cancel a one-shot run when the client disconnects mid-run? Asserts the
//!   actual observed behavior; see the test body for the finding.
//! - [`ws_subscribe_receives_full_sequence_cross_runtime`] — a WebSocket
//!   subscriber on a per-worker `actix-rt` runtime receives the full event
//!   sequence for an async run whose writer lives on the shared runtime (the same
//!   cross-runtime `Notify` handoff, exercised over the WS transport).

mod support;

use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use futures_util::StreamExt as _;
use paigasus_helikon_runtime_actix::{AgentServer, SessionKey, SessionProvider};

/// **GO gate #1 — one-shot correctness under the default multi-worker server.**
///
/// Fire four concurrent one-shot echo runs against a server whose workers each
/// have their own `actix-rt` runtime. Each run's writer executes on the shared
/// process-wide runtime and its response subscriber on the receiving worker, so
/// every request round-trips the cross-runtime event-log handoff. All four must
/// aggregate to `completed`.
#[tokio::test]
async fn multi_worker_oneshot_correctness() {
    let base = support::spawn_echo_server();
    let client = reqwest::Client::new();

    let reqs = (0..4).map(|_| {
        let client = client.clone();
        let base = base.clone();
        async move {
            client
                .post(format!("{base}/agents/echo/runs"))
                .header("content-type", "application/json")
                .body(r#"{"input":"hello"}"#)
                .send()
                .await
                .expect("request completes")
        }
    });

    let responses = tokio::time::timeout(
        Duration::from_secs(10),
        futures_util::future::join_all(reqs),
    )
    .await
    .expect("all four concurrent runs complete within 10s");

    for resp in responses {
        assert_eq!(resp.status(), 200, "each concurrent run returns 200");
        let v: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(v["status"], "completed", "each concurrent run completes");
        assert_eq!(v["output"], "echo");
    }
}

/// **GO gate #2 — same-session serialization across workers.**
///
/// Two concurrent one-shot requests sharing the same `X-Session-Id` must
/// serialize: the second run must not start until the first has completed. The
/// [`support::OrderingAgent`] records `[TICK_START, TICK_END]` per run while its
/// writer holds the shared per-session lock. If runs interleave the ticks would
/// be `[start, start, end, end]`; correct serialization yields
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
    let base = support::spawn_actix_server(server);

    let client = reqwest::Client::new();
    let make_req = || {
        client
            .post(format!("{base}/agents/ordering/runs"))
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

/// **INVESTIGATION (not a gate)** — does actix cancel a one-shot run when the
/// client disconnects mid-run?
///
/// The one-shot handler holds a `DropGuard` over the run's cancel token while it
/// awaits the terminal event. In the axum runtime a client disconnect drops the
/// handler future, which drops that guard and cancels the run (SMA-456 lock-in).
/// actix-web's dispatcher has a different lifecycle, so this test observes and
/// asserts the ACTUAL behavior rather than assuming parity.
///
/// The [`support::SignallingHangingAgent`] hangs 30s after signalling start. The
/// writer runs on the shared runtime regardless of the client, so:
/// - if the disconnect cancels the run, the writer's controlled stream ends
///   almost immediately and the session is finalized within a second;
/// - if it does not, the session stays empty until the 30s hang elapses.
///
/// A 3s poll window is a robust discriminator against the 30s hang.
///
/// FINDING: see the assertion below — actix-web does NOT cancel the one-shot
/// handler future on client disconnect. The run is driven to completion by the
/// writer on the shared runtime independent of the client connection. This is an
/// ACCEPTABLE documented divergence from axum; the SMA-343 acceptance criteria do
/// not require disconnect-cancellation.
#[tokio::test]
async fn oneshot_client_disconnect_behavior() {
    use tokio::io::AsyncWriteExt as _;

    let (started_tx, mut started_rx) = tokio::sync::mpsc::unbounded_channel();
    let (base, sessions) = support::spawn_hanging_server(started_tx);
    let addr = base
        .strip_prefix("http://")
        .expect("base url has http:// prefix")
        .to_owned();

    let session_id = "sma343-actix-disconnect";
    let body = r#"{"input":"hi"}"#;
    let len = body.len();
    let request = format!(
        "POST /agents/hanging/runs HTTP/1.1\r\n\
         Host: {addr}\r\n\
         Content-Type: application/json\r\n\
         X-Session-Id: {session_id}\r\n\
         Content-Length: {len}\r\n\
         Connection: close\r\n\
         \r\n\
         {body}"
    );

    {
        let mut client = tokio::net::TcpStream::connect(&addr).await.unwrap();
        client.write_all(request.as_bytes()).await.unwrap();

        // Wait until the run has demonstrably started server-side, then let
        // `client` drop at the end of this block — a real mid-run disconnect.
        tokio::time::timeout(Duration::from_secs(10), started_rx.recv())
            .await
            .expect("timed out waiting for the run to start")
            .expect("agent signalled run start");
    }

    // Observe whether the run is cancelled + finalized quickly after disconnect.
    // No `AuthLayer` on this server, so the run resolved its session under the
    // principal-less key — look it up the same way.
    let session = sessions
        .session(SessionKey::new(None, Some(session_id)))
        .await
        .unwrap();
    let cancelled_and_finalized = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let snapshot = session.snapshot().await.unwrap();
            if !snapshot.messages.is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .is_ok();

    eprintln!(
        "INVESTIGATION oneshot_client_disconnect: cancelled_and_finalized_within_3s = {cancelled_and_finalized}"
    );

    // actix-web does not tie handler-future cancellation to client disconnect
    // the way axum/hyper does, so the run keeps running on the shared runtime and
    // the session is NOT finalized inside the 3s window.
    assert!(
        !cancelled_and_finalized,
        "unexpected: actix cancelled+finalized the one-shot run on disconnect within 3s — \
         the documented divergence (no disconnect-cancel) no longer holds; update the finding"
    );
}

/// **Cross-runtime WebSocket subscribe.**
///
/// On the default multi-worker server, create an async run (its writer runs on
/// the shared process-wide runtime) and then WebSocket-subscribe from a per-worker
/// `actix-rt` runtime. The subscription may land on a different worker than the
/// one that accepted the create request, so the full event sequence arriving over
/// the socket proves the `EventLog` `Notify` handoff works across the two runtimes
/// via the WS transport (not just the one-shot/SSE bodies).
#[tokio::test]
async fn ws_subscribe_receives_full_sequence_cross_runtime() {
    let base = support::spawn_echo_server();
    let run_id = support::create_async_run(&base, "echo").await;

    let host = base.strip_prefix("http://").unwrap_or(&base);
    let url = format!("ws://{host}/agents/echo/runs/{run_id}/events");
    let (mut ws, _) = tokio_tungstenite::connect_async(url)
        .await
        .expect("WS handshake should succeed for a known run");

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
    .expect("WS stream must complete within 5s, not hang");

    assert_eq!(
        serde_json::to_value(&got).unwrap(),
        serde_json::to_value(support::echo_script()).unwrap(),
        "the full event sequence must arrive over the cross-runtime WS subscription"
    );
}
