//! Handler for the `GET /agents/{name}/runs/{id}/events` WebSocket endpoint.
//!
//! Implements the **404-before-upgrade** pattern: the agent name and run id are
//! validated against the registry *before* the HTTP connection is promoted to a
//! WebSocket. A missing or name-mismatched run returns a plain HTTP 404 without
//! initiating an upgrade handshake.
//!
//! Once the handshake succeeds, [`handle_socket`] replays all previously
//! recorded events from sequence 0 and then delivers live events in real time.
//! The stream closes naturally once the first terminal event is delivered.
//! Client disconnects are observed via the inbound half of the socket; they do
//! **not** cancel the underlying run.

use std::sync::Arc;

use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        Path, State,
    },
    response::Response,
};
use futures_util::StreamExt;
use uuid::Uuid;

use crate::{error::ServerError, event_log::is_terminal, registry::RunHandle, server::AppState};

/// `GET /agents/{name}/runs/{id}/events` — subscribe to run events via WebSocket.
///
/// Performs a 404 check *before* accepting the WebSocket upgrade:
///
/// - If `id` is not a valid UUID, returns `400 Bad Request`.
/// - If no run with `id` exists in the registry, or the run's agent name does
///   not match `name`, returns `404 Not Found`.
/// - Otherwise returns `101 Switching Protocols` and streams all events for the
///   run, starting from sequence 0, as JSON text frames.
///
/// The stream closes after the terminal event (`RunCompleted` / `RunFailed`) is
/// delivered. Client disconnects are observed and handled gracefully; they do
/// **not** cancel the underlying run (WS subscribers are read-only observers).
///
/// # Errors
///
/// - [`ServerError::BadRequest`] (400) — `id` is not a valid UUID.
/// - [`ServerError::UnknownAgent`] (404) — the run does not exist or is owned
///   by a different agent.
pub(crate) async fn events<Ctx: Send + Sync + 'static>(
    State(state): State<AppState<Ctx>>,
    Path((name, id)): Path<(String, String)>,
    ws: WebSocketUpgrade,
) -> Result<Response, ServerError> {
    // Parse the run id; a non-UUID string is a client error (400).
    let run_id = Uuid::parse_str(&id)
        .map_err(|_| ServerError::BadRequest(format!("invalid run id: {id}")))?;

    // Look up the run; absence or agent-name mismatch returns 404 without
    // upgrading the connection.
    let handle = state
        .registry
        .get(run_id)
        .filter(|h| h.agent_name == name)
        .ok_or_else(|| ServerError::UnknownAgent(format!("{name}/{id}")))?;

    // Run confirmed — accept the WebSocket upgrade.
    Ok(ws.on_upgrade(move |socket| handle_socket(socket, handle)))
}

/// Drive the WebSocket connection for a single run subscription.
///
/// Subscribes to the run's [`EventLog`](crate::event_log::EventLog) from
/// sequence 0, replaying any already-recorded events before switching to live
/// delivery. The loop ends when:
///
/// - The event stream ends (terminal event reached and delivered), or
/// - The client closes the connection (`socket.recv()` returns `None` or an
///   error).
///
/// Both halves of the socket are polled concurrently so that a client
/// disconnect is detected promptly even while events are still being replayed.
/// Disconnect does **not** cancel the run.
async fn handle_socket(mut socket: WebSocket, handle: Arc<RunHandle>) {
    let mut sub = handle.log.subscribe(0);
    let mut saw_terminal = false;

    loop {
        tokio::select! {
            // Next event from the log (replay + live tail).
            ev = sub.next() => {
                match ev {
                    Some(ev) => {
                        if is_terminal(&ev) {
                            saw_terminal = true;
                        }
                        let text = match serde_json::to_string(&ev) {
                            Ok(t) => t,
                            Err(_) => break,
                        };
                        if socket.send(Message::text(text)).await.is_err() {
                            break;
                        }
                    }
                    // Log stream ended. If no real terminal was delivered (start
                    // error / terminal-less stream), send a final synthetic
                    // `RunFailed` frame before the Close so the client always sees
                    // a terminal frame.
                    None => {
                        if let Some(frame) = handle.synthetic_terminal_frame(saw_terminal) {
                            if let Ok(text) = serde_json::to_string(&frame) {
                                let _ = socket.send(Message::text(text)).await;
                            }
                        }
                        let _ = socket.send(Message::Close(None)).await;
                        break;
                    }
                }
            }
            // Inbound frames from the client (drain to observe close/disconnect).
            msg = socket.recv() => {
                match msg {
                    // Client closed the connection or a network error occurred.
                    None | Some(Err(_)) => break,
                    // A graceful WebSocket close frame: stop sending immediately
                    // instead of waiting for the next send to fail.
                    Some(Ok(Message::Close(_))) => break,
                    // Ignore inbound data, ping, and pong frames.
                    Some(Ok(_)) => {}
                }
            }
        }
    }
    // Dropping the socket finalises the TCP-level close.
}
