//! Handler for the `GET /agents/{name}/runs/{id}/events` WebSocket endpoint,
//! implemented over [`actix_ws`].
//!
//! Implements the **404-before-upgrade** pattern: the agent name, run id, and
//! owning principal are validated against the registry *before* the HTTP
//! connection is promoted to a WebSocket (via [`actix_ws::handle`]). A
//! malformed run id returns a plain HTTP 400. A missing run, a name mismatch,
//! or a run owned by a different principal all return a plain HTTP 404 — in
//! every case without initiating the upgrade handshake, and indistinguishably
//! from one another, so the endpoint cannot be used as an existence oracle for
//! harvested run ids.
//!
//! Once the handshake succeeds, a spawned task replays all previously recorded
//! events from sequence 0 and then delivers live events in real time. The stream
//! closes naturally once the first terminal event is delivered. Client
//! disconnects are observed via the inbound half of the socket; they do **not**
//! cancel the underlying run (WS subscribers are read-only observers, so no
//! cancel `DropGuard` is held).

use actix_web::{
    web::{self, Data},
    HttpRequest, HttpResponse,
};
use futures_util::StreamExt;
use uuid::Uuid;

use crate::{auth::Principal, error::ServerError, event_log::is_terminal, server::AppState};

/// `GET /agents/{name}/runs/{id}/events` — subscribe to run events via WebSocket.
///
/// Performs the 404 check *before* accepting the WebSocket upgrade:
///
/// - If `id` is not a valid UUID, returns `400 Bad Request`.
/// - If no run with `id` exists in the registry, the run's agent name does not
///   match `name`, or the run belongs to a different principal than the one
///   resolved for this request, returns `404 Not Found`.
/// - Otherwise returns `101 Switching Protocols` and streams all events for the
///   run, starting from sequence 0, as JSON text frames.
///
/// The stream closes after the terminal event (`RunCompleted` / `RunFailed`) is
/// delivered. If the run ends without a real terminal event (a start error or an
/// otherwise terminal-less stream), a synthetic `run_failed` frame is sent before
/// the Close, so the client always observes a terminal frame — mirroring the SSE
/// transport. Client disconnects are observed and handled gracefully; they do
/// **not** cancel the underlying run (WS subscribers are read-only observers).
///
/// # Errors
///
/// - [`ServerError::BadRequest`] (400) — `id` is not a valid UUID, or the request
///   was not a valid WebSocket upgrade.
/// - [`ServerError::UnknownAgent`] (404) — the run does not exist, is owned by
///   a different agent, or is owned by a different principal than the caller.
///   A run owned by a different principal is deliberately reported the same
///   way as a missing run: a distinct status would confirm the run id exists
///   and belongs to someone else, turning this endpoint into an existence
///   oracle for harvested run ids.
pub(crate) async fn events<Ctx: Send + Sync + 'static>(
    state: Data<AppState<Ctx>>,
    path: web::Path<(String, String)>,
    req: HttpRequest,
    body: web::Payload,
) -> Result<HttpResponse, ServerError> {
    let (name, id) = path.into_inner();

    // Parse the run id; a non-UUID string is a client error (400).
    let run_id = Uuid::parse_str(&id)
        .map_err(|_| ServerError::BadRequest(format!("invalid run id: {id}")))?;

    // Read the principal in a scoped block, matching the pattern in
    // `handlers/runs.rs`. There the scoping is load-bearing: that handler's
    // `Ref` would otherwise straddle `context.build(..).await`, and actix
    // request extensions are `RefCell`-backed with no `Send` bound on the
    // handler future, so holding a `Ref` across an `.await` compiles and then
    // panics at runtime with "already mutably borrowed". Here there is no
    // `.await` between this read and its use, and `actix_ws::handle` below
    // never touches request extensions (it only reads `req.head()` and the
    // `Sec-WebSocket-Protocol` header), so that panic path is not actually
    // live in this handler — the block is kept anyway for consistency with
    // the rest of the crate, not because removing it would reintroduce a bug.
    let principal: Option<String> = {
        use actix_web::HttpMessage as _;
        req.extensions().get::<Principal>().map(|p| p.0.clone())
    };

    // Absence, an agent-name mismatch, and a principal mismatch all return the
    // SAME 404 *before* the WebSocket upgrade is initiated. A distinct status
    // for the last case would confirm the run exists and belongs to someone
    // else — an existence oracle for harvested run ids.
    let handle = state
        .registry
        .get(run_id)
        .filter(|h| h.agent_name == name)
        .filter(|h| h.principal.as_deref() == principal.as_deref())
        .ok_or_else(|| ServerError::UnknownAgent(format!("{name}/{id}")))?;

    // Run confirmed — perform the WebSocket upgrade handshake. A failed upgrade
    // means the request itself was not a valid WebSocket upgrade — a client
    // error, not a server fault — so it maps to 400, not 500.
    let (response, mut session, mut msg_stream) = actix_ws::handle(&req, body)
        .map_err(|e| ServerError::BadRequest(format!("invalid websocket upgrade: {e}")))?;

    // Drive the subscription on a detached task. It is spawned on the worker's
    // `actix-rt` runtime (via `actix_web::rt::spawn`); the writer that feeds the
    // run's `EventLog` runs on the process-wide runtime, so this is the
    // cross-runtime `Notify` handoff. The task holds no cancel token — dropping
    // it on client disconnect leaves the run running (read-only observer).
    actix_web::rt::spawn(async move {
        let mut sub = handle.log.subscribe(0);
        let mut saw_terminal = false;

        loop {
            tokio::select! {
                // Next event from the log (replay + live tail).
                ev = sub.next() => match ev {
                    Some(ev) => {
                        if is_terminal(&ev) {
                            saw_terminal = true;
                        }
                        let Ok(text) = serde_json::to_string(&ev) else { break };
                        if session.text(text).await.is_err() {
                            // Client went away between polls; stop sending.
                            break;
                        }
                    }
                    // Log stream ended. If no real terminal was delivered (start
                    // error / terminal-less stream), send a final synthetic
                    // `RunFailed` frame before the Close so the client always sees
                    // a terminal frame, then close and finish.
                    None => {
                        if let Some(frame) = handle.synthetic_terminal_frame(saw_terminal) {
                            if let Ok(text) = serde_json::to_string(&frame) {
                                let _ = session.text(text).await;
                            }
                        }
                        let _ = session.close(None).await;
                        break;
                    }
                },
                // Inbound frames from the client (drain to observe close/disconnect).
                // Data and pong frames are ignored — this is a read-only observer —
                // but ping frames MUST be answered: unlike axum, whose tungstenite
                // layer replies automatically, `actix-ws` leaves pong to the
                // application, so an unanswered ping fails client keepalive.
                msg = msg_stream.next() => match msg {
                    None | Some(Err(_)) | Some(Ok(actix_ws::Message::Close(_))) => break,
                    Some(Ok(actix_ws::Message::Ping(bytes))) => {
                        if session.pong(&bytes).await.is_err() {
                            // Client went away between polls; stop sending.
                            break;
                        }
                    }
                    Some(Ok(_)) => {}
                }
            }
        }
        // Dropping `session`/`msg_stream` finalises the socket. The run is
        // untouched — no cancellation on disconnect.
    });

    Ok(response)
}
