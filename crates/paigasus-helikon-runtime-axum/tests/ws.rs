//! Integration tests for the WebSocket run-events endpoint.

mod support;

use std::sync::Arc;

use futures_util::StreamExt;
use paigasus_helikon_core::AgentEvent;
use paigasus_helikon_runtime_axum::AgentServer;

/// **AC1** — connecting to an existing, already-completed run replays the full
/// event sequence and then closes the stream (server sends a Close frame once
/// the terminal event has been delivered).
#[tokio::test]
async fn ws_replays_completed_run_then_closes() {
    let addr = support::spawn_echo_server().await;
    let run_id = support::create_async_run(addr, "echo").await;

    // Small yield so the scripted agent (which completes synchronously) has time
    // to finish before we subscribe. The subscribe stream handles both in-progress
    // and completed runs, so this is a belt-and-suspenders courtesy.
    tokio::time::sleep(tokio::time::Duration::from_millis(20)).await;

    let url = format!("ws://{addr}/agents/echo/runs/{run_id}/events");
    let (mut ws, _) = tokio_tungstenite::connect_async(url)
        .await
        .expect("WS handshake should succeed for a known run");

    // Bound the collection so a regression (e.g. a missing Close frame) fails
    // fast instead of hanging the test indefinitely.
    let got = tokio::time::timeout(std::time::Duration::from_secs(5), async {
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

    // Full event sequence must be replayed, event-for-event.
    assert_eq!(
        serde_json::to_value(&got).unwrap(),
        serde_json::to_value(support::echo_script()).unwrap(),
    );
}

/// **AC2** — connecting to an unknown run id must fail the WebSocket handshake
/// (the server returns 404, not 101, so `connect_async` returns an error).
#[tokio::test]
async fn ws_unknown_id_404_before_upgrade() {
    let addr = support::spawn_echo_server().await;
    let url = format!("ws://{addr}/agents/echo/runs/{}/events", uuid::Uuid::nil());
    let err = tokio_tungstenite::connect_async(url)
        .await
        .expect_err("handshake should fail: server returns 404, not 101");
    assert_handshake_404(err);
}

/// Assert a failed WebSocket handshake was specifically an HTTP 404, not some
/// other transport-level failure.
fn assert_handshake_404(err: tokio_tungstenite::tungstenite::Error) {
    match err {
        tokio_tungstenite::tungstenite::Error::Http(resp) => {
            assert_eq!(resp.status(), 404, "handshake must fail with HTTP 404");
        }
        other => panic!("expected an HTTP 404 handshake failure, got: {other:?}"),
    }
}

/// A WebSocket connection that targets the correct run id but the wrong agent
/// name must fail the upgrade (server returns 404 before the 101 handshake).
#[tokio::test]
async fn ws_name_mismatch_404_before_upgrade() {
    let addr = support::spawn_echo_server().await;
    let run_id = support::create_async_run(addr, "echo").await;
    // The run exists (agent "echo"), but the URL references a different agent.
    let url = format!("ws://{addr}/agents/other/runs/{run_id}/events");
    let err = tokio_tungstenite::connect_async(url)
        .await
        .expect_err("agent-name mismatch should fail the WS handshake (404, not 101)");
    assert_handshake_404(err);
}

/// A start-erroring run, reached over WebSocket, must surface a final synthetic
/// `RunFailed` frame, then a Close.
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
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { server.serve_with_listener(listener).await.unwrap() });

    // Create the (start-erroring) run via async mode to obtain a run id; it stays
    // registered (TTL 300s) so the WS handshake's registry check passes.
    let run_id = support::create_async_run(addr, "echo").await;
    tokio::time::sleep(std::time::Duration::from_millis(30)).await;

    let url = format!("ws://{addr}/agents/echo/runs/{run_id}/events");
    let (mut ws, _) = tokio_tungstenite::connect_async(url)
        .await
        .expect("WS handshake should succeed for a registered run");
    let got = tokio::time::timeout(std::time::Duration::from_secs(5), async {
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

    assert_eq!(got.len(), 1, "exactly one synthetic terminal frame");
    assert!(
        matches!(&got[0], AgentEvent::RunFailed { error } if !error.is_empty()),
        "expected a non-empty RunFailed, got {:?}",
        got[0]
    );
}

/// A run that yields real events then ends with no terminal must get a final
/// synthetic `RunFailed` frame (generic message) over WebSocket, then a Close.
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
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { server.serve_with_listener(listener).await.unwrap() });

    let run_id = support::create_async_run(addr, "echo").await;
    tokio::time::sleep(std::time::Duration::from_millis(30)).await;

    let url = format!("ws://{addr}/agents/echo/runs/{run_id}/events");
    let (mut ws, _) = tokio_tungstenite::connect_async(url)
        .await
        .expect("WS handshake");
    let got = tokio::time::timeout(std::time::Duration::from_secs(5), async {
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

    assert_eq!(got.len(), 2);
    assert!(matches!(&got[0], AgentEvent::TokenDelta { text } if text == "hi"));
    assert!(
        matches!(&got[1], AgentEvent::RunFailed { error }
            if error == "run ended before producing a terminal event"),
        "expected generic RunFailed last, got {:?}",
        got[1]
    );
}
