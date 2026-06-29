//! Integration tests for the WebSocket run-events endpoint.

mod support;

use futures_util::StreamExt;

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

    let mut got = Vec::new();
    while let Some(Ok(msg)) = ws.next().await {
        if msg.is_text() {
            got.push(support::parse_event(msg.to_text().unwrap()));
        }
    }

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
    let result = tokio_tungstenite::connect_async(url).await;
    assert!(
        result.is_err(),
        "handshake should fail: server returns 404, not 101"
    );
}

/// A WebSocket connection that targets the correct run id but the wrong agent
/// name must fail the upgrade (server returns 404 before the 101 handshake).
#[tokio::test]
async fn ws_name_mismatch_404_before_upgrade() {
    let addr = support::spawn_echo_server().await;
    let run_id = support::create_async_run(addr, "echo").await;
    // The run exists (agent "echo"), but the URL references a different agent.
    let url = format!("ws://{addr}/agents/other/runs/{run_id}/events");
    let result = tokio_tungstenite::connect_async(url).await;
    assert!(
        result.is_err(),
        "agent-name mismatch should fail the WS handshake (server returns 404, not 101)"
    );
}
