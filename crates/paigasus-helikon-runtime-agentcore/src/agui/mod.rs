//! AG-UI protocol mode: SSE at `/invocations` and a WebSocket at `/ws`, on port 8080.
//!
//! AG-UI and the HTTP protocol are alternative `serverProtocol` settings for one
//! AgentCore container, so a deployment runs one or the other. They share port 8080 and
//! the `/invocations` path: a container configured `serverProtocol: HTTP` that calls
//! [`AgentCoreServer::serve_agui`] will serve traffic successfully but with the wrong
//! event vocabulary. Pick the mode that matches the runtime's configured protocol.

pub(crate) mod map;
pub(crate) mod sse;
pub(crate) mod types;
pub(crate) mod ws;

use axum::{
    routing::{get, post},
    Router,
};

use crate::{error::AgentCoreError, ping, server::AgentCoreServer};

/// Fixed bind address for AG-UI mode. The same port as the HTTP protocol, per AWS's
/// contract; the two are alternative protocols for one container, not concurrent ones.
const AGUI_ADDR: &str = "0.0.0.0:8080";

impl<Ctx: Send + Sync + 'static> AgentCoreServer<Ctx> {
    /// Build the AG-UI router: `POST /invocations` (SSE), `GET /ws`, and `GET /ping`.
    ///
    /// Pure: spawns nothing. Suitable for embedding or for testing with `tower`'s
    /// `ServiceExt::oneshot` (WebSocket upgrades excepted — those need a real listener).
    ///
    /// This router's `/invocations` and `/ping` paths collide with
    /// [`AgentCoreServer::router`]'s. `Router::merge` panics on overlapping routes, so
    /// merge one or the other into a larger app, never both.
    ///
    /// AG-UI's `/invocations` is **SSE only** — it does not honour
    /// `Accept: application/json`, because the AG-UI contract defines no buffered form.
    pub fn agui_router(&self) -> Router {
        Router::new()
            .route("/ping", get(ping::ping))
            .route("/invocations", post(sse::invocations::<Ctx>))
            .route("/ws", get(ws::ws_upgrade::<Ctx>))
            .with_state(self.state_for_agui())
    }

    /// Serve the configured agent over AG-UI: binds `0.0.0.0:8080` and serves
    /// [`AgentCoreServer::agui_router`] until the process is terminated.
    ///
    /// Logs `"ready in {ms}ms"` immediately after the listener is bound, exactly like
    /// [`AgentCoreServer::serve`].
    ///
    /// # Errors
    ///
    /// Returns [`AgentCoreError::Internal`] if binding the listener or the serve loop
    /// fails. A bind failure here most often means another mode is already on 8080 —
    /// AG-UI and the HTTP protocol cannot both run in one container.
    pub async fn serve_agui(self) -> Result<(), AgentCoreError> {
        let start = std::time::Instant::now();
        let router = self.agui_router();
        let listener = tokio::net::TcpListener::bind(AGUI_ADDR)
            .await
            .map_err(|e| {
                AgentCoreError::Internal(format!(
                    "failed to bind {AGUI_ADDR}: {e} \
                 (AG-UI and the HTTP protocol both use 8080; run only one per container)"
                ))
            })?;
        let elapsed_ms = start.elapsed().as_millis();
        tracing::info!(elapsed_ms = elapsed_ms as u64, "ready in {elapsed_ms}ms");
        axum::serve(listener, router)
            .await
            .map_err(|e| AgentCoreError::Internal(e.to_string()))
    }
}
