//! Integration tests for the WebSocket run-events endpoint
//! (`GET /agents/{name}/runs/{id}/events`), driven over `actix-ws`.
//!
//! Mirrors the axum runtime's `tests/ws.rs` acceptance criteria, adapted to the
//! actix harness: the server is driven from a dedicated `actix_web::rt::System`
//! thread (see [`support`]), so the base handed back is an `http://…` URL that we
//! rewrite to `ws://…` before connecting with `tokio_tungstenite::connect_async`.

mod support;

use std::time::Duration;

use futures_util::StreamExt;
use paigasus_helikon_core::AgentEvent;
use paigasus_helikon_runtime_actix::AgentServer;
use std::sync::Arc;

/// Rewrite an `http://host:port` base URL (as returned by the actix harness) into
/// the `ws://host:port/agents/{name}/runs/{id}/events` WebSocket URL.
fn ws_url(base: &str, name: &str, run_id: &str) -> String {
    let host = base.strip_prefix("http://").unwrap_or(base);
    format!("ws://{host}/agents/{name}/runs/{run_id}/events")
}

/// Assert a failed WebSocket handshake was specifically the given HTTP status,
/// not some other transport-level failure — proving the server rejected the
/// request *before* upgrading (no `101 Switching Protocols`).
fn assert_handshake_status(err: tokio_tungstenite::tungstenite::Error, expected: u16) {
    match err {
        tokio_tungstenite::tungstenite::Error::Http(resp) => {
            assert_eq!(
                resp.status(),
                expected,
                "handshake must fail with HTTP {expected}"
            );
        }
        other => panic!("expected an HTTP {expected} handshake failure, got: {other:?}"),
    }
}

/// Drain a connected WebSocket to completion (until the server closes it),
/// collecting every text frame as a decoded [`AgentEvent`]. Bounded by a 5s
/// timeout so a regression (e.g. a missing Close frame) fails fast rather than
/// hanging the test.
async fn drain_events<S>(mut ws: S) -> Vec<AgentEvent>
where
    S: StreamExt<
            Item = Result<
                tokio_tungstenite::tungstenite::Message,
                tokio_tungstenite::tungstenite::Error,
            >,
        > + Unpin,
{
    tokio::time::timeout(Duration::from_secs(5), async {
        let mut got = Vec::new();
        while let Some(Ok(msg)) = ws.next().await {
            if msg.is_text() {
                got.push(support::parse_event(msg.to_text().unwrap()));
            }
        }
        got
    })
    .await
    .expect("WS stream must complete within 5s, not hang")
}

/// **AC — replay + tail.** Connecting to an existing, already-completed run
/// replays the full event sequence and then closes the stream (the server sends a
/// Close frame once the terminal event has been delivered).
#[tokio::test]
async fn ws_replays_completed_run_then_closes() {
    let base = support::spawn_echo_server();
    let run_id = support::create_async_run(&base, "echo").await;

    // Small yield so the scripted agent (which completes synchronously) has time
    // to finish before we subscribe. The subscribe stream handles both in-progress
    // and completed runs, so this is a belt-and-suspenders courtesy.
    tokio::time::sleep(Duration::from_millis(20)).await;

    let (ws, _) = tokio_tungstenite::connect_async(ws_url(&base, "echo", &run_id))
        .await
        .expect("WS handshake should succeed for a known run");

    let got = drain_events(ws).await;

    // Full event sequence must be replayed, event-for-event.
    assert_eq!(
        serde_json::to_value(&got).unwrap(),
        serde_json::to_value(support::echo_script()).unwrap(),
    );
}

/// **AC — 400 on a non-UUID id, before upgrade.** A malformed run id must fail
/// the WebSocket handshake with HTTP 400 (no `101` upgrade).
#[tokio::test]
async fn ws_bad_uuid_400_before_upgrade() {
    let base = support::spawn_echo_server();
    let err = tokio_tungstenite::connect_async(ws_url(&base, "echo", "not-a-uuid"))
        .await
        .expect_err("a non-UUID id should fail the WS handshake (400, not 101)");
    assert_handshake_status(err, 400);
}

/// **AC — 404 before upgrade (unknown run).** Connecting to a valid-but-unknown
/// run id must fail the WebSocket handshake (the server returns 404, not 101).
#[tokio::test]
async fn ws_unknown_id_404_before_upgrade() {
    let base = support::spawn_echo_server();
    let err =
        tokio_tungstenite::connect_async(ws_url(&base, "echo", &uuid::Uuid::nil().to_string()))
            .await
            .expect_err("handshake should fail: server returns 404, not 101");
    assert_handshake_status(err, 404);
}

/// **AC — 404 before upgrade (agent-name mismatch).** A connection that targets
/// the correct run id but the wrong agent name must fail the upgrade (404).
#[tokio::test]
async fn ws_name_mismatch_404_before_upgrade() {
    let base = support::spawn_echo_server();
    let run_id = support::create_async_run(&base, "echo").await;
    // The run exists (agent "echo"), but the URL references a different agent.
    let err = tokio_tungstenite::connect_async(ws_url(&base, "other", &run_id))
        .await
        .expect_err("agent-name mismatch should fail the WS handshake (404, not 101)");
    assert_handshake_status(err, 404);
}

/// **AC — synthetic terminal (start error).** A start-erroring run, reached over
/// WebSocket, must surface a final synthetic `RunFailed` frame, then a Close.
#[tokio::test]
async fn ws_emits_synthetic_run_failed_on_start_error() {
    let server = AgentServer::<()>::builder()
        .with_default_context()
        .runner(Arc::new(support::FailingRunner))
        .agent(Arc::new(support::ScriptedAgent {
            name: "echo".into(),
            events: support::echo_script(),
        }))
        .build()
        .expect("server builds");
    let base = support::spawn_actix_server(server);

    // Create the (start-erroring) run via async mode to obtain a run id; it stays
    // registered (TTL 300s) so the WS handshake's registry check passes.
    let run_id = support::create_async_run(&base, "echo").await;
    tokio::time::sleep(Duration::from_millis(30)).await;

    let (ws, _) = tokio_tungstenite::connect_async(ws_url(&base, "echo", &run_id))
        .await
        .expect("WS handshake should succeed for a registered run");
    let got = drain_events(ws).await;

    assert_eq!(got.len(), 1, "exactly one synthetic terminal frame");
    assert!(
        matches!(&got[0], AgentEvent::RunFailed { error } if !error.is_empty()),
        "expected a non-empty RunFailed, got {:?}",
        got[0]
    );
}

/// **AC — synthetic terminal (terminal-less stream).** A run that yields real
/// events then ends with no terminal must get a final synthetic `RunFailed` frame
/// (generic message) over WebSocket, then a Close.
#[tokio::test]
async fn ws_emits_synthetic_run_failed_after_terminalless_stream() {
    let server = AgentServer::<()>::builder()
        .with_default_context()
        .runner(Arc::new(support::PartialThenEndRunner))
        .agent(Arc::new(support::ScriptedAgent {
            name: "echo".into(),
            events: vec![],
        }))
        .build()
        .expect("server builds");
    let base = support::spawn_actix_server(server);

    let run_id = support::create_async_run(&base, "echo").await;
    tokio::time::sleep(Duration::from_millis(30)).await;

    let (ws, _) = tokio_tungstenite::connect_async(ws_url(&base, "echo", &run_id))
        .await
        .expect("WS handshake");
    let got = drain_events(ws).await;

    assert_eq!(got.len(), 2);
    assert!(matches!(&got[0], AgentEvent::TokenDelta { text } if text == "hi"));
    assert!(
        matches!(&got[1], AgentEvent::RunFailed { error }
            if error == "run ended before producing a terminal event"),
        "expected generic RunFailed last, got {:?}",
        got[1]
    );
}

/// **AC — read-only observer.** A WebSocket client disconnecting must NOT cancel
/// the underlying run: the WS handler holds no cancel `DropGuard`.
///
/// A `hanging` agent emits `RunStarted` then hangs for 30s before it would emit a
/// terminal event. Client A subscribes, sees `RunStarted`, then disconnects
/// mid-run. Client B then re-subscribes: it must still replay `RunStarted` and
/// then observe silence (the run is still hanging — not cancelled). If A's
/// disconnect had cancelled the run, the writer would finalize and B would see a
/// terminal frame + Close instead of a live, tailing stream.
#[tokio::test]
async fn ws_disconnect_does_not_cancel_run() {
    let (started_tx, mut started_rx) = tokio::sync::mpsc::unbounded_channel();
    let (base, _sessions) = support::spawn_hanging_server(started_tx);
    let run_id = support::create_async_run(&base, "hanging").await;

    // Wait until the run has demonstrably started server-side.
    tokio::time::timeout(Duration::from_secs(10), started_rx.recv())
        .await
        .expect("timed out waiting for the run to start")
        .expect("agent signalled run start");

    let url = ws_url(&base, "hanging", &run_id);

    // Client A: subscribe, receive RunStarted, then disconnect mid-run.
    {
        let (mut ws_a, _) = tokio_tungstenite::connect_async(&url)
            .await
            .expect("client A WS handshake");
        let first = tokio::time::timeout(Duration::from_secs(2), ws_a.next())
            .await
            .expect("client A receives a frame within 2s")
            .expect("client A stream not closed")
            .expect("client A frame ok");
        assert!(
            matches!(
                support::parse_event(first.to_text().unwrap()),
                AgentEvent::RunStarted { .. }
            ),
            "client A should first receive RunStarted"
        );
        // ws_a dropped here — a real mid-run disconnect.
    }

    // Client B: re-subscribe. The run must still be live.
    let (mut ws_b, _) = tokio_tungstenite::connect_async(&url)
        .await
        .expect("client B WS handshake");
    let first = tokio::time::timeout(Duration::from_secs(2), ws_b.next())
        .await
        .expect("client B receives a replay frame within 2s")
        .expect("client B stream open")
        .expect("client B frame ok");
    assert!(
        matches!(
            support::parse_event(first.to_text().unwrap()),
            AgentEvent::RunStarted { .. }
        ),
        "client B should replay RunStarted (the run is still registered)"
    );

    // The run is still hanging: client B must NOT receive any further frame (no
    // terminal, no Close) within a 1s window. A returned frame here would mean
    // A's disconnect cancelled/finalized the run — a read-only violation.
    let next = tokio::time::timeout(Duration::from_secs(1), ws_b.next()).await;
    assert!(
        next.is_err(),
        "run must still be live after a WS disconnect (read-only observer), but client B saw {next:?}"
    );
}

/// **Regression — inbound ping is answered with a pong.** `actix-ws` leaves pong
/// to the application, unlike axum, whose tungstenite layer replies
/// automatically. Without an explicit reply a client keepalive goes unanswered
/// and the peer tears the connection down as dead. Uses the hanging agent so the
/// socket stays open long enough to round-trip the frame.
#[tokio::test]
async fn ws_answers_client_ping_with_pong() {
    use futures_util::SinkExt as _;
    use tokio_tungstenite::tungstenite::Message;

    let (started_tx, mut started_rx) = tokio::sync::mpsc::unbounded_channel();
    let (base, _sessions) = support::spawn_hanging_server(started_tx);
    let run_id = support::create_async_run(&base, "hanging").await;

    tokio::time::timeout(Duration::from_secs(10), started_rx.recv())
        .await
        .expect("timed out waiting for the run to start")
        .expect("agent signalled run start");

    let (mut ws, _) = tokio_tungstenite::connect_async(ws_url(&base, "hanging", &run_id))
        .await
        .expect("WS handshake");

    let payload: Vec<u8> = b"helikon".to_vec();
    ws.send(Message::Ping(payload.clone().into()))
        .await
        .expect("send ping");

    // The replayed `RunStarted` text frame may arrive first; skip non-pong frames.
    let pong = tokio::time::timeout(Duration::from_secs(5), async {
        while let Some(Ok(msg)) = ws.next().await {
            if let Message::Pong(bytes) = msg {
                return Some(bytes);
            }
        }
        None
    })
    .await
    .expect("a pong must arrive within 5s — the server ignored the ping")
    .expect("socket closed before a pong arrived");

    assert_eq!(
        pong.as_ref(),
        payload.as_slice(),
        "the pong must echo the ping payload back verbatim (RFC 6455 §5.5.3)"
    );
}
